//! Golden-frame regression: render a fixed scene and compare it to a committed
//! PNG, failing when the frame moves.
//!
//! This is what catches a layout change nobody meant to make. The scene, the
//! size, and the font are all pinned, and the font is loaded sealed, so a
//! character it does not cover draws .notdef instead of whatever the machine
//! has installed. The fixture font is ASCII only, so the arrow in a palette
//! label is one of those. A frame therefore depends on committed bytes alone.
//!
//! When a change to the rendering is intended, regenerate with:
//!
//! ```sh
//! UPDATE_GOLDEN=1 cargo test --test render_golden
//! ```
//!
//! and look at the new image before committing it. On failure the actual frame
//! and a diff heatmap are written next to the golden under `target/`.

use std::path::{Path, PathBuf};

use bnksound::domain::{DeviceForm, SinkForm, SourceForm, Stream, StreamKind, cubic_to_linear};
use bnksound::render::buffer::PixelBuffer;
use bnksound::render::image::IconCache;
use bnksound::render::paint::paint_frame;
use bnksound::render::png;
use bnksound::render::primitives::{Painter, Rect};
use bnksound::render::text::Font;
use bnksound::state;
use bnksound::ui::layout;
use bnksound::ui::theme::Palette;
use bnksound::ui::{Focus, UiState};
use bnksound::view::snapshot::build_snapshot;

/// Big enough for the chrome and three columns, small enough that the golden
/// stays a reasonable thing to keep in the repository.
const WIDTH: u32 = 380;
const HEIGHT: u32 = 300;

/// Per-channel difference at which two pixels count as disagreeing.
const TOLERANCE: u32 = 16;

/// Pixels may differ up to this fraction before the frame counts as changed.
/// Not zero, because a rounding change in one anti-aliased edge is noise; well
/// below anything that moves an element.
const MAX_DIFFERING: f64 = 0.20;

#[test]
fn the_mixer_frame_matches_its_golden() {
    check_golden("golden-frame.png", render(&mixer_scene(), Overlay::None));
}

/// The overlays are laid out separately from the body, so they get their own
/// golden rather than riding on the mixer's.
#[test]
fn the_command_palette_frame_matches_its_golden() {
    check_golden(
        "golden-palette.png",
        render(&palette_scene(), Overlay::Palette),
    );
}

#[test]
fn the_profile_menu_frame_matches_its_golden() {
    check_golden(
        "golden-profile-menu.png",
        render(&profile_scene(), Overlay::ProfileMenu),
    );
}

/// Compare a frame to its golden, or write the golden when asked to.
fn check_golden(name: &str, got: PixelBuffer) {
    let path = fixture(name);
    let encoded = png::encode_rgb(got.pixels(), got.width(), got.height());

    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &encoded).expect("write golden");
        eprintln!("updated {}", path.display());
        return;
    }

    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}\nrun `UPDATE_GOLDEN=1 cargo test --test render_golden` to create it",
            path.display()
        )
    });
    let want = png::decode(&bytes).expect("decode golden");
    assert_eq!(
        (want.width, want.height),
        (got.width(), got.height()),
        "golden {name} is a different size"
    );

    let differing = got
        .pixels()
        .iter()
        .zip(&want.pixels)
        .filter(|(a, b)| delta(**a, **b) > TOLERANCE)
        .count();
    let pct = differing as f64 * 100.0 / want.pixels.len() as f64;
    if pct <= MAX_DIFFERING {
        return;
    }

    // Leave the evidence somewhere the developer can look at it.
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    let _ = std::fs::create_dir_all(&out);
    let actual = out.join(name);
    let _ = std::fs::write(&actual, &encoded);
    let heat: Vec<u32> = got
        .pixels()
        .iter()
        .zip(&want.pixels)
        .map(|(a, b)| {
            if delta(*a, *b) > TOLERANCE {
                0xffff_0000
            } else {
                let dim = |s: u32| ((*b >> s) & 0xff) / 3;
                0xff00_0000 | (dim(16) << 16) | (dim(8) << 8) | dim(0)
            }
        })
        .collect();
    let diff = out.join(format!("diff-{name}"));
    let _ = std::fs::write(&diff, png::encode_rgb(&heat, want.width, want.height));

    panic!(
        "{name}: {pct:.2}% of pixels differ (limit {MAX_DIFFERING}%)\n  \
         actual: {}\n  diff:   {}\n  \
         if the change is intended: UPDATE_GOLDEN=1 cargo test --test render_golden",
        actual.display(),
        diff.display()
    );
}

fn delta(a: u32, b: u32) -> u32 {
    let ch = |v: u32, s: u32| (v >> s) & 0xff;
    let d = |s: u32| ch(a, s).abs_diff(ch(b, s));
    d(16).max(d(8)).max(d(0))
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Which overlay a scene is rendered with, since each is laid out separately
/// from the body and needs its own golden.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Overlay {
    None,
    Palette,
    ProfileMenu,
}

/// Render a scene through the same path both shells use.
fn render(app: &state::App, overlay: Overlay) -> PixelBuffer {
    let font = Font::from_path_sealed(&fixture("test-font.ttf")).expect("fixture font");
    let snapshot = build_snapshot(app, |_| None);
    let mut ui = UiState::new();
    if overlay == Overlay::Palette {
        ui.focus = Focus::Palette;
        // Pinned so the caret is drawn every run, not on a blink phase.
        ui.caret_visible = true;
    }
    ui.profile_menu_open = overlay == Overlay::ProfileMenu;
    let layout = layout::project(&snapshot, &ui, Rect::new(0, 0, WIDTH as i32, HEIGHT as i32));

    let mut buffer = PixelBuffer::new(WIDTH, HEIGHT);
    {
        let (pixels, w, h) = buffer.parts();
        let mut painter = Painter::new(pixels, w, h);
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
    buffer
}

/// A default sink, a source, and a muted application row: one of each kind of
/// column, with the states that change how a column draws.
fn mixer_scene() -> state::App {
    let mut app = state::empty();
    for (id, kind, name, form, cubic, is_default) in [
        (
            1,
            StreamKind::Sink,
            "BNK Headset",
            Some(DeviceForm::Output(SinkForm::Headset)),
            0.33_f32,
            true,
        ),
        (
            2,
            StreamKind::Source,
            "Yeti Nano",
            Some(DeviceForm::Input(SourceForm::Microphone)),
            0.70,
            false,
        ),
    ] {
        app.streams.insert(
            id,
            Stream {
                id,
                kind,
                name: name.to_string(),
                app_id: None,
                binary: None,
                pid: None,
                node_name: Some(format!("node.{id}")),
                media_name: None,
                media_role: None,
                channel_volumes: vec![cubic_to_linear(cubic); 2],
                muted: false,
                xdg: None,
                form,
                is_default,
                target_sink_name: None,
            },
        );
    }
    app.streams.insert(
        10,
        Stream {
            id: 10,
            kind: StreamKind::Application,
            name: "Player".to_string(),
            app_id: Some("com.example.Player".to_string()),
            binary: None,
            pid: None,
            node_name: Some("node.10".to_string()),
            media_name: None,
            media_role: None,
            channel_volumes: vec![cubic_to_linear(0.84); 2],
            muted: true,
            xdg: None,
            form: None,
            is_default: false,
            target_sink_name: None,
        },
    );
    app
}

/// The same mixer with the command palette open over it. An unfiltered list
/// runs longer than the panel can show, so the frame carries the row window,
/// the two-tone labels, and the scrollbar beside them.
fn palette_scene() -> state::App {
    let mut app = mixer_scene();
    app.palette = Some(state::CommandPalette::default());
    app
}

/// The same mixer with the profile dropdown open: more than one profile, one
/// of them active, so the rule and the accented row are both in frame.
fn profile_scene() -> state::App {
    let mut app = mixer_scene();
    app.profiles.profiles = ["Default", "Gaming", "Work"]
        .into_iter()
        .map(|name| bnksound::profile::Profile {
            name: name.to_string(),
            ..Default::default()
        })
        .collect();
    app.profiles.active = Some("Gaming".to_string());
    app
}
