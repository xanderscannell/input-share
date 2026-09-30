// Server (--script) and client (--dry-run) as real processes on 127.0.0.1.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_input-share");

/// Kills the process on drop so a failed assert leaves nothing running.
struct Proc(Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn(args: &[&str]) -> (Proc, mpsc::Receiver<String>) {
    let mut child = Command::new(BIN).args(args).stdout(Stdio::piped()).spawn().unwrap();
    let out = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    (Proc(child), rx)
}

const SCRIPT: &str = "
wait connected
key 1d down        # Ctrl held locally before crossing
move 1000 540
move 1919 540      # cross: Enter y=0.5
wait remote
move 970 540       # center is 960,540
key 1e down
key 1e up
key 1d up          # Ctrl's up stays on the server, never forwarded
move 940 540       # client at x=10 moves -20: past its left edge, Leave
wait local
move 1919 100      # cross again
wait remote
key 30 down
button left down
                   # script ends: session closes, client must release both
";

#[test]
fn crossing_moves_keys_return_and_release_on_disconnect() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("e2e");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let key = dir.join("key.hex");
    let script = dir.join("script.txt");
    std::fs::write(&script, SCRIPT).unwrap();
    let (key, script) = (key.to_str().unwrap(), script.to_str().unwrap());

    assert!(Command::new(BIN).args(["keygen", "--key", key]).status().unwrap().success());

    let (mut server, srv_out) = spawn(&[
        "server", "--bind", "127.0.0.1:0", "--key", key, "--script", script, "--screen", "1920x1080",
    ]);
    let first = srv_out.recv_timeout(Duration::from_secs(10)).expect("server printed nothing");
    let addr = first.strip_prefix("listening ").expect(&first).to_string();

    let (_client, cl_out) = spawn(&["client", &addr, "--key", key, "--dry-run", "--screen", "2560x1600"]);
    let mut got = Vec::new();
    loop {
        let line = cl_out.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|_| panic!("client stalled after {got:#?}"));
        let done = line == "disconnected";
        got.push(line);
        if done {
            break;
        }
    }
    assert!(server.0.wait().unwrap().success(), "server script failed");

    let want = [
        "connected",
        "enter 0 800", // y 540 of 1080 -> 800 of 1600
        "move 0 800",
        "move 10 800",
        "key 1e down",
        "key 1e up",
        "leave 0.500",
        "release-all", // nothing held on this Leave
        "enter 0 148", // y 100 of 1080 -> 148 of 1600
        "move 0 148",
        "key 30 down",
        "button Left down",
        "release-all",
        "key 30 up",
        "button Left up",
        "disconnected",
    ];
    assert_eq!(got, want);
}
