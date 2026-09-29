// Held key and button tracker. The client uses it to release everything on
// Leave/disconnect/exit; the server uses it to pass through key-ups for keys
// held locally when control crossed to Remote. See DESIGN.md "Safety rails".

use crate::proto::{Button, Msg};
use std::collections::BTreeSet;

#[derive(Default)]
pub struct Held {
    keys: BTreeSet<(u16, bool)>,
    buttons: BTreeSet<Button>,
}

impl Held {
    /// Record a key event. Returns true if it changed state: a new press, or
    /// the release of a key that was held. Auto-repeat downs return false.
    pub fn key(&mut self, scancode: u16, extended: bool, down: bool) -> bool {
        if down {
            self.keys.insert((scancode, extended))
        } else {
            self.keys.remove(&(scancode, extended))
        }
    }

    /// Same contract as `key`, for mouse buttons.
    pub fn button(&mut self, button: Button, down: bool) -> bool {
        if down {
            self.buttons.insert(button)
        } else {
            self.buttons.remove(&button)
        }
    }

    /// Track any input message; non-input messages are ignored.
    pub fn track(&mut self, msg: &Msg) {
        match *msg {
            Msg::Key { scancode, extended, down } => {
                self.key(scancode, extended, down);
            }
            Msg::Button { button, down } => {
                self.button(button, down);
            }
            _ => {}
        }
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.buttons.is_empty()
    }

    /// Release messages for everything still down, then forget it all.
    pub fn release_all(&mut self) -> Vec<Msg> {
        let keys = std::mem::take(&mut self.keys)
            .into_iter()
            .map(|(scancode, extended)| Msg::Key { scancode, extended, down: false });
        let buttons = std::mem::take(&mut self.buttons)
            .into_iter()
            .map(|button| Msg::Button { button, down: false });
        keys.chain(buttons).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(scancode: u16, extended: bool) -> Msg {
        Msg::Key { scancode, extended, down: false }
    }

    #[test]
    fn release_all_returns_exactly_what_is_down_in_any_order() {
        // Ctrl, right Ctrl (same scancode, extended), A, Shift.
        let presses = [(0x1D, false), (0x1D, true), (0x1E, false), (0x2A, false)];
        let orders = [[0, 1, 2, 3], [3, 2, 1, 0], [2, 0, 3, 1], [1, 3, 0, 2]];
        for order in orders {
            let mut h = Held::default();
            for &i in &order {
                let (s, e) = presses[i];
                assert!(h.key(s, e, true));
            }
            assert!(h.key(0x1E, false, false)); // release A
            h.button(Button::Right, true);
            h.button(Button::Left, true);
            h.button(Button::Left, false);
            let mut got = h.release_all();
            got.sort_by_key(|m| format!("{m:?}"));
            let mut want = vec![
                up(0x1D, false),
                up(0x1D, true),
                up(0x2A, false),
                Msg::Button { button: Button::Right, down: false },
            ];
            want.sort_by_key(|m| format!("{m:?}"));
            assert_eq!(got, want, "order {order:?}");
            assert!(h.is_empty());
            assert!(h.release_all().is_empty());
        }
    }

    #[test]
    fn repeats_and_stray_ups() {
        let mut h = Held::default();
        assert!(h.key(0x1E, false, true));
        assert!(!h.key(0x1E, false, true)); // auto-repeat
        assert_eq!(h.release_all(), vec![up(0x1E, false)]);
        assert!(!h.key(0x1E, false, false)); // up for a key never seen down
        assert!(!h.button(Button::X1, false));
        assert!(h.is_empty());
    }

    #[test]
    fn track_follows_messages() {
        let mut h = Held::default();
        h.track(&Msg::Key { scancode: 0x38, extended: true, down: true });
        h.track(&Msg::Button { button: Button::Middle, down: true });
        h.track(&Msg::Heartbeat);
        h.track(&Msg::Button { button: Button::Middle, down: false });
        assert_eq!(h.release_all(), vec![up(0x38, true)]);
    }
}
