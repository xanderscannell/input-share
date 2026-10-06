// Client: connect out, reconnect with backoff, turn server messages into
// injected input. Injection goes through a sink so `--dry-run` can print
// instead of calling SendInput.

use crate::clipboard::{Clipboard, Tracker};
use crate::edge::Edge;
use crate::layout::{ClientCursor, Layout, Step};
use crate::keys::Held;
use crate::net::{self, Key};
use crate::proto::{Button, Msg, VERSION};
use crate::status::{OnStatus, Status};
use std::io;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows::core::BOOL;
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{XBUTTON1, XBUTTON2};

/// Something to inject on this machine.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    MoveTo(i32, i32),
    /// A Key, Button or Wheel message.
    Input(Msg),
}

pub type Sink<'a> = &'a mut dyn FnMut(Act);

/// An owned sink the client thread can take with it.
pub type BoxSink = Box<dyn FnMut(Act) + Send>;

/// Reads this machine's monitor layout. Called at each crossing, so monitors
/// plugged in between crossings are picked up.
pub type LayoutFn = Box<dyn Fn() -> Layout + Send>;

/// The `--dry-run` sink: one line per injected event.
pub fn print_act(a: Act) {
    match a {
        Act::MoveTo(x, y) => println!("move {x} {y}"),
        Act::Input(Msg::Key { scancode, extended, down }) => {
            println!("key {scancode:x}{} {}", if extended { " ext" } else { "" }, if down { "down" } else { "up" })
        }
        Act::Input(Msg::Button { button, down }) => println!("button {button:?} {}", if down { "down" } else { "up" }),
        Act::Input(Msg::Wheel { vertical, delta }) => println!("wheel {} {delta}", if vertical { "v" } else { "h" }),
        Act::Input(m) => println!("{m:?}"),
    }
}

/// Pixel `x` in a span of `w` pixels starting at `left`, as SendInput's
/// absolute 0..=65535 units. Windows maps back with floor(n * w / 65536), so
/// round up to land on exactly `x`.
fn to_absolute(x: i32, left: i32, w: i32) -> i32 {
    let (x, w) = ((x - left) as i64, w.max(1) as i64);
    ((x * 65536 + w - 1) / w).clamp(0, 65535) as i32
}

/// Everything the real sink has pressed and not released, so the console
/// handler can release it if the process is killed with Ctrl+C or closed.
static INJECTED: LazyLock<Mutex<Held>> = LazyLock::new(Default::default);

fn send(input: INPUT) {
    if unsafe { SendInput(&[input], size_of::<INPUT>() as i32) } != 1 {
        eprintln!("SendInput failed (elevated window or secure desktop?)");
    }
}

fn inject(a: Act) {
    let mouse = |dx, dy, data: u32, flags| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    };
    let input = match a {
        Act::MoveTo(x, y) => {
            // Read per move (cheap) so a monitor change mid-session cannot skew moves.
            let screen = crate::win::virtual_screen();
            mouse(
                to_absolute(x, screen.left, screen.w),
                to_absolute(y, screen.top, screen.h),
                0,
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            )
        }
        Act::Input(Msg::Key { scancode, extended, down }) => {
            let mut flags = KEYEVENTF_SCANCODE;
            if extended {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            if !down {
                flags |= KEYEVENTF_KEYUP;
            }
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: scancode, dwFlags: flags, time: 0, dwExtraInfo: 0 },
                },
            }
        }
        Act::Input(Msg::Button { button, down }) => {
            let (flags, data) = match (button, down) {
                (Button::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                (Button::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
                (Button::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                (Button::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
                (Button::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                (Button::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                (Button::X1, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
                (Button::X1, false) => (MOUSEEVENTF_XUP, XBUTTON1),
                (Button::X2, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
                (Button::X2, false) => (MOUSEEVENTF_XUP, XBUTTON2),
            };
            mouse(0, 0, data as u32, flags)
        }
        Act::Input(Msg::Wheel { vertical, delta }) => {
            mouse(0, 0, delta as u32, if vertical { MOUSEEVENTF_WHEEL } else { MOUSEEVENTF_HWHEEL })
        }
        Act::Input(_) => return,
    };
    if let Act::Input(m) = a {
        INJECTED.lock().unwrap().track(&m);
    }
    send(input);
}

unsafe extern "system" fn on_console_close(_ctrl: u32) -> BOOL {
    let ups = INJECTED.lock().map(|mut h| h.release_all()).unwrap_or_default();
    for m in ups {
        inject(Act::Input(m));
    }
    false.into() // let the default handler end the process
}

/// The real sink: SendInput on this machine, with release-on-exit installed.
pub fn send_input_sink() -> impl FnMut(Act) + Send {
    if let Err(e) = unsafe { SetConsoleCtrlHandler(Some(on_console_close), true) } {
        eprintln!("warning: no Ctrl+C handler, keys may stick on exit: {e}");
    }
    inject
}

fn release_all(held: &mut Held, sink: Sink) {
    println!("release-all");
    for m in held.release_all() {
        sink(Act::Input(m));
    }
}

/// A running client. `stop()` (or dropping it) closes the connection, which
/// runs release-all, and ends the reconnect loop.
pub struct ClientHandle {
    stop: Arc<AtomicBool>,
    /// The live connection, so `stop()` can break a blocking read.
    sock: Arc<Mutex<Option<TcpStream>>>,
    thread: Option<JoinHandle<()>>,
    on_status: OnStatus,
}

/// Connect to `addr` (default port if none given) and reconnect with backoff
/// until stopped. `clipboard`: share this one (None in dry runs and tests).
pub fn start(
    addr: &str,
    key: Key,
    edge: Edge,
    layout: LayoutFn,
    mut sink: BoxSink,
    clipboard: Option<Clipboard>,
    on_status: OnStatus,
) -> ClientHandle {
    let addr = if addr.contains(':') { addr.to_string() } else { format!("{addr}:{}", net::DEFAULT_PORT) };
    let stop = Arc::new(AtomicBool::new(false));
    let sock = Arc::new(Mutex::new(None::<TcpStream>));
    let thread = {
        let (stop, sock, on_status) = (stop.clone(), sock.clone(), on_status.clone());
        thread::spawn(move || {
            let mut backoff = Duration::from_millis(250);
            while !stop.load(Ordering::SeqCst) {
                on_status(Status::Connecting(addr.clone()));
                match connect(&addr) {
                    Ok(stream) => {
                        backoff = Duration::from_millis(250);
                        *sock.lock().unwrap() = stream.try_clone().ok();
                        // stop() may have run before the socket was stored.
                        if stop.load(Ordering::SeqCst) {
                            break;
                        }
                        let reason = match session(stream, &key, edge, &*layout, &mut *sink, clipboard, &on_status, &stop) {
                            Ok(()) => "closed".to_string(),
                            Err(e) => e.to_string(),
                        };
                        *sock.lock().unwrap() = None;
                        on_status(Status::Disconnected(reason));
                    }
                    Err(e) => on_status(Status::Retrying(format!("{addr}: {e}"))),
                }
                // Sleep in small steps so stop() is not held up by the backoff.
                let until = Instant::now() + backoff;
                while Instant::now() < until && !stop.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(20));
                }
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        })
    };
    ClientHandle { stop, sock, thread: Some(thread), on_status }
}

/// Connect with a timeout: a plain connect to an unreachable LAN host can hang
/// for about 20 s on Windows.
// ponytail: a connect in flight cannot be interrupted, so stop() can wait up to
// this timeout; a non-blocking connect polled against the stop flag if that lag matters.
fn connect(addr: &str) -> io::Result<TcpStream> {
    let sa = addr.to_socket_addrs()?.next().ok_or_else(|| io::Error::other("address did not resolve"))?;
    TcpStream::connect_timeout(&sa, Duration::from_secs(2))
}

impl ClientHandle {
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else { return };
        self.stop.store(true, Ordering::SeqCst);
        if let Some(s) = &*self.sock.lock().unwrap() {
            let _ = s.shutdown(Shutdown::Both);
        }
        let _ = thread.join();
        (self.on_status)(Status::Stopped);
    }
}

impl Drop for ClientHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[allow(clippy::too_many_arguments)] // one per thing a session uses; a struct would only rename them
fn session(
    stream: TcpStream,
    key: &Key,
    edge: Edge,
    layout: &dyn Fn() -> Layout,
    sink: Sink,
    clipboard: Option<Clipboard>,
    on_status: &OnStatus,
    stop: &AtomicBool,
) -> io::Result<()> {
    let peer = stream.peer_addr()?;
    let (tx, mut rx) = net::handshake(stream, key, true)?;
    on_status(Status::Connected(peer.to_string()));
    let desk = layout().bounds();
    tx.send(&Msg::Hello { version: VERSION, w: desk.w, h: desk.h })?;
    tx.spawn_heartbeat(net::HEARTBEAT);

    let mut held = Held::default();
    let mut clip = clipboard.map(Tracker::new);
    let mut cursor: Option<ClientCursor> = None;
    let res = loop {
        let m = match rx.recv() {
            Ok(m) => m,
            Err(e) => break Err(e),
        };
        // stop() shuts the socket down to end this read, but Windows can refuse
        // that on a live connection (WSAENOTCONN on the try_clone copy), and
        // the server's heartbeat keeps the read alive. So check here too.
        if stop.load(Ordering::SeqCst) {
            break Ok(());
        }
        match m {
            Msg::Hello { version, .. } if version != VERSION => {
                break Err(io::Error::other(format!("server protocol version {version}, want {VERSION}")));
            }
            Msg::Enter { y_frac } => {
                let c = ClientCursor::enter(edge, layout(), y_frac);
                let (x, y) = c.pos();
                println!("enter {x} {y}");
                sink(Act::MoveTo(x, y));
                if cursor.is_none() {
                    on_status(Status::Remote);
                }
                cursor = Some(c);
            }
            Msg::MouseMove { dx, dy } => {
                let Some(c) = &mut cursor else { continue };
                match c.apply(dx, dy) {
                    Step::Move(x, y) => sink(Act::MoveTo(x, y)),
                    Step::Leave(y_frac) => {
                        println!("leave {y_frac:.3}");
                        cursor = None;
                        on_status(Status::Local);
                        release_all(&mut held, sink);
                        // The clipboard goes ahead of control.
                        let parts = clip.as_mut().map(Tracker::outgoing).unwrap_or_default();
                        if let Err(e) = parts.iter().try_for_each(|p| tx.send(p)) {
                            break Err(e);
                        }
                        if let Err(e) = tx.send(&Msg::Leave { y_frac }) {
                            break Err(e);
                        }
                    }
                }
            }
            Msg::Clipboard { last, data } => {
                if let Some(c) = &mut clip {
                    c.incoming(last, data);
                }
            }
            Msg::Key { .. } | Msg::Button { .. } | Msg::Wheel { .. } => {
                held.track(&m);
                sink(Act::Input(m));
            }
            _ => {}
        }
    };
    if cursor.is_some() {
        on_status(Status::Local);
    }
    release_all(&mut held, sink);
    tx.shutdown();
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BUG-004: with stop set, a session ends at the next message even though
    /// its socket is never shut down and the server keeps heartbeating.
    #[test]
    fn session_ends_on_stop_without_a_socket_shutdown() {
        use std::net::TcpListener;
        use std::sync::mpsc;
        let key = net::keygen();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (tx, rx) = net::handshake(l.accept().unwrap().0, &key, false).unwrap();
            tx.spawn_heartbeat(Duration::from_millis(100));
            (tx, rx) // kept alive by the caller: this side never closes
        });
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let on: OnStatus = Arc::new(|_| {});
            let layout = || Layout::single(crate::edge::Rect { left: 0, top: 0, w: 100, h: 100 });
            let stop = AtomicBool::new(true);
            let r = session(TcpStream::connect(addr).unwrap(), &key, Edge::Right, &layout, &mut |_| {}, None, &on, &stop);
            let _ = done.send(r.is_ok());
        });
        let _peer = server.join().unwrap();
        assert_eq!(finished.recv_timeout(Duration::from_secs(3)), Ok(true), "session ignored stop");
    }

    #[test]
    fn absolute_units_land_on_the_exact_pixel() {
        // Windows maps absolute units back to a pixel as floor(n * w / 65536).
        for (left, w) in [(0, 1920), (0, 2560), (-1920, 3840), (0, 1366), (100, 7)] {
            for x in [left, left + 1, left + w / 2, left + w - 2, left + w - 1] {
                let n = to_absolute(x, left, w) as i64;
                assert_eq!(left as i64 + n * w as i64 / 65536, x as i64, "x {x} in {left}+{w}");
            }
        }
    }
}
