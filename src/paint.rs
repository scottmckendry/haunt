//! Turn a libghostty-vt render-state snapshot into ANSI output for Ghostty.
//!
//! Dirty rows are repainted, but only the cells whose glyph, style, hyperlink
//! or width actually changed are written. The diff is against the last frame
//! this pane painted, so a one-cell update costs one cell rather than a whole
//! row. Cells with the default background emit no background SGR, preserving
//! Ghostty's opacity and blur.

use anyhow::Result;
use libghostty_vt::{
    Error, Terminal,
    render::{CellIterator, CursorVisualStyle, RowIterator, Snapshot},
    screen::{CellWide, cell},
    style::{RgbColor, Style, StyleColor, Underline},
    terminal::{Point, PointCoordinate},
    unicode,
};

use crate::graphics::Graphics;

/// Kitty's Unicode placeholder base character. The image ID is carried in the
/// cell's foreground colour; row/column/ID-high values are combining diacritics.
const PLACEHOLDER: u32 = 0x10EEEE;

/// A pane's destination rectangle in outer-terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

/// A cell as it was last written to the outer terminal.
#[derive(Clone, Default, PartialEq, Eq)]
struct PaintedCell {
    text: Box<str>,
    /// 0 marks the second column of a wide glyph, which has no cell of its own.
    width: u16,
    sgr: Sgr,
    uri: Option<Box<str>>,
}

/// Painting options that are not part of a snapshot.
#[derive(Default)]
pub struct PaintState {
    pub selection_fg: Option<RgbColor>,
    pub selection_bg: Option<RgbColor>,
    painted: Vec<PaintedCell>,
    painted_at: Option<Area>,
}

impl PaintState {
    /// Whether the cache still describes cells painted at this destination.
    fn cache_valid(&self, area: Area) -> bool {
        self.painted_at == Some(area)
            && self.painted.len() == usize::from(area.w) * usize::from(area.h)
    }

    fn reset_cache(&mut self, area: Area) {
        self.painted = vec![PaintedCell::default(); usize::from(area.w) * usize::from(area.h)];
        self.painted_at = Some(area);
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "render handles are passed separately to keep their borrows disjoint"
)]
pub fn draw<'a>(
    snapshot: &Snapshot<'a, '_>,
    term: &Terminal<'_, '_>,
    graphics: &Graphics,
    pane_id: u64,
    row_it: &mut RowIterator<'a>,
    cell_it: &mut CellIterator<'a>,
    state: &mut PaintState,
    out: &mut Vec<u8>,
    area: Area,
    force: bool,
) -> Result<()> {
    if area.w == 0 || area.h == 0 {
        return Ok(());
    }

    // A moved or resized pane has no valid cache, so paint every row even if
    // the render state says only some rows are dirty.
    let mut full = force;
    if !state.cache_valid(area) {
        state.reset_cache(area);
        full = true;
    }

    let mut row_iter = row_it.update(snapshot)?;
    let mut last_sgr: Option<Sgr> = None;
    let mut last_uri: Option<String> = None;
    let mut text = String::new();
    let mut chars = Vec::new();
    let mut y = 0u16;

    while let Some(row) = row_iter.next() {
        if y >= area.h {
            break;
        }
        if !full && !row.dirty()? {
            y += 1;
            continue;
        }

        let selection = row.selection()?;
        let mut cell_iter = cell_it.update(row)?;
        let mut x = 0u16;
        let mut cur_col: Option<u16> = None;

        while let Some(cell) = cell_iter.next() {
            if x >= area.w {
                break;
            }

            // OSC 8 hyperlinks and placeholder IDs are not part of the
            // render state, so resolve them through the raw cell. Batch the
            // raw queries so each cell crosses the FFI boundary once.
            let raw = cell.raw_cell()?;
            let (codepoint, wide, has_styling, has_hyperlink) = raw.get_multi((
                cell::Codepoint,
                cell::Wide,
                cell::HasStyling,
                cell::HasHyperlink,
            ))?;

            let style = if has_styling {
                cell.style()?
            } else {
                Style::default()
            };

            // The tail cell of a wide glyph has no content of its own: the
            // preceding wide cell already advanced x past both columns. Skip
            // it so we don't paint a redundant blank after the wide glyph.
            if matches!(wide, CellWide::SpacerTail) {
                continue;
            }
            let placeholder = codepoint == PLACEHOLDER;
            let fg = if placeholder {
                match placeholder_outer_id(graphics, pane_id, &style) {
                    Some(id) => Some(image_id_color(id)),
                    None => cell.fg_color()?,
                }
            } else {
                cell.fg_color()?
            };
            let selected = selection.is_some_and(|range| x >= range.start_x && x <= range.end_x);
            let sgr = sgr_for(state, &style, fg, cell.bg_color()?, selected);

            let uri = if has_hyperlink {
                hyperlink_uri(term, x, y as u32)?
            } else {
                None
            };

            text.clear();
            cell.graphemes_utf8(&mut text)?;
            if placeholder {
                // The third diacritic carries the high 8 bits of the inner
                // image ID. Forwarded IDs stay below 2^24, so drop it.
                if let Some((idx, _)) = text.char_indices().nth(3) {
                    text.truncate(idx);
                }
            }
            if text.is_empty() || text.chars().all(|c| c == '\0') {
                text.clear();
                text.push(' ');
            }

            let width = if text.len() == 1 && text.as_bytes()[0].is_ascii_graphic() || text == " " {
                1
            } else {
                chars.clear();
                chars.extend(text.chars());
                let (_, width) = unicode::grapheme_width(&chars);
                width.max(1) as u16
            };

            // A wide glyph that would spill past the pane edge is painted as
            // a blank so the row is still fully cleared.
            if width > area.w.saturating_sub(x) {
                text.clear();
                text.push(' ');
            }
            let width = if text == " " { 1 } else { width };

            // Compare against the cell last written at this position. A dirty
            // row is only a hint: most of its cells are usually unchanged, and
            // rewriting them makes outer Ghostty redraw the whole row.
            let index = usize::from(y) * usize::from(area.w) + usize::from(x);
            let cached = &state.painted[index];
            let changed = full
                || cached.text.as_ref() != text
                || cached.width != width
                || cached.sgr != sgr
                || cached.uri.as_deref() != uri.as_deref();

            if changed {
                if uri.as_deref() != last_uri.as_deref() {
                    set_hyperlink(out, uri.as_deref());
                    last_uri = uri.clone();
                }
                if last_sgr != Some(sgr) {
                    emit_sgr(out, &sgr);
                    last_sgr = Some(sgr);
                }
                move_to(out, area.y + y, area.x + x, &mut cur_col);
                out.extend_from_slice(text.as_bytes());
                cur_col = Some(area.x + x + width);

                let painted = PaintedCell {
                    text: text.clone().into_boxed_str(),
                    width,
                    sgr,
                    uri: uri.map(Into::into),
                };
                state.painted[index] = painted;
                if width > 1 {
                    for tail in 1..usize::from(width) {
                        state.painted[index + tail] = PaintedCell::default();
                    }
                }
            }

            x += width;
        }

        row.set_dirty(false)?;
        y += 1;
    }

    if last_uri.is_some() {
        set_hyperlink(out, None);
    }

    Ok(())
}

fn hyperlink_uri(term: &Terminal<'_, '_>, x: u16, y: u32) -> Result<Option<String>> {
    let grid = term.grid_ref(Point::Viewport(PointCoordinate { x, y }))?;
    let mut buf = [0u8; 512];
    let bytes = match grid.hyperlink_uri(&mut buf) {
        Ok(0) => return Ok(None),
        Ok(n) => buf[..n].to_vec(),
        Err(Error::OutOfSpace { required }) => {
            let mut big = vec![0u8; required];
            let n = grid.hyperlink_uri(&mut big)?;
            big.truncate(n);
            big
        }
        Err(err) => return Err(err.into()),
    };
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

fn set_hyperlink(out: &mut Vec<u8>, uri: Option<&str>) {
    out.extend_from_slice(b"\x1b]8;;");
    if let Some(uri) = uri {
        out.extend_from_slice(uri.as_bytes());
    }
    out.extend_from_slice(b"\x1b\\");
}

/// Resolve a placeholder cell's inner image ID from its style, then translate
/// it to the ID used by the forwarded image data.
fn placeholder_outer_id(graphics: &Graphics, pane_id: u64, style: &Style) -> Option<u32> {
    let inner_low = match style.fg_color {
        StyleColor::Rgb(c) => u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b),
        StyleColor::Palette(index) => u32::from(index.0),
        StyleColor::None => return None,
    };
    graphics.outer_image_id(pane_id, inner_low)
}

/// Encode an image ID as the RGB foreground colour a placeholder cell expects.
fn image_id_color(id: u32) -> RgbColor {
    RgbColor {
        r: (id >> 16) as u8,
        g: (id >> 8) as u8,
        b: id as u8,
    }
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

#[derive(Clone, Copy, PartialEq, Eq, Default)]
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
    out.extend_from_slice(b"\x1b[0");

    if sgr.bold {
        out.extend_from_slice(b";1");
    }
    if sgr.faint {
        out.extend_from_slice(b";2");
    }
    if sgr.italic {
        out.extend_from_slice(b";3");
    }
    match sgr.underline {
        0 => {}
        1 => out.extend_from_slice(b";4"),
        // SGR 4:<style> uses the same numbering as Ghostty's underline enum:
        // 1 single, 2 double, 3 curly, 4 dotted, 5 dashed.
        n => {
            out.extend_from_slice(b";4:");
            push_dec(out, u32::from(n));
        }
    }
    if sgr.blink {
        out.extend_from_slice(b";5");
    }
    if sgr.inverse {
        out.extend_from_slice(b";7");
    }
    if sgr.invisible {
        out.extend_from_slice(b";8");
    }
    if sgr.strikethrough {
        out.extend_from_slice(b";9");
    }
    if sgr.overline {
        out.extend_from_slice(b";53");
    }

    if let Some(c) = sgr.fg {
        out.extend_from_slice(b";38;2;");
        push_color(out, c);
    }
    if let Some(c) = sgr.bg {
        out.extend_from_slice(b";48;2;");
        push_color(out, c);
    }
    if let Some(c) = sgr.ul {
        out.extend_from_slice(b";58;2;");
        push_color(out, c);
    }

    out.push(b'm');
}

fn push_color(out: &mut Vec<u8>, c: RgbColor) {
    push_dec(out, u32::from(c.r));
    out.push(b';');
    push_dec(out, u32::from(c.g));
    out.push(b';');
    push_dec(out, u32::from(c.b));
}

fn push_dec(out: &mut Vec<u8>, mut n: u32) {
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(&buf[i..]);
}

fn move_to(out: &mut Vec<u8>, y: u16, x: u16, cur_col: &mut Option<u16>) {
    if *cur_col == Some(x) {
        return;
    }
    out.extend_from_slice(b"\x1b[");
    push_dec(out, u32::from(y) + 1);
    out.push(b';');
    push_dec(out, u32::from(x) + 1);
    out.push(b'H');
    *cur_col = Some(x);
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::{
        RenderState, Terminal,
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
        let mut term: Terminal<'static, 'static> = Terminal::new(20, 4).unwrap();
        term.resize(20, 4, 8, 16).unwrap();
        term.vt_write(text);
        term
    }

    fn draw_term(
        term: &Terminal<'static, 'static>,
        render_state: &mut RenderState<'static>,
        rows: &mut RowIterator<'static>,
        cells: &mut CellIterator<'static>,
        state: &mut PaintState,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let snapshot = render_state.update(term)?;
        let graphics = Graphics::new()?;
        draw(
            &snapshot,
            term,
            &graphics,
            0,
            rows,
            cells,
            state,
            out,
            area(20, 4),
            false,
        )
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
    fn mixed_graphemes_do_not_leak_between_cells() {
        let term = term_with("A字e\u{301}Z".as_bytes());
        let mut render_state = RenderState::new().unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        let mut out = Vec::new();
        draw_term(
            &term,
            &mut render_state,
            &mut rows,
            &mut cells,
            &mut PaintState::default(),
            &mut out,
        )
        .unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("A字"), "{text:?}");
        assert!(text.contains("e\u{301}Z"), "{text:?}");
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
            &mut PaintState::default(),
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
        let mut state = PaintState::default();

        let mut first = Vec::new();
        draw_term(
            &term,
            &mut render_state,
            &mut rows,
            &mut cells,
            &mut state,
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
            &mut state,
            &mut second,
        )
        .unwrap();
        assert!(second.is_empty(), "dirty rows were repainted: {second:?}");
    }

    #[test]
    fn only_changed_cells_are_painted() {
        let mut term = term_with(b"hello world");
        let mut render_state = RenderState::new().unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        let mut state = PaintState::default();

        let mut first = Vec::new();
        draw_term(
            &term,
            &mut render_state,
            &mut rows,
            &mut cells,
            &mut state,
            &mut first,
        )
        .unwrap();

        // Replace the 'e' with an 'X'; the rest of the row is unchanged.
        term.vt_write(b"\x1b[1;2HX");
        let mut second = Vec::new();
        draw_term(
            &term,
            &mut render_state,
            &mut rows,
            &mut cells,
            &mut state,
            &mut second,
        )
        .unwrap();

        let text = String::from_utf8_lossy(&second);
        assert!(text.contains('X'), "changed cell was not painted: {text:?}");
        assert!(
            !text.contains("ello"),
            "unchanged cells were painted: {text:?}"
        );
        assert!(
            !text.contains("world"),
            "unchanged cells were painted: {text:?}"
        );
    }

    #[test]
    fn clearing_a_line_repaints_spaces() {
        let mut term = term_with(b"hello");
        let mut render_state = RenderState::new().unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        let mut state = PaintState::default();
        let mut first = Vec::new();
        draw_term(
            &term,
            &mut render_state,
            &mut rows,
            &mut cells,
            &mut state,
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
            &mut state,
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

        let mut state = PaintState {
            selection_fg: None,
            selection_bg: Some(RgbColor {
                r: 0x3c,
                g: 0x40,
                b: 0x48,
            }),
            ..PaintState::default()
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
            &mut state,
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
    fn hyperlinks_are_opened_for_runs_and_closed() {
        let term = term_with(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ plain");
        let mut render_state = RenderState::new().unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        let mut out = Vec::new();
        draw_term(
            &term,
            &mut render_state,
            &mut rows,
            &mut cells,
            &mut PaintState::default(),
            &mut out,
        )
        .unwrap();

        let text = String::from_utf8_lossy(&out);
        let open = "\x1b]8;;https://example.com\x1b\\";
        let close = "\x1b]8;;\x1b\\";
        assert!(text.contains(open), "hyperlink was not opened: {text:?}");
        assert!(text.contains(close), "hyperlink was not closed: {text:?}");
        assert_eq!(
            text.matches(open).count(),
            1,
            "link reopened per cell: {text:?}"
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
            &mut PaintState::default(),
            &mut out,
        )
        .unwrap();

        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("\x1b[0;7m"), "inverse missing: {text:?}");
    }

    #[test]
    fn placeholder_cells_use_forwarded_image_id_and_drop_high_diacritic() {
        let mut term: Terminal<'static, 'static> = Terminal::new(10, 3).unwrap();
        term.resize(10, 3, 8, 16).unwrap();
        term.set_kitty_image_storage_limit(1024 * 1024).unwrap();
        term.vt_write(
            b"\x1b_Ga=t,f=24,s=1,v=1,i=7;/wAA\x1b\\\
              \x1b_Ga=p,U=1,i=7,p=3,c=1,r=1\x1b\\",
        );
        // Inner image ID 7 (RGB 0,0,7), placement ID 3 (underline), and a
        // placeholder cluster carrying row, column, and high-ID diacritics.
        term.vt_write(
            "\x1b[38;2;0;0;7m\x1b[58;2;0;0;3m\x1b[1;1H\u{10EEEE}\u{305}\u{305}\u{30D}".as_bytes(),
        );

        let mut graphics = Graphics::new().unwrap();
        let mut out = Vec::new();
        let area = Area {
            x: 0,
            y: 0,
            w: 10,
            h: 3,
        };
        graphics.draw_pane(1, &term, &mut out, area).unwrap();
        assert_eq!(graphics.outer_image_id(1, 7), Some(1));

        let mut render_state = RenderState::new().unwrap();
        let snapshot = render_state.update(&term).unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        out.clear();
        draw(
            &snapshot,
            &term,
            &graphics,
            1,
            &mut rows,
            &mut cells,
            &mut PaintState::default(),
            &mut out,
            area,
            true,
        )
        .unwrap();

        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("38;2;0;0;1"), "{text:?}");
        assert!(text.contains("\u{10EEEE}\u{305}\u{305}"), "{text:?}");
        assert!(
            !text.contains("\u{30D}"),
            "high ID diacritic should be dropped: {text:?}"
        );
    }

    #[test]
    fn wide_glyphs_do_not_paint_their_spacer_tail() {
        let term = term_with("📦 v0.1.0".as_bytes());
        let mut render_state = RenderState::new().unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        let mut out = Vec::new();
        draw_term(
            &term,
            &mut render_state,
            &mut rows,
            &mut cells,
            &mut PaintState::default(),
            &mut out,
        )
        .unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("📦 v0.1.0"),
            "spacer tail was painted: {text:?}"
        );
    }
}
