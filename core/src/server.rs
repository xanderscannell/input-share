// Server state machine and connection handling. See DESIGN.md "How it works".
//
// Input sources (hooks, or --script) call `Server::on_input` directly under a
// mutex: a low-level hook must decide swallow/pass synchronously, so it cannot
// wait on a channel. `on_input` does no I/O; outgoing messages go onto a
// channel that a writer thread drains to the socket.

use crate::edge::{self, Edge, Rect};
use crate::keys::Held;
use crate::net::{self, Key};
use crate::proto::{Button, Msg, VERSION};
use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
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
    desk: Rect,
    remote: bool,
    /// Some while a client session is up.
    out: Option<mpsc::Sender<Msg>>,
    /// Keys and buttons that went down while Local, so their ups stay local.
    local: Held,
    /// Everything physically down, for the panic hotkey.
    phys: Held,
    /// A session thread is running (set by `session`).
    live: bool,
}

impl Server {
    pub fn new(edge: Edge, desk: Rect) -> Self {
        Server { edge, desk, remote: false, out: None, local: Held::default(), phys: Held::default(), live: false }
    }

    fn center(&self) -> (i32, i32) {
        (self.desk.left + self.desk.w / 2, self.desk.top + self.desk.h / 2)
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
                Input::Move { x, y } if self.out.is_some() && edge::server_hit(self.edge, self.desk, x) => {
                    self.send(Msg::Enter { y_frac: self.desk.y_frac(y) });
                    self.remote = true;
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
        self.remote = false;
        Some(edge::server_return_point(self.edge, self.desk, y_frac))
    }

    /// Force Local and end the session (its writer thread drains and closes).
    pub fn disconnect(&mut self) {
        self.remote = false;
        self.out = None;
    }
}

type Shared = Arc<Mutex<Server>>;

/// One client session. Returns when the connection ends, for any reason.
fn session(shared: &Shared, stream: TcpStream, key: &Key, set_cursor: &dyn Fn(i32, i32)) -> io::Result<()> {
    let (tx, mut rx) = net::handshake(stream, key, false)?;
    let desk = shared.lock().unwrap().desk;
    tx.send(&Msg::Hello { version: VERSION, w: desk.w, h: desk.h })?;
    tx.spawn_heartbeat(net::HEARTBEAT);

    let (out, queue) = mpsc::channel();
    let writer = {
        let tx = tx.clone();
        thread::spawn(move || {
            for m in queue {
                if tx.send(&m).is_err() {
                    break;
                }
            }
            tx.shutdown(); // queue closed: session over, flush and FIN
        })
    };
    {
        let mut s = shared.lock().unwrap();
        s.out = Some(out);
        s.live = true;
    }
    println!("connected");

    let res = loop {
        match rx.recv() {
            Ok(Msg::Leave { y_frac }) => {
                let p = shared.lock().unwrap().on_leave(y_frac);
                if let Some((x, y)) = p {
                    set_cursor(x, y);
                }
            }
            Ok(Msg::Hello { version, .. }) if version != VERSION => {
                break Err(io::Error::other(format!("client protocol version {version}, want {VERSION}")));
            }
            Ok(_) => {}
            Err(e) => break Err(e),
        }
    };
    shared.lock().unwrap().disconnect();
    let _ = writer.join();
    shared.lock().unwrap().live = false;
    println!("disconnected");
    res
}

fn accept_loop(listener: TcpListener, shared: Shared, key: Key, set_cursor: fn(i32, i32)) {
    // ponytail: one client at a time; a second connection waits until the first ends.
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if let Err(e) = session(&shared, stream, &key, &set_cursor) {
            eprintln!("session ended: {e}");
        }
    }
}

/// Server with a scripted fake input source instead of hooks.
/// Bind, then accept clients on a background thread.
fn start(bind: &str, key: Key, edge: Edge, desk: Rect, set_cursor: fn(i32, i32)) -> io::Result<Shared> {
    let listener = TcpListener::bind(bind)?;
    println!("listening {}", listener.local_addr()?);
    let shared: Shared = Arc::new(Mutex::new(Server::new(edge, desk)));
    let s = shared.clone();
    thread::spawn(move || accept_loop(listener, s, key, set_cursor));
    Ok(shared)
}

pub fn run_script(bind: &str, key: Key, edge: Edge, desk: Rect, script: &Path) -> io::Result<()> {
    let text = std::fs::read_to_string(script)?;
    let cmds = parse_script(&text)?;
    let shared = start(bind, key, edge, desk, |x, y| println!("cursor {x} {y}"))?;

    for cmd in cmds {
        match cmd {
            Cmd::Input(ev) => {
                let v = shared.lock().unwrap().on_input(ev);
                if let Some((x, y)) = v.cursor {
                    println!("cursor {x} {y}");
                }
            }
            Cmd::Sleep(d) => thread::sleep(d),
            Cmd::Wait(w) => wait_until(&shared, w)?,
        }
    }

    // Script done: end the session cleanly so queued events reach the client.
    shared.lock().unwrap().disconnect();
    wait_until(&shared, Wait::Idle)
}

fn wait_until(shared: &Shared, w: Wait) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let ok = {
            let s = shared.lock().unwrap();
            match w {
                Wait::Connected => s.out.is_some(),
                Wait::Remote => s.remote,
                Wait::Local => !s.remote,
                Wait::Idle => !s.live,
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

#[derive(Debug, Clone, Copy)]
enum Wait {
    Connected,
    Remote,
    Local,
    Idle,
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

    const DESK: Rect = Rect { left: 0, top: 0, w: 1920, h: 1080 };

    fn connected() -> (Server, mpsc::Receiver<Msg>) {
        let mut s = Server::new(Edge::Right, DESK);
        let (tx, rx) = mpsc::channel();
        s.out = Some(tx);
        (s, rx)
    }

    fn key(scancode: u16, down: bool) -> Input {
        Input::Key { scancode, extended: false, down }
    }

    #[test]
    fn no_crossing_without_a_client() {
        let mut s = Server::new(Edge::Right, DESK);
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

static HOOKED: OnceLock<Shared> = OnceLock::new();

/// Run the state machine for one hook event. Returns true to swallow it.
/// Only ever takes the mutex briefly: the net thread holds it for no I/O.
fn hook_verdict(ev: Input) -> bool {
    let Some(shared) = HOOKED.get() else { return false };
    let v = shared.lock().unwrap_or_else(|e| e.into_inner()).on_input(ev);
    if let Some((x, y)) = v.cursor {
        set_cursor(x, y);
    }
    !v.pass
}

fn set_cursor(x: i32, y: i32) {
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

/// Real server: hooks on this thread with a message loop. Never returns on success.
pub fn run_hooks(bind: &str, key: Key, edge: Edge, desk: Rect) -> io::Result<()> {
    let shared = start(bind, key, edge, desk, set_cursor)?;
    HOOKED.set(shared).map_err(|_| io::Error::other("hooks already running"))?;
    unsafe {
        let module = GetModuleHandleW(None).map_err(io::Error::other)?;
        SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(module.into()), 0).map_err(io::Error::other)?;
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), Some(module.into()), 0).map_err(io::Error::other)?;
        println!("hooks installed; panic hotkey is Ctrl+Alt+Shift+Esc");
        // Hooks are called on this thread, from inside GetMessage.
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
    }
    Ok(())
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
