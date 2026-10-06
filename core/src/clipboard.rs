// Text clipboard sharing. When control leaves a computer, it sends its
// clipboard text along, but only if the clipboard changed since the session
// started or since it last sent or received text. So crossing back never
// replaces a richer clipboard (an image, formatted text) with stale text.

use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};

/// Lives in the Win32_System_Ole feature, too big to enable for one constant.
const CF_UNICODETEXT: u32 = 13;

/// The most UTF-8 that fits one message: a Noise message is at most 65535
/// bytes, 16 of them the tag, and the message's own tag byte.
// ponytail: longer text is not shared; split it over several messages if that matters.
pub const MAX_TEXT: usize = 65535 - 16 - 1;

/// Where clipboard text comes from and goes to: Windows' clipboard, or a fake in tests.
#[derive(Clone, Copy)]
pub struct Clipboard {
    /// Changes whenever the clipboard's contents change.
    pub seq: fn() -> u32,
    /// The clipboard's text, if it holds any.
    pub get: fn() -> Option<String>,
    pub set: fn(&str),
}

pub const WINDOWS: Clipboard = Clipboard { seq: win_seq, get: win_get, set: win_set };

/// One computer's side of the clipboard for one session.
pub struct Tracker {
    cb: Clipboard,
    seen: u32,
}

impl Tracker {
    pub fn new(cb: Clipboard) -> Self {
        Tracker { cb, seen: (cb.seq)() }
    }

    /// Text to send as control leaves this computer: only if the clipboard
    /// changed since the last `outgoing` or `incoming`, holds text, and fits.
    pub fn outgoing(&mut self) -> Option<String> {
        let seq = (self.cb.seq)();
        if seq == self.seen {
            return None;
        }
        self.seen = seq;
        let text = (self.cb.get)()?;
        if text.len() > MAX_TEXT {
            eprintln!("clipboard text is {} bytes, over the {MAX_TEXT} limit: not shared", text.len());
            return None;
        }
        Some(text)
    }

    /// Text the other computer sent.
    pub fn incoming(&mut self, text: &str) {
        (self.cb.set)(text);
        self.seen = (self.cb.seq)();
    }
}

fn win_seq() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// Another program may hold the clipboard open for a moment, so retry briefly.
fn open() -> bool {
    for _ in 0..10 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    eprintln!("clipboard busy: text not shared");
    false
}

fn win_get() -> Option<String> {
    if !open() {
        return None;
    }
    let text = unsafe {
        GetClipboardData(CF_UNICODETEXT).ok().and_then(|h| {
            let h = HGLOBAL(h.0);
            let p = GlobalLock(h) as *const u16;
            if p.is_null() {
                return None;
            }
            // NUL-terminated, but never read past the allocation.
            let all = std::slice::from_raw_parts(p, GlobalSize(h) / 2);
            let len = all.iter().position(|&c| c == 0).unwrap_or(all.len());
            let s = String::from_utf16_lossy(&all[..len]);
            let _ = GlobalUnlock(h);
            Some(s)
        })
    };
    let _ = unsafe { CloseClipboard() };
    text
}

fn win_set(text: &str) {
    if !open() {
        return;
    }
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    unsafe {
        let _ = EmptyClipboard();
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
                let _ = GlobalUnlock(h);
            }
            // Once set, the clipboard owns the memory; free it only on failure.
            if p.is_null() || SetClipboardData(CF_UNICODETEXT, Some(HANDLE(h.0))).is_err() {
                let _ = GlobalFree(Some(h));
            }
        }
        let _ = CloseClipboard();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    // A fake clipboard; only this test uses it.
    static SEQ: AtomicU32 = AtomicU32::new(0);
    static TEXT: Mutex<Option<String>> = Mutex::new(None);

    fn copy(text: Option<&str>) {
        *TEXT.lock().unwrap() = text.map(String::from);
        SEQ.fetch_add(1, Ordering::SeqCst);
    }

    const FAKE: Clipboard = Clipboard {
        seq: || SEQ.load(Ordering::SeqCst),
        get: || TEXT.lock().unwrap().clone(),
        set: |t| copy(Some(t)),
    };

    #[test]
    fn sends_only_what_changed_here() {
        copy(Some("from before the session"));
        let mut t = Tracker::new(FAKE);
        assert_eq!(t.outgoing(), None, "a clipboard from before the session stays here");

        copy(Some("copied"));
        assert_eq!(t.outgoing().as_deref(), Some("copied"));
        assert_eq!(t.outgoing(), None, "crossing again without copying sends nothing");

        t.incoming("from the other side");
        assert_eq!(TEXT.lock().unwrap().as_deref(), Some("from the other side"));
        assert_eq!(t.outgoing(), None, "received text is never echoed back");

        copy(None); // an image, say
        assert_eq!(t.outgoing(), None, "no text: nothing to send");

        copy(Some(&"x".repeat(MAX_TEXT + 1)));
        assert_eq!(t.outgoing(), None, "too long to share");
    }
}
