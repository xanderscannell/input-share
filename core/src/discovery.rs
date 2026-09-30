// LAN discovery. A running server broadcasts a UDP beacon once a second; a
// client listens and lists the hosts it heard from recently. Beacons are
// untrusted (anyone on the LAN can send one): they only help pick an address,
// and connecting still requires the Noise handshake with the shared key.
//
// Beacon: `input-share/1 <tcp-port> <fingerprint> <hostname>`, one datagram.

use crate::net::Key;
use blake2::{Blake2s256, Digest};
use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const DISCOVERY_PORT: u16 = 24801;
pub const INTERVAL: Duration = Duration::from_secs(1);
/// A host disappears from the list this long after its last beacon.
pub const EXPIRY: Duration = Duration::from_secs(3);
const PREFIX: &str = "input-share/1";
const MAX_HOSTNAME: usize = 64;

/// First 8 bytes of BLAKE2s(key) as 16 lowercase hex characters. Safe to
/// show and broadcast: it reveals nothing useful about a 256-bit random key.
pub fn fingerprint(key: &Key) -> String {
    Blake2s256::digest(key)[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// `1a2b3c4d5e6f7a8b` as `1a2b 3c4d 5e6f 7a8b`, for reading aloud.
pub fn grouped(fp: &str) -> String {
    fp.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Beacon {
    pub port: u16,
    pub fingerprint: String,
    pub hostname: String,
}

impl Beacon {
    pub fn to_bytes(&self) -> Vec<u8> {
        format!("{PREFIX} {} {} {}", self.port, self.fingerprint, self.hostname).into_bytes()
    }

    /// Parse an untrusted datagram. Rejects anything malformed. The hostname
    /// is cleaned: control characters dropped, at most 64 characters.
    pub fn parse(data: &[u8]) -> Option<Beacon> {
        let text = std::str::from_utf8(data).ok()?;
        let mut parts = text.splitn(4, ' ');
        if parts.next()? != PREFIX {
            return None;
        }
        let port: u16 = parts.next()?.parse().ok().filter(|&p| p != 0)?;
        let fingerprint = parts.next()?;
        if fingerprint.len() != 16 || !fingerprint.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let hostname: String =
            parts.next().unwrap_or("").chars().filter(|c| !c.is_control()).take(MAX_HOSTNAME).collect();
        let hostname = hostname.trim().to_string();
        Some(Beacon { port, fingerprint: fingerprint.to_string(), hostname })
    }
}

/// A host heard recently, ready to show in a list.
#[derive(Debug, Clone, PartialEq)]
pub struct Host {
    /// Sender IP with the beacon's TCP port: what to connect to.
    pub addr: SocketAddr,
    pub hostname: String,
    pub fingerprint: String,
    /// Its fingerprint matches our key, so the handshake will succeed.
    pub paired: bool,
}

/// Beacons seen, keyed by sender IP. Time is passed in so expiry is testable.
#[derive(Default)]
pub struct Seen {
    by_ip: HashMap<IpAddr, (Beacon, Instant)>,
}

impl Seen {
    /// Record a datagram; garbage is ignored. Returns whether it was a beacon.
    pub fn record(&mut self, from: IpAddr, data: &[u8], now: Instant) -> bool {
        match Beacon::parse(data) {
            Some(b) => {
                self.by_ip.insert(from, (b, now));
                true
            }
            None => false,
        }
    }

    /// Hosts heard within `EXPIRY`: paired first, then by hostname and address.
    pub fn hosts(&self, now: Instant, own_fingerprint: &str) -> Vec<Host> {
        let mut hosts: Vec<Host> = self
            .by_ip
            .iter()
            .filter(|(_, (_, at))| now.saturating_duration_since(*at) < EXPIRY)
            .map(|(ip, (b, _))| Host {
                addr: SocketAddr::new(*ip, b.port),
                hostname: b.hostname.clone(),
                fingerprint: b.fingerprint.clone(),
                paired: b.fingerprint == own_fingerprint,
            })
            .collect();
        hosts.sort_by(|a, b| (!a.paired, &a.hostname, a.addr).cmp(&(!b.paired, &b.hostname, b.addr)));
        hosts
    }
}

/// This machine's name for the beacon.
pub fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".into())
}

/// Sends a beacon every `INTERVAL` until stopped.
pub struct Announcer {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Start beaconing from `bind` to `to` (real use: `0.0.0.0:0` to
/// `255.255.255.255:24801`; tests: 127.0.0.1 only).
pub fn announce(bind: SocketAddr, to: SocketAddr, beacon: Beacon) -> io::Result<Announcer> {
    let sock = UdpSocket::bind(bind)?;
    sock.set_broadcast(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let stop = stop.clone();
        let data = beacon.to_bytes();
        thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let _ = sock.send_to(&data, to); // a lost beacon is fine; the next one follows
                let until = Instant::now() + INTERVAL;
                while Instant::now() < until && !stop.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(20));
                }
            }
        })
    };
    Ok(Announcer { stop, thread: Some(thread) })
}

impl Announcer {
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Announcer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Listens for beacons in the background until stopped.
pub struct Listener {
    seen: Arc<Mutex<Seen>>,
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Listen on `bind` (real use: `0.0.0.0:24801`; tests: `127.0.0.1:0`).
pub fn listen(bind: SocketAddr) -> io::Result<Listener> {
    let sock = UdpSocket::bind(bind)?;
    let addr = sock.local_addr()?;
    // Wake regularly so stop() is noticed without a datagram arriving.
    sock.set_read_timeout(Some(Duration::from_millis(100)))?;
    let seen: Arc<Mutex<Seen>> = Default::default();
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let (seen, stop) = (seen.clone(), stop.clone());
        thread::spawn(move || {
            let mut buf = [0u8; 512];
            while !stop.load(Ordering::SeqCst) {
                if let Ok((n, from)) = sock.recv_from(&mut buf) {
                    seen.lock().unwrap().record(from.ip(), &buf[..n], Instant::now());
                }
            }
        })
    };
    Ok(Listener { seen, addr, stop, thread: Some(thread) })
}

impl Listener {
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn hosts(&self, own_fingerprint: &str) -> Vec<Host> {
        self.seen.lock().unwrap().hosts(Instant::now(), own_fingerprint)
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::keygen;

    fn beacon(fp: &str, host: &str) -> Beacon {
        Beacon { port: 24800, fingerprint: fp.into(), hostname: host.into() }
    }

    const FP_A: &str = "0123456789abcdef";
    const FP_B: &str = "fedcba9876543210";

    #[test]
    fn fingerprint_is_stable_short_hex_and_key_specific() {
        let k = keygen();
        let fp = fingerprint(&k);
        assert_eq!(fp, fingerprint(&k));
        assert_eq!(fp.len(), 16);
        assert!(fp.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_ne!(fp, fingerprint(&keygen()));
        // Known answer, so a change of hash or truncation is caught.
        assert_eq!(fingerprint(&[0u8; 32]), "320b5ea99e653bc2");
        assert_eq!(grouped(FP_A), "0123 4567 89ab cdef");
    }

    #[test]
    fn beacon_round_trips_and_rejects_garbage() {
        let b = beacon(FP_A, "DESKTOP-01");
        assert_eq!(Beacon::parse(&b.to_bytes()), Some(b));
        assert_eq!(Beacon::parse(b"input-share/1 24800 0123456789abcdef My PC").unwrap().hostname, "My PC");
        for bad in [
            &b""[..],
            b"input-share/2 24800 0123456789abcdef x",
            b"input-share/1 0 0123456789abcdef x",
            b"input-share/1 70000 0123456789abcdef x",
            b"input-share/1 24800 0123456789ABCDEF x",
            b"input-share/1 24800 0123456789abcde x",
            b"input-share/1 24800",
            b"\xff\xfe garbage",
        ] {
            assert_eq!(Beacon::parse(bad), None, "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn hostname_is_cleaned() {
        let long = format!("input-share/1 1 {FP_A} {}", "x".repeat(200));
        assert_eq!(Beacon::parse(long.as_bytes()).unwrap().hostname.len(), 64);
        let nasty = format!("input-share/1 1 {FP_A} a\u{7}b\r\nc\t");
        assert_eq!(Beacon::parse(nasty.as_bytes()).unwrap().hostname, "abc");
    }

    #[test]
    fn seen_expires_marks_paired_and_sorts_paired_first() {
        let t0 = Instant::now();
        let mut s = Seen::default();
        let (ip1, ip2): (IpAddr, IpAddr) = ("10.0.0.5".parse().unwrap(), "10.0.0.9".parse().unwrap());
        assert!(s.record(ip1, &beacon(FP_B, "aaa").to_bytes(), t0));
        assert!(s.record(ip2, &beacon(FP_A, "zzz").to_bytes(), t0 + Duration::from_secs(1)));
        assert!(!s.record(ip1, b"garbage", t0 + Duration::from_secs(1)));

        let hosts = s.hosts(t0 + Duration::from_secs(2), FP_A);
        assert_eq!(hosts.len(), 2);
        assert_eq!((hosts[0].hostname.as_str(), hosts[0].paired), ("zzz", true)); // paired first
        assert_eq!(hosts[0].addr, "10.0.0.9:24800".parse().unwrap());
        assert!(!hosts[1].paired);

        // ip1's last beacon was at t0: gone at t0 + 3 s; ip2's at t0 + 1 s: still there.
        let hosts = s.hosts(t0 + EXPIRY, FP_A);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].hostname, "zzz");
        assert!(s.hosts(t0 + Duration::from_secs(4), FP_A).is_empty());

        // A fresh beacon brings a host back.
        s.record(ip1, &beacon(FP_B, "aaa").to_bytes(), t0 + Duration::from_secs(4));
        assert_eq!(s.hosts(t0 + Duration::from_secs(4), FP_A).len(), 1);
    }

    #[test]
    fn beacons_travel_over_127_0_0_1() {
        let key = keygen();
        let listener = listen("127.0.0.1:0".parse().unwrap()).unwrap();
        let to = listener.local_addr();

        // Garbage first: must not show up as a host.
        let junk = UdpSocket::bind("127.0.0.1:0").unwrap();
        junk.send_to(b"hello there", to).unwrap();

        let b = Beacon { port: 5555, fingerprint: fingerprint(&key), hostname: "TESTHOST".into() };
        let announcer = announce("127.0.0.1:0".parse().unwrap(), to, b).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let hosts = loop {
            let h = listener.hosts(&fingerprint(&key));
            if !h.is_empty() || Instant::now() > deadline {
                break h;
            }
            thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(hosts.len(), 1, "{hosts:?}");
        assert_eq!(hosts[0].addr, "127.0.0.1:5555".parse().unwrap());
        assert_eq!(hosts[0].hostname, "TESTHOST");
        assert!(hosts[0].paired);
        assert!(!listener.hosts(&fingerprint(&keygen()))[0].paired);

        let t = Instant::now();
        announcer.stop();
        listener.stop();
        assert!(t.elapsed() < Duration::from_millis(500), "stop took {:?}", t.elapsed());
    }
}
