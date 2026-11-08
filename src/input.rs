//! Translate crossterm key events into libghostty-vt key events and encode
//! them for the inner application, respecting its keyboard protocol modes.

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use libghostty_vt::{
    Terminal,
    key::{self, Action, Key, Mods},
};

pub fn encode(
    terminal: &Terminal<'_, '_>,
    ev: KeyEvent,
    encoder: &mut key::Encoder,
    event: &mut key::Event,
    out: &mut Vec<u8>,
) -> Result<()> {
    let action = match ev.kind {
        KeyEventKind::Press => Action::Press,
        KeyEventKind::Repeat => Action::Repeat,
        KeyEventKind::Release => Action::Release,
    };

    let mut mods = mods_from(ev.modifiers);

    let mapped: Option<(Key, Option<String>, char)> = match ev.code {
        KeyCode::Char(c) => {
            let key = key_for_char(c).unwrap_or(Key::Unidentified);
            // A control character has no printable text; let the encoder
            // synthesize it from key + mods.
            let text = if ev.modifiers.contains(KeyModifiers::CONTROL) || c.is_control() {
                None
            } else {
                Some(c.to_string())
            };
            let unshifted = if c.is_ascii_alphabetic() {
                c.to_ascii_lowercase()
            } else {
                c
            };
            // Legacy terminals cannot distinguish Ctrl+Shift+letter from
            // Ctrl+letter and conventionally send the control code. Kitty
            // terminals must keep SHIFT to report the distinction.
            if mods.contains(Mods::CTRL) && c.is_ascii_alphabetic() && !kitty(terminal) {
                mods.remove(Mods::SHIFT);
            }
            Some((key, text, unshifted))
        }
        KeyCode::Backspace => Some((Key::Backspace, None, '\0')),
        KeyCode::Enter => Some((Key::Enter, None, '\0')),
        KeyCode::Tab => Some((Key::Tab, None, '\0')),
        KeyCode::BackTab => {
            mods |= Mods::SHIFT;
            Some((Key::Tab, None, '\0'))
        }
        KeyCode::Esc => Some((Key::Escape, None, '\0')),
        KeyCode::Delete => Some((Key::Delete, None, '\0')),
        KeyCode::Insert => Some((Key::Insert, None, '\0')),
        KeyCode::Home => Some((Key::Home, None, '\0')),
        KeyCode::End => Some((Key::End, None, '\0')),
        KeyCode::PageUp => Some((Key::PageUp, None, '\0')),
        KeyCode::PageDown => Some((Key::PageDown, None, '\0')),
        KeyCode::Up => Some((Key::ArrowUp, None, '\0')),
        KeyCode::Down => Some((Key::ArrowDown, None, '\0')),
        KeyCode::Left => Some((Key::ArrowLeft, None, '\0')),
        KeyCode::Right => Some((Key::ArrowRight, None, '\0')),
        KeyCode::F(n) => f_key(n).map(|key| (key, None, '\0')),
        KeyCode::Null => Some((Key::Space, None, '\0')),
        _ => None,
    };

    let Some((key, text, unshifted)) = mapped else {
        // Printable Unicode still needs to reach the application.
        if let KeyCode::Char(c) = ev.code
            && action != Action::Release
        {
            out.extend_from_slice(c.to_string().as_bytes());
        }
        return Ok(());
    };

    let mut consumed = Mods::empty();
    if text.is_some() && mods.contains(Mods::SHIFT) {
        // The platform already folded shift into the text (a -> A).
        consumed |= Mods::SHIFT;
    }

    event
        .set_action(action)
        .set_key(key)
        .set_mods(mods)
        .set_consumed_mods(consumed)
        .set_unshifted_codepoint(unshifted)
        .set_utf8(text.clone());

    encoder
        .set_options_from_terminal(terminal)
        .encode_to_vec(event, out)?;

    // If the encoder produced nothing for printable text, do not drop it.
    if out.is_empty()
        && let Some(text) = text
        && action != Action::Release
    {
        out.extend_from_slice(text.as_bytes());
    }

    Ok(())
}

fn kitty(terminal: &Terminal<'_, '_>) -> bool {
    terminal
        .kitty_keyboard_flags()
        .is_ok_and(|flags| flags.contains(key::KittyKeyFlags::DISAMBIGUATE))
}

pub(crate) fn mods_from(m: KeyModifiers) -> Mods {
    let mut mods = Mods::empty();
    if m.contains(KeyModifiers::SHIFT) {
        mods |= Mods::SHIFT;
    }
    if m.contains(KeyModifiers::CONTROL) {
        mods |= Mods::CTRL;
    }
    if m.contains(KeyModifiers::ALT) {
        mods |= Mods::ALT;
    }
    if m.contains(KeyModifiers::SUPER) {
        mods |= Mods::SUPER;
    }
    mods
}

fn f_key(n: u8) -> Option<Key> {
    Some(match n {
        1 => Key::F1,
        2 => Key::F2,
        3 => Key::F3,
        4 => Key::F4,
        5 => Key::F5,
        6 => Key::F6,
        7 => Key::F7,
        8 => Key::F8,
        9 => Key::F9,
        10 => Key::F10,
        11 => Key::F11,
        12 => Key::F12,
        13 => Key::F13,
        14 => Key::F14,
        15 => Key::F15,
        16 => Key::F16,
        17 => Key::F17,
        18 => Key::F18,
        19 => Key::F19,
        20 => Key::F20,
        21 => Key::F21,
        22 => Key::F22,
        23 => Key::F23,
        24 => Key::F24,
        _ => return None,
    })
}

fn key_for_char(c: char) -> Option<Key> {
    Some(match c {
        'a'..='z' => letter(c as u8 - b'a'),
        'A'..='Z' => letter(c as u8 - b'A'),
        '0'..='9' => digit(c as u8 - b'0'),
        ' ' => Key::Space,
        '`' => Key::Backquote,
        '\\' => Key::Backslash,
        '[' => Key::BracketLeft,
        ']' => Key::BracketRight,
        ',' => Key::Comma,
        '=' => Key::Equal,
        '-' => Key::Minus,
        '.' => Key::Period,
        '\'' => Key::Quote,
        ';' => Key::Semicolon,
        '/' => Key::Slash,
        _ => return None,
    })
}

fn letter(n: u8) -> Key {
    const LETTERS: [Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    LETTERS[n as usize]
}

fn digit(n: u8) -> Key {
    const DIGITS: [Key; 10] = [
        Key::Digit0,
        Key::Digit1,
        Key::Digit2,
        Key::Digit3,
        Key::Digit4,
        Key::Digit5,
        Key::Digit6,
        Key::Digit7,
        Key::Digit8,
        Key::Digit9,
    ];
    DIGITS[n as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::key::Encoder;

    fn term_with(seq: &[u8]) -> Terminal<'static, 'static> {
        let mut term = Terminal::new(libghostty_vt::TerminalOptions {
            cols: 80,
            rows: 24,
            max_scrollback: 1000,
        })
        .unwrap();
        term.vt_write(seq);
        term
    }

    fn encoded(seq: &[u8], ev: KeyEvent) -> Vec<u8> {
        let term = term_with(seq);
        let mut encoder = Encoder::new().unwrap();
        let mut event = libghostty_vt::key::Event::new().unwrap();
        let mut out = Vec::new();
        encode(&term, ev, &mut encoder, &mut event, &mut out).unwrap();
        out
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn with_kind(mut ev: KeyEvent, kind: KeyEventKind) -> KeyEvent {
        ev.kind = kind;
        ev
    }

    const KITTY_EVENTS: &[u8] = b"\x1b[>3u";
    const KITTY_ALL: &[u8] = b"\x1b[>11u";

    #[test]
    fn legacy_ctrl_shift_letter_folds_to_control_code() {
        let ev = key(
            KeyCode::Char('A'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(encoded(b"", ev), b"\x01");
        assert_eq!(encoded(b"\x1b[>1u", ev), b"\x1b[97;6u");
    }

    #[test]
    fn kitty_reports_event_types() {
        let ctrl_a = key(KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!(encoded(KITTY_EVENTS, ctrl_a), b"\x1b[97;5u");
        assert_eq!(
            encoded(KITTY_EVENTS, with_kind(ctrl_a, KeyEventKind::Release)),
            b"\x1b[97;5:3u"
        );
        assert_eq!(
            encoded(KITTY_EVENTS, with_kind(ctrl_a, KeyEventKind::Repeat)),
            b"\x1b[97;5:2u"
        );

        let plain_a = key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(
            encoded(KITTY_EVENTS, with_kind(plain_a, KeyEventKind::Release)),
            b"\x1b[97;1:3u"
        );
    }

    #[test]
    fn legacy_ignores_release_events() {
        let ctrl_a = key(KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!(encoded(b"", with_kind(ctrl_a, KeyEventKind::Release)), b"");
        let plain_a = key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(encoded(b"", with_kind(plain_a, KeyEventKind::Release)), b"");
    }

    #[test]
    fn alternate_and_modifier_encodings() {
        let alt_a = key(KeyCode::Char('a'), KeyModifiers::ALT);
        assert_eq!(encoded(b"", alt_a), b"\x1ba");
        assert_eq!(encoded(b"\x1b[>1u", alt_a), b"\x1b[97;3u");

        let shift_a = key(KeyCode::Char('A'), KeyModifiers::SHIFT);
        assert_eq!(encoded(b"", shift_a), b"A");
        assert_eq!(encoded(KITTY_ALL, shift_a), b"\x1b[97;2u");
    }

    #[test]
    fn unmapped_unicode_falls_back_to_utf8() {
        let e_acute = key(KeyCode::Char('é'), KeyModifiers::NONE);
        assert_eq!(encoded(b"", e_acute), "é".as_bytes());
        assert_eq!(encoded(KITTY_EVENTS, e_acute), "é".as_bytes());
    }
}
