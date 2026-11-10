//! Configuration: shell command, auxiliary apps, and their direct chords.
//!
//! Loaded from `$XDG_CONFIG_HOME/haunt/config.toml` (usually
//! `~/.config/haunt/config.toml`). A missing file is created with a commented example.

use std::{
    env,
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyEventState, KeyModifiers};
use serde::Deserialize;

use crate::shell;

const EXAMPLE_CONFIG: &str = "\
# Configuration: https://github.com/scottmckendry/haunt\n\
# Uncomment and customize this example to add an app.\n\
# [[app]]\n\
# name = \"lazygit\"\n\
# bind = \"ctrl+alt+g\"\n\
# cmd = [\"lazygit\"]\n\
# size = \"full\"\n";

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

impl std::fmt::Display for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (modifier, name) in [
            (KeyModifiers::CONTROL, "ctrl+"),
            (KeyModifiers::ALT, "alt+"),
            (KeyModifiers::SHIFT, "shift+"),
            (KeyModifiers::SUPER, "super+"),
        ] {
            if self.mods.contains(modifier) {
                write!(f, "{name}")?;
            }
        }
        match self.key {
            KeyCode::Char(' ') => write!(f, "space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::F(n) => write!(f, "f{n}"),
            key => write!(f, "{}", format!("{key:?}").to_ascii_lowercase()),
        }
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
    pub reload_bind: Chord,
    pub help_bind: Option<Chord>,
    pub shell: Vec<String>,
    pub separator: char,
    pub apps: Vec<AppConfig>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let Some(path) = config_path() else {
            return Self::parse("");
        };
        Self::load_path(&path)
    }

    /// Reload without creating a missing configuration file.
    pub fn reload() -> Result<Self> {
        let path = config_path().context("cannot determine config path")?;
        let text = fs::read_to_string(&path)
            .with_context(|| format!("could not read config: {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("invalid config: {}", path.display()))
    }

    fn load_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("could not create config directory: {}", parent.display())
                })?;
            }
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(mut file) => file
                    .write_all(EXAMPLE_CONFIG.as_bytes())
                    .with_context(|| format!("could not write config: {}", path.display()))?,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("could not create config: {}", path.display()));
                }
            }
        }
        let text = fs::read_to_string(path)
            .with_context(|| format!("could not read config: {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("invalid config: {}", path.display()))
    }

    pub(crate) fn parse(text: &str) -> Result<Self> {
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

        let reload_bind = Chord::parse(raw.reload_bind.as_deref().unwrap_or("ctrl+shift+r"))
            .context("invalid reload_bind")?;
        let explicit_help = raw.help_bind.is_some();
        let mut help_bind = Some(
            Chord::parse(raw.help_bind.as_deref().unwrap_or("ctrl+shift+h"))
                .context("invalid help_bind")?,
        );
        if help_bind == Some(reload_bind) {
            if explicit_help {
                bail!("help and reload bindings conflict");
            }
            help_bind = None;
        }
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

            if chord == reload_bind {
                bail!("app {:?} shares the reload binding", raw_app.name);
            }
            if Some(chord) == help_bind {
                if explicit_help {
                    bail!("app {:?} shares the help binding", raw_app.name);
                }
                help_bind = None;
            }
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
            reload_bind,
            help_bind,
            shell,
            separator,
            apps,
        })
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
    reload_bind: Option<String>,
    #[serde(default)]
    help_bind: Option<String>,
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

    #[test]
    fn help_binding_is_configurable_and_cannot_conflict() {
        let cfg = Config::parse("help_bind = 'alt+f1'").unwrap();
        assert_eq!(cfg.help_bind.unwrap().to_string(), "alt+f1");
        assert!(Config::parse("help_bind = 'ctrl+shift+r'").is_err());
        assert!(
            Config::parse(
                "help_bind='ctrl+shift+h'\n[[app]]\nname='a'\nbind='ctrl+shift+h'\ncmd=['a']"
            )
            .is_err()
        );
        assert!(
            Config::parse("[[app]]\nname='a'\nbind='ctrl+shift+h'\ncmd=['a']")
                .unwrap()
                .help_bind
                .is_none()
        );
        assert!(
            Config::parse("reload_bind='ctrl+shift+h'")
                .unwrap()
                .help_bind
                .is_none()
        );
    }

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
    fn missing_config_is_created_with_example_and_existing_config_is_preserved() {
        let dir = std::env::temp_dir().join(format!("haunt-config-test-{}", std::process::id()));
        let path = dir.join("haunt/config.toml");
        let _ = fs::remove_dir_all(&dir);

        let cfg = Config::load_path(&path).unwrap();
        assert!(cfg.apps.is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), EXAMPLE_CONFIG);
        let example = EXAMPLE_CONFIG
            .lines()
            .skip_while(|line| *line != "# [[app]]")
            .filter_map(|line| line.strip_prefix("# "))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(Config::parse(&example).unwrap().apps.len(), 1);

        let text = "[[app]]\nname = 'a'\nbind = 'ctrl+a'\ncmd = ['a']\n";
        fs::write(&path, text).unwrap();
        assert_eq!(Config::load_path(&path).unwrap().apps.len(), 1);
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        fs::remove_dir_all(dir).unwrap();
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
        assert_eq!(Config::parse("").unwrap().separator, ' ');
        assert_eq!(Config::parse("separator = '│'").unwrap().separator, '│');
        for value in ["", "ab", "界", "\u{301}", "\u{1b}"] {
            let input = format!("separator = {}", toml::Value::String(value.into()));
            assert!(Config::parse(&input).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn reload_binding_is_configurable_and_cannot_conflict() {
        let app = "[[app]]\nname = 'a'\nbind = 'ctrl+shift+r'\ncmd = ['a']\n";
        assert!(Config::parse(app).is_err());
        let cfg = Config::parse(&format!("reload_bind = 'ctrl+alt+r'\n{app}")).unwrap();
        assert_eq!(cfg.reload_bind, Chord::parse("ctrl+alt+r").unwrap());
        assert!(Config::parse("reload_bind = 'bad+key'").is_err());
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
