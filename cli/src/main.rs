use input_share_core::edge::{Edge, Rect};
use input_share_core::status::{OnStatus, Status};
use input_share_core::{client, net, server, win};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

const USAGE: &str = "usage:
  input-share keygen [--key key.hex]
  input-share server [--bind 0.0.0.0:24800] [--key key.hex] [--edge right|left]
                     [--script FILE --screen WxH]   (without --script: real hooks)
  input-share client HOST[:PORT] [--key key.hex] [--edge right|left] [--dry-run --screen WxH]
                     (without --dry-run the screen is read from Windows)

--edge is the server's edge that leads to the client (default right); give both
ends the same value.";

/// Value after `--name`, if present.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn parse_screen(s: &str) -> Result<Rect, String> {
    let (w, h) = s.split_once('x').ok_or("--screen wants WxH")?;
    let (w, h) = (w.parse().map_err(|_| "bad --screen width")?, h.parse().map_err(|_| "bad --screen height")?);
    if w < 1 || h < 1 {
        return Err("--screen must be positive".into());
    }
    Ok(Rect { left: 0, top: 0, w, h })
}

/// Status lines on stdout (the e2e test reads them), details on stderr.
fn print_status() -> OnStatus {
    Arc::new(|s| match s {
        Status::Listening(addr) => println!("listening {addr}"),
        Status::Connected(_) => println!("connected"),
        Status::Disconnected(reason) => {
            eprintln!("session ended: {reason}");
            println!("disconnected");
        }
        Status::Retrying(why) => eprintln!("connect {why}"),
        Status::Connecting(_) | Status::Remote | Status::Local | Status::Stopped => {}
    })
}

fn park_forever() -> ! {
    loop {
        std::thread::park();
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let key_path = PathBuf::from(flag(args, "--key").unwrap_or("key.hex".into()));
    let edge = match flag(args, "--edge").as_deref() {
        None | Some("right") => Edge::Right,
        Some("left") => Edge::Left,
        Some(e) => return Err(format!("--edge must be right or left, not {e}")),
    };
    let load_key = || net::load_key(&key_path).map_err(|e| format!("{}: {e}", key_path.display()));
    let screen = || parse_screen(&flag(args, "--screen").ok_or("--screen WxH is required here")?);

    match args.first().map(String::as_str) {
        Some("keygen") => {
            if key_path.exists() {
                return Err(format!("{} already exists; delete it first to make a new key", key_path.display()));
            }
            std::fs::write(&key_path, net::key_to_hex(&net::keygen())).map_err(|e| e.to_string())?;
            println!("wrote {}; copy it to the other machine", key_path.display());
            Ok(())
        }
        Some("server") => {
            let bind = flag(args, "--bind").unwrap_or(format!("0.0.0.0:{}", net::DEFAULT_PORT));
            if let Some(script) = flag(args, "--script") {
                return server::run_script(&bind, load_key()?, edge, screen()?, script.as_ref(), print_status())
                    .map_err(|e| e.to_string());
            }
            let key = load_key()?;
            win::dpi_aware();
            server::run_hooks(&bind, key, edge, win::virtual_screen(), print_status()).map_err(|e| e.to_string())
        }
        Some("client") => {
            let host = args.get(1).filter(|h| !h.starts_with("--")).ok_or("client needs HOST")?;
            let _client = if args.iter().any(|a| a == "--dry-run") {
                client::start(host, load_key()?, edge, screen()?, Box::new(client::print_act), print_status())
            } else {
                let key = load_key()?;
                win::dpi_aware();
                let screen = win::virtual_screen();
                client::start(host, key, edge, screen, Box::new(client::send_input_sink(screen)), print_status())
            };
            park_forever()
        }
        _ => Err(USAGE.into()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
