//! Inherit the outer Ghostty's colour theme.
//!
//! `ghostty +show-config --changes-only=false` prints the fully resolved
//! configuration: foreground, background, cursor colour, and all 256 palette
//! entries (theme files expanded, generated cube and ramp included). Feeding
//! those into libghostty-vt as *defaults* means inner programs resolve ANSI
//! colours through the user's actual theme and get matching answers to OSC
//! 4/10/11/12 colour queries.
//!
//! Default-background cells still emit no background SGR, so the painter keeps
//! Ghostty's window opacity and blur.

use std::{
    env,
    path::PathBuf,
    process::{Command, Stdio},
};

use anyhow::Result;
use libghostty_vt::{
    Terminal,
    style::{Palette, RgbColor},
    terminal::ColorScheme,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct Theme {
    pub fg: Option<RgbColor>,
    pub bg: Option<RgbColor>,
    pub cursor: Option<RgbColor>,
    pub selection_fg: Option<RgbColor>,
    pub selection_bg: Option<RgbColor>,
    pub palette: Palette,
}

impl Theme {
    /// Read the theme of the Ghostty instance hosting haunt.
    ///
    /// Returns `None` when not running under Ghostty or when the CLI is
    /// unavailable, in which case libghostty-vt's built-in defaults apply.
    pub fn load() -> Option<Self> {
        let under_ghostty = env::var("TERM_PROGRAM").ok().as_deref() == Some("ghostty")
            || env::var_os("GHOSTTY_RESOURCES_DIR").is_some();
        if !under_ghostty {
            return None;
        }

        let bin = env::var_os("GHOSTTY_BIN_DIR")
            .map(PathBuf::from)
            .map(|dir| dir.join("ghostty"))
            .filter(|path| path.is_file())
            .unwrap_or_else(|| PathBuf::from("ghostty"));

        let output = Command::new(bin)
            .args(["+show-config", "--no-pager", "--changes-only=false"])
            .stdin(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }

        Some(Self::parse(&String::from_utf8_lossy(&output.stdout)))
    }

    fn parse(text: &str) -> Self {
        let mut theme = Self::default();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "foreground" => theme.fg = RgbColor::parse(value).ok(),
                "background" => theme.bg = RgbColor::parse(value).ok(),
                "cursor-color" => theme.cursor = RgbColor::parse(value).ok(),
                "selection-foreground" => theme.selection_fg = RgbColor::parse(value).ok(),
                "selection-background" => theme.selection_bg = RgbColor::parse(value).ok(),
                "palette" => {
                    if let Ok((index, rgb)) = Palette::parse_palette_entry(value) {
                        theme.palette.set(index, rgb);
                    }
                }
                _ => {}
            }
        }

        theme
    }

    /// Install the theme as libghostty-vt's defaults for a pane's terminal.
    ///
    /// Per-index OSC overrides set by the inner program are preserved.
    pub fn apply(self, term: &mut Terminal<'_, '_>) -> Result<()> {
        term.set_default_fg_color(self.fg)?
            .set_default_bg_color(self.bg)?
            .set_default_cursor_color(self.cursor)?
            .set_default_color_palette(Some(self.palette))?;
        Ok(())
    }

    /// The colour scheme to report for `CSI ? 996 n` (and mode 2031).
    ///
    /// Uses the same luminance threshold Ghostty uses to classify its
    /// background as light or dark.
    pub fn scheme(self) -> Option<ColorScheme> {
        let bg = self.bg?;
        Some(if bg.perceived_luminance() > 0.5 {
            ColorScheme::Light
        } else {
            ColorScheme::Dark
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::style::PaletteIndex;

    #[test]
    fn parses_resolved_config() {
        let theme = Theme::parse(
            "\
font-family = JetBrainsMono Nerd Font
background = #16181a
foreground = #ffffff
cursor-color = #abcdef
selection-foreground = #ffffff
selection-background = #3c4048
palette = 0=#16181a
palette = 1=#e8796f
palette = 15=#ffffff
",
        );

        assert_eq!(
            theme.fg,
            Some(RgbColor {
                r: 0xff,
                g: 0xff,
                b: 0xff
            })
        );
        assert_eq!(
            theme.selection_bg,
            Some(RgbColor {
                r: 0x3c,
                g: 0x40,
                b: 0x48
            })
        );
        assert_eq!(
            theme.selection_fg,
            Some(RgbColor {
                r: 0xff,
                g: 0xff,
                b: 0xff
            })
        );
        assert_eq!(
            theme.bg,
            Some(RgbColor {
                r: 0x16,
                g: 0x18,
                b: 0x1a
            })
        );
        assert_eq!(
            theme.cursor,
            Some(RgbColor {
                r: 0xab,
                g: 0xcd,
                b: 0xef
            })
        );
        assert_eq!(
            theme.palette.get(PaletteIndex::RED),
            RgbColor {
                r: 0xe8,
                g: 0x79,
                b: 0x6f
            }
        );
    }

    #[test]
    fn ignores_unrelated_entries_and_comments() {
        let theme = Theme::parse("# background = #000000\nbackground-opacity = 0.85\n");
        assert_eq!(theme.bg, None);
        assert_eq!(theme.scheme(), None);
    }

    #[test]
    fn inner_queries_answer_with_the_theme() {
        use std::sync::{Arc, Mutex};

        use libghostty_vt::{Terminal, TerminalOptions};

        let theme = Theme::parse("background = #16181a\npalette = 1=#e8796f\n");
        let mut term = Terminal::new(TerminalOptions {
            cols: 10,
            rows: 2,
            max_scrollback: 0,
        })
        .unwrap();
        let responses = Arc::new(Mutex::new(Vec::<u8>::new()));
        {
            let responses = Arc::clone(&responses);
            term.on_pty_write(move |_term, data| {
                responses.lock().unwrap().extend_from_slice(data);
            })
            .unwrap();
        }
        theme.apply(&mut term).unwrap();

        term.vt_write(b"\x1b]11;?\x1b\\");
        term.vt_write(b"\x1b]4;1;?\x1b\\");

        let got = String::from_utf8_lossy(&responses.lock().unwrap()).to_string();
        assert!(got.contains("]11;rgb:1616/1818/1a1a"), "got {got:?}");
        assert!(got.contains("]4;1;rgb:e8e8/7979/6f6f"), "got {got:?}");
    }

    #[test]
    fn classifies_light_and_dark_backgrounds() {
        let dark = Theme {
            bg: Some(RgbColor {
                r: 0x16,
                g: 0x18,
                b: 0x1a,
            }),
            ..Default::default()
        };
        assert_eq!(dark.scheme(), Some(ColorScheme::Dark));

        let light = Theme {
            bg: Some(RgbColor {
                r: 0xfa,
                g: 0xfa,
                b: 0xfa,
            }),
            ..Default::default()
        };
        assert_eq!(light.scheme(), Some(ColorScheme::Light));
    }
}
