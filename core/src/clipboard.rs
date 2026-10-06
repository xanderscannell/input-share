// Clipboard sharing: text, the HTML and RTF that carry its formatting, and
// images. When control leaves a computer, it sends its clipboard along, but
// only if the clipboard changed since the session started or since it last
// sent or received one. So crossing back never replaces what the other
// computer has with stale contents.
//
// The clipboard travels as one blob, deflated and split over Clipboard
// messages. Per format: a kind byte, a u32 LE length, then the bytes. Text is
// UTF-8; HTML and RTF are Windows' own bytes without the terminating NUL; an
// image is a CF_DIBV5 (a BITMAPV5HEADER, then the pixels). Copied files are
// their paths (UTF-8, NUL between them) on this computer, and on the wire a
// `files` archive of what they hold, read as the transfer starts.
//
// A small blob goes just ahead of the crossing. A big one, or any files,
// would hold the crossing up, so an Offer goes instead and the blob moves
// over a second connection (a transfer) in the background. The client always
// opens that connection, pulling from the server or pushing to it, because
// the server is the side that accepts connections.

use crate::files;
use crate::net::{Receiver, Sender};
use crate::proto::Msg;
use std::borrow::Cow;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW,
    SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::core::w;

// These live in the Win32_System_Ole feature, too big to enable for three constants.
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const CF_DIBV5: u32 = 17;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Format {
    Text = 0,
    Html = 1,
    Rtf = 2,
    Image = 3,
    Files = 4,
}

/// A clipboard's shareable formats, in the order apps should see them.
pub type Contents = Vec<(Format, Vec<u8>)>;

/// The biggest blob (before compression) sent just ahead of a crossing.
pub const MAX_INLINE: usize = 1 << 20;

/// The biggest clipboard shared at all, before compression. Past it, only the
/// text is sent, if that fits.
pub const MAX_SIZE: usize = 100 << 20;

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
    /// The folder files from the other computer are unpacked into. Emptied
    /// each time new ones arrive.
    pub received: fn() -> PathBuf,
}

pub const WINDOWS: Clipboard =
    Clipboard { seq: win_seq, get: win_get, set: win_set, received: || std::env::temp_dir().join("input-share-clipboard") };

/// A Files entry's paths.
pub fn paths(data: &[u8]) -> Vec<PathBuf> {
    String::from_utf8_lossy(data).split('\0').filter(|p| !p.is_empty()).map(PathBuf::from).collect()
}

/// Paths as a Files entry.
pub fn files_entry(paths: &[PathBuf]) -> (Format, Vec<u8>) {
    let joined: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    (Format::Files, joined.join("\0").into_bytes())
}

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
            3 => Some(Format::Image),
            4 => Some(Format::Files),
            _ => None,
        };
        if let Some(f) = format {
            out.push((f, data.to_vec()));
        }
        b = &rest[4 + len..];
    }
    Some(out)
}

/// Level 1: fast. A screenshot still shrinks severalfold.
fn compress(blob: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec(blob, 1)
}

fn unzip(z: &[u8]) -> Option<Contents> {
    decode(&miniz_oxide::inflate::decompress_to_vec_with_limit(z, MAX_SIZE).ok()?)
}

/// The blob as it goes on the wire: each Files entry's paths replaced by the
/// files themselves, read now. Other blobs pass through untouched.
fn with_files(blob: &[u8]) -> io::Result<Cow<'_, [u8]>> {
    let Some(mut c) = decode(blob) else { return Err(io::Error::other("unreadable clipboard")) };
    if !c.iter().any(|(f, _)| *f == Format::Files) {
        return Ok(Cow::Borrowed(blob));
    }
    for (format, data) in &mut c {
        if *format == Format::Files {
            *data = files::pack(&paths(data), MAX_SIZE)?;
        }
    }
    let blob = encode(&c);
    if blob.len() > MAX_SIZE {
        return Err(io::Error::other(format!("clipboard is over the {MAX_SIZE}-byte limit")));
    }
    Ok(Cow::Owned(blob))
}

/// A compressed blob as Clipboard messages, the last one marked.
fn parts(z: &[u8]) -> impl Iterator<Item = Msg> + '_ {
    let n = z.len().div_ceil(MAX_CHUNK);
    z.chunks(MAX_CHUNK).enumerate().map(move |(i, c)| Msg::Clipboard { last: i + 1 == n, data: c.to_vec() })
}

/// What to send as control leaves this computer.
pub enum Outgoing {
    /// Nothing new to share.
    Nothing,
    /// Small: send these just ahead of the crossing.
    Inline(Vec<Msg>),
    /// Big: send `Msg::Offer` instead, then this blob with `send_big` over a transfer.
    Big(Arc<Vec<u8>>),
}

/// One computer's side of the clipboard for one session.
pub struct Tracker {
    cb: Clipboard,
    /// The sequence number after this side last sent or set the clipboard.
    seen: u32,
    /// Inline Clipboard messages received so far, until the last one.
    pending: Vec<u8>,
    too_big: bool,
    /// Bumped by every send, receive and offer, so a transfer that lands
    /// after something newer is dropped.
    generation: u64,
    /// The sequence number when the last offer came: a copy here since then wins.
    offer_seq: u32,
    /// The last offer's generation, for a push that arrives on its own connection.
    pub awaited: u64,
    /// This side's last big clipboard, for the other computer to pull.
    pub big: Option<Arc<Vec<u8>>>,
}

impl Tracker {
    pub fn new(cb: Clipboard) -> Self {
        let seq = (cb.seq)();
        Tracker { cb, seen: seq, pending: Vec::new(), too_big: false, generation: 0, offer_seq: seq, awaited: 0, big: None }
    }

    /// Nothing unless the clipboard changed since the last `outgoing` or
    /// `incoming`, holds a shared format, and fits. Files are always Big:
    /// they are read only once the transfer starts.
    pub fn outgoing(&mut self) -> Outgoing {
        let seq = (self.cb.seq)();
        if seq == self.seen {
            return Outgoing::Nothing;
        }
        self.seen = seq;
        let all = (self.cb.get)();
        if all.is_empty() {
            return Outgoing::Nothing;
        }
        let has_files = all.iter().any(|(f, _)| *f == Format::Files);
        let mut blob = encode(&all);
        if blob.len() > MAX_SIZE {
            // An image or formatting is what makes it big; the text alone may fit.
            blob = encode(&all.into_iter().filter(|(f, _)| *f == Format::Text).collect());
        }
        if blob.is_empty() || blob.len() > MAX_SIZE {
            eprintln!("clipboard is over the {MAX_SIZE}-byte limit: not shared");
            return Outgoing::Nothing;
        }
        self.generation += 1;
        if blob.len() <= MAX_INLINE && !has_files {
            return Outgoing::Inline(parts(&compress(&blob)).collect());
        }
        let blob = Arc::new(blob);
        self.big = Some(blob.clone());
        Outgoing::Big(blob)
    }

    /// One inline Clipboard message. The last one sets this clipboard.
    pub fn incoming(&mut self, last: bool, data: Vec<u8>) {
        // Twice the limit: room for deflate's overhead on data that does not compress.
        if self.pending.len() + data.len() <= 2 * MAX_INLINE {
            self.pending.extend(data);
        } else {
            self.too_big = true;
        }
        if !last {
            return;
        }
        let z = std::mem::take(&mut self.pending);
        if std::mem::take(&mut self.too_big) {
            eprintln!("clipboard from the other computer is too big: not pasted here");
            return;
        }
        self.generation += 1;
        self.apply(&z);
    }

    /// The other computer sent an Offer. Returns the generation to `finish` with.
    pub fn offered(&mut self) -> u64 {
        self.generation += 1;
        self.offer_seq = (self.cb.seq)();
        self.awaited = self.generation;
        self.generation
    }

    /// A transfer for the offer of `generation` arrived. Dropped if anything
    /// happened since: another send or receive, or a copy on this computer.
    pub fn finish(&mut self, generation: u64, z: &[u8]) {
        if generation == self.generation && (self.cb.seq)() == self.offer_seq {
            self.apply(z);
        }
    }

    /// The session ended: transfers still on their way are dropped.
    pub fn end(&mut self) {
        self.generation += 1;
        self.big = None;
    }

    fn apply(&mut self, z: &[u8]) {
        let c = unzip(z).filter(|c| !c.is_empty()).ok_or_else(|| "was unreadable or too big".to_string());
        // Files arrive as an archive: unpack it, and paste the unpacked copies.
        let c = c.and_then(|mut c| {
            for (format, data) in &mut c {
                if *format == Format::Files {
                    let top = files::unpack(data, &(self.cb.received)()).map_err(|e| format!("had files that could not be saved: {e}"))?;
                    *data = files_entry(&top).1;
                }
            }
            Ok(c)
        });
        match c {
            Ok(c) => {
                (self.cb.set)(&c);
                self.seen = (self.cb.seq)();
            }
            Err(why) => eprintln!("clipboard from the other computer {why}: not pasted here"),
        }
    }
}

fn stopped() -> io::Error {
    io::Error::other("stopped")
}

/// Send a big blob (from `Outgoing::Big`) over a transfer connection, then close our side.
pub fn send_big(tx: &Sender, blob: &[u8], stop: &AtomicBool) -> io::Result<()> {
    // Reading and compressing 100 MB can outlast the receiver's timeout; heartbeats cover it.
    tx.spawn_heartbeat(crate::net::HEARTBEAT);
    let sent = with_files(blob).and_then(|blob| {
        parts(&compress(&blob)).try_for_each(|m| if stop.load(Ordering::SeqCst) { Err(stopped()) } else { tx.send(&m) })
    });
    tx.shutdown();
    sent
}

/// Receive a big blob over a transfer connection, still compressed, for `Tracker::finish`.
pub fn recv_big(rx: &mut Receiver, stop: &AtomicBool) -> io::Result<Vec<u8>> {
    let mut z = Vec::new();
    loop {
        if stop.load(Ordering::SeqCst) {
            return Err(stopped());
        }
        match rx.recv()? {
            Msg::Clipboard { last, data } => {
                if z.len() + data.len() > 2 * MAX_SIZE {
                    return Err(io::Error::other("clipboard transfer is too big"));
                }
                z.extend(data);
                if last {
                    return Ok(z);
                }
            }
            Msg::Heartbeat => {}
            _ => return Err(io::Error::other("unexpected message in a clipboard transfer")),
        }
    }
}

fn win_seq() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// Each shared format's clipboard id, in the order apps see them: text
/// before the image, so an app that takes both (Word, given cells copied from
/// Excel) still pastes the text.
fn formats() -> [(Format, u32); 5] {
    unsafe {
        [
            (Format::Html, RegisterClipboardFormatW(w!("HTML Format"))),
            (Format::Rtf, RegisterClipboardFormatW(w!("Rich Text Format"))),
            (Format::Text, CF_UNICODETEXT),
            // Windows makes CF_DIBV5 from any bitmap on the clipboard, and the
            // other bitmap formats from it when we set it.
            (Format::Image, CF_DIBV5),
            (Format::Files, CF_HDROP),
        ]
    }
}

/// The paths in a CF_HDROP: a DROPFILES (offset of the list at 0, wide flag
/// at 16), then each path NUL-terminated, then one more NUL.
// ponytail: only the wide (UTF-16) form, which everything since Windows 2000 writes.
fn hdrop_paths(b: &[u8]) -> Option<Vec<PathBuf>> {
    let at = |i: usize| b.get(i..i + 4).map(|x| u32::from_le_bytes(x.try_into().unwrap()));
    if at(16)? == 0 {
        return None;
    }
    let wide: Vec<u16> = b.get(at(0)? as usize..)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    Some(wide.split(|&c| c == 0).take_while(|p| !p.is_empty()).map(|p| PathBuf::from(String::from_utf16_lossy(p))).collect())
}

fn hdrop(paths: &[PathBuf]) -> Vec<u8> {
    let mut b = vec![0u8; 20];
    b[..4].copy_from_slice(&20u32.to_le_bytes());
    b[16..20].copy_from_slice(&1u32.to_le_bytes());
    for p in paths {
        b.extend(p.to_string_lossy().encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
    }
    b.extend([0, 0]);
    b
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
        // Text formats end at a NUL; the allocation can be longer than the data.
        let data = match format {
            Format::Text => {
                let wide: Vec<u16> =
                    bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
                String::from_utf16_lossy(&wide).into_bytes()
            }
            Format::Html | Format::Rtf => bytes.into_iter().take_while(|&b| b != 0).collect(),
            Format::Image => bytes,
            Format::Files => match hdrop_paths(&bytes) {
                Some(p) if !p.is_empty() => files_entry(&p).1,
                _ => continue,
            },
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
                Format::Html | Format::Rtf => data.iter().copied().chain([0]).collect(),
                Format::Image => data.clone(),
                Format::Files => hdrop(&paths(data)),
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
    use std::sync::atomic::AtomicU32;

    fn text(s: &str) -> (Format, Vec<u8>) {
        (Format::Text, s.as_bytes().to_vec())
    }

    #[test]
    fn file_lists_round_trip_through_hdrop() {
        let p = vec![PathBuf::from(r"C:\Users\me\café.txt"), PathBuf::from(r"D:\folder")];
        assert_eq!(hdrop_paths(&hdrop(&p)), Some(p.clone()));
        assert_eq!(paths(&files_entry(&p).1), p);
        let mut ansi = hdrop(&p);
        ansi[16] = 0;
        assert_eq!(hdrop_paths(&ansi), None, "the old ANSI form is not read");
        assert_eq!(hdrop_paths(&[1, 2]), None, "too short");
    }

    #[test]
    fn blob_round_trips_skips_unknown_kinds_and_rejects_cuts() {
        let c = vec![(Format::Html, b"<b>hi</b>".to_vec()), (Format::Rtf, vec![]), text("hi"), (Format::Image, vec![1, 2])];
        let mut b = encode(&c);
        assert_eq!(decode(&b), Some(c.clone()));
        assert_eq!(decode(&b[..3]), None, "cut inside a length");
        assert_eq!(decode(&b[..b.len() - 1]), None, "cut inside the data");
        b.extend([9, 1, 0, 0, 0, b'?']); // a kind from a newer version
        assert_eq!(decode(&b), Some(c.clone()));
        assert_eq!(unzip(&compress(&b)), Some(c), "survives compression");
        assert_eq!(unzip(b"not deflate"), None);
    }

    /// Overwrites this computer's clipboard, so it only runs when asked:
    /// `cargo test -p input-share-core -- --ignored`.
    #[test]
    #[ignore]
    fn real_clipboard_round_trip() {
        // A 2x1 32-bit top-down BITMAPV5HEADER image (124-byte header, BI_RGB).
        let mut dib = vec![0u8; 124];
        dib[..4].copy_from_slice(&124u32.to_le_bytes());
        dib[4..8].copy_from_slice(&2i32.to_le_bytes());
        dib[8..12].copy_from_slice(&(-1i32).to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        dib[20..24].copy_from_slice(&8u32.to_le_bytes()); // image size
        dib.extend([0, 0, 255, 255, 255, 0, 0, 255]); // red, blue (BGRA)
        let c = vec![
            (Format::Html, "Version:0.9\r\nStartHTML:0\r\n<b>h\u{e9}</b>".as_bytes().to_vec()),
            (Format::Rtf, br"{\rtf1 {\b bold}}".to_vec()),
            text("h\u{e9}llo \u{1F600}\r\nline 2"),
            (Format::Image, dib.clone()),
        ];
        let before = win_seq();
        win_set(&c);
        assert_ne!(win_seq(), before, "setting changes the sequence number");
        let got = win_get();
        assert_eq!(got[..3], c[..3]);
        let (Format::Image, img) = &got[3] else { panic!("no image: {:?}", got.iter().map(|f| f.0).collect::<Vec<_>>()) };
        assert!(img.starts_with(&dib[..16]) && img.len() >= dib.len(), "the image reads back");

        let file = std::env::temp_dir().join("input-share-real-clipboard.txt");
        std::fs::write(&file, "x").unwrap();
        let entry = files_entry(std::slice::from_ref(&file));
        win_set(&vec![entry.clone()]);
        assert_eq!(win_get(), vec![entry]);
        let _ = std::fs::remove_file(file);
    }

    // A fake clipboard; only the next test uses it.
    static SEQ: AtomicU32 = AtomicU32::new(0);
    static BOARD: Mutex<Contents> = Mutex::new(Vec::new());

    fn copy(c: Contents) {
        *BOARD.lock().unwrap() = c;
        SEQ.fetch_add(1, Ordering::SeqCst);
    }

    fn board() -> Contents {
        BOARD.lock().unwrap().clone()
    }

    const FAKE: Clipboard = Clipboard {
        seq: || SEQ.load(Ordering::SeqCst),
        get: board,
        set: |c| copy(c.clone()),
        received: || std::env::temp_dir().join(format!("input-share-test-fake-{}", std::process::id())),
    };

    fn inline(o: Outgoing) -> Vec<Msg> {
        match o {
            Outgoing::Inline(msgs) => msgs,
            Outgoing::Nothing => panic!("nothing to send"),
            Outgoing::Big(_) => panic!("sent as big"),
        }
    }

    /// What the other computer ends up with after `msgs` arrive.
    fn deliver(t: &mut Tracker, msgs: Vec<Msg>) -> Contents {
        copy(vec![]);
        for m in msgs {
            let Msg::Clipboard { last, data } = m else { panic!("{m:?}") };
            t.incoming(last, data);
        }
        board()
    }

    #[test]
    fn sends_only_what_changed_here() {
        copy(vec![text("from before the session")]);
        let mut t = Tracker::new(FAKE);
        assert!(matches!(t.outgoing(), Outgoing::Nothing), "a clipboard from before the session stays here");

        let formatted = vec![(Format::Html, b"<i>copied</i>".to_vec()), text("copied")];
        copy(formatted.clone());
        let msgs = inline(t.outgoing());
        assert_eq!(msgs.len(), 1);
        assert!(matches!(t.outgoing(), Outgoing::Nothing), "crossing again without copying sends nothing");
        assert_eq!(deliver(&mut t, msgs), formatted);
        assert!(matches!(t.outgoing(), Outgoing::Nothing), "received contents are never echoed back");

        copy(vec![]); // something unshared, a file say
        assert!(matches!(t.outgoing(), Outgoing::Nothing));

        // Up to MAX_INLINE goes inline, over several messages when it does not compress.
        let mut x = 0x2545_f491_u32; // xorshift: does not compress
        let noise: Vec<u8> = (0..MAX_CHUNK * 2)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        let several = vec![(Format::Rtf, noise), text("several")];
        copy(several.clone());
        let msgs = inline(t.outgoing());
        assert!(msgs.len() >= 3, "{}", msgs.len());
        assert_eq!(deliver(&mut t, msgs), several);
    }

    #[test]
    fn big_clipboards_are_offered_and_stale_transfers_dropped() {
        // Its own fake: the test above runs in parallel.
        static SEQ: AtomicU32 = AtomicU32::new(0);
        static BOARD: Mutex<Contents> = Mutex::new(Vec::new());
        fn copy(c: Contents) {
            *BOARD.lock().unwrap() = c;
            SEQ.fetch_add(1, Ordering::SeqCst);
        }
        let fake = Clipboard {
            seq: || SEQ.load(Ordering::SeqCst),
            get: || BOARD.lock().unwrap().clone(),
            set: |c| copy(c.clone()),
            received: || std::env::temp_dir().join(format!("input-share-test-big-{}", std::process::id())),
        };
        let image = vec![(Format::Image, vec![7; MAX_INLINE + 1])];

        let mut t = Tracker::new(fake);
        copy(image.clone());
        let Outgoing::Big(blob) = t.outgoing() else { panic!("not offered") };
        assert!(t.big.is_some(), "kept for the other computer to pull");
        let z = compress(&blob);

        // As the receiving side: the transfer lands and is pasted.
        copy(vec![]);
        let g = t.offered();
        t.finish(g, &z);
        assert_eq!(*BOARD.lock().unwrap(), image);

        // Something newer arrived first: the late transfer is dropped.
        copy(vec![]);
        let g = t.offered();
        t.incoming(true, compress(&encode(&vec![text("newer")])));
        t.finish(g, &z);
        assert_eq!(*BOARD.lock().unwrap(), vec![text("newer")]);

        // A copy here while it travelled wins too.
        let g = t.offered();
        copy(vec![text("copied here")]);
        t.finish(g, &z);
        assert_eq!(*BOARD.lock().unwrap(), vec![text("copied here")]);

        // Too big even for a transfer: only the text goes.
        copy(vec![(Format::Image, vec![0; MAX_SIZE]), text("plain")]);
        let msgs = inline(t.outgoing());
        copy(vec![]);
        for m in msgs {
            let Msg::Clipboard { last, data } = m else { panic!() };
            t.incoming(last, data);
        }
        assert_eq!(*BOARD.lock().unwrap(), vec![text("plain")]);
    }
}
