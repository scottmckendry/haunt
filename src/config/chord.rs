//! Key chords: parsing, matching, and display.

use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub mods: KeyModifiers,
    pub key: KeyCode,
}

impl Chord {
    pub fn parse(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect();
        let Some((key, mod_parts)) = parts.split_last() else {
            bail!("empty key binding");
        };

        let mut mods = KeyModifiers::NONE;
        for modifier in mod_parts {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" | "option" | "opt" => mods |= KeyModifiers::ALT,
                "shift" => mods |= KeyModifiers::SHIFT,
                "super" | "cmd" | "meta" | "win" => mods |= KeyModifiers::SUPER,
                other => bail!("unknown modifier {other:?} in {s:?}"),
            }
        }

        let lower = key.to_ascii_lowercase();
        let parsed = match lower.as_str() {
            "space" => KeyCode::Char(' '),
            "enter" | "return" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "esc" | "escape" => KeyCode::Esc,
            "backspace" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" | "ins" => KeyCode::Insert,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            other => {
                if let Some(num) = other.strip_prefix('f')
                    && let Ok(n) = num.parse::<u8>()
                    && (1..=24).contains(&n)
                {
                    KeyCode::F(n)
                } else if lower.chars().count() == 1 {
                    KeyCode::Char(lower.chars().next().expect("one char"))
                } else {
                    bail!("unknown key {key:?} in {s:?}");
                }
            }
        };

        Ok(Self { mods, key: parsed })
    }

    /// Whether this chord is the one in `ev`.
    ///
    /// Letters are compared case-insensitively. Terminals may express Shift in
    /// two ways: as the `SHIFT` modifier, or by folding it into the key's case.
    /// Both are normalised against what the chord asks for.
    pub fn matches(&self, ev: &KeyEvent) -> bool {
        ev.kind != KeyEventKind::Release && match_key(self.key, self.mods, ev) == Some(self.mods)
    }
}

/// The event's modifiers with Shift normalised, or `None` if the key differs.
///
/// Terminals may report Shift as the modifier or by uppercasing the letter;
/// this adjusts `ev`'s modifiers to the spelling the chord expects.
fn match_key(key: KeyCode, wanted: KeyModifiers, ev: &KeyEvent) -> Option<KeyModifiers> {
    let (KeyCode::Char(want), KeyCode::Char(got)) = (key, ev.code) else {
        return (key == ev.code).then_some(ev.modifiers);
    };
    if !want.is_ascii_alphabetic() || !got.is_ascii_alphabetic() || !want.eq_ignore_ascii_case(&got)
    {
        return (key == ev.code).then_some(ev.modifiers);
    }

    let mut mods = ev.modifiers;
    let wants_shift = wanted.contains(KeyModifiers::SHIFT);
    let has_shift = mods.contains(KeyModifiers::SHIFT);
    if wants_shift
        && !has_shift
        && got.is_ascii_uppercase()
        && !ev.state.contains(KeyEventState::CAPS_LOCK)
    {
        // Shift was folded into the letter's case.
        mods.insert(KeyModifiers::SHIFT);
    } else if !wants_shift && has_shift && got.is_ascii_uppercase() {
        // Shift only changed the letter's case.
        mods.remove(KeyModifiers::SHIFT);
    }
    Some(mods)
}

impl std::fmt::Display for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (modifier, name) in [
            (KeyModifiers::CONTROL, "ctrl+"),
            (KeyModifiers::ALT, "alt+"),
            (KeyModifiers::SHIFT, "shift+"),
            (KeyModifiers::SUPER, "super+"),
        ] {
            if self.mods.contains(modifier) {
                f.write_str(name)?;
            }
        }
        write_key(f, self.key)
    }
}

fn write_key(f: &mut std::fmt::Formatter<'_>, key: KeyCode) -> std::fmt::Result {
    match key {
        KeyCode::Char(' ') => f.write_str("space"),
        KeyCode::Char(c) => write!(f, "{c}"),
        KeyCode::F(n) => write!(f, "f{n}"),
        KeyCode::Enter => f.write_str("enter"),
        KeyCode::Tab => f.write_str("tab"),
        KeyCode::Esc => f.write_str("esc"),
        KeyCode::Backspace => f.write_str("backspace"),
        KeyCode::Delete => f.write_str("delete"),
        KeyCode::Insert => f.write_str("insert"),
        KeyCode::Home => f.write_str("home"),
        KeyCode::End => f.write_str("end"),
        KeyCode::PageUp => f.write_str("pageup"),
        KeyCode::PageDown => f.write_str("pagedown"),
        KeyCode::Up => f.write_str("up"),
        KeyCode::Down => f.write_str("down"),
        KeyCode::Left => f.write_str("left"),
        KeyCode::Right => f.write_str("right"),
        // Unreachable from `Chord::parse`, but keeps the fallback honest.
        key => write!(f, "{}", format!("{key:?}").to_ascii_lowercase()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new_with_kind(code, mods, KeyEventKind::Press)
    }

    #[test]
    fn parses_modifiers_and_keys() {
        let chord = Chord::parse("ctrl+alt+p").unwrap();
        assert_eq!(chord.mods, KeyModifiers::CONTROL | KeyModifiers::ALT);
        assert_eq!(chord.key, KeyCode::Char('p'));

        // macOS users can spell Alt as Option.
        let chord = Chord::parse("ctrl+option+p").unwrap();
        assert_eq!(chord.mods, KeyModifiers::CONTROL | KeyModifiers::ALT);
        assert_eq!(chord.key, KeyCode::Char('p'));

        let chord = Chord::parse("ctrl+alt+space").unwrap();
        assert_eq!(chord.key, KeyCode::Char(' '));

        let chord = Chord::parse("shift+f5").unwrap();
        assert_eq!(chord.key, KeyCode::F(5));
        assert_eq!(chord.mods, KeyModifiers::SHIFT);
    }

    #[test]
    fn rejects_unknown() {
        assert!(Chord::parse("ctrl+banana").is_err());
        assert!(Chord::parse("hyper+p").is_err());
        assert!(Chord::parse("").is_err());
    }

    #[test]
    fn display_round_trips_parsed_chords() {
        for input in [
            "ctrl+alt+p",
            "shift+f5",
            "ctrl+alt+space",
            "super+enter",
            "ctrl+pageup",
        ] {
            assert_eq!(Chord::parse(input).unwrap().to_string(), input);
        }
    }

    #[test]
    fn matches_case_insensitively() {
        let chord = Chord::parse("ctrl+alt+p").unwrap();
        assert!(chord.matches(&key(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        )));
        // Shift only changed the letter's case.
        assert!(chord.matches(&key(
            KeyCode::Char('P'),
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT
        )));
        assert!(!chord.matches(&key(KeyCode::Char('p'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn matches_folded_shift() {
        // Kitty's alternate-key reports strip SHIFT and uppercase the letter.
        let chord = Chord::parse("ctrl+shift+g").unwrap();
        assert!(chord.matches(&key(KeyCode::Char('G'), KeyModifiers::CONTROL)));
        assert!(chord.matches(&key(
            KeyCode::Char('G'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(chord.matches(&key(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(!chord.matches(&key(KeyCode::Char('g'), KeyModifiers::CONTROL)));
    }
}
