// Wire messages: 1 tag byte + fixed little-endian fields (Clipboard: a flag, then bytes).

pub const VERSION: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Button {
    Left = 0,
    Right = 1,
    Middle = 2,
    X1 = 3,
    X2 = 4,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Hello { version: u16, w: i32, h: i32 },
    Enter { y_frac: f32 },
    Leave { y_frac: f32 },
    MouseMove { dx: i32, dy: i32 },
    Button { button: Button, down: bool },
    Wheel { vertical: bool, delta: i32 },
    Key { scancode: u16, extended: bool, down: bool },
    Heartbeat,
    /// Part of a clipboard (see `clipboard`), sent just before control
    /// crosses to the other computer. `last` marks the final part.
    Clipboard { last: bool, data: Vec<u8> },
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    Empty,
    UnknownTag(u8),
    /// Wrong length for the tag: truncated or trailing bytes.
    BadLength { tag: u8, got: usize },
    BadValue,
}

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(13);
        match *self {
            Msg::Hello { version, w, h } => {
                b.push(1);
                b.extend(version.to_le_bytes());
                b.extend(w.to_le_bytes());
                b.extend(h.to_le_bytes());
            }
            Msg::Enter { y_frac } => {
                b.push(2);
                b.extend(y_frac.to_le_bytes());
            }
            Msg::Leave { y_frac } => {
                b.push(3);
                b.extend(y_frac.to_le_bytes());
            }
            Msg::MouseMove { dx, dy } => {
                b.push(4);
                b.extend(dx.to_le_bytes());
                b.extend(dy.to_le_bytes());
            }
            Msg::Button { button, down } => b.extend([5, button as u8, down as u8]),
            Msg::Wheel { vertical, delta } => {
                b.extend([6, vertical as u8]);
                b.extend(delta.to_le_bytes());
            }
            Msg::Key { scancode, extended, down } => {
                b.push(7);
                b.extend(scancode.to_le_bytes());
                b.extend([extended as u8, down as u8]);
            }
            Msg::Heartbeat => b.push(8),
            Msg::Clipboard { last, ref data } => {
                b.extend([9, last as u8]);
                b.extend(data);
            }
        }
        b
    }

    pub fn decode(buf: &[u8]) -> Result<Msg, DecodeError> {
        let (&tag, p) = buf.split_first().ok_or(DecodeError::Empty)?;
        let want = match tag {
            1 => 10,
            2 | 3 => 4,
            4 => 8,
            5 => 2,
            6 => 5,
            7 => 4,
            8 => 0,
            9 => p.len().max(1), // any length, but the flag must be there
            _ => return Err(DecodeError::UnknownTag(tag)),
        };
        if p.len() != want {
            return Err(DecodeError::BadLength { tag, got: p.len() });
        }
        let u16_at = |i: usize| u16::from_le_bytes([p[i], p[i + 1]]);
        let i32_at = |i: usize| i32::from_le_bytes(p[i..i + 4].try_into().unwrap());
        let f32_at = |i: usize| f32::from_le_bytes(p[i..i + 4].try_into().unwrap());
        let bool_at = |i: usize| match p[i] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DecodeError::BadValue),
        };
        Ok(match tag {
            1 => Msg::Hello { version: u16_at(0), w: i32_at(2), h: i32_at(6) },
            2 => Msg::Enter { y_frac: f32_at(0) },
            3 => Msg::Leave { y_frac: f32_at(0) },
            4 => Msg::MouseMove { dx: i32_at(0), dy: i32_at(4) },
            5 => {
                let button = match p[0] {
                    0 => Button::Left,
                    1 => Button::Right,
                    2 => Button::Middle,
                    3 => Button::X1,
                    4 => Button::X2,
                    _ => return Err(DecodeError::BadValue),
                };
                Msg::Button { button, down: bool_at(1)? }
            }
            6 => Msg::Wheel { vertical: bool_at(0)?, delta: i32_at(1) },
            7 => Msg::Key { scancode: u16_at(0), extended: bool_at(2)?, down: bool_at(3)? },
            9 => Msg::Clipboard { last: bool_at(0)?, data: p[1..].to_vec() },
            _ => Msg::Heartbeat,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Vec<Msg> {
        vec![
            Msg::Hello { version: VERSION, w: 1920, h: -1080 },
            Msg::Enter { y_frac: 0.25 },
            Msg::Leave { y_frac: 1.0 },
            Msg::MouseMove { dx: -5, dy: i32::MAX },
            Msg::Button { button: Button::X2, down: true },
            Msg::Wheel { vertical: false, delta: -120 },
            Msg::Key { scancode: 0x1D, extended: true, down: false },
            Msg::Heartbeat,
            Msg::Clipboard { last: true, data: vec![0, 255, 7] },
        ]
    }

    #[test]
    fn round_trip_every_message() {
        for m in all() {
            assert_eq!(Msg::decode(&m.encode()), Ok(m.clone()), "{m:?}");
        }
        for b in [Button::Left, Button::Right, Button::Middle, Button::X1, Button::X2] {
            let m = Msg::Button { button: b, down: false };
            assert_eq!(Msg::decode(&m.encode()), Ok(m));
        }
    }

    #[test]
    fn rejects_truncated_and_trailing() {
        // Clipboard is any length, so every cut of it past the flag is a valid (shorter) message.
        for m in all().into_iter().filter(|m| !matches!(m, Msg::Clipboard { .. })) {
            let e = m.encode();
            for n in 1..e.len() {
                assert!(matches!(Msg::decode(&e[..n]), Err(DecodeError::BadLength { .. })), "{m:?} cut at {n}");
            }
            let mut long = e.clone();
            long.push(0);
            assert!(matches!(Msg::decode(&long), Err(DecodeError::BadLength { .. })));
        }
        assert_eq!(Msg::decode(&[]), Err(DecodeError::Empty));
    }

    #[test]
    fn rejects_unknown_tags_and_bad_values() {
        for t in [0u8, 10, 255] {
            assert_eq!(Msg::decode(&[t]), Err(DecodeError::UnknownTag(t)));
        }
        assert_eq!(Msg::decode(&[5, 5, 0]), Err(DecodeError::BadValue));
        assert_eq!(Msg::decode(&[5, 0, 2]), Err(DecodeError::BadValue));
        assert_eq!(Msg::decode(&[7, 0, 0, 1, 7]), Err(DecodeError::BadValue));
        assert_eq!(Msg::decode(&[9]), Err(DecodeError::BadLength { tag: 9, got: 0 }), "no flag");
        assert_eq!(Msg::decode(&[9, 2, 0]), Err(DecodeError::BadValue));
    }
}
