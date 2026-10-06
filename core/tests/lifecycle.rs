// Start and stop a server (scripted input) and a client (collecting sink)
// in-process over 127.0.0.1. Nothing touches the real mouse or keyboard.

use input_share_core::client::{self, Act};
use input_share_core::clipboard::Format;
use input_share_core::edge::{Edge, Rect};
use input_share_core::layout::Layout;
use input_share_core::net::{self, keygen};
use input_share_core::proto::Msg;
use input_share_core::server::{self, Input};
use input_share_core::status::{OnStatus, Status};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const DESK: Rect = Rect { left: 0, top: 0, w: 1920, h: 1080 };
const LAPTOP: Rect = Rect { left: 0, top: 0, w: 2560, h: 1600 };
const A_DOWN: Act = Act::Input(Msg::Key { scancode: 0x1E, extended: false, down: true });
const A_UP: Act = Act::Input(Msg::Key { scancode: 0x1E, extended: false, down: false });

type Log<T> = Arc<Mutex<Vec<T>>>;

fn laptop() -> client::LayoutFn {
    Box::new(|| Layout::single(LAPTOP))
}

fn recorder() -> (OnStatus, Log<Status>) {
    let log: Log<Status> = Default::default();
    let l = log.clone();
    (Arc::new(move |s| l.lock().unwrap().push(s)), log)
}

fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn has<T>(log: &Log<T>, f: impl Fn(&T) -> bool) -> bool {
    log.lock().unwrap().iter().any(f)
}

/// Server and client connected, control on the client, with A held there.
fn connected_with_a_held() -> (server::ServerHandle, client::ClientHandle, Log<Status>, Log<Status>, Log<Act>) {
    let key = keygen();
    let (s_on, s_log) = recorder();
    let server = server::start("127.0.0.1:0", key, Edge::Right, Layout::single(DESK), false, None, s_on).unwrap();
    let addr = server.local_addr().to_string();

    let acts: Log<Act> = Default::default();
    let a = acts.clone();
    let (c_on, c_log) = recorder();
    let client = client::start(&addr, key, Edge::Right, laptop(), Box::new(move |act| a.lock().unwrap().push(act)), None, c_on);

    wait_for("server Connected", || has(&s_log, |s| matches!(s, Status::Connected(_))));
    server.input(Input::Move { x: 1919, y: 540 });
    server.input(Input::Key { scancode: 0x1E, extended: false, down: true });
    wait_for("A down on the client", || has(&acts, |a| *a == A_DOWN));
    wait_for("client Remote", || has(&c_log, |s| *s == Status::Remote));
    wait_for("server Remote", || has(&s_log, |s| *s == Status::Remote));
    (server, client, s_log, c_log, acts)
}

fn assert_port_free(addr: &str) {
    TcpListener::bind(addr).unwrap_or_else(|e| panic!("port {addr} still taken after stop: {e}"));
}

#[test]
fn stopping_the_client_releases_keys_and_reports() {
    let (server, client, s_log, c_log, acts) = connected_with_a_held();
    let addr = server.local_addr().to_string();

    let t = Instant::now();
    client.stop();
    assert!(t.elapsed() < Duration::from_secs(2), "client stop took {:?}", t.elapsed());
    assert_eq!(acts.lock().unwrap().last(), Some(&A_UP), "release-all must run on stop");
    let c = c_log.lock().unwrap().clone();
    assert!(c.iter().any(|s| matches!(s, Status::Disconnected(_))), "{c:?}");
    assert!(c.contains(&Status::Local), "{c:?}");
    assert_eq!(c.last(), Some(&Status::Stopped));

    // The server notices the client left and hands control back.
    wait_for("server Disconnected", || has(&s_log, |s| matches!(s, Status::Disconnected(_))));
    wait_for("server Local", || has(&s_log, |s| *s == Status::Local));

    let t = Instant::now();
    server.stop();
    assert!(t.elapsed() < Duration::from_secs(2), "server stop took {:?}", t.elapsed());
    assert_eq!(s_log.lock().unwrap().last(), Some(&Status::Stopped));
    assert_port_free(&addr);
}

#[test]
fn stopping_the_server_frees_the_port_and_releases_the_client() {
    let (server, client, s_log, c_log, acts) = connected_with_a_held();
    let addr = server.local_addr().to_string();

    let t = Instant::now();
    server.stop();
    assert!(t.elapsed() < Duration::from_secs(2), "server stop took {:?}", t.elapsed());
    let s = s_log.lock().unwrap().clone();
    assert!(s.contains(&Status::Local), "{s:?}");
    assert_eq!(s.last(), Some(&Status::Stopped));
    assert_port_free(&addr);

    wait_for("A released on the client", || acts.lock().unwrap().last() == Some(&A_UP));
    wait_for("client Disconnected", || has(&c_log, |s| matches!(s, Status::Disconnected(_))));

    let t = Instant::now();
    client.stop(); // it is in its reconnect loop now
    assert!(t.elapsed() < Duration::from_secs(2), "client stop took {:?}", t.elapsed());
    assert_eq!(c_log.lock().unwrap().last(), Some(&Status::Stopped));
}

#[test]
fn stop_is_prompt_with_no_client() {
    let (s_on, s_log) = recorder();
    let server = server::start("127.0.0.1:0", keygen(), Edge::Right, Layout::single(DESK), false, None, s_on).unwrap();
    let addr = server.local_addr().to_string();
    assert_eq!(s_log.lock().unwrap().first(), Some(&Status::Listening(addr.clone())));
    let t = Instant::now();
    drop(server); // Drop stops too
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    assert_port_free(&addr);

    let (c_on, c_log) = recorder();
    let client = client::start(&addr, keygen(), Edge::Right, laptop(), Box::new(|_| {}), None, c_on);
    wait_for("client Retrying", || has(&c_log, |s| matches!(s, Status::Retrying(_))));
    let t = Instant::now();
    client.stop();
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
}

/// Run `f` on a thread; true if it returned within `limit`. A stop that hangs
/// fails the test instead of hanging it (the stuck thread is left behind).
fn finishes_within(limit: Duration, f: impl FnOnce() + Send + 'static) -> bool {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        f();
        let _ = done.send(());
    });
    finished.recv_timeout(limit).is_ok()
}

/// BUG-004: a stop must not depend on the other side closing. Here the client
/// stays alive (heartbeats) and never closes, even after the server's FIN.
#[test]
fn stopping_the_server_is_prompt_even_if_the_client_never_closes() {
    let key = keygen();
    let (s_on, s_log) = recorder();
    let server = server::start("127.0.0.1:0", key, Edge::Right, Layout::single(DESK), false, None, s_on).unwrap();
    let (tx, rx) = net::handshake(TcpStream::connect(server.local_addr()).unwrap(), &key, true).unwrap();
    tx.spawn_heartbeat(Duration::from_millis(100));
    wait_for("server Connected", || has(&s_log, |s| matches!(s, Status::Connected(_))));

    assert!(finishes_within(Duration::from_secs(3), move || server.stop()), "server stop waited on the client");
    drop((tx, rx));
}

/// A fake clipboard per computer, so a test never touches the real one.
macro_rules! fake_clipboard {
    ($name:ident) => {
        mod $name {
            use input_share_core::clipboard::{Clipboard, Contents};
            use std::sync::Mutex;
            use std::sync::atomic::{AtomicU32, Ordering};
            static SEQ: AtomicU32 = AtomicU32::new(0);
            static BOARD: Mutex<Contents> = Mutex::new(Vec::new());
            pub fn copy(c: Contents) {
                *BOARD.lock().unwrap() = c;
                SEQ.fetch_add(1, Ordering::SeqCst);
            }
            pub fn board() -> Contents {
                BOARD.lock().unwrap().clone()
            }
            pub const CLIP: Clipboard = Clipboard { seq: || SEQ.load(Ordering::SeqCst), get: board, set: |c| copy(c.clone()) };
        }
    };
}
fake_clipboard!(desk_clip);
fake_clipboard!(laptop_clip);

/// Clipboards over 1 MB go on transfer connections: pulled by the laptop
/// when control arrives, pushed by it when control returns.
#[test]
fn big_clipboards_cross_both_ways_over_transfers() {
    let key = keygen();
    let (s_on, s_log) = recorder();
    let server =
        server::start("127.0.0.1:0", key, Edge::Right, Layout::single(DESK), false, Some(desk_clip::CLIP), s_on).unwrap();
    let addr = server.local_addr().to_string();
    let (c_on, c_log) = recorder();
    let client = client::start(&addr, key, Edge::Right, laptop(), Box::new(|_| {}), Some(laptop_clip::CLIP), c_on);
    wait_for("server Connected", || has(&s_log, |s| matches!(s, Status::Connected(_))));

    let image = vec![(Format::Image, (0..3u32 << 20).map(|i| (i % 251) as u8).collect::<Vec<_>>())];
    desk_clip::copy(image.clone());
    server.input(Input::Move { x: 1919, y: 540 });
    wait_for("client Remote", || has(&c_log, |s| *s == Status::Remote));
    wait_for("the image pulled to the laptop", || laptop_clip::board() == image);

    let photo = vec![(Format::Image, vec![9; 2 << 20]), (Format::Text, b"caption".to_vec())];
    laptop_clip::copy(photo.clone());
    server.input(Input::Move { x: 960 - 50, y: 540 }); // parked at the center: 50 px left
    wait_for("server Local", || has(&s_log, |s| *s == Status::Local));
    wait_for("the photo pushed to the desktop", || desk_clip::board() == photo);

    client.stop();
    let t = Instant::now();
    server.stop();
    assert!(t.elapsed() < Duration::from_secs(2), "server stop took {:?}", t.elapsed());
    assert_port_free(&addr);
}
