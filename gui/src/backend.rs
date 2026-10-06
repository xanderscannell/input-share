// Everything the GUI's commands do, without Tauri, so it can be tested
// headless. `main.rs` wraps each method in a `#[tauri::command]`.
//
// Demo mode (`--demo`) never installs hooks, never injects and never leaves
// 127.0.0.1: sharing runs a scripted server; browsing starts two fake peers
// (one with our key, one without) that beacon to a loopback listener; the
// client's input goes to a sink that drops it. Its home folder is a temp dir.

use input_share_core::client::{self, ClientHandle};
use input_share_core::clipboard;
use input_share_core::config::{self, Home};
use input_share_core::discovery::{self, Announcer, Beacon, Listener};
use input_share_core::edge::{Edge, Rect};
use input_share_core::layout::Layout;
use input_share_core::net::{self, Key};
use input_share_core::server::{self, ServerHandle};
use input_share_core::status::{OnStatus, Status};
use input_share_core::win;
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;

const DEMO_SCREEN: Rect = Rect { left: 0, top: 0, w: 1920, h: 1080 };

/// Status as the web UI receives it (a Tauri event payload).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StatusEvent {
    pub kind: &'static str,
    pub detail: String,
}

impl From<Status> for StatusEvent {
    fn from(s: Status) -> Self {
        let (kind, detail) = match s {
            Status::Listening(a) => ("listening", a),
            Status::Connecting(a) => ("connecting", a),
            Status::Retrying(why) => ("retrying", why),
            Status::Connected(peer) => ("connected", peer),
            Status::Remote => ("remote", String::new()),
            Status::Local => ("local", String::new()),
            Status::Disconnected(why) => ("disconnected", why),
            Status::Stopped => ("stopped", String::new()),
        };
        StatusEvent { kind, detail }
    }
}

#[derive(Debug, Serialize)]
pub struct Boot {
    pub demo: bool,
    /// `--demo-state`: render this canned state (screenshots), no backend calls.
    pub demo_state: Option<String>,
    /// `--demo-theme`: force light or dark instead of following the system.
    pub theme: Option<String>,
    pub fingerprint: Option<String>,
    pub hostname: String,
    pub edge: String,
    pub port: u16,
    pub host: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct HostView {
    pub addr: String,
    pub name: String,
    pub fingerprint: String,
    pub paired: bool,
}

/// Fake peers for demo browsing, kept alive while browsing.
struct DemoPeers {
    _server: ServerHandle,
    _paired: Announcer,
    _stranger: Announcer,
}

pub struct Backend {
    home: Home,
    demo: bool,
    demo_state: Option<String>,
    theme: Option<String>,
    on_status: OnStatus,
    server: Option<ServerHandle>,
    client: Option<ClientHandle>,
    listener: Option<Listener>,
    announcer: Option<Announcer>,
    peers: Option<DemoPeers>,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// This machine's LAN address, to show the other computer where to connect.
/// A UDP "connect" only picks the outgoing interface; no packet is sent.
fn lan_ip() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?; // TEST-NET-1: never routed, never contacted
    s.local_addr().ok().map(|a| a.ip()).filter(|ip| !ip.is_unspecified())
}

impl Backend {
    pub fn new(home: Home, demo: bool, demo_state: Option<String>, theme: Option<String>, on_status: OnStatus) -> Self {
        Backend {
            home,
            demo,
            demo_state,
            theme,
            on_status,
            server: None,
            client: None,
            listener: None,
            announcer: None,
            peers: None,
        }
    }

    fn key(&self) -> Result<Key, String> {
        self.home.load_key().map_err(err)?.ok_or_else(|| "Make or import a key first.".to_string())
    }

    fn edge(&self) -> Edge {
        match self.home.load_config().ok().as_ref().and_then(|c| c.get(config::EDGE)) {
            Some("left") => Edge::Left,
            _ => Edge::Right,
        }
    }

    fn port(&self) -> u16 {
        let c = self.home.load_config().unwrap_or_default();
        c.get(config::PORT).and_then(|p| p.parse().ok()).unwrap_or(net::DEFAULT_PORT)
    }

    pub fn boot(&self) -> Result<Boot, String> {
        let c = self.home.load_config().map_err(err)?;
        // A corrupt key file shows as no fingerprint; the Keys screen reports it.
        let fingerprint = self.home.load_key().ok().flatten().map(|k| discovery::grouped(&discovery::fingerprint(&k)));
        Ok(Boot {
            demo: self.demo,
            demo_state: self.demo_state.clone(),
            theme: self.theme.clone(),
            fingerprint,
            hostname: discovery::hostname(),
            edge: if self.edge() == Edge::Left { "left" } else { "right" }.into(),
            port: self.port(),
            host: c.get(config::HOST).unwrap_or("").into(),
        })
    }

    pub fn start_sharing(&mut self) -> Result<String, String> {
        if self.server.is_some() {
            return Err("Already sharing.".into());
        }
        let key = self.key()?;
        if self.demo {
            let s = server::start("127.0.0.1:0", key, self.edge(), Layout::single(DEMO_SCREEN), false, self.on_status.clone())
                .map_err(err)?;
            let addr = s.local_addr().to_string();
            self.server = Some(s);
            return Ok(addr);
        }

        // Real: hooks on, listening on the LAN, beaconing so clients can find us.
        let port = self.port();
        let s = server::start(&format!("0.0.0.0:{port}"), key, self.edge(), win::layout(), true, self.on_status.clone())
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::AddrInUse => format!("Port {port} is already in use. Choose another in Settings."),
                _ => e.to_string(),
            })?;
        let beacon = Beacon { port, fingerprint: discovery::fingerprint(&key), hostname: discovery::hostname() };
        let broadcast = SocketAddr::from(([255, 255, 255, 255], discovery::DISCOVERY_PORT));
        // Discovery is a convenience: if beaconing fails, typing the address still works.
        self.announcer = discovery::announce("0.0.0.0:0".parse().unwrap(), broadcast, beacon).ok();
        self.server = Some(s);
        Ok(match lan_ip() {
            Some(ip) => format!("{ip}:{port}"),
            None => format!("port {port}"),
        })
    }

    pub fn stop_sharing(&mut self) {
        self.announcer = None; // Drop stops it
        if let Some(s) = self.server.take() {
            s.stop();
        }
    }

    pub fn start_browsing(&mut self) -> Result<(), String> {
        if self.listener.is_some() {
            return Ok(());
        }
        if !self.demo {
            let bind = SocketAddr::from(([0, 0, 0, 0], discovery::DISCOVERY_PORT));
            self.listener = Some(discovery::listen(bind).map_err(|e| match e.kind() {
                std::io::ErrorKind::AddrInUse => {
                    "Another program is using the discovery port. Type the other computer's address instead.".to_string()
                }
                _ => e.to_string(),
            })?);
            return Ok(());
        }
        let key = self.key()?;
        let listener = discovery::listen("127.0.0.1:0".parse().unwrap()).map_err(err)?;
        let to = listener.local_addr();
        let peer = server::start("127.0.0.1:0", key, self.edge(), Layout::single(DEMO_SCREEN), false, Arc::new(|_| {}))
            .map_err(err)?;
        let paired = discovery::announce(
            "127.0.0.1:0".parse().unwrap(),
            to,
            Beacon { port: peer.local_addr().port(), fingerprint: discovery::fingerprint(&key), hostname: "DESKTOP-DEMO".into() },
        )
        .map_err(err)?;
        // A second loopback address, so the listener sees a different sender.
        let stranger = discovery::announce(
            "127.0.0.2:0".parse().unwrap(),
            to,
            Beacon {
                port: net::DEFAULT_PORT,
                fingerprint: discovery::fingerprint(&net::keygen()),
                hostname: "OFFICE-PC".into(),
            },
        )
        .map_err(err)?;
        self.listener = Some(listener);
        self.peers = Some(DemoPeers { _server: peer, _paired: paired, _stranger: stranger });
        Ok(())
    }

    pub fn hosts(&self) -> Result<Vec<HostView>, String> {
        let Some(listener) = &self.listener else { return Ok(vec![]) };
        let own = self.home.load_key().ok().flatten().map(|k| discovery::fingerprint(&k)).unwrap_or_default();
        Ok(listener
            .hosts(&own)
            .into_iter()
            .map(|h| HostView {
                addr: h.addr.to_string(),
                name: h.hostname,
                fingerprint: discovery::grouped(&h.fingerprint),
                paired: h.paired,
            })
            .collect())
    }

    pub fn stop_browsing(&mut self) {
        self.listener = None; // Drop stops it
        // Demo: the client may be connected to the fake host; it goes on disconnect.
        if self.client.is_none() {
            self.peers = None;
        }
    }

    pub fn connect(&mut self, addr: &str) -> Result<(), String> {
        self.disconnect();
        let key = self.key()?;
        addr.parse::<SocketAddr>().map_err(|_| format!("{addr} is not an address like 192.168.1.20:24800."))?;
        let (layout, sink, clip): (client::LayoutFn, client::BoxSink, _) = if self.demo {
            (Box::new(|| Layout::single(DEMO_SCREEN)), Box::new(|_| {}), None) // demo: input goes nowhere
        } else {
            (Box::new(win::layout), Box::new(client::send_input_sink()), Some(clipboard::WINDOWS))
        };
        let c = client::start(addr, key, self.edge(), layout, sink, clip, self.on_status.clone());
        self.client = Some(c);
        let mut cfg = self.home.load_config().map_err(err)?;
        cfg.set(config::HOST, addr).map_err(err)?;
        self.home.save_config(&cfg).map_err(err)
    }

    pub fn disconnect(&mut self) {
        if let Some(c) = self.client.take() {
            c.stop();
        }
        // Demo: done with the fake hosts unless the host list is still open
        // (connect disconnects first, while the list is open).
        if self.listener.is_none() {
            self.peers = None;
        }
    }

    pub fn key_generate(&mut self, replace: bool) -> Result<String, String> {
        let k = self.home.generate_key(replace).map_err(err)?;
        Ok(discovery::grouped(&discovery::fingerprint(&k)))
    }

    pub fn key_import(&mut self, text: &str, replace: bool) -> Result<String, String> {
        let k = self.home.import_key(text, replace).map_err(|e| match e.kind() {
            std::io::ErrorKind::InvalidData => "That is not a key. A key is 64 characters, 0 to 9 and a to f.".into(),
            _ => e.to_string(),
        })?;
        Ok(discovery::grouped(&discovery::fingerprint(&k)))
    }

    /// Show key.hex in Explorer so it can be copied to the other computer.
    pub fn key_reveal(&self) -> Result<String, String> {
        let path = self.home.key_path();
        if !self.demo {
            std::process::Command::new("explorer.exe").arg(format!("/select,{}", path.display())).spawn().map_err(err)?;
        }
        Ok(path.display().to_string())
    }

    pub fn save_settings(&mut self, edge: &str, port: u16) -> Result<(), String> {
        // A running session keeps the settings it started with, so changing
        // them now would silently do nothing until the next start.
        if self.is_running() || self.listener.is_some() {
            return Err("Stop sharing or disconnect first, then change settings.".into());
        }
        if !matches!(edge, "left" | "right") {
            return Err("Choose left or right.".into());
        }
        if port < 1024 {
            return Err("Use a port from 1024 to 65535.".into());
        }
        let mut c = self.home.load_config().map_err(err)?;
        c.set(config::EDGE, edge).map_err(err)?;
        c.set(config::PORT, &port.to_string()).map_err(err)?;
        self.home.save_config(&c).map_err(err)
    }

    /// Sharing or connected (or trying to connect): closing the window should
    /// hide it to the tray instead of quitting.
    pub fn is_running(&self) -> bool {
        self.server.is_some() || self.client.is_some()
    }

    /// Stop everything (window closed or app quitting).
    pub fn shutdown(&mut self) {
        self.disconnect();
        self.stop_sharing();
        self.stop_browsing();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::thread;
    use std::time::{Duration, Instant};

    fn demo() -> (Backend, Arc<Mutex<Vec<StatusEvent>>>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("input-share-gui-test-{}-{:?}", std::process::id(), thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = Home::at(&dir).unwrap();
        home.generate_key(false).unwrap();
        let log: Arc<Mutex<Vec<StatusEvent>>> = Default::default();
        let l = log.clone();
        let b = Backend::new(home, true, None, None, Arc::new(move |s| l.lock().unwrap().push(s.into())));
        (b, log, dir)
    }

    fn wait_for(what: &str, cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn demo_sharing_starts_and_stops() {
        let (mut b, log, dir) = demo();
        assert!(!b.is_running());
        let addr = b.start_sharing().unwrap();
        assert!(addr.starts_with("127.0.0.1:"), "{addr}");
        assert!(b.is_running(), "closing the window must now hide it to the tray");
        assert!(b.start_sharing().is_err(), "second start must be refused");
        b.stop_sharing();
        assert!(!b.is_running());
        let kinds: Vec<_> = log.lock().unwrap().iter().map(|e| e.kind).collect();
        assert_eq!(kinds.first(), Some(&"listening"));
        assert_eq!(kinds.last(), Some(&"stopped"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn demo_browsing_finds_two_hosts_and_connects_to_the_paired_one() {
        let (mut b, log, dir) = demo();
        b.start_browsing().unwrap();
        wait_for("two hosts", || b.hosts().unwrap().len() == 2);
        let hosts = b.hosts().unwrap();
        assert!(hosts[0].paired && hosts[0].name == "DESKTOP-DEMO", "{hosts:?}");
        assert!(!hosts[1].paired && hosts[1].name == "OFFICE-PC", "{hosts:?}");

        b.connect(&hosts[0].addr).unwrap();
        b.stop_browsing(); // what the GUI does next: the fake host must stay up
        wait_for("connected", || log.lock().unwrap().iter().any(|e| e.kind == "connected"));
        assert_eq!(b.boot().unwrap().host, hosts[0].addr, "the chosen host is remembered");
        b.shutdown();
        assert_eq!(log.lock().unwrap().last().map(|e| e.kind), Some("stopped"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn keys_and_settings_validate_input() {
        let (mut b, _log, dir) = demo();
        assert!(b.key_generate(false).is_err(), "replacing needs confirmation");
        let fp = b.key_generate(true).unwrap();
        assert_eq!(fp.len(), 19, "grouped: four groups of four and three spaces");
        assert!(b.key_import("nonsense", true).unwrap_err().contains("64 characters"));
        assert!(b.connect("not an address").is_err());
        assert!(b.save_settings("up", 24800).is_err());
        assert!(b.save_settings("left", 80).is_err());
        b.save_settings("left", 25000).unwrap();
        let boot = b.boot().unwrap();
        assert_eq!((boot.edge.as_str(), boot.port), ("left", 25000));

        // Locked while anything is running: sharing, and looking for hosts.
        b.start_sharing().unwrap();
        assert!(b.save_settings("right", 24800).is_err());
        b.stop_sharing();
        b.start_browsing().unwrap();
        assert!(b.save_settings("right", 24800).is_err());
        b.stop_browsing();
        b.save_settings("right", 24800).unwrap();
        assert_eq!(b.boot().unwrap().edge, "right", "a refused save changes nothing, a later one works");
        let _ = std::fs::remove_dir_all(dir);
    }
}
