//! Turn a libghostty-vt render-state snapshot into ANSI output for Ghostty.
//!
//! Only dirty rows are repainted, and every cell in a dirty row is written,
//! including blank cells. This clears stale glyphs without `ED 2`, which would
//! also delete Ghostty's Kitty image storage. Cells with the default
//! background emit no background SGR, preserving Ghostty's opacity and blur.

use anyhow::Result;
use libghostty_vt::{
    render::{CellIterator, CursorVisualStyle, RowIterator, Snapshot},
    style::{RgbColor, Style, StyleColor, Underline},
    unicode,
};

/// A pane's destination rectangle in outer-terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

/// Painting options that are not part of a snapshot.
#[derive(Default)]
pub struct PaintState {
    pub selection_fg: Option<RgbColor>,
    pub selection_bg: Option<RgbColor>,
}

pub fn draw<'a>(
    snapshot: &Snapshot<'a, '_>,
    row_it: &mut RowIterator<'a>,
    cell_it: &mut CellIterator<'a>,
    state: &PaintState,
    out: &mut Vec<u8>,
    area: Area,
    force: bool,
) -> Result<()> {
    if area.w == 0 || area.h == 0 {
        return Ok(());
    }

    let mut row_iter = row_it.update(snapshot)?;
    let mut last_sgr: Option<Sgr> = None;
    let mut y = 0u16;

    while let Some(row) = row_iter.next() {
        if y >= area.h {
            break;
        }
        if !force && !row.dirty()? {
            y += 1;
            continue;
        }

        let mut cell_iter = cell_it.update(row)?;
        let mut x = 0u16;
        let mut cur_col: Option<u16> = None;

        while let Some(cell) = cell_iter.next() {
            if x >= area.w {
                break;
            }

            let style = if cell.has_styling()? {
                cell.style()?
            } else {
                Style::default()
            };
            let sgr = sgr_for(
                state,
                &style,
                cell.fg_color()?,
                cell.bg_color()?,
                cell.is_selected()?,
            );

            let mut text = String::new();
            cell.graphemes_utf8(&mut text)?;
            if text.is_empty() || text.chars().all(|c| c == '\0') {
                text.clear();
                text.push(' ');
            }

            let chars: Vec<char> = text.chars().collect();
            let (_, width) = unicode::grapheme_width(&chars);
            let width = width.max(1) as u16;

            // A wide glyph that would spill past the pane edge is painted as
            // a blank so the row is still fully cleared.
            if width > area.w.saturating_sub(x) {
                text.clear();
                text.push(' ');
            }
            let width = if text == " " { 1 } else { width };

            if last_sgr != Some(sgr) {
                emit_sgr(out, &sgr);
                last_sgr = Some(sgr);
            }
            move_to(out, area.y + y, area.x + x, &mut cur_col);
            out.extend_from_slice(text.as_bytes());
            cur_col = Some(area.x + x + width);

            x += width;
        }

        row.set_dirty(false)?;
        y += 1;
    }

    Ok(())
}

/// Emit the outer cursor for the focused pane.
pub fn draw_cursor(snapshot: &Snapshot<'_, '_>, out: &mut Vec<u8>, area: Area) -> Result<()> {
    let visible = snapshot.cursor_visible()?;
    let pos = snapshot.cursor_viewport()?;
    let inside = pos.map(|p| p.x < area.w && p.y < area.h).unwrap_or(false);

    if visible && inside {
        let p = pos.expect("checked above");
        out.extend_from_slice(
            format!("\x1b[{};{}H\x1b[?25h", area.y + p.y + 1, area.x + p.x + 1).as_bytes(),
        );
        let blinking = snapshot.cursor_blinking()?;
        let shape = match snapshot.cursor_visual_style()? {
            CursorVisualStyle::Block | CursorVisualStyle::BlockHollow => {
                if blinking {
                    1
                } else {
                    2
                }
            }
            CursorVisualStyle::Underline => {
                if blinking {
                    3
                } else {
                    4
                }
            }
            CursorVisualStyle::Bar => {
                if blinking {
                    5
                } else {
                    6
                }
            }
            _ => 0,
        };
        out.extend_from_slice(format!("\x1b[{shape} q").as_bytes());
    } else {
        out.extend_from_slice(b"\x1b[?25l");
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Sgr {
    bold: bool,
    faint: bool,
    italic: bool,
    underline: u8,
    blink: bool,
    inverse: bool,
    invisible: bool,
    strikethrough: bool,
    overline: bool,
    fg: Option<RgbColor>,
    bg: Option<RgbColor>,
    ul: Option<RgbColor>,
}

fn sgr_for(
    state: &PaintState,
    style: &Style,
    fg: Option<RgbColor>,
    bg: Option<RgbColor>,
    selected: bool,
) -> Sgr {
    let underline = match style.underline {
        Underline::None => 0,
        Underline::Single => 1,
        Underline::Double => 2,
        Underline::Curly => 3,
        Underline::Dotted => 4,
        Underline::Dashed => 5,
        _ => 0,
    };
    let ul = match style.underline_color {
        StyleColor::Rgb(rgb) => Some(rgb),
        _ => None,
    };
    let (fg, bg, inverse) = if selected {
        // Prefer the theme's selection colours. Until theme inheritance lands,
        // fall back to inverse so the highlight is still opaque.
        (
            state.selection_fg.or(fg),
            state.selection_bg.or(bg),
            state.selection_bg.is_none(),
        )
    } else {
        (fg, bg, style.inverse)
    };

    Sgr {
        bold: style.bold,
        faint: style.faint,
        italic: style.italic,
        underline,
        blink: style.blink,
        inverse,
        invisible: style.invisible,
        strikethrough: style.strikethrough,
        overline: style.overline,
        fg,
        bg,
        ul,
    }
}

fn emit_sgr(out: &mut Vec<u8>, sgr: &Sgr) {
    // Reset first: simpler and avoids leaking state between cells. SGR 0 also
    // returns the background to the terminal default, preserving transparency.
    let mut params: Vec<String> = vec!["0".to_string()];

    if sgr.bold {
        params.push("1".into());
    }
    if sgr.faint {
        params.push("2".into());
    }
    if sgr.italic {
        params.push("3".into());
    }
    match sgr.underline {
        0 => {}
        1 => params.push("4".into()),
        // SGR 4:<style> uses the same numbering as Ghostty's underline enum:
        // 1 single, 2 double, 3 curly, 4 dotted, 5 dashed.
        n => params.push(format!("4:{n}")),
    }
    if sgr.blink {
        params.push("5".into());
    }
    if sgr.inverse {
        params.push("7".into());
    }
    if sgr.invisible {
        params.push("8".into());
    }
    if sgr.strikethrough {
        params.push("9".into());
    }
    if sgr.overline {
        params.push("53".into());
    }

    match sgr.fg {
        Some(c) => params.push(format!("38;2;{};{};{}", c.r, c.g, c.b)),
        None => params.push("39".into()),
    }
    match sgr.bg {
        Some(c) => params.push(format!("48;2;{};{};{}", c.r, c.g, c.b)),
        None => params.push("49".into()),
    }
    if let Some(c) = sgr.ul {
        params.push(format!("58;2;{};{};{}", c.r, c.g, c.b));
    }

    out.extend_from_slice(b"\x1b[");
    out.extend_from_slice(params.join(";").as_bytes());
    out.push(b'm');
}

fn move_to(out: &mut Vec<u8>, y: u16, x: u16, cur_col: &mut Option<u16>) {
    if *cur_col == Some(x) {
        return;
    }
    out.extend_from_slice(format!("\x1b[{};{}H", y + 1, x + 1).as_bytes());
    *cur_col = Some(x);
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::{
        RenderState, Terminal, TerminalOptions,
        render::{CellIterator, RowIterator},
        selection::Selection,
        terminal::{Point, PointCoordinate},
    };

    fn area(cols: u16, rows: u16) -> Area {
        Area {
            x: 0,
            y: 0,
            w: cols,
            h: rows,
        }
    }

    fn term_with(text: &[u8]) -> Terminal<'static, 'static> {
        let mut term: Terminal<'static, 'static> = Terminal::new(TerminalOptions {
            cols: 20,
            rows: 4,
            max_scrollback: 0,
        })
        .unwrap();
        term.resize(20, 4, 8, 16).unwrap();
        term.vt_write(text);
        term
    }

    fn draw_term(
        term: &Terminal<'static, 'static>,
        render_state: &mut RenderState<'static>,
        rows: &mut RowIterator<'static>,
        cells: &mut CellIterator<'static>,
        state: &PaintState,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let snapshot = render_state.update(term)?;
        draw(&snapshot, rows, cells, state, out, area(20, 4), false)
    }

    fn select(term: &Terminal<'static, 'static>, start: u16, end: u16) {
        let start = term
            .grid_ref(Point::Viewport(PointCoordinate { x: start, y: 0 }))
            .unwrap();
        let end = term
            .grid_ref(Point::Viewport(PointCoordinate { x: end, y: 0 }))
            .unwrap();
        let selection = Selection::new(start, end, false);
        term.set_selection(Some(&selection)).unwrap();
    }

    #[test]
    fn default_background_cells_emit_no_background() {
        let term = term_with(b"hello");
        let mut render_state = RenderState::new().unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        let mut out = Vec::new();
        draw_term(
            &term,
            &mut render_state,
            &mut rows,
            &mut cells,
            &PaintState::default(),
            &mut out,
        )
        .unwrap();

        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("hello"), "{text:?}");
        assert!(
            !text.contains("48;2"),
            "default background was painted: {text:?}"
        );
    }

    #[test]
    fn clean_snapshots_paint_nothing() {
        let term = term_with(b"hello");
        let mut render_state = RenderState::new().unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();

        let mut first = Vec::new();
        draw_term(
            &term,
            &mut render_state,
            &mut rows,
            &mut cells,
            &PaintState::default(),
            &mut first,
        )
        .unwrap();
        assert!(!first.is_empty());

        let mut second = Vec::new();
        draw_term(
            &term,
            &mut render_state,
            &mut rows,
            &mut cells,
            &PaintState::default(),
            &mut second,
        )
        .unwrap();
        assert!(second.is_empty(), "dirty rows were repainted: {second:?}");
    }

    #[test]
    fn clearing_a_line_repaints_spaces() {
        let mut term = term_with(b"hello");
        let mut render_state = RenderState::new().unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        let mut first = Vec::new();
        draw_term(
            &term,
            &mut render_state,
            &mut rows,
            &mut cells,
            &PaintState::default(),
            &mut first,
        )
        .unwrap();

        term.vt_write(b"\r\x1b[2K");
        let mut second = Vec::new();
        draw_term(
            &term,
            &mut render_state,
            &mut rows,
            &mut cells,
            &PaintState::default(),
            &mut second,
        )
        .unwrap();
        let text = String::from_utf8_lossy(&second);
        assert!(
            text.contains(' '),
            "cleared row painted no blanks: {text:?}"
        );
        assert!(!text.contains("hello"), "stale text remained: {text:?}");
    }

    #[test]
    fn selected_blank_cells_paint_the_selection_background() {
        let term = term_with(b"hello");
        select(&term, 0, 9);

        let state = PaintState {
            selection_fg: None,
            selection_bg: Some(RgbColor {
                r: 0x3c,
                g: 0x40,
                b: 0x48,
            }),
        };
        let mut render_state = RenderState::new().unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        let mut out = Vec::new();
        draw_term(
            &term,
            &mut render_state,
            &mut rows,
            &mut cells,
            &state,
            &mut out,
        )
        .unwrap();

        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("48;2;60;64;72"),
            "selection background missing: {text:?}"
        );
    }

    #[test]
    fn selection_falls_back_to_inverse_without_theme_colours() {
        let term = term_with(b"hello");
        select(&term, 0, 4);

        let mut render_state = RenderState::new().unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        let mut out = Vec::new();
        draw_term(
            &term,
            &mut render_state,
            &mut rows,
            &mut cells,
            &PaintState::default(),
            &mut out,
        )
        .unwrap();

        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("\x1b[0;7;"), "inverse missing: {text:?}");
    }
}
