// Clipboard sharing: plain text, plus the HTML and RTF that carry formatting.
// When control leaves a computer, it sends its clipboard along, but only if
// the clipboard changed since the session started or since it last sent or
// received one. So crossing back never replaces what the other computer has
// (an image, say) with stale contents.
//
// On the wire the clipboard is one blob split over Clipboard messages. Per
// format: a kind byte, a u32 LE length, then the bytes. Text is UTF-8; HTML
// and RTF are Windows' own bytes, without the terminating NUL.

use crate::proto::Msg;
use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW,
    SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::core::w;

/// Lives in the Win32_System_Ole feature, too big to enable for one constant.
const CF_UNICODETEXT: u32 = 13;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Format {
    Text = 0,
    Html = 1,
    Rtf = 2,
}

/// A clipboard's shareable formats, richest first.
pub type Contents = Vec<(Format, Vec<u8>)>;

/// The most clipboard data sent at a crossing. It goes ahead of control, so a
/// big one would hold the crossing up.
// ponytail: past this, formatting is dropped, then text too; send big
// clipboards over a second connection in the background if that matters.
pub const MAX_SIZE: usize = 1 << 20;

/// The most data per Clipboard message: a Noise message is at most 65535
/// bytes, 16 of them the AEAD tag, then the message's tag and `last` bytes.
pub const MAX_CHUNK: usize = 65535 - 16 - 2;

/// Where the clipboard comes from and goes to: Windows' clipboard, or a fake in tests.
#[derive(Clone, Copy)]
pub struct Clipboard {
    /// Changes whenever the clipboard's contents change.
    pub seq: fn() -> u32,
    /// Empty when the clipboard holds none of the shared formats.
    pub get: fn() -> Contents,
    pub set: fn(&Contents),
}

pub const WINDOWS: Clipboard = Clipboard { seq: win_seq, get: win_get, set: win_set };

pub fn encode(c: &Contents) -> Vec<u8> {
    let mut b = Vec::new();
    for (format, data) in c {
        b.push(*format as u8);
        b.extend((data.len() as u32).to_le_bytes());
        b.extend(data);
    }
    b
}

/// None if the blob is cut short. Unknown kinds (a newer peer's) are skipped.
pub fn decode(mut b: &[u8]) -> Option<Contents> {
    let mut out = Vec::new();
    while let Some((&kind, rest)) = b.split_first() {
        let len = u32::from_le_bytes(rest.get(..4)?.try_into().unwrap()) as usize;
        let data = rest[4..].get(..len)?;
        let format = match kind {
            0 => Some(Format::Text),
            1 => Some(Format::Html),
            2 => Some(Format::Rtf),
            _ => None,
        };
        if let Some(f) = format {
            out.push((f, data.to_vec()));
        }
        b = &rest[4 + len..];
    }
    Some(out)
}

/// One computer's side of the clipboard for one session.
pub struct Tracker {
    cb: Clipboard,
    seen: u32,
    /// Clipboard messages received so far, until the last one.
    pending: Vec<u8>,
    too_big: bool,
}

impl Tracker {
    pub fn new(cb: Clipboard) -> Self {
        Tracker { cb, seen: (cb.seq)(), pending: Vec::new(), too_big: false }
    }

    /// Messages to send as control leaves this computer: none unless the
    /// clipboard changed since the last `outgoing` or `incoming`, holds a
    /// shared format, and fits.
    pub fn outgoing(&mut self) -> Vec<Msg> {
        let seq = (self.cb.seq)();
        if seq == self.seen {
            return vec![];
        }
        self.seen = seq;
        let all = (self.cb.get)();
        if all.is_empty() {
            return vec![];
        }
        let mut blob = encode(&all);
        if blob.len() > MAX_SIZE {
            // Formatting is usually what makes it big; the text alone may fit.
            blob = encode(&all.into_iter().filter(|(f, _)| *f == Format::Text).collect());
        }
        if blob.len() > MAX_SIZE || blob.is_empty() {
            eprintln!("clipboard is over the {MAX_SIZE}-byte limit: not shared");
            return vec![];
        }
        let n = blob.len().div_ceil(MAX_CHUNK);
        blob.chunks(MAX_CHUNK).enumerate().map(|(i, c)| Msg::Clipboard { last: i + 1 == n, data: c.to_vec() }).collect()
    }

    /// One Clipboard message from the other computer. The last one sets this clipboard.
    pub fn incoming(&mut self, last: bool, data: Vec<u8>) {
        if self.pending.len() + data.len() <= MAX_SIZE {
            self.pending.extend(data);
        } else {
            self.too_big = true;
        }
        if !last {
            return;
        }
        let blob = std::mem::take(&mut self.pending);
        match decode(&blob) {
            Some(c) if !std::mem::take(&mut self.too_big) && !c.is_empty() => {
                (self.cb.set)(&c);
                self.seen = (self.cb.seq)();
            }
            _ => eprintln!("clipboard from the other computer was unreadable or too big: not pasted here"),
        }
    }
}

fn win_seq() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// Each shared format's clipboard id, richest first: the order apps see them in.
fn formats() -> [(Format, u32); 3] {
    unsafe {
        [
            (Format::Html, RegisterClipboardFormatW(w!("HTML Format"))),
            (Format::Rtf, RegisterClipboardFormatW(w!("Rich Text Format"))),
            (Format::Text, CF_UNICODETEXT),
        ]
    }
}

/// Another program may hold the clipboard open for a moment, so retry briefly.
fn open() -> bool {
    for _ in 0..10 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    eprintln!("clipboard busy: not shared");
    false
}

/// The whole allocation behind one format, if the clipboard has it. Call while open.
unsafe fn read(id: u32) -> Option<Vec<u8>> {
    unsafe {
        let h = HGLOBAL(GetClipboardData(id).ok()?.0);
        let p = GlobalLock(h) as *const u8;
        if p.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(p, GlobalSize(h)).to_vec();
        let _ = GlobalUnlock(h);
        Some(bytes)
    }
}

/// Put one format on the clipboard. Call while open, after EmptyClipboard.
unsafe fn put(id: u32, bytes: &[u8]) {
    unsafe {
        let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) else { return };
        let p = GlobalLock(h) as *mut u8;
        if !p.is_null() {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            let _ = GlobalUnlock(h);
        }
        // Once set, the clipboard owns the memory; free it only on failure.
        if p.is_null() || SetClipboardData(id, Some(HANDLE(h.0))).is_err() {
            let _ = GlobalFree(Some(h));
        }
    }
}

fn win_get() -> Contents {
    if !open() {
        return vec![];
    }
    let mut out = Vec::new();
    for (format, id) in formats() {
        let Some(bytes) = (unsafe { read(id) }) else { continue };
        // Both end at a NUL; the allocation can be longer than the data.
        let data = match format {
            Format::Text => {
                let wide: Vec<u16> =
                    bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
                String::from_utf16_lossy(&wide).into_bytes()
            }
            _ => bytes.into_iter().take_while(|&b| b != 0).collect(),
        };
        out.push((format, data));
    }
    let _ = unsafe { CloseClipboard() };
    out
}

fn win_set(c: &Contents) {
    if !open() {
        return;
    }
    let ids = formats();
    unsafe {
        let _ = EmptyClipboard();
        for (format, data) in c {
            let Some(&(_, id)) = ids.iter().find(|(f, _)| f == format) else { continue };
            let bytes: Vec<u8> = match format {
                Format::Text => String::from_utf8_lossy(data).encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect(),
                _ => data.iter().copied().chain([0]).collect(),
            };
            put(id, &bytes);
        }
        let _ = CloseClipboard();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn text(s: &str) -> (Format, Vec<u8>) {
        (Format::Text, s.as_bytes().to_vec())
    }

    #[test]
    fn blob_round_trips_skips_unknown_kinds_and_rejects_cuts() {
        let c = vec![(Format::Html, b"<b>hi</b>".to_vec()), (Format::Rtf, vec![]), text("hi")];
        let mut b = encode(&c);
        assert_eq!(decode(&b), Some(c.clone()));
        assert_eq!(decode(&b[..3]), None, "cut inside a length");
        assert_eq!(decode(&b[..b.len() - 1]), None, "cut inside the data");
        b.extend([9, 1, 0, 0, 0, b'?']); // a kind from a newer version
        assert_eq!(decode(&b), Some(c));
        assert_eq!(decode(&[0, 5, 0, 0, 0, b'a']), None, "length runs past the end");
    }

    // A fake clipboard; only the next test uses it.
    static SEQ: AtomicU32 = AtomicU32::new(0);
    static BOARD: Mutex<Contents> = Mutex::new(Vec::new());

    fn copy(c: Contents) {
        *BOARD.lock().unwrap() = c;
        SEQ.fetch_add(1, Ordering::SeqCst);
    }

    const FAKE: Clipboard = Clipboard {
        seq: || SEQ.load(Ordering::SeqCst),
        get: || BOARD.lock().unwrap().clone(),
        set: |c| copy(c.clone()),
    };

    /// What the other computer ends up with after `msgs` arrive.
    fn deliver(t: &mut Tracker, msgs: Vec<Msg>) -> Contents {
        copy(vec![]);
        for m in msgs {
            let Msg::Clipboard { last, data } = m else { panic!("{m:?}") };
            t.incoming(last, data);
        }
        BOARD.lock().unwrap().clone()
    }

    /// Overwrites this computer's clipboard, so it only runs when asked:
    /// `cargo test -p input-share-core -- --ignored`.
    #[test]
    #[ignore]
    fn real_clipboard_round_trip() {
        let c = vec![
            (Format::Html, "Version:0.9\r\nStartHTML:0\r\n<b>h\u{e9}</b>".as_bytes().to_vec()),
            (Format::Rtf, br"{\rtf1 {\b bold}}".to_vec()),
            text("h\u{e9}llo \u{1F600}\r\nline 2"),
        ];
        let before = win_seq();
        win_set(&c);
        assert_ne!(win_seq(), before, "setting changes the sequence number");
        assert_eq!(win_get(), c);
    }

    #[test]
    fn sends_only_what_changed_here() {
        copy(vec![text("from before the session")]);
        let mut t = Tracker::new(FAKE);
        assert!(t.outgoing().is_empty(), "a clipboard from before the session stays here");

        let formatted = vec![(Format::Html, b"<i>copied</i>".to_vec()), text("copied")];
        copy(formatted.clone());
        let msgs = t.outgoing();
        assert_eq!(msgs.len(), 1);
        assert!(t.outgoing().is_empty(), "crossing again without copying sends nothing");
        assert_eq!(deliver(&mut t, msgs), formatted);
        assert!(t.outgoing().is_empty(), "received contents are never echoed back");

        copy(vec![]); // an image, say
        assert!(t.outgoing().is_empty(), "nothing shareable: nothing to send");

        // Big formatting spans several messages and arrives whole.
        let big = vec![(Format::Rtf, vec![b'r'; MAX_CHUNK * 2 + 5]), text("big")];
        copy(big.clone());
        let msgs = t.outgoing();
        assert_eq!(msgs.len(), 3);
        assert_eq!(deliver(&mut t, msgs), big);

        // Formatting too big to send: the text still goes.
        copy(vec![(Format::Html, vec![b'h'; MAX_SIZE]), text("plain")]);
        let msgs = t.outgoing();
        assert_eq!(deliver(&mut t, msgs), vec![text("plain")]);

        copy(vec![text(&"x".repeat(MAX_SIZE))]);
        assert!(t.outgoing().is_empty(), "too big even as text");
    }
}
