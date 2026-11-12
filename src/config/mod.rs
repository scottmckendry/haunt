//! Configuration: shell command, auxiliary apps, and their direct chords.
//!
//! Loaded from `$XDG_CONFIG_HOME/haunt/config.toml` (usually
//! `~/.config/haunt/config.toml`). A missing file is created with a commented
//! example. Apps are named tables, so the table key doubles as the app name.

mod chord;

use std::{
    env,
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use serde::Deserialize;

use crate::shell;

pub use chord::Chord;

const EXAMPLE_CONFIG: &str = include_str!("example.toml");
const DEFAULT_RELOAD_BIND: &str = "ctrl+shift+r";
const DEFAULT_HELP_BIND: &str = "ctrl+shift+h";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppSize {
    #[default]
    Half,
    Full,
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
    pub banner: bool,
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
        Self::parse_file(&path)
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
        Self::parse_file(path)
    }

    fn parse_file(path: &Path) -> Result<Self> {
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

        let separator = parse_separator(raw.separator)?;
        let binds = parse_binds(raw.reload_bind.as_deref(), raw.help_bind.as_deref())?;
        let apps = raw.apps.into_iter().map(|(name, app)| RawApp {
            name,
            bind: app.bind,
            cmd: app.cmd,
            size: app.size,
        });
        let (apps, help_bind) = build_apps(apps, &binds)?;

        Ok(Self {
            reload_bind: binds.reload,
            help_bind,
            shell,
            separator,
            banner: raw.banner.unwrap_or(true),
            apps,
        })
    }
}

fn parse_separator(raw: Option<String>) -> Result<char> {
    let separator = raw.unwrap_or_else(|| " ".into());
    let mut chars = separator.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None)
            if !c.is_control() && libghostty_vt::unicode::grapheme_width(&[c]).1 == 1 =>
        {
            Ok(c)
        }
        _ => bail!("`separator` must be a single printable, one-cell character"),
    }
}

struct Binds {
    reload: Chord,
    help: Option<Chord>,
    /// Whether `help_bind` was set explicitly, which makes collisions errors.
    help_explicit: bool,
}

fn parse_binds(reload: Option<&str>, help: Option<&str>) -> Result<Binds> {
    let reload =
        Chord::parse(reload.unwrap_or(DEFAULT_RELOAD_BIND)).context("invalid reload_bind")?;

    let help_explicit = help.is_some();
    let mut help =
        Some(Chord::parse(help.unwrap_or(DEFAULT_HELP_BIND)).context("invalid help_bind")?);
    if help == Some(reload) {
        if help_explicit {
            bail!("help and reload bindings conflict");
        }
        help = None;
    }

    Ok(Binds {
        reload,
        help,
        help_explicit,
    })
}

fn build_apps(
    raw_apps: impl IntoIterator<Item = RawApp>,
    binds: &Binds,
) -> Result<(Vec<AppConfig>, Option<Chord>)> {
    let mut help = binds.help;
    let mut apps = Vec::new();
    let mut seen_binds: Vec<(Chord, String)> = Vec::new();

    for raw_app in raw_apps {
        if raw_app.name.trim().is_empty() {
            bail!("app with empty name");
        }
        if raw_app.cmd.is_empty() || raw_app.cmd[0].trim().is_empty() {
            bail!("app {:?} has an empty `cmd`", raw_app.name);
        }

        let chord = Chord::parse(&raw_app.bind)
            .with_context(|| format!("app {:?} has an invalid bind", raw_app.name))?;

        if chord == binds.reload {
            bail!("app {:?} shares the reload binding", raw_app.name);
        }
        if Some(chord) == help {
            if binds.help_explicit {
                bail!("app {:?} shares the help binding", raw_app.name);
            }
            help = None;
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

    Ok((apps, help))
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
    reload_bind: Option<String>,
    help_bind: Option<String>,
    shell: Option<Vec<String>>,
    separator: Option<String>,
    banner: Option<bool>,
    #[serde(default)]
    apps: IndexMap<String, RawAppEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAppEntry {
    bind: String,
    cmd: Vec<String>,
    #[serde(default)]
    size: AppSize,
}

/// An app entry paired with its table key.
struct RawApp {
    name: String,
    bind: String,
    cmd: Vec<String>,
    size: AppSize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_binding_is_configurable_and_cannot_conflict() {
        let cfg = Config::parse("help_bind = 'alt+f1'").unwrap();
        assert_eq!(cfg.help_bind.unwrap().to_string(), "alt+f1");
        assert!(Config::parse("help_bind = 'ctrl+shift+r'").is_err());
        assert!(
            Config::parse("help_bind='ctrl+shift+h'\n[apps.a]\nbind='ctrl+shift+h'\ncmd=['a']")
                .is_err()
        );
        assert!(
            Config::parse("[apps.a]\nbind='ctrl+shift+h'\ncmd=['a']")
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
            .skip_while(|line| *line != "# [apps.lazygit]")
            .filter_map(|line| line.strip_prefix("# "))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(Config::parse(&example).unwrap().apps.len(), 1);

        let text = "[apps.a]\nbind = 'ctrl+a'\ncmd = ['a']\n";
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

            [apps.pi]
            bind = "ctrl+alt+p"
            cmd = ["pi"]
            size = "half"

            [apps.lazygit]
            bind = "ctrl+alt+g"
            cmd = ["lazygit"]
            size = "full"
            "#,
        )
        .unwrap();

        assert_eq!(cfg.shell, ["nu", "-l"]);
        assert_eq!(cfg.apps.len(), 2);
        assert_eq!(cfg.apps[1].name, "lazygit");
        assert_eq!(cfg.apps[1].size, AppSize::Full);
    }

    #[test]
    fn keeps_apps_in_declaration_order() {
        let cfg = Config::parse(
            r#"
            [apps.zebra]
            bind = "ctrl+z"
            cmd = ["zebra"]

            [apps."my editor"]
            bind = "ctrl+e"
            cmd = ["nvim", "."]

            [apps.alpha]
            bind = "ctrl+a"
            cmd = ["alpha"]
            "#,
        )
        .unwrap();

        let names: Vec<_> = cfg.apps.iter().map(|app| app.name.as_str()).collect();
        assert_eq!(names, ["zebra", "my editor", "alpha"]);
        assert_eq!(cfg.apps[1].cmd, ["nvim", "."]);
        assert_eq!(cfg.apps[1].size, AppSize::Half);
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
        let app = "[apps.a]\nbind = 'ctrl+shift+r'\ncmd = ['a']\n";
        assert!(Config::parse(app).is_err());
        let cfg = Config::parse(&format!("reload_bind = 'ctrl+alt+r'\n{app}")).unwrap();
        assert_eq!(cfg.reload_bind, Chord::parse("ctrl+alt+r").unwrap());
        assert!(Config::parse("reload_bind = 'bad+key'").is_err());
    }

    #[test]
    fn rejects_duplicate_binds() {
        let err = Config::parse(
            r#"
            [apps.a]
            bind = "ctrl+alt+p"
            cmd = ["a"]

            [apps.b]
            bind = "ctrl+alt+p"
            cmd = ["b"]
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("share the same bind"));
    }

    #[test]
    fn rejects_unknown_fields_and_duplicate_app_tables() {
        assert!(Config::parse("separatorr = '|'").is_err());
        assert!(Config::parse("[apps.a]\nbind = 'ctrl+p'\ncmd = ['a']\nextra = 1").is_err());
        assert!(
            Config::parse(
                "[apps.a]\nbind = 'ctrl+p'\ncmd = ['a']\n\
                 [apps.a]\nbind = 'ctrl+q'\ncmd = ['b']",
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_whitespace_programs() {
        assert!(Config::parse("shell = ['  ']").is_err());
        assert!(Config::parse("[apps.a]\nbind = 'ctrl+p'\ncmd = ['  ']").is_err());
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
        let err = Config::parse("[apps.empty]\nbind = 'ctrl+alt+p'\ncmd = []").unwrap_err();
        assert!(err.to_string().contains("empty `cmd`"));
    }
}
