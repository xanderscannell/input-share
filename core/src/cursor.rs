// Hide the parked desktop cursor while control is on the other computer.
//
// Windows has no per-process "hide the cursor everywhere", so this swaps every
// system cursor for a blank one and restores the user's scheme afterwards.
// The risk is a cursor left invisible, so restore happens on every way back:
// Local, disconnect, panic hotkey (which goes Local), stop, exit, and at
// startup in case an earlier run crashed while hidden. Real mode only: demo
// and script mode never touch system cursors.

use crate::status::Status;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateCursor, OCR_APPSTARTING, OCR_CROSS, OCR_HAND, OCR_IBEAM, OCR_NO, OCR_NORMAL, OCR_SIZEALL, OCR_SIZENESW,
    OCR_SIZENS, OCR_SIZENWSE, OCR_SIZEWE, OCR_UP, OCR_WAIT, SPI_SETCURSORS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SetSystemCursor, SystemParametersInfoW,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Hide,
    Restore,
}

/// Decides when to hide and restore. Pure, so the rules are unit-tested.
#[derive(Debug, Default)]
pub struct CursorGate {
    hidden: bool,
}

impl CursorGate {
    /// The server's status stream drives it: hide on Remote, restore on
    /// anything that means control is back here or the session is over.
    pub fn on_status(&mut self, s: &Status) -> Option<Action> {
        let want_hidden = match s {
            Status::Remote => true,
            Status::Local | Status::Disconnected(_) | Status::Stopped => false,
            _ => return None,
        };
        self.set(want_hidden)
    }

    /// The status stream ended (stop, drop, exit): restore if still hidden.
    pub fn finish(&mut self) -> Option<Action> {
        self.set(false)
    }

    fn set(&mut self, hidden: bool) -> Option<Action> {
        if self.hidden == hidden {
            return None;
        }
        self.hidden = hidden;
        Some(if hidden { Action::Hide } else { Action::Restore })
    }
}

const CURSORS: [windows::Win32::UI::WindowsAndMessaging::SYSTEM_CURSOR_ID; 13] = [
    OCR_NORMAL,
    OCR_IBEAM,
    OCR_WAIT,
    OCR_CROSS,
    OCR_UP,
    OCR_SIZENWSE,
    OCR_SIZENESW,
    OCR_SIZEWE,
    OCR_SIZENS,
    OCR_SIZEALL,
    OCR_NO,
    OCR_HAND,
    OCR_APPSTARTING,
];

/// Put the user's cursor scheme back (reloaded from their settings). Safe to
/// call any time; it is also the fix for a cursor left invisible by a crash.
pub fn restore() {
    let _ = unsafe { SystemParametersInfoW(SPI_SETCURSORS, 0, None, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
}

fn hide() {
    // 32x32 monochrome: AND plane all ones (keep the screen), XOR all zeros
    // (change nothing), so the cursor is fully transparent.
    let and_plane = [0xFFu8; 32 * 32 / 8];
    let xor_plane = [0u8; 32 * 32 / 8];
    for id in CURSORS {
        // SetSystemCursor takes ownership and destroys the cursor, so each
        // slot needs a fresh one.
        let blank = unsafe { CreateCursor(None, 0, 0, 32, 32, and_plane.as_ptr().cast(), xor_plane.as_ptr().cast()) };
        match blank {
            Ok(c) => {
                if unsafe { SetSystemCursor(c, id) }.is_err() {
                    // Never leave a half-hidden set: put everything back.
                    restore();
                    return;
                }
            }
            Err(_) => {
                restore();
                return;
            }
        }
    }
}

pub fn apply(a: Action) {
    match a {
        Action::Hide => hide(),
        Action::Restore => restore(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(gate: &mut CursorGate, events: &[Status]) -> Vec<Action> {
        events.iter().filter_map(|s| gate.on_status(s)).collect()
    }

    #[test]
    fn hides_on_remote_and_restores_when_control_returns() {
        let mut g = CursorGate::default();
        let acts = run(
            &mut g,
            &[Status::Listening("0.0.0.0:24800".into()), Status::Connected("10.0.0.2:5000".into()), Status::Remote, Status::Local],
        );
        assert_eq!(acts, [Action::Hide, Action::Restore]);
    }

    #[test]
    fn every_way_back_restores() {
        for back in [Status::Local, Status::Disconnected("peer closed".into()), Status::Stopped] {
            let mut g = CursorGate::default();
            assert_eq!(run(&mut g, &[Status::Remote, back.clone()]), [Action::Hide, Action::Restore], "{back:?}");
        }
        // The stream ending while hidden (stop, drop, exit) restores too.
        let mut g = CursorGate::default();
        g.on_status(&Status::Remote);
        assert_eq!(g.finish(), Some(Action::Restore));
        assert_eq!(g.finish(), None);
    }

    #[test]
    fn no_repeats_and_nothing_when_never_remote() {
        let mut g = CursorGate::default();
        assert_eq!(run(&mut g, &[Status::Remote, Status::Remote]), [Action::Hide]);
        let mut g = CursorGate::default();
        assert_eq!(run(&mut g, &[Status::Local, Status::Disconnected("x".into()), Status::Stopped]), []);
        assert_eq!(g.finish(), None, "never hidden, so nothing to restore");
    }

    #[test]
    fn panic_hotkey_path_restores() {
        // The panic hotkey disconnects, which reports Local first.
        let mut g = CursorGate::default();
        assert_eq!(
            run(&mut g, &[Status::Remote, Status::Local, Status::Disconnected("closed".into())]),
            [Action::Hide, Action::Restore]
        );
    }
}
