//! Kitty graphics forwarding.
//!
//! Each pane parses Kitty graphics with its own [`libghostty_vt::Terminal`].
//! This module re-emits the resulting images and placements to the outer
//! Ghostty, translating pane-local coordinates into outer-terminal cells.
//!
//! Image data is transmitted once per image generation and then kept in
//! Ghostty's image storage. Placements are deleted and re-created whenever a
//! pane's visible placement geometry changes (scroll, resize, reveal), but not
//! on every frame: cell repaints alone leave placements untouched.

use std::collections::HashMap;

use anyhow::Result;
use libghostty_vt::{
    Terminal,
    kitty::graphics::{Image, ImageFormat, PlacementIterator, PlacementRenderInfo},
};

use crate::{base64, paint::Area};

/// Base64 payload bytes per Kitty graphics APC chunk. The protocol recommends
/// chunks of at most 4096 base64 characters; 3072 raw bytes encode to exactly
/// 4096 characters with no padding.
const CHUNK_BYTES: usize = 3072;

/// Kitty image IDs are 32-bit, but Unicode placeholders encode the low 24 bits
/// in a cell's foreground colour and the high 8 bits in an optional diacritic.
/// Keeping forwarded IDs below 2^24 lets `paint.rs` rewrite placeholder cells
/// without also having to synthesize the high-bit diacritic.
const MAX_IMAGE_ID: u32 = 0x00ff_ffff;

pub struct Graphics {
    next_id: u32,
    panes: HashMap<u64, PaneGraphics>,
    pending_image_deletes: Vec<u32>,
}

#[derive(Default)]
struct PaneGraphics {
    images: HashMap<u32, ImageState>,
    placements: Vec<PlacementSig>,
}

#[derive(Clone, Copy)]
struct ImageState {
    outer_id: u32,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PlacementSig {
    image: u32,
    placement_id: u32,
    is_virtual: bool,
    origin_x: u16,
    origin_y: u16,
    left: i32,
    top: i32,
    cols: u32,
    rows: u32,
    x_offset: u32,
    y_offset: u32,
    source_x: u32,
    source_y: u32,
    source_width: u32,
    source_height: u32,
    z: i32,
}

impl Graphics {
    pub fn new() -> Result<Self> {
        Ok(Self {
            next_id: 1,
            panes: HashMap::new(),
            pending_image_deletes: Vec::new(),
        })
    }

    /// Drop a pane's graphics state and queue deletion of its image data.
    ///
    /// A live pane can be dropped this way too, not only a closed one: Ghostty
    /// discards the image data it decoded for a surface when that surface is
    /// resized, while the pane's own storage keeps the image and its placement,
    /// so forgetting what was forwarded is what makes the next frame re-send it
    /// instead of believing the old images are still in place.
    pub fn remove_pane(&mut self, pane_id: u64) {
        if let Some(pane) = self.panes.remove(&pane_id) {
            self.pending_image_deletes
                .extend(pane.images.values().map(|image| image.outer_id));
        }
    }

    /// Start a frame: delete image data for removed panes and placements for
    /// panes that are no longer visible.
    pub fn begin_frame(&mut self, visible: &[u64], out: &mut Vec<u8>) {
        for id in self.pending_image_deletes.drain(..) {
            delete(out, 'I', id);
        }

        for (pane_id, pane) in &mut self.panes {
            if visible.contains(pane_id) || pane.placements.is_empty() {
                continue;
            }
            for image in pane.images.values() {
                delete(out, 'i', image.outer_id);
            }
            pane.placements.clear();
        }
    }

    /// Translate the low 24 bits of an inner image ID to the forwarded ID.
    ///
    /// Kitty Unicode placeholders encode the image ID in the cell's
    /// foreground colour. The painter uses this to rewrite placeholder cells
    /// so they refer to the namespaced image data that was sent to Ghostty.
    pub fn outer_image_id(&self, pane_id: u64, inner_low: u32) -> Option<u32> {
        self.panes
            .get(&pane_id)?
            .images
            .iter()
            .find(|(inner_id, _)| **inner_id & MAX_IMAGE_ID == inner_low & MAX_IMAGE_ID)
            .map(|(_, image)| image.outer_id)
    }

    /// Emit any new image data and the current placements for one pane.
    ///
    /// Returns true when the caller must repaint the pane's cells even if
    /// they are not marked dirty. Placeholder cells can be painted before the
    /// image they reference has finished arriving; once the image mapping or
    /// a virtual placement appears, those cells need their foreground IDs
    /// rewritten.
    pub fn draw_pane(
        &mut self,
        pane_id: u64,
        term: &Terminal<'_, '_>,
        out: &mut Vec<u8>,
        area: Area,
    ) -> Result<bool> {
        if area.w == 0 || area.h == 0 {
            return Ok(false);
        }

        let Some(storage) = term.kitty_graphics()? else {
            return Ok(false);
        };
        // A zero generation means the storage has never been mutated, so
        // there is nothing to forward yet.
        if storage.generation()? == 0 {
            return Ok(false);
        }

        let mut iterator = PlacementIterator::new()?;
        let mut placements = iterator.update(&storage)?;
        let mut seen = Vec::<u32>::new();
        let mut placements_now = Vec::new();
        let mut placement_cmds = Vec::new();
        let mut image_changed = false;
        let mut repaint = false;

        while placements.next().is_some() {
            let inner_id = placements.image_id()?;
            let placement_id = placements.placement_id()?;
            let Some(image) = storage.image(inner_id) else {
                continue;
            };
            // v1 can expose an image while its pixels are still loading.
            if image.data()?.is_none() {
                continue;
            }

            if !seen.contains(&inner_id) {
                seen.push(inner_id);
            }
            let (outer_id, changed, new_image) =
                self.ensure_image(pane_id, inner_id, &image, out)?;
            image_changed |= changed;
            repaint |= new_image;

            // Virtual (Unicode placeholder) placements are not anchored to a
            // cell, but Ghostty still needs them in its storage for the
            // placeholder cells to resolve. Forward them once; the cells
            // themselves are painted by `paint.rs`.
            if placements.is_virtual()? {
                let (sig, cmd) = virtual_placement(&placements, area, outer_id, placement_id)?;
                placements_now.push(sig);
                placement_cmds.push(cmd);
                continue;
            }

            let info = placements.placement_render_info(&image, term)?;
            if !info.viewport_visible {
                continue;
            }

            let Some((sig, cmd)) =
                placement_geometry(&placements, &info, area, outer_id, placement_id)?
            else {
                continue;
            };
            placements_now.push(sig);
            placement_cmds.push(cmd);
        }

        let pane = self.panes.entry(pane_id).or_default();
        let old_virtual: Vec<PlacementSig> = pane
            .placements
            .iter()
            .copied()
            .filter(|placement| placement.is_virtual)
            .collect();

        // Forget images the pane's application deleted. `I` deletes their
        // placements too, and releases the image data when unused.
        let stale: Vec<u32> = pane
            .images
            .keys()
            .copied()
            .filter(|id| !seen.contains(id))
            .collect();
        for inner_id in stale {
            if let Some(image) = pane.images.remove(&inner_id) {
                delete(out, 'I', image.outer_id);
            }
        }

        if pane.placements == placements_now && !image_changed {
            return Ok(repaint);
        }

        // Replacing image data can invalidate its existing placements. Delete
        // every placement for the pane before re-emitting the current set.
        if !pane.placements.is_empty() {
            for image in pane.images.values() {
                delete(out, 'i', image.outer_id);
            }
        }
        for cmd in placement_cmds {
            out.extend_from_slice(&cmd);
        }
        let new_virtual: Vec<PlacementSig> = placements_now
            .iter()
            .copied()
            .filter(|placement| placement.is_virtual)
            .collect();
        repaint |= old_virtual != new_virtual;
        pane.placements = placements_now;
        Ok(repaint)
    }

    fn ensure_image(
        &mut self,
        pane_id: u64,
        inner_id: u32,
        image: &Image<'_>,
        out: &mut Vec<u8>,
    ) -> Result<(u32, bool, bool)> {
        let generation = image.generation()?;
        let existing = self
            .panes
            .get(&pane_id)
            .and_then(|pane| pane.images.get(&inner_id))
            .copied();

        let (outer_id, changed, new_image) = match existing {
            Some(state) if state.generation == generation => {
                return Ok((state.outer_id, false, false));
            }
            Some(state) => (state.outer_id, true, false),
            None => {
                let id = self.next_id;
                self.next_id = (self.next_id + 1) & MAX_IMAGE_ID;
                if self.next_id == 0 {
                    self.next_id = 1;
                }
                (id, true, true)
            }
        };

        transmit(out, outer_id, image)?;
        self.panes.entry(pane_id).or_default().images.insert(
            inner_id,
            ImageState {
                outer_id,
                generation,
            },
        );
        Ok((outer_id, changed, new_image))
    }
}

/// Resolve a placement into an outer-terminal cell plus a placement command.
fn placement_geometry(
    placement: &libghostty_vt::kitty::graphics::PlacementIteration<'_, '_>,
    info: &PlacementRenderInfo,
    area: Area,
    outer_id: u32,
    placement_id: u32,
) -> Result<Option<(PlacementSig, Vec<u8>)>> {
    let cols = info.grid_cols.max(1) as i32;
    let rows = info.grid_rows.max(1) as i32;

    let left = info.viewport_col.max(0);
    let top = info.viewport_row.max(0);
    let right = (info.viewport_col + cols).min(area.w as i32);
    let bottom = (info.viewport_row + rows).min(area.h as i32);
    if right <= left || bottom <= top {
        return Ok(None);
    }

    let crop_left = (left - info.viewport_col) as u32;
    let crop_top = (top - info.viewport_row) as u32;
    let crop_right = (info.viewport_col + cols - right) as u32;
    let crop_bottom = (info.viewport_row + rows - bottom) as u32;
    let clipped = crop_left > 0 || crop_top > 0 || crop_right > 0 || crop_bottom > 0;
    let visible_cols = (right - left) as u32;
    let visible_rows = (bottom - top) as u32;

    let (source_x, source_y, source_width, source_height) = if clipped {
        let sw = u64::from(info.source_width);
        let sh = u64::from(info.source_height);
        let cols = u64::from(cols as u32);
        let rows = u64::from(rows as u32);
        (
            info.source_x + (sw * u64::from(crop_left) / cols) as u32,
            info.source_y + (sh * u64::from(crop_top) / rows) as u32,
            (sw * u64::from(visible_cols) / cols).max(1) as u32,
            (sh * u64::from(visible_rows) / rows).max(1) as u32,
        )
    } else {
        (
            info.source_x,
            info.source_y,
            info.source_width,
            info.source_height,
        )
    };

    let (x_offset, y_offset) = if clipped {
        (0, 0)
    } else {
        (placement.x_offset()?, placement.y_offset()?)
    };
    let (dest_cols, dest_rows) = if clipped {
        (visible_cols, visible_rows)
    } else {
        // Preserve the original cell spec. Zero means "native image size";
        // substituting the resolved grid size would rescale the image to
        // whole cells and change its aspect ratio slightly.
        (placement.columns()?, placement.rows()?)
    };
    let z = placement.z()?;

    let sig = PlacementSig {
        image: outer_id,
        placement_id,
        is_virtual: false,
        origin_x: area.x,
        origin_y: area.y,
        left,
        top,
        cols: dest_cols,
        rows: dest_rows,
        x_offset,
        y_offset,
        source_x,
        source_y,
        source_width,
        source_height,
        z,
    };

    let mut cmd = Vec::new();
    cmd.extend_from_slice(
        format!(
            "\x1b[{};{}H\x1b_Ga=p,i={outer_id},p={placement_id},c={dest_cols},r={dest_rows},\
             x={x_offset},y={y_offset},X={source_x},Y={source_y},\
             w={source_width},h={source_height},z={z},C=1,q=2\x1b\\",
            area.y as i32 + top + 1,
            area.x as i32 + left + 1,
        )
        .as_bytes(),
    );
    Ok(Some((sig, cmd)))
}

/// Describe and forward one virtual (Unicode placeholder) placement.
///
/// Virtual placements have no screen pin, so no cursor movement is needed.
/// The placement ID matters: placeholder cells carry it in their underline
/// colour, and Ghostty resolves `(image ID, placement ID)` against its stored
/// virtual placements.
fn virtual_placement(
    placement: &libghostty_vt::kitty::graphics::PlacementIteration<'_, '_>,
    area: Area,
    outer_id: u32,
    placement_id: u32,
) -> Result<(PlacementSig, Vec<u8>)> {
    let cols = placement.columns()?;
    let rows = placement.rows()?;
    let x_offset = placement.x_offset()?;
    let y_offset = placement.y_offset()?;
    let source_x = placement.source_x()?;
    let source_y = placement.source_y()?;
    let source_width = placement.source_width()?;
    let source_height = placement.source_height()?;
    let z = placement.z()?;

    let sig = PlacementSig {
        image: outer_id,
        placement_id,
        is_virtual: true,
        origin_x: area.x,
        origin_y: area.y,
        left: 0,
        top: 0,
        cols,
        rows,
        x_offset,
        y_offset,
        source_x,
        source_y,
        source_width,
        source_height,
        z,
    };

    let cmd = format!(
        "\x1b_Ga=p,U=1,i={outer_id},p={placement_id},c={cols},r={rows},\
         x={x_offset},y={y_offset},X={source_x},Y={source_y},\
         w={source_width},h={source_height},z={z},C=1,q=2\x1b\\"
    )
    .into_bytes();
    Ok((sig, cmd))
}

/// Transmit one image as raw RGBA, replacing any existing outer image ID.
fn transmit(out: &mut Vec<u8>, id: u32, image: &Image<'_>) -> Result<()> {
    let (width, height) = (image.width()?, image.height()?);
    if width == 0 || height == 0 {
        return Ok(());
    }
    let rgba = rgba(image)?;
    if rgba.is_empty() {
        return Ok(());
    }

    let chunks = rgba.chunks(CHUNK_BYTES);
    let total = chunks.len();
    for (index, chunk) in chunks.enumerate() {
        let more = usize::from(index + 1 < total);
        if index == 0 {
            out.extend_from_slice(
                format!("\x1b_Ga=t,f=32,s={width},v={height},i={id},q=2,m={more};").as_bytes(),
            );
        } else {
            out.extend_from_slice(format!("\x1b_Gm={more};").as_bytes());
        }
        base64::encode_into(chunk, out);
        out.extend_from_slice(b"\x1b\\");
    }
    Ok(())
}

fn rgba(image: &Image<'_>) -> Result<Vec<u8>> {
    let Some(data) = image.data()? else {
        return Ok(Vec::new());
    };
    let pixels = u64::from(image.width()?) * u64::from(image.height()?);
    let mut out = Vec::with_capacity((pixels * 4) as usize);

    match image.format()? {
        ImageFormat::Rgba => out.extend_from_slice(data),
        ImageFormat::Rgb => {
            let (pixels, _) = data.as_chunks::<3>();
            for pixel in pixels {
                out.extend_from_slice(pixel);
                out.push(0xff);
            }
        }
        ImageFormat::Gray => {
            for &gray in data {
                out.extend_from_slice(&[gray, gray, gray, 0xff]);
            }
        }
        ImageFormat::GrayAlpha => {
            let (pixels, _) = data.as_chunks::<2>();
            for pixel in pixels {
                out.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
        }
        // libghostty decodes PNG to RGBA before storing, so this should not
        // happen. Drop unknown formats rather than sending invalid data.
        _ => return Ok(Vec::new()),
    }

    // libghostty guarantees the data length matches the dimensions; keep a
    // defensive truncate so a malformed image cannot corrupt the outer store.
    out.truncate((pixels * 4) as usize);
    Ok(out)
}

/// Delete placements (`i`, keeps data) or placements and data (`I`).
fn delete(out: &mut Vec<u8>, action: char, image_id: u32) {
    out.extend_from_slice(format!("\x1b_Ga=d,d={action},i={image_id},q=2\x1b\\").as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::terminal::Terminal;

    fn terminal() -> Terminal<'static, 'static> {
        let mut term = Terminal::new(20, 10).unwrap();
        term.resize(20, 10, 8, 16).unwrap();
        term.set_kitty_image_storage_limit(1024 * 1024).unwrap();
        term
    }

    fn area(x: u16, y: u16, w: u16, h: u16) -> Area {
        Area { x, y, w, h }
    }

    #[test]
    fn transmits_once_and_places_at_pane_offset() {
        let mut term = terminal();
        // One red pixel, transmitted then placed at the cursor (0,0).
        term.vt_write(b"\x1b_Ga=t,f=24,s=1,v=1,i=7;/wAA\x1b\\\x1b_Ga=p,i=7\x1b\\");

        let mut graphics = Graphics::new().unwrap();
        let mut out = Vec::new();
        assert!(
            graphics
                .draw_pane(1, &term, &mut out, area(10, 2, 10, 8))
                .unwrap(),
            "a new image mapping must request a cell repaint"
        );
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("a=t,f=32,s=1,v=1,i=1"), "{text:?}");
        assert!(text.contains("a=p,i=1"), "{text:?}");
        assert!(!text.contains("a=d"), "unexpected delete: {text:?}");
        assert!(text.contains("\x1b[3;11H"), "{text:?}");

        // No content or geometry change: don't re-transmit or re-place.
        let mut again = Vec::new();
        assert!(
            !graphics
                .draw_pane(1, &term, &mut again, area(10, 2, 10, 8))
                .unwrap(),
            "an unchanged image must not force a repaint"
        );
        assert!(again.is_empty(), "{:?}", String::from_utf8_lossy(&again));
    }

    #[test]
    fn moving_the_pane_origin_replaces_placements() {
        let mut term = terminal();
        term.vt_write(b"\x1b_Ga=t,f=24,s=1,v=1,i=7;/wAA\x1b\\\x1b_Ga=p,i=7\x1b\\");

        let mut graphics = Graphics::new().unwrap();
        let mut out = Vec::new();
        graphics
            .draw_pane(1, &term, &mut out, area(0, 0, 10, 8))
            .unwrap();
        out.clear();
        graphics
            .draw_pane(1, &term, &mut out, area(10, 2, 10, 8))
            .unwrap();

        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("a=d,d=i,i=1"), "{text:?}");
        assert!(text.contains("\x1b[3;11H"), "{text:?}");
    }

    #[test]
    fn scrolling_moves_placements() {
        let mut term = terminal();
        // Put a 1x1 image near the bottom, then scroll it up three rows.
        term.vt_write(b"\x1b[9;1H\x1b_Ga=t,f=24,s=1,v=1,i=7;/wAA\x1b\\\x1b_Ga=p,i=7\x1b\\");
        term.vt_write(b"\r\n\r\n\r\n");

        let mut graphics = Graphics::new().unwrap();
        let mut out = Vec::new();
        graphics
            .draw_pane(1, &term, &mut out, area(0, 0, 20, 10))
            .unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("\x1b[7;1H"), "{text:?}");
    }

    #[test]
    fn hiding_a_pane_deletes_its_placements() {
        let mut term = terminal();
        term.vt_write(b"\x1b_Ga=t,f=24,s=1,v=1,i=7;/wAA\x1b\\\x1b_Ga=p,i=7\x1b\\");

        let mut graphics = Graphics::new().unwrap();
        let mut out = Vec::new();
        graphics
            .draw_pane(1, &term, &mut out, area(0, 0, 20, 10))
            .unwrap();
        out.clear();

        graphics.begin_frame(&[], &mut out);
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("a=d,d=i,i=1"), "{text:?}");
    }

    #[test]
    fn removing_a_pane_deletes_its_images() {
        let mut term = terminal();
        term.vt_write(b"\x1b_Ga=t,f=24,s=1,v=1,i=7;/wAA\x1b\\\x1b_Ga=p,i=7\x1b\\");

        let mut graphics = Graphics::new().unwrap();
        let mut out = Vec::new();
        graphics
            .draw_pane(1, &term, &mut out, area(0, 0, 20, 10))
            .unwrap();
        graphics.remove_pane(1);
        out.clear();
        graphics.begin_frame(&[], &mut out);
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("a=d,d=I,i=1"), "{text:?}");
    }

    #[test]
    fn replacing_image_data_drops_old_placements() {
        let mut term = terminal();
        term.vt_write(b"\x1b_Ga=t,f=24,s=1,v=1,i=7;/wAA\x1b\\\x1b_Ga=p,i=7\x1b\\");

        let mut graphics = Graphics::new().unwrap();
        let mut out = Vec::new();
        graphics
            .draw_pane(1, &term, &mut out, area(0, 0, 20, 10))
            .unwrap();
        out.clear();

        // v1 drops the old placement when an image is retransmitted without
        // a new placement, so the forwarded image must be deleted too.
        term.vt_write(b"\x1b_Ga=t,f=24,s=1,v=1,i=7;AP8A\x1b\\");
        graphics
            .draw_pane(1, &term, &mut out, area(0, 0, 20, 10))
            .unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("a=d,d=I,i=1"), "{text:?}");

        // A subsequent placement forwards the replacement pixels again.
        out.clear();
        term.vt_write(b"\x1b_Ga=p,i=7\x1b\\");
        graphics
            .draw_pane(1, &term, &mut out, area(0, 0, 20, 10))
            .unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("a=t,f=32,s=1,v=1,i=2"), "{text:?}");
        assert!(text.contains("AP8A/w=="), "{text:?}");
        assert!(text.contains("a=p,i=2"), "{text:?}");
    }

    #[test]
    fn clipping_crops_a_placement_at_the_pane_edge() {
        let mut term = Terminal::new(20, 10).unwrap();
        term.resize(20, 10, 1, 1).unwrap();
        term.set_kitty_image_storage_limit(1024 * 1024).unwrap();

        // A 2x2 red image placed at the right edge; one column spills past
        // the pane's 20-column area.
        let mut seq = b"\x1b[10;20H\x1b_Ga=t,f=24,s=2,v=2,i=7;".to_vec();
        base64::encode_into(&[255; 12], &mut seq);
        seq.extend_from_slice(b"\x1b\\\x1b_Ga=p,i=7\x1b\\");
        term.vt_write(&seq);

        let mut graphics = Graphics::new().unwrap();
        let mut out = Vec::new();
        graphics
            .draw_pane(1, &term, &mut out, area(0, 0, 20, 10))
            .unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("a=p,i=1"), "{text:?}");
        assert!(text.contains("c=1,"), "not clipped: {text:?}");
        assert!(text.contains("w=1,"), "source not cropped: {text:?}");
    }

    #[test]
    fn png_decoder_is_installed() {
        // The global hook must be installed before any concurrent libghostty
        // calls. Run this test in a separate, single-test process.
        const CHILD: &str = "HAUNT_PNG_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "graphics::tests::png_decoder_is_installed"])
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
        let mut term = terminal();
        // 1x1 PNG from the libghostty-vt documentation.
        term.vt_write(
            b"\x1b_Ga=T,f=100,i=9;\
              iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAA\
              DUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==\
              \x1b\\",
        );
        let storage = term.kitty_graphics().unwrap().unwrap();
        let image = storage.image(9).expect("PNG should be decoded");
        assert_eq!(image.format().unwrap(), ImageFormat::Rgba);
        assert_eq!((image.width().unwrap(), image.height().unwrap()), (1, 1));
    }

    #[test]
    fn rgb_and_gray_convert_to_rgba() {
        let mut term = terminal();
        term.vt_write(b"\x1b_Ga=t,f=24,s=1,v=2,i=1;/wAA/wAA\x1b\\");
        let storage = term.kitty_graphics().unwrap().unwrap();
        let image = storage.image(1).unwrap();
        assert_eq!(rgba(&image).unwrap(), [255, 0, 0, 255, 255, 0, 0, 255]);
    }

    #[test]
    fn forwards_virtual_placements_and_namespaces_placeholder_ids() {
        let mut term = terminal();
        // Image data plus a Unicode-placeholder (virtual) placement. Snacks
        // sends this shape and encodes the inner image ID in the cells.
        term.vt_write(
            b"\x1b_Ga=t,f=24,s=1,v=1,i=7;/wAA\x1b\\\
              \x1b_Ga=p,U=1,i=7,p=3,c=2,r=2\x1b\\",
        );

        let mut graphics = Graphics::new().unwrap();
        let mut out = Vec::new();
        graphics
            .draw_pane(1, &term, &mut out, area(0, 0, 20, 10))
            .unwrap();

        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("a=t,f=32,s=1,v=1,i=1"), "{text:?}");
        assert!(text.contains("a=p,U=1,i=1,p=3,c=2,r=2"), "{text:?}");
        assert_eq!(graphics.outer_image_id(1, 7), Some(1));

        // The virtual placement is tracked, so hiding the pane removes it and
        // revealing the pane re-emits it.
        out.clear();
        graphics.begin_frame(&[], &mut out);
        assert!(
            String::from_utf8_lossy(&out).contains("a=d,d=i,i=1"),
            "{:?}",
            String::from_utf8_lossy(&out)
        );

        out.clear();
        graphics
            .draw_pane(1, &term, &mut out, area(0, 0, 20, 10))
            .unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("a=p,U=1,i=1,p=3,c=2,r=2"), "{text:?}");
        assert!(
            !text.contains("a=t,f=32"),
            "image data should be cached: {text:?}"
        );
    }
}
