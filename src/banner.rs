//! The launch banner, written into the main pane as if the shell had printed it.
//!
//! `banner = false` in the config turns it off; the ghost is left out whenever
//! the outer terminal is not one that draws Kitty images.

use std::io::Write as _;
use std::path::Path;

use libghostty_vt::{style::PaletteIndex, unicode};

use crate::{app::Size, base64, config::Config, machine, theme::Theme};
/// The ghost, embedded so the binary carries its own copy.
const GHOST_PNG: &[u8] = include_bytes!("../assets/ghost.png");

/// The Kitty image ID used inside the main pane. There is one banner, in one
/// pane, so one fixed ID is enough. It is chosen high because programs number
/// their own images from one and could otherwise overwrite the ghost.
const GHOST_ID: u32 = 0x0048_4155;
/// The ghost's placement ID, so re-placing it after a resize replaces the
/// existing placement instead of stacking another on top of it.
const GHOST_PLACEMENT: u32 = 1;
/// Clear cells between the ghost and the text column.
const TEXT_GAP: u16 = 3;
/// Base64 payload per Kitty chunk: 3072 raw bytes encode to 4096 characters.
const CHUNK_BYTES: usize = 3072;

/// The wordmark, the version, and the tones of the text column. All ANSI, so the
/// pane resolves them through the user's Ghostty theme: the wordmark is bright
/// cyan, the labels and version a colour blended from bright black and the theme's
/// foreground (bright black alone is often too dim against the background to read
/// comfortably, see [`secondary_ink`]), and the values the default foreground.
const WORDMARK: &str = "\x1b[1;36m";
const RESET: &str = "\x1b[0m";
/// The ink for labels and the version when there is no palette to read: the
/// terminal's own foreground, which is at least readable.
const DEFAULT_INK: &str = "\x1b[0;39m";

/// The banner bytes, and whether they include a ghost the main pane now holds.
pub struct Banner {
    pub bytes: Vec<u8>,
    pub ghost: bool,
}

/// Draw the banner for a pane of `size` whose cursor is at `cursor`.
///
/// The cursor is where the pane's cursor stands when the banner is written,
/// which is normally just after the shell's first prompt. `ghost` says whether
/// the terminal can be assumed to draw Kitty images; without one the banner sits
/// flush left.
pub fn draw(
    cfg: &Config,
    size: Size,
    cursor: (u16, u16),
    ghost: bool,
    theme: Option<Theme>,
) -> Banner {
    let probed = probe_lines(cfg);
    let rows = probed.len() as u16 + 1;
    let cols = if ghost {
        ghost_cols(GHOST_PNG, rows, size)
    } else {
        0
    };
    let indent = if ghost { cols + TEXT_GAP } else { 0 };
    let secondary = secondary_ink(theme);
    let lines = lines(
        &probed,
        usize::from(size.cols.saturating_sub(indent)),
        &secondary,
    );

    let mut out = Vec::with_capacity(4 * 1024);
    // Open the space at the top. A scroll does not move the cursor, so the
    // absolute move at the end is what puts the prompt back under it.
    let _ = write!(out, "\x1b[{rows}T");

    if ghost {
        transmit_png(&mut out, GHOST_PNG);
        // Behind the text so the lines read cleanly over it, and `C=1` to clear
        // the cells it covers first: the ghost is translucent, and the prompt it
        // scrolls over would otherwise show through.
        let _ = write!(
            out,
            "\x1b[1;1H\x1b_Ga=p,i={GHOST_ID},p={GHOST_PLACEMENT},q=2,C=1,c={cols},r={rows},z=-1\x1b\\"
        );
    }

    for (index, runs) in lines.iter().enumerate() {
        let _ = write!(out, "\x1b[{};{}H", index + 1, u32::from(indent) + 1);
        for (ink, text) in runs {
            let _ = write!(out, "{ink}{text}");
        }
        // Clear the rest of the row, so a long line cannot leave a tail of the
        // content it scrolled over.
        out.extend_from_slice(b"\x1b[K\x1b[0m");
    }

    let _ = write!(
        out,
        "\x1b[{};{}H\x1b[0m",
        usize::from(cursor.1) + usize::from(rows) + 1,
        usize::from(cursor.0) + 1
    );
    Banner { bytes: out, ghost }
}

/// The banner's rows: the wordmark, then one row per probed line. A row is the
/// styled runs to write on it, in order.
fn lines<'a>(
    probed: &[(&'static str, String)],
    budget: usize,
    secondary: &'a str,
) -> Vec<Vec<(&'a str, String)>> {
    let label_w = probed
        .iter()
        .map(|(label, _)| display_width(label))
        .max()
        .unwrap_or(6)
        .max(6);

    // The version sits in the values column, not hard against the wordmark.
    let wordmark = "haunt";
    let gap = (label_w + 1).saturating_sub(display_width(wordmark));
    let mut out = vec![vec![
        (WORDMARK, wordmark.into()),
        (
            secondary,
            format!("{}v{}", " ".repeat(gap), env!("CARGO_PKG_VERSION")),
        ),
    ]];
    for (name, value) in probed {
        let label = clip(&format!("{name:<label_w$} "), budget);
        let room = budget.saturating_sub(display_width(&label));
        out.push(vec![(secondary, label), (RESET, clip(value, room))]);
    }
    out
}

/// The ink for the version and labels: bright black lifted toward the theme's
/// foreground, or [`DEFAULT_INK`] with no theme to read a palette from.
fn secondary_ink(theme: Option<Theme>) -> String {
    let Some(theme) = theme else {
        return DEFAULT_INK.into();
    };
    let Some(fg) = theme.fg else {
        return DEFAULT_INK.into();
    };
    let dim = theme.palette.get(PaletteIndex::BRIGHT_BLACK);
    format!(
        "\x1b[0;38;2;{};{};{}m",
        blend(dim.r, fg.r),
        blend(dim.g, fg.g),
        blend(dim.b, fg.b)
    )
}

/// Lift `from` a quarter of the way toward `to`, rounded to the nearest
/// component. A quarter is enough to read without competing with the values.
fn blend(from: u8, to: u8) -> u8 {
    ((u16::from(from) * 3 + u16::from(to) + 2) / 4) as u8
}

/// The lines about the machine, in order. A probe that returns nothing is
/// skipped.
fn probe_lines(cfg: &Config) -> Vec<(&'static str, String)> {
    let shell = cfg.shell.first().map(String::as_str);
    [
        (
            "host",
            Some(format!("{}@{}", machine::user(), machine::hostname())),
        ),
        ("system", machine::system()),
        ("kernel", machine::kernel()),
        ("uptime", machine::uptime().map(machine::format_uptime)),
        ("shell", shell.map(shell_line)),
        ("terminal", machine::terminal()),
    ]
    .into_iter()
    .filter_map(|(label, value)| {
        let text = value.filter(|text| !text.trim().is_empty())?;
        Some((label, sanitize(text.trim())))
    })
    .collect()
}

/// The shell's name, plus its version if it reports one.
fn shell_line(program: &str) -> String {
    let name = Path::new(program)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_owned());
    match machine::version(program) {
        Some(version) => format!("{name} {version}"),
        None => name,
    }
}

/// Replace control characters with spaces, so a probe's output cannot smuggle
/// escape sequences or line breaks into the banner.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The cells an image of its natural size needs to keep its aspect over `rows`.
fn ghost_cols(png: &[u8], rows: u16, size: Size) -> u16 {
    let Some((w, h)) = png_size(png) else {
        return 0;
    };
    let tall = u64::from(rows.max(1)) * u64::from(size.cell_h.max(1));
    let wide = tall * u64::from(w) / u64::from(h.max(1));
    // Clamped to a fraction of a wide pane, so a pathological cell size cannot
    // push the text off screen.
    (wide / u64::from(size.cell_w.max(1))).clamp(1, 40) as u16
}

/// The PNG's pixel size, read out of the IHDR.
fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    let header = png.get(16..24)?;
    let (width, height) = header.split_at(4);
    Some((
        u32::from_be_bytes(width.try_into().ok()?),
        u32::from_be_bytes(height.try_into().ok()?),
    ))
}

/// Transmit a PNG into the pane's image store, letting libghostty decode it.
fn transmit_png(out: &mut Vec<u8>, png: &[u8]) {
    let chunks = png.chunks(CHUNK_BYTES);
    let total = chunks.len().max(1);
    for (index, chunk) in chunks.enumerate() {
        let more = usize::from(index + 1 < total);
        if index == 0 {
            let _ = write!(out, "\x1b_Ga=t,f=100,i={GHOST_ID},q=2,m={more};");
        } else {
            let _ = write!(out, "\x1b_Gm={more};");
        }
        base64::encode_into(chunk, out);
        out.extend_from_slice(b"\x1b\\");
    }
}

/// The cells a string occupies.
fn display_width(text: &str) -> usize {
    text.chars()
        .map(|c| unicode::grapheme_width(&[c]).1 as usize)
        .sum()
}

/// Truncate to `cells` terminal columns, marking a real cut with an ellipsis.
/// Values reach this already sanitised, so every character counts.
fn clip(text: &str, cells: usize) -> String {
    if cells == 0 {
        return String::new();
    }
    let width = |c: char| unicode::grapheme_width(&[c]).1 as usize;
    let mut out = String::with_capacity(text.len());
    let mut used = 0;
    for c in text.chars() {
        let wide = width(c);
        if used + wide > cells {
            // Drop whole cells until there is room for the ellipsis itself.
            while used + width('…') > cells {
                let Some(last) = out.chars().next_back() else {
                    return String::new();
                };
                used -= width(last);
                out.pop();
            }
            out.push('…');
            return out;
        }
        out.push(c);
        used += wide;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::{
        RenderState, Terminal,
        render::{CellIterator, RowIterator},
    };

    fn size(cols: u16, rows: u16) -> Size {
        Size {
            cols,
            rows,
            cell_w: 8,
            cell_h: 16,
        }
    }

    /// A pane with `prompt` printed on it, the way a shell leaves it.
    fn pane(prompt: &str, cols: u16, rows: u16) -> Terminal<'static, 'static> {
        let mut term: Terminal<'static, 'static> = Terminal::new(cols, rows).unwrap();
        term.resize(cols, rows, 8, 16).unwrap();
        term.set_kitty_image_storage_limit(4 * 1024 * 1024).unwrap();
        term.vt_write(prompt.as_bytes());
        term
    }

    /// The pane's visible rows, as trimmed text.
    fn screen(term: &Terminal<'static, 'static>) -> Vec<String> {
        let mut render_state = RenderState::new().unwrap();
        let snapshot = render_state.update(term).unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        let mut row_iter = rows.update(&snapshot).unwrap();
        let mut out = Vec::new();
        while let Some(row) = row_iter.next() {
            let mut iter = cells.update(row).unwrap();
            let mut line = String::new();
            while let Some(cell) = iter.next() {
                let mut text = String::new();
                cell.graphemes_utf8(&mut text).unwrap();
                line.push_str(&text.replace('\0', " "));
            }
            out.push(line.trim_end().to_string());
        }
        out
    }

    fn cfg() -> Config {
        Config::parse("shell = ['/bin/sh']").unwrap()
    }

    /// The banner's height: the wordmark, plus one row per line.
    fn rows() -> usize {
        probe_lines(&cfg()).len() + 1
    }

    #[test]
    fn the_banner_sits_above_the_prompt_and_leaves_it_where_it_was() {
        let banner = draw(&cfg(), size(80, 24), (2, 1), false, None);
        assert!(!banner.bytes.is_empty(), "the banner had nothing to say");
        let mut term = pane("hello\r\n$ ", 80, 24);
        term.vt_write(&banner.bytes);

        let screen = screen(&term);
        assert!(screen[0].contains("haunt"), "{:?}", screen[0]);
        assert!(screen[0].contains("v0."), "no version: {:?}", screen[0]);
        // Both prompt rows survived, one banner lower than the shell left them.
        assert_eq!(screen.get(rows()), Some(&"hello".into()), "{screen:?}");
        assert_eq!(screen.get(rows() + 1), Some(&"$".into()), "{screen:?}");
        assert_eq!(
            usize::from(term.cursor_y().unwrap()),
            1 + rows(),
            "the cursor was not returned to the prompt"
        );
        assert_eq!(term.cursor_x().unwrap(), 2);
    }

    #[test]
    fn the_banner_names_the_machine_the_shell_and_the_terminal() {
        let labels: Vec<&str> = probe_lines(&cfg())
            .iter()
            .map(|(label, _)| *label)
            .collect();
        // The terminal line is absent when the terminal does not name itself,
        // as on a CI runner with no `TERM_PROGRAM`.
        let mut expected = vec!["host", "system", "kernel", "uptime", "shell"];
        if machine::terminal().is_some() {
            expected.push("terminal");
        }
        assert_eq!(labels, expected);
        let shell = probe_lines(&cfg())
            .iter()
            .find(|(label, _)| *label == "shell")
            .map(|(_, value)| value.clone())
            .unwrap();
        assert!(shell.starts_with("sh"), "{shell}");
        assert!(
            !shell.contains("  "),
            "a version gap with nothing in it: {shell}"
        );
    }

    #[test]
    fn a_narrow_terminal_clips_the_banner_instead_of_wrapping_it() {
        for cols in [20u16, 32, 48, 64] {
            let banner = draw(&cfg(), size(cols, 24), (2, 0), false, None);
            let mut term = pane("$ ", cols, 24);
            term.vt_write(&banner.bytes);
            for (row, line) in screen(&term).iter().enumerate().take(rows() + 1) {
                assert!(
                    display_width(line) <= usize::from(cols),
                    "{cols} columns, row {row}: {line:?}"
                );
            }
        }
    }

    #[test]
    fn the_ghost_sits_behind_the_text_and_beside_the_lines() {
        let banner = draw(&cfg(), size(80, 24), (0, 0), true, None);
        assert!(banner.ghost);
        let text = String::from_utf8_lossy(&banner.bytes).into_owned();
        assert!(
            text.contains(&format!("a=p,i={GHOST_ID}")),
            "the ghost was never placed: {text}"
        );
        assert!(text.contains("z=-1"), "the ghost is not behind the text");
        // A near-square ghost over the banner's rows is about two columns per
        // row, and the text starts a few columns after it.
        let cols = ghost_cols(GHOST_PNG, rows() as u16, size(80, 24));
        assert!(cols >= 8, "{cols}");
        assert!(
            text.contains(&format!("\x1b[1;{}H", cols + TEXT_GAP + 1)),
            "the text did not start beside the ghost: {text}"
        );
    }

    #[test]
    fn without_an_image_terminal_the_banner_sits_flush_left() {
        let banner = draw(&cfg(), size(80, 24), (0, 0), false, None);
        assert!(!banner.ghost);
        let text = String::from_utf8_lossy(&banner.bytes).into_owned();
        assert!(!text.contains("_Ga="), "{text}");
        assert!(text.contains("\x1b[1;1H"), "the banner was not flush left");
    }

    #[test]
    fn secondary_ink_mixes_a_little_foreground_into_bright_black() {
        use libghostty_vt::style::{Palette, RgbColor};

        let mut palette = Palette::default();
        palette.set(PaletteIndex::BRIGHT_BLACK, RgbColor { r: 0, g: 0, b: 0 });
        let theme = Theme {
            fg: Some(RgbColor {
                r: 255,
                g: 255,
                b: 255,
            }),
            palette,
            ..Theme::default()
        };
        assert_eq!(secondary_ink(Some(theme)), "\x1b[0;38;2;64;64;64m");
        assert_eq!(blend(0, 0), 0);
        assert_eq!(blend(255, 255), 255);
        assert_eq!(secondary_ink(None), "\x1b[0;39m");
    }

    #[test]
    fn png_size_reads_the_ihdr() {
        let mut png = vec![0u8; 24];
        png[16..20].copy_from_slice(&3u32.to_be_bytes());
        png[20..24].copy_from_slice(&2u32.to_be_bytes());
        assert_eq!(png_size(&png), Some((3, 2)));
        assert_eq!(png_size(b"short"), None);
        let (w, h) = png_size(GHOST_PNG).expect("the ghost is a PNG");
        assert!(w > 100 && h > 100, "{w}x{h}");
    }

    #[test]
    fn ghost_columns_keep_the_aspect_ratio() {
        let mut png = vec![0u8; 24];
        png[16..20].copy_from_slice(&240u32.to_be_bytes());
        png[20..24].copy_from_slice(&232u32.to_be_bytes());
        // Seven rows at 8x16 is 112px tall, so ~116px wide at 8px per cell.
        assert_eq!(ghost_cols(&png, 7, size(80, 24)), 14);
        assert_eq!(ghost_cols(&png, 1, size(80, 24)), 2);
        assert_eq!(ghost_cols(b"", 6, size(80, 24)), 0);
    }

    #[test]
    fn the_ghost_is_transmitted_in_one_frame() {
        let mut out = Vec::new();
        transmit_png(&mut out, GHOST_PNG);
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.starts_with(&format!("\u{1b}_Ga=t,f=100,i={GHOST_ID},q=2,m=0;")),
            "{text}"
        );
        assert!(text.ends_with("\u{1b}\\"), "the APC was left open: {text}");
        // One chunk of base64, so exactly one pair of APC delimiters.
        assert_eq!(out.iter().filter(|&&b| b == 0x1b).count(), 2);
    }

    #[test]
    fn clip_marks_only_real_truncation() {
        assert_eq!(clip("uptime", 10), "uptime");
        assert_eq!(clip("0123456789", 6), "01234…");
        assert_eq!(clip("界界界", 5), "界界…");
        assert_eq!(clip("界界界", 4), "界…");
        assert_eq!(clip("wide", 0), "");
        assert_eq!(sanitize("a\u{1b}[31mb"), "a [31mb");
    }

    #[test]
    fn the_banner_is_a_wordmark_and_a_row_per_line() {
        let probed: Vec<(&'static str, String)> = vec![
            ("host", "scott@ncase".into()),
            ("kernel", "Linux 6.12.9".into()),
            ("uptime", "3 days, 4 hrs".into()),
        ];
        let text: Vec<String> = lines(&probed, 40, RESET)
            .iter()
            .map(|runs| runs.iter().map(|(_, run)| run.as_str()).collect())
            .collect();
        // The version starts in the same column as the values below it.
        assert_eq!(text[0], "haunt  v0.3.1");
        assert_eq!(text[1], "host   scott@ncase");
        assert_eq!(text[2], "kernel Linux 6.12.9");
        assert_eq!(text[3], "uptime 3 days, 4 hrs");

        // A value too wide for the pane is cut, never wrapped onto the next row.
        let tight = lines(&probed, 12, RESET);
        assert!(display_width(&tight[2].iter().map(|(_, t)| t.as_str()).collect::<String>()) <= 12);
        assert_eq!(
            clip("NixOS 25.05", 11),
            "NixOS 25.05",
            "eleven cells is exactly eleven"
        );
        assert_eq!(clip("NixOS 25.05", 10), "NixOS 25.…");
    }

    /// The ghost has to survive the trip to the outer terminal, which needs the
    /// PNG decoder installed before any libghostty call. Same trick as
    /// `graphics::tests::png_decoder_is_installed`: re-exec this test alone.
    #[test]
    fn the_ghost_is_forwarded_and_comes_back_after_a_resize() {
        const CHILD: &str = "HAUNT_GHOST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "banner::tests::the_ghost_is_forwarded_and_comes_back_after_a_resize",
                ])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        // SAFETY: this subprocess runs only this test, before creating terminals.
        unsafe {
            libghostty_vt::kitty::graphics::set_png_decoder(Some(Box::new(
                libghostty_vt::kitty::graphics::RustPngDecoder::new(),
            )))
        }
        .unwrap();
        forward_and_refresh();
    }

    fn forward_and_refresh() {
        use crate::{graphics::Graphics, paint::Area};

        let banner = draw(&cfg(), size(80, 24), (2, 1), true, None);
        assert!(banner.ghost);
        let mut term = pane("$ ", 80, 24);
        term.vt_write(&banner.bytes);

        let area = Area {
            x: 0,
            y: 0,
            w: 80,
            h: 24,
        };
        let mut graphics = Graphics::new().unwrap();
        let mut out = Vec::new();
        let repaint = graphics
            .draw_pane(0, &term, &mut out, area)
            .expect("the pane's image forwards");

        // libghostty decodes the PNG and haunt re-sends it as raw RGBA under a
        // fresh outer ID, at the banner's first row, behind the text.
        assert!(repaint, "a new image must repaint the cells");
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("a=t,f=32"), "no pixels forwarded: {text}");
        assert!(text.contains("a=p,i=1,"), "no placement forwarded: {text}");
        assert!(
            text.contains("z=-1"),
            "the ghost is not behind the text: {text}"
        );

        // A resize. The outer terminal has dropped the pixels, so the forwarder
        // has to forget them and the pane has to place the image again.
        for _ in 0..3 {
            term.resize(80, 30, 8, 16).unwrap();
            graphics.remove_pane(0);
            out.clear();
            graphics.draw_pane(0, &term, &mut out, area).unwrap();
            let text = String::from_utf8_lossy(&out);
            assert!(
                text.contains("a=t,f=32"),
                "the pixels were not re-sent: {text}"
            );
            assert!(
                text.contains("\x1b[1;1H"),
                "the ghost was not put back on the banner's row: {text}"
            );
            assert_eq!(
                text.matches("a=p,").count(),
                1,
                "a resize stacked another ghost: {text}"
            );
        }
    }
}
