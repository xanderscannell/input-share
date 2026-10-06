// Files and folders copied to the clipboard, packed into one archive for a
// transfer and unpacked on the other computer.
//
// Archive: per entry, a u32 LE path length, the path (UTF-8, `/` between
// folders, the first part the copied item's own name), then 0 for a folder,
// or 1, a u32 LE length and the file's bytes. Folders come before what they hold.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn too_big(limit: usize) -> io::Error {
    io::Error::other(format!("the files are over the {limit}-byte limit"))
}

/// Pack these files and folders, folders with everything in them. Fails as
/// soon as the archive passes `limit`, so a huge folder is not read in full.
// ponytail: links (and junctions) are skipped, not followed, since one can
// loop back on its own folder; follow them with loop detection if that matters.
pub fn pack(paths: &[PathBuf], limit: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    for p in paths {
        let name = p.file_name().and_then(|n| n.to_str());
        let name = name.ok_or_else(|| io::Error::other(format!("{}: a name that cannot be shared", p.display())))?;
        pack_one(p, name.to_string(), &mut out, limit)?;
    }
    Ok(out)
}

fn pack_one(path: &Path, rel: String, out: &mut Vec<u8>, limit: usize) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if (out.len() + rel.len()) as u64 + meta.len() > limit as u64 {
        return Err(too_big(limit));
    }
    out.extend((rel.len() as u32).to_le_bytes());
    out.extend(rel.as_bytes());
    if meta.is_dir() {
        out.push(0);
        let mut children = fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()?;
        children.sort_by_key(|e| e.file_name());
        for e in children {
            let name = e.file_name();
            let name = name.to_str().ok_or_else(|| io::Error::other(format!("{}: a name that cannot be shared", e.path().display())))?;
            pack_one(&e.path(), format!("{rel}/{name}"), out, limit)?;
        }
    } else {
        let data = fs::read(path)?;
        if out.len() + 5 + data.len() > limit {
            return Err(too_big(limit));
        }
        out.push(1);
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(data);
    }
    Ok(())
}

/// A file or folder name that stays inside the folder it is written to, and
/// that Windows stores as given.
fn safe_name(n: &str) -> bool {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2",
        "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    // Windows drops trailing dots and spaces, which would make ".." of "...".
    let device = n.split('.').next().unwrap_or("").trim_end();
    !n.is_empty()
        && !n.ends_with(['.', ' '])
        && !n.chars().any(|c| c < ' ' || r#"<>:"/\|?*"#.contains(c))
        && !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(device))
}

fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("bad file archive: {why}"))
}

/// Empty `dir` (the last files received go), then unpack the archive into
/// it. Returns the top-level files and folders, to put on the clipboard.
pub fn unpack(mut b: &[u8], dir: &Path) -> io::Result<Vec<PathBuf>> {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir)?;
    let u32_at = |b: &[u8]| -> io::Result<usize> {
        Ok(u32::from_le_bytes(b.get(..4).ok_or_else(|| bad("cut short"))?.try_into().unwrap()) as usize)
    };
    let mut top = Vec::new();
    while !b.is_empty() {
        let len = u32_at(b)?;
        let rel = b[4..].get(..len).ok_or_else(|| bad("cut short"))?;
        let rel = std::str::from_utf8(rel).map_err(|_| bad("a name is not UTF-8"))?;
        let parts: Vec<&str> = rel.split('/').collect();
        if !parts.iter().all(|p| safe_name(p)) {
            return Err(bad(&format!("unsafe name {rel:?}")));
        }
        let path = parts.iter().fold(dir.to_path_buf(), |p, part| p.join(part));
        b = &b[4 + len..];
        match b.split_first() {
            Some((0, rest)) => {
                fs::create_dir_all(&path)?;
                b = rest;
            }
            Some((1, rest)) => {
                let n = u32_at(rest)?;
                let data = rest[4..].get(..n).ok_or_else(|| bad("cut short"))?;
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&path, data)?;
                b = &rest[4 + n..];
            }
            _ => return Err(bad("unknown entry")),
        }
        let first = dir.join(parts[0]);
        if !top.contains(&first) {
            top.push(first);
        }
    }
    Ok(top)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("input-share-files-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn folders_and_files_round_trip() {
        let src = scratch("src");
        fs::write(src.join("notes.txt"), "hello").unwrap();
        fs::create_dir_all(src.join("folder/empty")).unwrap();
        fs::write(src.join("folder/inner.bin"), [0u8, 1, 255]).unwrap();

        let archive = pack(&[src.join("notes.txt"), src.join("folder")], 1 << 20).unwrap();
        let dst = scratch("dst");
        fs::write(dst.join("stale.txt"), "from last time").unwrap();
        let top = unpack(&archive, &dst).unwrap();
        assert_eq!(top, [dst.join("notes.txt"), dst.join("folder")]);
        assert_eq!(fs::read_to_string(dst.join("notes.txt")).unwrap(), "hello");
        assert_eq!(fs::read(dst.join("folder/inner.bin")).unwrap(), [0, 1, 255]);
        assert!(dst.join("folder/empty").is_dir());
        assert!(!dst.join("stale.txt").exists(), "the last files received are cleared");

        assert!(pack(&[src.join("folder")], 20).is_err(), "over the limit");
        assert!(unpack(&archive[..archive.len() - 1], &dst).is_err(), "cut short");
        let _ = fs::remove_dir_all(src);
        let _ = fs::remove_dir_all(dst);
    }

    #[test]
    fn names_that_escape_or_misbehave_are_refused() {
        for ok in ["a.txt", "My File (2).docx", ".gitignore", "console.txt"] {
            assert!(safe_name(ok), "{ok}");
        }
        for bad in ["", ".", "..", "...", "a ", "a.", "C:x", "a\\b", "nul", "CON.txt", "com1 .log", "x\u{1}"] {
            assert!(!safe_name(bad), "{bad:?}");
        }
        let dst = scratch("evil");
        let mut archive = Vec::new();
        let rel = "ok/../../escaped.txt";
        archive.extend((rel.len() as u32).to_le_bytes());
        archive.extend(rel.as_bytes());
        archive.extend([1, 1, 0, 0, 0, b'x']);
        assert!(unpack(&archive, &dst).is_err());
        assert!(!dst.parent().unwrap().join("escaped.txt").exists());
        let _ = fs::remove_dir_all(dst);
    }
}
