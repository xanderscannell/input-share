// Client: connect out, reconnect with backoff, turn server messages into
// injected input. Injection goes through a sink so `--dry-run` can print
// instead of calling SendInput.

use crate::edge::{ClientCursor, Edge, Rect, Step};
use crate::keys::Held;
use crate::net::{self, Key};
use crate::proto::{Msg, VERSION};
use std::io;
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

/// Something to inject on this machine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Act {
    MoveTo(i32, i32),
    /// A Key, Button or Wheel message.
    Input(Msg),
}

pub type Sink<'a> = &'a mut dyn FnMut(Act);

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

fn release_all(held: &mut Held, sink: Sink) {
    println!("release-all");
    for m in held.release_all() {
        sink(Act::Input(m));
    }
}

/// Connect to `addr` forever, reconnecting with backoff. Never returns.
pub fn run(addr: &str, key: Key, edge: Edge, screen: Rect, sink: Sink) -> ! {
    let addr = if addr.contains(':') { addr.to_string() } else { format!("{addr}:{}", net::DEFAULT_PORT) };
    let mut backoff = Duration::from_millis(250);
    loop {
        match TcpStream::connect(&addr) {
            Ok(stream) => {
                backoff = Duration::from_millis(250);
                if let Err(e) = session(stream, &key, edge, screen, sink) {
                    eprintln!("session ended: {e}");
                }
                println!("disconnected");
            }
            Err(e) => eprintln!("connect {addr}: {e}"),
        }
        thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

fn session(stream: TcpStream, key: &Key, edge: Edge, screen: Rect, sink: Sink) -> io::Result<()> {
    let (tx, mut rx) = net::handshake(stream, key, true)?;
    println!("connected");
    tx.send(&Msg::Hello { version: VERSION, w: screen.w, h: screen.h })?;
    tx.spawn_heartbeat(net::HEARTBEAT);

    let mut held = Held::default();
    let mut cursor: Option<ClientCursor> = None;
    let res = loop {
        let m = match rx.recv() {
            Ok(m) => m,
            Err(e) => break Err(e),
        };
        match m {
            Msg::Hello { version, .. } if version != VERSION => {
                break Err(io::Error::other(format!("server protocol version {version}, want {VERSION}")));
            }
            Msg::Enter { y_frac } => {
                let c = ClientCursor::enter(edge, screen, y_frac);
                let (x, y) = c.pos();
                println!("enter {x} {y}");
                sink(Act::MoveTo(x, y));
                cursor = Some(c);
            }
            Msg::MouseMove { dx, dy } => {
                let Some(c) = &mut cursor else { continue };
                match c.apply(dx, dy) {
                    Step::Move(x, y) => sink(Act::MoveTo(x, y)),
                    Step::Leave(y_frac) => {
                        println!("leave {y_frac:.3}");
                        cursor = None;
                        release_all(&mut held, sink);
                        if let Err(e) = tx.send(&Msg::Leave { y_frac }) {
                            break Err(e);
                        }
                    }
                }
            }
            Msg::Key { .. } | Msg::Button { .. } | Msg::Wheel { .. } => {
                held.track(&m);
                sink(Act::Input(m));
            }
            _ => {}
        }
    };
    release_all(&mut held, sink);
    tx.shutdown();
    res
}
