// Framing, Noise NNpsk0 handshake, heartbeat and timeout.
// Frame: u16 length (LE) + Noise ciphertext.

use crate::proto::Msg;
use snow::{Builder, StatelessTransportState};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const PARAMS: &str = "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s";
pub const DEFAULT_PORT: u16 = 24800;
pub const HEARTBEAT: Duration = Duration::from_secs(1);
pub const TIMEOUT: Duration = Duration::from_secs(3);

pub type Key = [u8; 32];

fn noise_err(e: snow::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

/// True for a read that hit the timeout (Windows says TimedOut, Unix WouldBlock).
#[cfg(test)]
pub fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
}

pub fn keygen() -> Key {
    // An X25519 private key is 32 bytes from the CSPRNG; reuse it as the PSK.
    let kp = Builder::new(PARAMS.parse().unwrap()).generate_keypair().unwrap();
    kp.private.try_into().unwrap()
}

pub fn key_to_hex(key: &Key) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn key_from_hex(s: &str) -> io::Result<Key> {
    let s = s.trim();
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "key must be 64 hex characters");
    if s.len() != 64 || !s.is_ascii() {
        return Err(bad());
    }
    let mut key = [0u8; 32];
    for (i, b) in key.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|_| bad())?;
    }
    Ok(key)
}

pub fn load_key(path: &Path) -> io::Result<Key> {
    key_from_hex(&std::fs::read_to_string(path)?)
}

fn write_frame(mut w: impl Write, data: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(2 + data.len());
    buf.extend((data.len() as u16).to_le_bytes());
    buf.extend(data);
    w.write_all(&buf) // one write so TCP_NODELAY sends one segment
}

fn read_frame(mut r: impl Read, buf: &mut [u8; 65535]) -> io::Result<usize> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => io::Error::new(e.kind(), "peer closed the connection"),
        _ => e,
    })?;
    let len = u16::from_le_bytes(len) as usize;
    r.read_exact(&mut buf[..len])?;
    Ok(len)
}

/// Sending half. Clone it to share between the event path and the heartbeat.
#[derive(Clone)]
pub struct Sender {
    noise: Arc<StatelessTransportState>,
    // The nonce and the socket write sit under one lock so wire order matches nonce order.
    out: Arc<Mutex<(TcpStream, u64)>>,
}

impl Sender {
    pub fn send(&self, msg: &Msg) -> io::Result<()> {
        let mut out = self.out.lock().unwrap();
        let mut ct = [0u8; 64];
        let n = self.noise.write_message(out.1, &msg.encode(), &mut ct).map_err(noise_err)?;
        out.1 += 1;
        write_frame(&out.0, &ct[..n])
    }

    /// Send Heartbeat every `every` on a background thread until a send fails.
    pub fn spawn_heartbeat(&self, every: Duration) {
        let tx = self.clone();
        thread::spawn(move || while tx.send(&Msg::Heartbeat).is_ok() {
            thread::sleep(every);
        });
    }

    /// Close our sending side after everything already written (TCP FIN). The
    /// peer's reads end; our Receiver keeps working until the peer closes too,
    /// which avoids the RST that closing with unread data would cause.
    pub fn shutdown(&self) {
        let _ = self.out.lock().unwrap().0.shutdown(std::net::Shutdown::Write);
    }
}

/// Receiving half. Any message, heartbeat included, proves the peer is alive.
pub struct Receiver {
    noise: Arc<StatelessTransportState>,
    stream: TcpStream,
    nonce: u64,
    buf: Box<[u8; 65535]>,
}

impl Receiver {
    /// Block for the next message. A silence longer than the timeout (default
    /// `TIMEOUT`) returns an error for which `is_timeout` is true.
    pub fn recv(&mut self) -> io::Result<Msg> {
        let n = read_frame(&self.stream, &mut self.buf)?;
        let mut pt = [0u8; 64];
        let m = self.noise.read_message(self.nonce, &self.buf[..n], &mut pt).map_err(noise_err)?;
        self.nonce += 1;
        Msg::decode(&pt[..m]).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))
    }

    #[cfg(test)]
    pub fn set_timeout(&self, t: Duration) -> io::Result<()> {
        self.stream.set_read_timeout(Some(t))
    }
}

/// Run the handshake on a connected stream. The client (the side that
/// connected out) is the Noise initiator. A wrong key fails here.
pub fn handshake(stream: TcpStream, key: &Key, initiator: bool) -> io::Result<(Sender, Receiver)> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    let b = Builder::new(PARAMS.parse().unwrap()).psk(0, key).map_err(noise_err)?;
    let mut hs = if initiator { b.build_initiator() } else { b.build_responder() }.map_err(noise_err)?;
    let mut buf = Box::new([0u8; 65535]);
    let mut scratch = [0u8; 128];
    // NNpsk0: initiator -> (psk, e), responder -> (e, ee).
    for my_turn in if initiator { [true, false] } else { [false, true] } {
        if my_turn {
            let n = hs.write_message(&[], &mut scratch).map_err(noise_err)?;
            write_frame(&stream, &scratch[..n])?;
        } else {
            let n = read_frame(&stream, &mut buf)?;
            hs.read_message(&buf[..n], &mut scratch).map_err(noise_err)?;
        }
    }
    let noise = Arc::new(hs.into_stateless_transport_mode().map_err(noise_err)?);
    let tx = Sender { noise: noise.clone(), out: Arc::new(Mutex::new((stream.try_clone()?, 0))) };
    let rx = Receiver { noise, stream, nonce: 0, buf };
    Ok((tx, rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::Button;
    use std::net::TcpListener;
    use std::time::Instant;

    /// Handshake a client and a server over 127.0.0.1 with the given keys.
    type End = io::Result<(Sender, Receiver)>;

    fn pair(ck: Key, sk: Key) -> (End, End) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let server = thread::spawn(move || handshake(l.accept().unwrap().0, &sk, false));
        let client = handshake(TcpStream::connect(addr).unwrap(), &ck, true);
        (client, server.join().unwrap())
    }

    #[test]
    fn messages_round_trip_both_ways() {
        let k = keygen();
        let (c, s) = pair(k, k);
        let ((ctx, mut crx), (stx, mut srx)) = (c.unwrap(), s.unwrap());
        let msgs = [
            Msg::Enter { y_frac: 0.5 },
            Msg::MouseMove { dx: 3, dy: -4 },
            Msg::Button { button: Button::Left, down: true },
            Msg::Key { scancode: 0x1E, extended: false, down: true },
            Msg::Heartbeat,
        ];
        for m in msgs {
            stx.send(&m).unwrap();
        }
        for m in msgs {
            assert_eq!(crx.recv().unwrap(), m);
        }
        ctx.send(&Msg::Leave { y_frac: 0.25 }).unwrap();
        assert_eq!(srx.recv().unwrap(), Msg::Leave { y_frac: 0.25 });
    }

    #[test]
    fn wrong_key_fails_handshake() {
        let (c, s) = pair(keygen(), keygen());
        assert!(s.is_err(), "server must reject a client with the wrong key");
        assert!(c.is_err(), "client must not get a session either");
    }

    #[test]
    fn missed_heartbeats_report_timeout() {
        let k = keygen();
        let (c, s) = pair(k, k);
        let ((ctx, _crx), (_stx, mut srx)) = (c.unwrap(), s.unwrap());
        srx.set_timeout(Duration::from_millis(300)).unwrap();

        // Heartbeats keep the link alive for longer than the timeout.
        ctx.spawn_heartbeat(Duration::from_millis(50));
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(700) {
            assert_eq!(srx.recv().unwrap(), Msg::Heartbeat);
        }
        // Closing ends the stream: recv errors (EOF), and so does the heartbeat thread.
        ctx.shutdown();
        while let Ok(m) = srx.recv() {
            assert_eq!(m, Msg::Heartbeat);
        }

        // A peer that stays connected but sends nothing: recv reports a timeout.
        let (c, s) = pair(k, k);
        let ((_ctx, _crx), (_stx, mut srx)) = (c.unwrap(), s.unwrap());
        srx.set_timeout(Duration::from_millis(200)).unwrap();
        let start = Instant::now();
        let err = srx.recv().unwrap_err();
        assert!(is_timeout(&err), "{err:?}");
        assert!(start.elapsed() >= Duration::from_millis(150));
    }

    #[test]
    fn key_hex_round_trip_and_rejects_garbage() {
        let k = keygen();
        assert_ne!(k, keygen());
        assert_eq!(key_from_hex(&format!("{}\r\n", key_to_hex(&k))).unwrap(), k);
        assert!(key_from_hex("abcd").is_err());
        assert!(key_from_hex(&"zz".repeat(32)).is_err());
    }
}
