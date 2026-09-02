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
    let ui = UiState::new();
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

    std::fs::write(&path, png::encode_rgb(buffer.pixels(), dev_w, dev_h))?;
    println!("wrote {path} ({dev_w}x{dev_h} at scale {scale})");
    Ok(())
}
