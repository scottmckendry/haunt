//! Configuration: shell command, auxiliary apps, and their direct chords.
//!
//! Loaded from `$XDG_CONFIG_HOME/haunt/config.toml` (usually
//! `~/.config/haunt/config.toml`). A missing file yields built-in defaults so
//! haunt works out of the box.

use std::{env, ffi::OsString, fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyEventState, KeyModifiers};
use serde::Deserialize;

use crate::shell;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppSize {
    #[default]
    Half,
    Full,
}

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
        if ev.kind == crossterm::event::KeyEventKind::Release {
            return false;
        }

        let mut ev_mods = ev.modifiers;
        let key_matches = match (self.key, ev.code) {
            (KeyCode::Char(a), KeyCode::Char(b)) => {
                if a.is_ascii_alphabetic() && b.is_ascii_alphabetic() && a.eq_ignore_ascii_case(&b)
                {
                    let wants_shift = self.mods.contains(KeyModifiers::SHIFT);
                    let has_shift = ev_mods.contains(KeyModifiers::SHIFT);
                    if wants_shift
                        && !has_shift
                        && b.is_ascii_uppercase()
                        && !ev.state.contains(KeyEventState::CAPS_LOCK)
                    {
                        // Shift was folded into the letter's case.
                        ev_mods.insert(KeyModifiers::SHIFT);
                    } else if !wants_shift && has_shift && b.is_ascii_uppercase() {
                        // Shift only changed the letter's case.
                        ev_mods.remove(KeyModifiers::SHIFT);
                    }
                    true
                } else {
                    a == b
                }
            }
            (a, b) => a == b,
        };

        key_matches && ev_mods == self.mods
    }
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub name: String,
    pub chord: Chord,
    pub cmd: Vec<String>,
    pub size: AppSize,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub shell: Vec<String>,
    pub separator: char,
    pub apps: Vec<AppConfig>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let Some(path) = config_path() else {
            return Ok(Self::builtin());
        };
        match fs::read_to_string(&path) {
            Ok(text) => {
                Self::parse(&text).with_context(|| format!("invalid config: {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::builtin()),
            Err(e) => Err(e).with_context(|| format!("could not read config: {}", path.display())),
        }
    }

    fn parse(text: &str) -> Result<Self> {
        let raw: RawConfig = toml::from_str(text)?;
        Self::from_raw(raw)
    }

    fn from_raw(raw: RawConfig) -> Result<Self> {
        let shell = raw.shell.unwrap_or_else(shell::default_shell);
        if shell.is_empty() || shell[0].trim().is_empty() {
            bail!("`shell` must not be empty");
        }

        let separator = raw.separator.unwrap_or_else(|| " ".into());
        let mut chars = separator.chars();
        let separator = match (chars.next(), chars.next()) {
            (Some(c), None)
                if !c.is_control() && libghostty_vt::unicode::grapheme_width(&[c]).1 == 1 =>
            {
                c
            }
            _ => bail!("`separator` must be a single printable, one-cell character"),
        };

        let mut apps = Vec::with_capacity(raw.app.len());
        let mut seen_binds: Vec<(Chord, String)> = Vec::new();
        let mut seen_names: Vec<String> = Vec::new();

        for raw_app in raw.app {
            if raw_app.name.trim().is_empty() {
                bail!("app with empty name");
            }
            if seen_names.contains(&raw_app.name) {
                bail!("duplicate app name {:?}", raw_app.name);
            }
            if raw_app.cmd.is_empty() || raw_app.cmd[0].trim().is_empty() {
                bail!("app {:?} has an empty `cmd`", raw_app.name);
            }
            seen_names.push(raw_app.name.clone());

            let chord = Chord::parse(&raw_app.bind)
                .with_context(|| format!("app {:?} has an invalid bind", raw_app.name))?;

            if let Some((_, other)) = seen_binds.iter().find(|(c, _)| *c == chord) {
                bail!(
                    "apps {:?} and {:?} share the same bind {:?}",
                    raw_app.name,
                    other,
                    raw_app.bind
                );
            }
            seen_binds.push((chord, raw_app.name.clone()));

            apps.push(AppConfig {
                name: raw_app.name,
                chord,
                cmd: raw_app.cmd,
                size: raw_app.size,
            });
        }

        Ok(Self {
            shell,
            separator,
            apps,
        })
    }

    fn builtin() -> Self {
        let shell = shell::default_shell();
        let apps = vec![
            builtin_app("pi", "ctrl+alt+p", vec!["pi".into()], AppSize::Half),
            builtin_app(
                "lazygit",
                "ctrl+alt+g",
                vec!["lazygit".into()],
                AppSize::Full,
            ),
            builtin_app("k9s", "ctrl+alt+k", vec!["k9s".into()], AppSize::Full),
            builtin_app("term", "ctrl+alt+space", shell.clone(), AppSize::Half),
        ];

        Self {
            shell,
            separator: ' ',
            apps,
        }
    }
}

fn builtin_app(name: &str, bind: &str, cmd: Vec<String>, size: AppSize) -> AppConfig {
    AppConfig {
        name: name.to_string(),
        chord: Chord::parse(bind).expect("built-in bind is valid"),
        cmd,
        size,
    }
}

fn config_path() -> Option<PathBuf> {
    config_path_from(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME"))
}

fn config_path_from(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    // The XDG spec requires relative paths to be ignored. An empty variable
    // would otherwise resolve the config against the current directory.
    let xdg = xdg.map(PathBuf::from).filter(|path| path.is_absolute());
    let base = match xdg {
        Some(path) => path,
        None => {
            let home = home.map(PathBuf::from).filter(|path| path.is_absolute())?;
            home.join(".config")
        }
    };
    Some(base.join("haunt/config.toml"))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    shell: Option<Vec<String>>,
    #[serde(default)]
    separator: Option<String>,
    #[serde(default)]
    app: Vec<RawApp>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawApp {
    name: String,
    bind: String,
    cmd: Vec<String>,
    #[serde(default)]
    size: AppSize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new_with_kind(code, mods, KeyEventKind::Press)
    }

    #[test]
    fn parses_modifiers_and_keys() {
        let chord = Chord::parse("ctrl+alt+p").unwrap();
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

    #[test]
    fn builtin_binds_are_unique() {
        let cfg = Config::builtin();
        let mut seen = Vec::new();
        for app in &cfg.apps {
            assert!(
                !seen.contains(&app.chord),
                "duplicate bind for {}",
                app.name
            );
            seen.push(app.chord);
        }
    }

    #[test]
    fn parses_a_config() {
        let cfg = Config::parse(
            r#"
            shell = ["nu", "-l"]

            [[app]]
            name = "pi"
            bind = "ctrl+alt+p"
            cmd = ["pi"]
            size = "half"

            [[app]]
            name = "lazygit"
            bind = "ctrl+alt+g"
            cmd = ["lazygit"]
            size = "full"
            "#,
        )
        .unwrap();

        assert_eq!(cfg.shell, ["nu", "-l"]);
        assert_eq!(cfg.apps.len(), 2);
        assert_eq!(cfg.apps[1].size, AppSize::Full);
    }

    #[test]
    fn separator_defaults_to_space_and_accepts_one_cell() {
        assert_eq!(Config::builtin().separator, ' ');
        assert_eq!(Config::parse("").unwrap().separator, ' ');
        assert_eq!(Config::parse("separator = '│'").unwrap().separator, '│');
        for value in ["", "ab", "界", "\u{301}", "\u{1b}"] {
            let input = format!("separator = {}", toml::Value::String(value.into()));
            assert!(Config::parse(&input).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn rejects_duplicate_binds() {
        let err = Config::parse(
            r#"
            [[app]]
            name = "a"
            bind = "ctrl+alt+p"
            cmd = ["a"]

            [[app]]
            name = "b"
            bind = "ctrl+alt+p"
            cmd = ["b"]
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("share the same bind"));
    }

    #[test]
    fn rejects_unknown_fields_and_duplicate_names() {
        assert!(Config::parse("separatorr = '|'").is_err());
        assert!(
            Config::parse("[[app]]\nname = 'a'\nbind = 'ctrl+p'\ncmd = ['a']\nextra = 1").is_err()
        );
        let err = Config::parse(
            "[[app]]\nname = 'a'\nbind = 'ctrl+p'\ncmd = ['a']\n\
             [[app]]\nname = 'a'\nbind = 'ctrl+q'\ncmd = ['b']",
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate app name"));
    }

    #[test]
    fn rejects_whitespace_programs() {
        assert!(Config::parse("shell = ['  ']").is_err());
        assert!(Config::parse("[[app]]\nname = 'a'\nbind = 'ctrl+p'\ncmd = ['  ']").is_err());
    }

    #[test]
    fn config_path_ignores_empty_and_relative_xdg() {
        assert_eq!(
            config_path_from(Some("/xdg".into()), Some("/home/me".into())),
            Some(PathBuf::from("/xdg/haunt/config.toml"))
        );
        assert_eq!(
            config_path_from(Some("".into()), Some("/home/me".into())),
            Some(PathBuf::from("/home/me/.config/haunt/config.toml"))
        );
        assert_eq!(
            config_path_from(Some("relative".into()), Some("/home/me".into())),
            Some(PathBuf::from("/home/me/.config/haunt/config.toml"))
        );
        assert_eq!(config_path_from(None, None), None);
        assert_eq!(config_path_from(None, Some("".into())), None);
    }

    #[test]
    fn rejects_empty_commands() {
        let err = Config::parse(
            r#"
            [[app]]
            name = "empty"
            bind = "ctrl+alt+p"
            cmd = []
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("empty `cmd`"));
    }
}
