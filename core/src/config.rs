// The GUI's home folder: `key.hex` and `config.txt`. The CLI keeps its own
// `--key` path and does not use this.
//
// config.txt is `key=value` lines. Unknown lines and comments are kept on
// rewrite, so a hand edit or a newer version's settings survive.

use crate::net::{self, Key};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const ROLE: &str = "role";
pub const HOST: &str = "host";
pub const PORT: &str = "port";
pub const EDGE: &str = "edge";

/// Where the home folder is: `INPUT_SHARE_HOME` if set, else `%APPDATA%\input-share`.
/// Pure, so the precedence is testable without touching the environment.
pub fn resolve(override_dir: Option<OsString>, appdata: Option<OsString>) -> Option<PathBuf> {
    match override_dir.filter(|d| !d.is_empty()) {
        Some(d) => Some(PathBuf::from(d)),
        None => appdata.filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join("input-share")),
    }
}

/// Write via a temp file and rename, so a crash mid-write never leaves a
/// half-written key or config behind.
fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

pub struct Home {
    dir: PathBuf,
}

impl Home {
    /// The user's home folder, created if missing.
    pub fn user() -> io::Result<Home> {
        let dir = resolve(std::env::var_os("INPUT_SHARE_HOME"), std::env::var_os("APPDATA"))
            .ok_or_else(|| io::Error::other("neither INPUT_SHARE_HOME nor APPDATA is set"))?;
        Home::at(dir)
    }

    /// A specific folder, created if missing (tests use a temp dir).
    pub fn at(dir: impl Into<PathBuf>) -> io::Result<Home> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Home { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn key_path(&self) -> PathBuf {
        self.dir.join("key.hex")
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir.join("config.txt")
    }

    /// The stored key, or None if there is none yet.
    pub fn load_key(&self) -> io::Result<Option<Key>> {
        match fs::read_to_string(self.key_path()) {
            Ok(text) => net::key_from_hex(&text).map(Some),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Store `key`. Replacing an existing key breaks the pairing with the
    /// other machine, so it needs `replace`; otherwise AlreadyExists.
    fn store_key(&self, key: &Key, replace: bool) -> io::Result<()> {
        if !replace && self.key_path().exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a key already exists; replacing it breaks the pairing with the other machine",
            ));
        }
        write_atomic(&self.key_path(), &net::key_to_hex(key))
    }

    pub fn generate_key(&self, replace: bool) -> io::Result<Key> {
        let key = net::keygen();
        self.store_key(&key, replace)?;
        Ok(key)
    }

    /// Import a key pasted as hex or read from another machine's key.hex.
    /// Malformed input is rejected before anything is written.
    pub fn import_key(&self, hex: &str, replace: bool) -> io::Result<Key> {
        let key = net::key_from_hex(hex)?;
        self.store_key(&key, replace)?;
        Ok(key)
    }

    pub fn load_config(&self) -> io::Result<Config> {
        match fs::read_to_string(self.config_path()) {
            Ok(text) => Ok(Config::parse(&text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e),
        }
    }

    pub fn save_config(&self, config: &Config) -> io::Result<()> {
        write_atomic(&self.config_path(), &config.to_text())
    }
}

/// `key=value` lines, in file order, with everything else kept verbatim.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Config {
    lines: Vec<String>,
}

fn split(line: &str) -> Option<(&str, &str)> {
    if line.trim_start().starts_with('#') {
        return None;
    }
    line.split_once('=').map(|(k, v)| (k.trim(), v.trim()))
}

impl Config {
    pub fn parse(text: &str) -> Config {
        Config { lines: text.lines().map(str::to_string).collect() }
    }

    pub fn to_text(&self) -> String {
        self.lines.iter().map(|l| format!("{l}\n")).collect()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines.iter().find_map(|l| split(l).filter(|(k, _)| *k == key).map(|(_, v)| v))
    }

    /// Set a value, replacing the key's line in place or appending one.
    /// A line break in the key or value would corrupt the file, so it errors.
    pub fn set(&mut self, key: &str, value: &str) -> io::Result<()> {
        if [key, value].iter().any(|s| s.contains(['\n', '\r'])) || key.contains('=') || key.trim().is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("bad config entry {key:?}")));
        }
        let line = format!("{key}={value}");
        match self.lines.iter().position(|l| split(l).is_some_and(|(k, _)| k == key)) {
            Some(i) => self.lines[i] = line,
            None => self.lines.push(line),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh temp folder per test; removed when dropped.
    struct TempHome(PathBuf);

    impl TempHome {
        fn new() -> TempHome {
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!("input-share-test-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            TempHome(dir.join("nested"))
        }
        fn home(&self) -> Home {
            Home::at(&self.0).unwrap()
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    #[test]
    fn resolve_prefers_the_override() {
        let (o, a) = (Some(OsString::from("D:\\custom")), Some(OsString::from("C:\\Users\\x\\AppData\\Roaming")));
        assert_eq!(resolve(o.clone(), a.clone()), Some(PathBuf::from("D:\\custom")));
        assert_eq!(resolve(None, a.clone()), Some(PathBuf::from("C:\\Users\\x\\AppData\\Roaming\\input-share")));
        assert_eq!(resolve(Some(OsString::new()), a), Some(PathBuf::from("C:\\Users\\x\\AppData\\Roaming\\input-share")));
        assert_eq!(resolve(None, None), None);
    }

    #[test]
    fn home_is_created_and_starts_empty() {
        let t = TempHome::new();
        let home = t.home();
        assert!(home.dir().is_dir(), "nested folder must be created");
        assert_eq!(home.load_key().unwrap(), None);
        assert_eq!(home.load_config().unwrap(), Config::default());
    }

    #[test]
    fn config_round_trip_keeps_unknown_lines_and_order() {
        let t = TempHome::new();
        let home = t.home();
        let text = "# written by hand\nrole=server\nfuture_setting = 42\n\nport=24800\n";
        std::fs::write(home.config_path(), text).unwrap();

        let mut c = home.load_config().unwrap();
        assert_eq!(c.get(ROLE), Some("server"));
        assert_eq!(c.get("future_setting"), Some("42"));
        assert_eq!(c.get(HOST), None);
        assert_eq!(c.get("# written by hand"), None);

        c.set(ROLE, "client").unwrap();
        c.set(HOST, "192.168.1.20").unwrap();
        home.save_config(&c).unwrap();
        assert_eq!(
            fs::read_to_string(home.config_path()).unwrap(),
            "# written by hand\nrole=client\nfuture_setting = 42\n\nport=24800\nhost=192.168.1.20\n"
        );
        assert_eq!(home.load_config().unwrap(), c);
        assert!(!home.dir().join("config.tmp").exists(), "temp file must be renamed away");
    }

    #[test]
    fn config_rejects_entries_that_would_corrupt_the_file() {
        let mut c = Config::default();
        assert!(c.set(HOST, "a\nrole=server").is_err());
        assert!(c.set("a=b", "x").is_err());
        assert!(c.set(" ", "x").is_err());
        assert!(c.set(HOST, "ok\r").is_err());
        assert_eq!(c, Config::default());
    }

    #[test]
    fn keys_generate_import_and_refuse_silent_replacement() {
        let t = TempHome::new();
        let home = t.home();
        let first = home.generate_key(false).unwrap();
        assert_eq!(home.load_key().unwrap(), Some(first));

        // Replacing needs the explicit flag, and a refusal changes nothing.
        let err = home.generate_key(false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(home.import_key(&net::key_to_hex(&net::keygen()), false).is_err());
        assert_eq!(home.load_key().unwrap(), Some(first));

        // Malformed hex is rejected before anything is written, even with replace.
        for bad in ["", "abcd", &"zz".repeat(32), &"a".repeat(65)] {
            assert!(home.import_key(bad, true).is_err(), "{bad:?}");
        }
        assert_eq!(home.load_key().unwrap(), Some(first));

        // Import as pasted from the other machine (whitespace and CRLF tolerated).
        let other = net::keygen();
        let pasted = format!("  {}\r\n", net::key_to_hex(&other));
        assert_eq!(home.import_key(&pasted, true).unwrap(), other);
        assert_eq!(home.load_key().unwrap(), Some(other));

        let replaced = home.generate_key(true).unwrap();
        assert_ne!(replaced, other);
        assert_eq!(home.load_key().unwrap(), Some(replaced));
    }

    #[test]
    fn a_corrupt_key_file_is_an_error_not_a_missing_key() {
        let t = TempHome::new();
        let home = t.home();
        fs::write(home.key_path(), "not a key").unwrap();
        assert!(home.load_key().is_err());
    }
}
