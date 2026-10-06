// Server state machine and connection handling.
//
// Input sources (hooks, or --script) call `Server::on_input` directly under a
// mutex: a low-level hook must decide swallow/pass synchronously, so it cannot
// wait on a channel. `on_input` does no I/O; outgoing messages go onto a
// channel that a writer thread drains to the socket.

use crate::clipboard::{self, Clipboard, Outgoing, Tracker};
use crate::edge::Edge;
use crate::layout::Layout;
use crate::keys::Held;
use crate::net::{self, Key};
use crate::proto::{Button, Msg, VERSION};
use crate::status::{OnStatus, Status};
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Input {
    /// Absolute cursor position the event would move to.
    Move { x: i32, y: i32 },
    Button { button: Button, down: bool },
    Wheel { vertical: bool, delta: i32 },
    Key { scancode: u16, extended: bool, down: bool },
}

#[derive(Debug, Default, PartialEq)]
pub struct Verdict {
    /// Let the event through to this machine (hook returns CallNextHookEx).
    pub pass: bool,
    /// Move the local cursor here.
    pub cursor: Option<(i32, i32)>,
}

const PASS: Verdict = Verdict { pass: true, cursor: None };
const SWALLOW: Verdict = Verdict { pass: false, cursor: None };

const SC_ESC: u16 = 0x01;
const SC_CTRL: u16 = 0x1D;
const SC_ALT: u16 = 0x38;
const SC_LSHIFT: u16 = 0x2A;
const SC_RSHIFT: u16 = 0x36;

pub struct Server {
    edge: Edge,
    /// Refreshed every 2 s in real mode, but only while Local (see `refresh_layout`).
    layout: Layout,
    remote: bool,
    /// Some while a client session is up.
    out: Option<mpsc::Sender<Msg>>,
    /// Keys and buttons that went down while Local, so their ups stay local.
    local: Held,
    /// Everything physically down, for the panic hotkey.
    phys: Held,
    /// Remote/Local changes, drained by a pump thread: `on_input` runs inside
    /// the hook and must never call out to a status callback directly.
    status: Option<mpsc::Sender<Status>>,
    /// None: never touch this computer's clipboard (scripts, demo mode).
    clipboard: Option<Clipboard>,
    /// The running session's clipboard, for its transfers.
    clip: Option<Arc<Mutex<Tracker>>>,
    /// A session is running (or starting): one client at a time.
    in_session: bool,
}

impl Server {
    pub fn new(edge: Edge, layout: Layout) -> Self {
        Server {
            edge,
            layout,
            remote: false,
            out: None,
            local: Held::default(),
            phys: Held::default(),
            status: None,
            clipboard: None,
            clip: None,
            in_session: false,
        }
    }

    fn set_remote(&mut self, remote: bool) {
        if self.remote != remote {
            self.remote = remote;
            if let Some(s) = &self.status {
                let _ = s.send(if remote { Status::Remote } else { Status::Local });
            }
        }
    }

    fn center(&self) -> (i32, i32) {
        self.layout.park()
    }

    /// Take a newly read monitor layout. Ignored while Remote: the park point
    /// is what mouse deltas are measured from, so moving it mid-session would
    /// send one bogus jump. The next refresh after returning picks it up.
    fn refresh_layout(&mut self, layout: Layout) {
        if !self.remote {
            self.layout = layout;
        }
    }

    fn send(&self, m: Msg) {
        if let Some(out) = &self.out {
            let _ = out.send(m);
        }
    }

    pub fn on_input(&mut self, ev: Input) -> Verdict {
        if let Input::Key { scancode, extended, down } = ev {
            self.phys.key(scancode, extended, down);
            let panic = down
                && scancode == SC_ESC
                && self.phys.has_scancode(SC_CTRL)
                && self.phys.has_scancode(SC_ALT)
                && (self.phys.has_scancode(SC_LSHIFT) || self.phys.has_scancode(SC_RSHIFT));
            if panic && self.remote {
                // Dropping the session makes the client release everything.
                self.disconnect();
                return SWALLOW;
            }
        }

        if !self.remote {
            match ev {
                Input::Move { x, y } if self.out.is_some() && self.layout.crossing_hit(self.edge, x, y) => {
                    self.send(Msg::Enter { y_frac: self.layout.y_frac(y) });
                    self.set_remote(true);
                    return Verdict { pass: false, cursor: Some(self.center()) };
                }
                Input::Key { scancode, extended, down } => {
                    self.local.key(scancode, extended, down);
                }
                Input::Button { button, down } => {
                    self.local.button(button, down);
                }
                _ => {}
            }
            return PASS;
        }

        match ev {
            // Swallowing the move keeps the cursor parked at center.
            Input::Move { x, y } => {
                let (cx, cy) = self.center();
                let (dx, dy) = (x - cx, y - cy);
                if dx != 0 || dy != 0 {
                    self.send(Msg::MouseMove { dx, dy });
                }
            }
            Input::Key { scancode, extended, down: false } if self.local.key(scancode, extended, false) => {
                return PASS;
            }
            Input::Button { button, down: false } if self.local.button(button, false) => return PASS,
            Input::Key { scancode, extended, down } => self.send(Msg::Key { scancode, extended, down }),
            Input::Button { button, down } => self.send(Msg::Button { button, down }),
            Input::Wheel { vertical, delta } => self.send(Msg::Wheel { vertical, delta }),
        }
        SWALLOW
    }

    /// Client pushed back across; returns where to put the local cursor.
    pub fn on_leave(&mut self, y_frac: f32) -> Option<(i32, i32)> {
        if !self.remote {
            return None;
        }
        self.set_remote(false);
        Some(self.layout.return_point(self.edge, y_frac))
    }

    /// Force Local and end the session (its writer thread drains and closes).
    pub fn disconnect(&mut self) {
        self.set_remote(false);
        self.out = None;
    }
}

type Shared = Arc<Mutex<Server>>;

/// One client session, from the client's first message on. Returns when the
/// connection ends, for any reason.
#[allow(clippy::too_many_arguments)] // one per thing a session uses; a struct would only rename them
fn session(
    shared: &Shared,
    peer: SocketAddr,
    tx: net::Sender,
    mut rx: net::Receiver,
    first: Msg,
    set_cursor: fn(i32, i32),
    stop: &AtomicBool,
    on_status: &OnStatus,
) -> io::Result<()> {
    let (desk, cb) = {
        let s = shared.lock().unwrap();
        (s.layout.bounds(), s.clipboard)
    };
    let clip = cb.map(|cb| Arc::new(Mutex::new(Tracker::new(cb))));
    tx.send(&Msg::Hello { version: VERSION, w: desk.w, h: desk.h })?;
    tx.spawn_heartbeat(net::HEARTBEAT);

    let (out, queue) = mpsc::channel();
    let writer = {
        let (tx, clip) = (tx.clone(), clip.clone());
        thread::spawn(move || {
            for m in queue {
                // The clipboard goes ahead of control. Read here, never in the
                // hook that queued the Enter.
                if matches!(m, Msg::Enter { .. })
                    && let Some(c) = &clip
                {
                    let out = c.lock().unwrap().outgoing();
                    let sent = match out {
                        Outgoing::Nothing => Ok(()),
                        Outgoing::Inline(parts) => parts.iter().try_for_each(|p| tx.send(p)),
                        Outgoing::Big(_) => tx.send(&Msg::Offer), // the client pulls it from `big`
                    };
                    if sent.is_err() {
                        break;
                    }
                }
                if tx.send(&m).is_err() {
                    break;
                }
            }
            tx.shutdown(); // queue closed: session over, flush and FIN
        })
    };
    {
        let mut s = shared.lock().unwrap();
        // `stop()` sets the flag before taking this lock, so checking it here
        // under the lock means a stopping server never starts a session.
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        s.out = Some(out);
        s.clip = clip.clone();
    }
    on_status(Status::Connected(peer.to_string()));

    let mut m = Ok(first);
    let res = loop {
        // Ending the session must not depend on the client closing (BUG-004):
        // a client that stays alive keeps this read going with heartbeats.
        if stop.load(Ordering::SeqCst) {
            break Ok(());
        }
        match m {
            Ok(Msg::Leave { y_frac }) => {
                let p = shared.lock().unwrap().on_leave(y_frac);
                if let Some((x, y)) = p {
                    set_cursor(x, y);
                }
            }
            Ok(Msg::Clipboard { last, data }) => {
                if let Some(c) = &clip {
                    c.lock().unwrap().incoming(last, data);
                }
            }
            Ok(Msg::Offer) => {
                // The client pushes it over a transfer, which reads `awaited`.
                if let Some(c) = &clip {
                    c.lock().unwrap().offered();
                }
            }
            Ok(Msg::Hello { version, .. }) if version != VERSION => {
                break Err(io::Error::other(format!("client protocol version {version}, want {VERSION}")));
            }
            Ok(_) => {}
            Err(e) => break Err(e),
        }
        m = rx.recv();
    };
    {
        let mut s = shared.lock().unwrap();
        s.disconnect();
        s.clip = None;
    }
    if let Some(c) = &clip {
        c.lock().unwrap().end();
    }
    let _ = writer.join();
    res
}

/// A clipboard transfer for the running session: the client pulls the
/// server's big clipboard, or pushes its own.
fn transfer(shared: &Shared, tx: &net::Sender, rx: &mut net::Receiver, pull: bool, stop: &AtomicBool) -> io::Result<()> {
    let clip = shared.lock().unwrap().clip.clone().ok_or_else(|| io::Error::other("no session to transfer for"))?;
    if pull {
        let blob = clip.lock().unwrap().big.clone().ok_or_else(|| io::Error::other("nothing offered"))?;
        clipboard::send_big(tx, &blob, stop)
    } else {
        // ponytail: assumes the session read the Offer before this transfer
        // began (it is sent first); a push that wins that race is dropped.
        let generation = clip.lock().unwrap().awaited;
        let z = clipboard::recv_big(rx, stop)?;
        clip.lock().unwrap().finish(generation, &z);
        Ok(())
    }
}

/// One accepted connection: a session, or a transfer for the session. The
/// client's first message says which.
fn connection(
    stream: TcpStream,
    shared: &Shared,
    key: &Key,
    set_cursor: fn(i32, i32),
    stop: &AtomicBool,
    on_status: &OnStatus,
) {
    let opened = (|| {
        stream.set_nonblocking(false)?;
        // A peer that stops reading must not block a send (and so `stop`) forever.
        stream.set_write_timeout(Some(net::TIMEOUT))?;
        let peer = stream.peer_addr()?;
        let (tx, mut rx) = net::handshake(stream, key, false)?;
        let first = rx.recv()?;
        io::Result::Ok((peer, tx, rx, first))
    })();
    let reason = match opened {
        Ok((_, tx, mut rx, Msg::Transfer { pull })) => {
            if let Err(e) = transfer(shared, &tx, &mut rx, pull, stop) {
                eprintln!("clipboard transfer: {e}");
            }
            tx.shutdown(); // send_big did already, on a pull
            return;
        }
        Ok((peer, tx, rx, first)) => {
            {
                let mut s = shared.lock().unwrap();
                if s.in_session {
                    // ponytail: one client at a time; another one retries until this one ends.
                    eprintln!("{peer}: already controlling another computer");
                    return;
                }
                s.in_session = true;
            }
            let r = session(shared, peer, tx, rx, first, set_cursor, stop, on_status);
            shared.lock().unwrap().in_session = false;
            match r {
                Ok(()) => "closed".to_string(),
                Err(e) => e.to_string(),
            }
        }
        Err(e) => e.to_string(),
    };
    on_status(Status::Disconnected(reason));
}

fn accept_loop(
    listener: TcpListener,
    shared: Shared,
    key: Key,
    set_cursor: fn(i32, i32),
    stop: Arc<AtomicBool>,
    on_status: OnStatus,
) {
    // The listener is non-blocking so this loop can notice `stop`.
    let mut conns: Vec<JoinHandle<()>> = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let (shared, stop, on_status) = (shared.clone(), stop.clone(), on_status.clone());
                conns.push(thread::spawn(move || connection(stream, &shared, &key, set_cursor, &stop, &on_status)));
            }
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
        conns.retain(|c| !c.is_finished());
    }
    // Sessions and transfers see `stop` within a heartbeat or a chunk.
    for c in conns {
        let _ = c.join();
    }
}

/// A running server. `stop()` (or dropping it) ends the session, unhooks,
/// frees the port and joins every thread it started.
pub struct ServerHandle {
    shared: Shared,
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    pump: Option<JoinHandle<()>>,
    hooks: Option<(u32, JoinHandle<()>)>,
    refresh: Option<JoinHandle<()>>,
    on_status: OnStatus,
}

/// Bind and accept clients on a background thread. With `hooks`, also install
/// the low-level hooks (real input) and re-read the monitor layout every 2 s;
/// without, feed input through `input()`. `clipboard`: share this one.
pub fn start(
    bind: &str,
    key: Key,
    edge: Edge,
    layout: Layout,
    hooks: bool,
    clipboard: Option<Clipboard>,
    on_status: OnStatus,
) -> io::Result<ServerHandle> {
    let listener = TcpListener::bind(bind)?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let shared: Shared = Arc::new(Mutex::new(Server::new(edge, layout)));

    let (status_tx, status_rx) = mpsc::channel();
    {
        let mut s = shared.lock().unwrap();
        s.status = Some(status_tx);
        s.clipboard = clipboard;
    }
    // Real mode hides the parked cursor while Remote. Restore first, in case an
    // earlier run crashed with it hidden. The pump does it (never the hook):
    // disconnect, the panic hotkey and stop all report Local through here, and
    // the pump ending (stop, drop) restores whatever is left.
    if hooks {
        crate::cursor::restore();
    }
    let pump = {
        let on_status = on_status.clone();
        let mut gate = hooks.then(crate::cursor::CursorGate::default);
        thread::spawn(move || {
            for s in status_rx {
                if let Some(a) = gate.as_mut().and_then(|g| g.on_status(&s)) {
                    crate::cursor::apply(a);
                }
                on_status(s);
            }
            if let Some(a) = gate.as_mut().and_then(|g| g.finish()) {
                crate::cursor::apply(a);
            }
        })
    };
    let set_cursor: fn(i32, i32) = if hooks { set_cursor_pos } else { |x, y| println!("cursor {x} {y}") };
    // On error, `shared` drops here, which drops the status sender and ends the pump.
    let hooks = if hooks { Some(spawn_hooks(shared.clone())?) } else { None };

    on_status(Status::Listening(addr.to_string()));
    let stop = Arc::new(AtomicBool::new(false));
    let accept = {
        let (shared, stop, on_status) = (shared.clone(), stop.clone(), on_status.clone());
        thread::spawn(move || accept_loop(listener, shared, key, set_cursor, stop, on_status))
    };
    // Monitors plugged in or rearranged later. Never enumerated inside a hook.
    let refresh = hooks.is_some().then(|| {
        let (shared, stop) = (shared.clone(), stop.clone());
        thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                for _ in 0..20 {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                let layout = crate::win::layout();
                shared.lock().unwrap().refresh_layout(layout);
            }
        })
    });
    Ok(ServerHandle { shared, addr, stop, accept: Some(accept), pump: Some(pump), hooks, refresh, on_status })
}

impl ServerHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Feed one input event (scripts and demo mode; hooks call `on_input` themselves).
    pub fn input(&self, ev: Input) -> Verdict {
        self.shared.lock().unwrap().on_input(ev)
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let Some(accept) = self.accept.take() else { return };
        self.stop.store(true, Ordering::SeqCst);
        // Local first, then unhook, so the user has their input back at once.
        self.shared.lock().unwrap().disconnect();
        if let Some((thread_id, hook_thread)) = self.hooks.take() {
            let _ = unsafe { PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            let _ = hook_thread.join();
        }
        let _ = accept.join(); // waits for the session to drain and close
        if let Some(refresh) = self.refresh.take() {
            let _ = refresh.join();
        }
        self.shared.lock().unwrap().status = None; // ends the pump
        if let Some(pump) = self.pump.take() {
            let _ = pump.join();
        }
        (self.on_status)(Status::Stopped);
    }

    fn wait(&self, w: Wait) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let ok = {
                let s = self.shared.lock().unwrap();
                match w {
                    Wait::Connected => s.out.is_some(),
                    Wait::Remote => s.remote,
                    Wait::Local => !s.remote,
                }
            };
            if ok {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, format!("script: wait {w:?} timed out")));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Server with a scripted fake input source instead of hooks.
pub fn run_script(bind: &str, key: Key, edge: Edge, layout: Layout, script: &Path, on_status: OnStatus) -> io::Result<()> {
    let cmds = parse_script(&std::fs::read_to_string(script)?)?;
    let server = start(bind, key, edge, layout, false, None, on_status)?;
    for cmd in cmds {
        match cmd {
            Cmd::Input(ev) => {
                if let Some((x, y)) = server.input(ev).cursor {
                    println!("cursor {x} {y}");
                }
            }
            Cmd::Sleep(d) => thread::sleep(d),
            Cmd::Wait(w) => server.wait(w)?,
        }
    }
    // Script done: stopping ends the session cleanly, so queued events reach the client.
    server.stop();
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum Wait {
    Connected,
    Remote,
    Local,
}

enum Cmd {
    Input(Input),
    Sleep(Duration),
    Wait(Wait),
}

/// One command per line; `#` starts a comment.
///   move X Y | key HEX [ext] down|up | button left|right|middle|x1|x2 down|up
///   wheel v|h DELTA | sleep MS | wait connected|remote|local
fn parse_script(text: &str) -> io::Result<Vec<Cmd>> {
    let mut cmds = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let bad = || io::Error::new(io::ErrorKind::InvalidData, format!("script line {}: {line}", n + 1));
        let w: Vec<&str> = line.split_whitespace().collect();
        let num = |s: &str| s.parse::<i32>().map_err(|_| bad());
        let down = |s: &str| match s {
            "down" => Ok(true),
            "up" => Ok(false),
            _ => Err(bad()),
        };
        let cmd = match w.as_slice() {
            ["move", x, y] => Cmd::Input(Input::Move { x: num(x)?, y: num(y)? }),
            ["key", sc, rest @ ..] => {
                let scancode = u16::from_str_radix(sc, 16).map_err(|_| bad())?;
                let (extended, d) = match rest {
                    ["ext", d] => (true, d),
                    [d] => (false, d),
                    _ => return Err(bad()),
                };
                Cmd::Input(Input::Key { scancode, extended, down: down(d)? })
            }
            ["button", b, d] => {
                let button = match *b {
                    "left" => Button::Left,
                    "right" => Button::Right,
                    "middle" => Button::Middle,
                    "x1" => Button::X1,
                    "x2" => Button::X2,
                    _ => return Err(bad()),
                };
                Cmd::Input(Input::Button { button, down: down(d)? })
            }
            ["wheel", v, delta] => Cmd::Input(Input::Wheel { vertical: *v == "v", delta: num(delta)? }),
            ["sleep", ms] => Cmd::Sleep(Duration::from_millis(num(ms)? as u64)),
            ["wait", "connected"] => Cmd::Wait(Wait::Connected),
            ["wait", "remote"] => Cmd::Wait(Wait::Remote),
            ["wait", "local"] => Cmd::Wait(Wait::Local),
            _ => return Err(bad()),
        };
        cmds.push(cmd);
    }
    Ok(cmds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edge::Rect;

    const DESK: Rect = Rect { left: 0, top: 0, w: 1920, h: 1080 };

    fn connected() -> (Server, mpsc::Receiver<Msg>) {
        let mut s = Server::new(Edge::Right, Layout::single(DESK));
        let (tx, rx) = mpsc::channel();
        s.out = Some(tx);
        (s, rx)
    }

    fn key(scancode: u16, down: bool) -> Input {
        Input::Key { scancode, extended: false, down }
    }

    #[test]
    fn layout_refresh_waits_until_local() {
        let (mut s, rx) = connected();
        let wide = Layout::single(Rect { left: 0, top: 0, w: 3840, h: 2160 });
        s.on_input(Input::Move { x: 1919, y: 540 }); // Remote, parked at (960, 540)
        s.refresh_layout(wide.clone());
        s.on_input(Input::Move { x: 961, y: 540 }); // still measured from the old park point
        s.on_leave(0.5);
        s.refresh_layout(wide.clone());
        assert_eq!(s.layout, wide);
        let sent: Vec<Msg> = rx.try_iter().collect();
        assert_eq!(sent[1], Msg::MouseMove { dx: 1, dy: 0 });
    }

    #[test]
    fn no_crossing_without_a_client() {
        let mut s = Server::new(Edge::Right, Layout::single(DESK));
        assert_eq!(s.on_input(Input::Move { x: 1919, y: 5 }), PASS);
        assert!(!s.remote);
    }

    #[test]
    fn cross_forward_and_return() {
        let (mut s, rx) = connected();
        assert_eq!(s.on_input(Input::Move { x: 1918, y: 540 }), PASS);
        assert_eq!(s.on_input(Input::Move { x: 1919, y: 0 }), Verdict { pass: false, cursor: Some((960, 540)) });
        assert_eq!(s.on_input(Input::Move { x: 963, y: 530 }), SWALLOW);
        assert_eq!(s.on_input(Input::Move { x: 960, y: 540 }), SWALLOW); // no-op move, nothing sent
        assert_eq!(s.on_input(Input::Wheel { vertical: true, delta: -120 }), SWALLOW);
        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            [
                Msg::Enter { y_frac: 0.0 },
                Msg::MouseMove { dx: 3, dy: -10 },
                Msg::Wheel { vertical: true, delta: -120 },
            ]
        );
        assert_eq!(s.on_leave(1.0), Some((1918, 1079)));
        assert_eq!(s.on_leave(1.0), None);
        assert_eq!(s.on_input(key(0x1E, true)), PASS);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn key_held_while_crossing_releases_locally() {
        let (mut s, rx) = connected();
        s.on_input(key(SC_CTRL, true)); // Ctrl down while Local
        s.on_input(Input::Button { button: Button::Left, down: true });
        s.on_input(Input::Move { x: 1919, y: 540 });
        assert_eq!(s.on_input(key(SC_CTRL, false)), PASS); // up stays local...
        assert_eq!(s.on_input(Input::Button { button: Button::Left, down: false }), PASS);
        assert_eq!(s.on_input(key(SC_CTRL, true)), SWALLOW); // ...a fresh press is remote
        assert_eq!(s.on_input(key(SC_CTRL, false)), SWALLOW);
        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            [
                Msg::Enter { y_frac: DESK.y_frac(540) },
                Msg::Key { scancode: SC_CTRL, extended: false, down: true },
                Msg::Key { scancode: SC_CTRL, extended: false, down: false },
            ]
        );
    }

    #[test]
    fn panic_hotkey_forces_local_and_drops_session() {
        let (mut s, _rx) = connected();
        s.on_input(Input::Move { x: 1919, y: 540 });
        for sc in [SC_CTRL, SC_ALT, SC_RSHIFT] {
            s.on_input(key(sc, true));
        }
        assert_eq!(s.on_input(key(SC_ESC, true)), SWALLOW);
        assert!(!s.remote);
        assert!(s.out.is_none());
    }

    #[test]
    fn script_parses_and_rejects() {
        let cmds = parse_script("# c\nmove 1 2\nkey 1d ext down\nbutton x2 up\nwheel h 120\nsleep 5\nwait remote\n").unwrap();
        assert_eq!(cmds.len(), 6);
        assert!(matches!(cmds[1], Cmd::Input(Input::Key { scancode: 0x1D, extended: true, down: true })));
        for bad in ["move 1", "key zz down", "button left sideways", "wait forever", "jump"] {
            assert!(parse_script(bad).is_err(), "{bad}");
        }
    }
}

// ---- Real input: low-level hooks. ----

/// Translate a WH_MOUSE_LL event. `data` is MSLLHOOKSTRUCT.mouseData.
fn mouse_event(msg: u32, x: i32, y: i32, data: u32) -> Option<Input> {
    let hi = (data >> 16) as u16;
    let button = |down| {
        let button = if hi == XBUTTON1 { Button::X1 } else { Button::X2 };
        Some(Input::Button { button, down })
    };
    match msg {
        WM_MOUSEMOVE => Some(Input::Move { x, y }),
        WM_LBUTTONDOWN => Some(Input::Button { button: Button::Left, down: true }),
        WM_LBUTTONUP => Some(Input::Button { button: Button::Left, down: false }),
        WM_RBUTTONDOWN => Some(Input::Button { button: Button::Right, down: true }),
        WM_RBUTTONUP => Some(Input::Button { button: Button::Right, down: false }),
        WM_MBUTTONDOWN => Some(Input::Button { button: Button::Middle, down: true }),
        WM_MBUTTONUP => Some(Input::Button { button: Button::Middle, down: false }),
        WM_XBUTTONDOWN => button(true),
        WM_XBUTTONUP => button(false),
        WM_MOUSEWHEEL => Some(Input::Wheel { vertical: true, delta: hi as i16 as i32 }),
        WM_MOUSEHWHEEL => Some(Input::Wheel { vertical: false, delta: hi as i16 as i32 }),
        _ => None,
    }
}

/// Translate a WH_KEYBOARD_LL event from KBDLLHOOKSTRUCT's scanCode and flags.
fn key_event(scan: u32, flags: KBDLLHOOKSTRUCT_FLAGS) -> Input {
    Input::Key { scancode: scan as u16, extended: flags.contains(LLKHF_EXTENDED), down: !flags.contains(LLKHF_UP) }
}

/// The server the hook callbacks drive. Set while hooks are installed; cleared
/// by the hook thread on its way out, so a new server can hook again.
static HOOKED: Mutex<Option<Shared>> = Mutex::new(None);

fn hooked() -> MutexGuard<'static, Option<Shared>> {
    // A panic inside an extern "system" callback aborts, so never unwrap here.
    HOOKED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run the state machine for one hook event. Returns true to swallow it.
/// Only ever takes the mutexes briefly: the net thread holds them for no I/O.
fn hook_verdict(ev: Input) -> bool {
    let Some(shared) = hooked().clone() else { return false };
    let v = shared.lock().unwrap_or_else(|e| e.into_inner()).on_input(ev);
    if let Some((x, y)) = v.cursor {
        set_cursor_pos(x, y);
    }
    !v.pass
}

fn set_cursor_pos(x: i32, y: i32) {
    let _ = unsafe { SetCursorPos(x, y) };
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        // Skip injected events, our own SetCursorPos park included.
        if info.flags & LLMHF_INJECTED == 0
            && let Some(ev) = mouse_event(wparam.0 as u32, info.pt.x, info.pt.y, info.mouseData)
            && hook_verdict(ev)
        {
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn key_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if !info.flags.contains(LLKHF_INJECTED) && hook_verdict(key_event(info.scanCode, info.flags)) {
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Install both hooks on a new thread that runs a message loop (hook callbacks
/// run inside its GetMessage). Returns that thread's id, for WM_QUIT on stop.
fn spawn_hooks(shared: Shared) -> io::Result<(u32, JoinHandle<()>)> {
    {
        let mut h = hooked();
        if h.is_some() {
            return Err(io::Error::other("hooks already running"));
        }
        *h = Some(shared);
    }
    let (tx, rx) = mpsc::channel();
    let thread = thread::spawn(move || {
        unsafe {
            let install = || -> windows::core::Result<(HHOOK, HHOOK)> {
                let module = GetModuleHandleW(None)?;
                let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(module.into()), 0)?;
                match SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), Some(module.into()), 0) {
                    Ok(keyboard) => Ok((mouse, keyboard)),
                    Err(e) => {
                        let _ = UnhookWindowsHookEx(mouse);
                        Err(e)
                    }
                }
            };
            match install() {
                Ok((mouse, keyboard)) => {
                    let mut msg = MSG::default();
                    // Create this thread's message queue before announcing the
                    // thread id, so stop()'s WM_QUIT cannot arrive too early.
                    let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                    let _ = tx.send(Ok(GetCurrentThreadId()));
                    // GetMessage returns 0 on WM_QUIT and -1 on error; stop on both.
                    while GetMessageW(&mut msg, None, 0, 0).0 > 0 {}
                    let _ = UnhookWindowsHookEx(mouse);
                    let _ = UnhookWindowsHookEx(keyboard);
                }
                Err(e) => {
                    let _ = tx.send(Err(io::Error::other(e)));
                }
            }
        }
        *hooked() = None;
    });
    match rx.recv() {
        Ok(Ok(thread_id)) => Ok((thread_id, thread)),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            let _ = thread.join();
            Err(io::Error::other("hook thread exited before installing hooks"))
        }
    }
}

/// Real server for the CLI: hooks until the process ends.
pub fn run_hooks(bind: &str, key: Key, edge: Edge, layout: Layout, on_status: OnStatus) -> io::Result<()> {
    let _server = start(bind, key, edge, layout, true, Some(clipboard::WINDOWS), on_status)?;
    println!("hooks installed; panic hotkey is Ctrl+Alt+Shift+Esc");
    loop {
        thread::park();
    }
}

#[cfg(test)]
mod hook_tests {
    use super::*;

    #[test]
    fn mouse_messages_translate() {
        assert_eq!(mouse_event(WM_MOUSEMOVE, -5, 7, 0), Some(Input::Move { x: -5, y: 7 }));
        assert_eq!(mouse_event(WM_RBUTTONUP, 0, 0, 0), Some(Input::Button { button: Button::Right, down: false }));
        let x2 = (XBUTTON2 as u32) << 16;
        assert_eq!(mouse_event(WM_XBUTTONDOWN, 0, 0, x2), Some(Input::Button { button: Button::X2, down: true }));
        let x1 = (XBUTTON1 as u32) << 16;
        assert_eq!(mouse_event(WM_XBUTTONUP, 0, 0, x1), Some(Input::Button { button: Button::X1, down: false }));
        // Wheel delta is the signed high word: one notch toward the user is -120.
        let down_notch = ((-120i16 as u16) as u32) << 16;
        assert_eq!(mouse_event(WM_MOUSEWHEEL, 0, 0, down_notch), Some(Input::Wheel { vertical: true, delta: -120 }));
        assert_eq!(mouse_event(WM_MOUSEHWHEEL, 0, 0, 240 << 16), Some(Input::Wheel { vertical: false, delta: 240 }));
        assert_eq!(mouse_event(WM_KEYDOWN, 0, 0, 0), None);
    }

    #[test]
    fn key_flags_translate() {
        assert_eq!(key_event(0x1E, KBDLLHOOKSTRUCT_FLAGS(0)), Input::Key { scancode: 0x1E, extended: false, down: true });
        assert_eq!(
            key_event(0x1D, LLKHF_EXTENDED | LLKHF_UP),
            Input::Key { scancode: 0x1D, extended: true, down: false }
        );
    }
}
