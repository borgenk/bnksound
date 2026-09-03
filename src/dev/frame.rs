//! Render one frame to a PNG without a compositor.
//!
//! The mixer's look is decided entirely by the shared layout and renderer, so a
//! frame can be produced and inspected without a window, a display, or an audio
//! server. Useful when iterating on the visuals, and it fails loudly if the
//! renderer stops producing a frame at all.
//!
//! ```sh
//! bnksound --render-frame [path] [width] [height] [scale]
//! ```
//!
//! The scale is the one a HiDPI window would paint at, so it doubles as a
//! magnifier for looking at small details.

use crate::dev::{Result, scene};
use crate::render::buffer::PixelBuffer;
use crate::render::image::IconCache;
use crate::render::paint::paint_frame;
use crate::render::png;
use crate::render::primitives::{Painter, Rect};
use crate::render::text::Font;
use crate::ui::UiState;
use crate::ui::layout;
use crate::ui::theme::Palette;
use crate::view::snapshot::build_snapshot;

const CORNER_RADIUS: f32 = 4.0;

/// Fade the alpha outside the rounded rectangle. Coverage comes from the
/// distance to the corner's circle, which keeps the curve smooth.
fn round_corners(pixels: &mut [u32], width: u32, height: u32, radius: f32) {
    if radius < 1.0 {
        return;
    }
    let (w, h) = (width as f32, height as f32);
    let span = radius.ceil() as u32;
    for y in 0..height {
        if y >= span && y < height - span {
            continue;
        }
        for x in 0..width {
            if x >= span && x < width - span {
                continue;
            }
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // How far past the corner circle the pixel sits.
            let dx = (radius - px).max(px - (w - radius)).max(0.0);
            let dy = (radius - py).max(py - (h - radius)).max(0.0);
            let cover = (0.5 - (dx.hypot(dy) - radius)).clamp(0.0, 1.0);
            let Some(slot) = pixels.get_mut((y * width + x) as usize) else {
                continue;
            };
            let alpha = ((*slot >> 24) as f32 * cover).round() as u32;
            *slot = (alpha << 24) | (*slot & 0x00ff_ffff);
        }
    }
}

/// Paint one frame of the showcase mixer and write it out as a PNG.
pub fn run(args: &[String]) -> Result<()> {
    let mut rest = args.iter().skip_while(|a| *a != "--render-frame").skip(1);
    let path = rest
        .next()
        .cloned()
        .unwrap_or_else(|| "frame.png".to_string());
    let width: i32 = rest.next().and_then(|s| s.parse().ok()).unwrap_or(560);
    let height: i32 = rest.next().and_then(|s| s.parse().ok()).unwrap_or(720);
    let scale: f32 = rest.next().and_then(|s| s.parse().ok()).unwrap_or(1.0);

    let font = Font::load()?;
    let app = scene::showcase();
    let snapshot = build_snapshot(&app, |_| None);
    // Meter levels are session state, not stream state, so the fixture's peaks
    // are folded in here.
    let mut ui = UiState::new();
    for (node, peaks) in scene::SHOWCASE_PEAKS {
        for row in snapshot.meter_routes.get(&node).into_iter().flatten() {
            ui.meters.apply(row, &peaks);
        }
    }
    let layout = layout::project(&snapshot, &ui, Rect::new(0, 0, width, height));

    // The window is measured in logical pixels and the buffer in device ones,
    // which is the only thing the scale changes.
    let (dev_w, dev_h) = (
        (width as f32 * scale).round() as u32,
        (height as f32 * scale).round() as u32,
    );
    let mut buffer = PixelBuffer::new(dev_w, dev_h);
    {
        let (pixels, w, h) = buffer.parts();
        let mut painter = Painter::scaled(pixels, w, h, scale);
        paint_frame(
            &mut painter,
            &snapshot,
            &ui,
            &layout,
            &font,
            &Palette::dark(),
            &mut IconCache::new(),
        );
    }

    {
        let (pixels, w, h) = buffer.parts();
        round_corners(pixels, w, h, CORNER_RADIUS * scale);
    }
    std::fs::write(&path, png::encode_rgba(buffer.pixels(), dev_w, dev_h))?;
    println!("wrote {path} ({dev_w}x{dev_h} at scale {scale})");
    Ok(())
}
