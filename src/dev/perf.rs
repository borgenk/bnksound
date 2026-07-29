//! Timing and allocation gate for the hot paths, with no compositor and no
//! audio server. Run with `bnksound --perf`, which `make perf` does.
//!
//! Everything expensive the mixer does per frame is a plain function over shared
//! state: build a snapshot, project a layout, paint a buffer. So the cost can be
//! measured exactly where it is paid, without a window.
//!
//! ```sh
//! make perf         # measure and compare against perf/baseline.txt
//! make perf-save    # accept the current numbers as the new baseline
//! ```
//!
//! Each scenario reports nanoseconds per operation and Rust allocations per
//! operation. The allocation count is the signal worth trusting: it is exactly
//! reproducible for a given code path, where a stopwatch moves with the machine,
//! the governor, and whatever else is running. Times are best-of-five to take
//! the least disturbed run rather than an average of interruptions.
//!
//! So the gate treats them differently. Any rise in allocations fails the run.
//! A timing fails only above [`TIME_GATE_FLOOR_NS`], because the scenarios
//! measured in nanoseconds swing past the ratio between two runs of the same
//! binary, and a gate that cries wolf is one nobody reads.

use std::collections::HashMap;
use std::hint::black_box;
use std::io;
use std::time::Instant;

use crate::dev::{Result, alloc, scene};
use crate::render::buffer::PixelBuffer;
use crate::render::image::IconCache;
use crate::render::paint::{paint_frame, paint_meters};
use crate::render::primitives::{Painter, Rect};
use crate::render::text::Font;
use crate::ui::UiState;
use crate::ui::layout::{self, Layout as UiLayout};
use crate::ui::meter::MeterState;
use crate::ui::theme::Palette;
use crate::view::snapshot::{ViewSnapshot, build_snapshot};

/// Where the accepted numbers live. Committed, so a regression is a diff.
const BASELINE_PATH: &str = "perf/baseline.txt";

/// How much worse than the baseline a timing may get before the run fails.
/// Wide enough that an unloaded machine does not cry wolf, tight enough that a
/// real change in the work being done shows up.
const REGRESSION_RATIO: f64 = 1.10;

/// Timings below this are reported but not gated. A scenario measured in tens
/// or hundreds of nanoseconds swings further than the ratio between two runs of
/// the same binary, purely on scheduling and clock speed, so gating it would
/// fail honest builds. Above it the measurement is steady enough to trust.
const TIME_GATE_FLOOR_NS: u64 = 10_000;

/// The default window, which is the size most frames are painted at.
const WINDOW: (i32, i32) = (560, 720);

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One measured number pair, keyed by scenario name.
struct Sample {
    name: &'static str,
    ns: u64,
    allocs: u64,
}

/// Measure every scenario, compare against the committed baseline, and fail the
/// run if anything regressed. `--save` accepts the current numbers instead.
pub fn run(args: &[String]) -> Result<()> {
    let save = args.iter().any(|a| a == "--save");

    let font = Font::load()?;
    let palette = Palette::dark();
    let samples = measure(&font, &palette);

    let baseline = load_baseline(BASELINE_PATH)?;
    report(&samples, baseline.as_ref());

    if save || baseline.is_none() {
        write_baseline(BASELINE_PATH, &samples)?;
        let what = if baseline.is_none() {
            "No baseline existed; wrote"
        } else {
            "Updated"
        };
        println!("\n{what} {BASELINE_PATH}. Commit it so future runs compare.");
        return Ok(());
    }

    let mut failures = Vec::new();
    for s in &samples {
        let Some(base) = baseline.as_ref().and_then(|b| b.get(s.name)) else {
            continue;
        };
        if s.ns >= TIME_GATE_FLOOR_NS && is_regression(s.ns, base.ns) {
            failures.push(format!(
                "  {} got {:.0}% slower ({} -> {})",
                s.name,
                (s.ns as f64 / base.ns.max(1) as f64 - 1.0) * 100.0,
                fmt_ns(base.ns),
                fmt_ns(s.ns)
            ));
        }
        // Allocation counts are exactly reproducible, so any rise is a real
        // change in what the code does rather than a noisy measurement.
        if s.allocs > base.allocs {
            failures.push(format!(
                "  {} allocates more ({} -> {} per op)",
                s.name, base.allocs, s.allocs
            ));
        }
    }

    if failures.is_empty() {
        println!("\nNothing regressed.");
        return Ok(());
    }
    println!("\nRegressed:");
    for f in &failures {
        println!("{f}");
    }
    println!("\nFix it, or accept the new numbers with `make perf-save`.");
    Err(format!("{} scenario(s) regressed", failures.len()).into())
}

/// Run every scenario. Each closure does one operation's worth of work and
/// nothing else, so the number belongs to the thing it names.
fn measure(font: &Font, palette: &Palette) -> Vec<Sample> {
    let mut samples = Vec::new();

    // The frame painters, which is where a 60 Hz tick spends its time today.
    //
    // The row counts are not just size: the painter looks a row up by scanning
    // the snapshot, so a busy mixer is what would show that scan going quadratic.
    // The grouped scene is here because a group draws an expand arrow, a path
    // the ungrouped scenes never reach.
    for (name, apps, grouped, (w, h), scale) in [
        ("paint_frame", 2, false, WINDOW, 1.0),
        ("paint_frame_scale2", 2, false, WINDOW, 2.0),
        ("paint_frame_large", 2, false, (1200, 900), 1.0),
        ("paint_frame_24_apps", 24, false, WINDOW, 1.0),
        ("paint_frame_64_apps", 64, false, WINDOW, 1.0),
        ("paint_frame_grouped", 24, true, WINDOW, 1.0),
    ] {
        let app = if grouped {
            scene::grouped(apps)
        } else {
            scene::mixer(apps)
        };
        let snapshot = build_snapshot(&app, |_| None);
        let ui = UiState::new();
        let ui_layout = layout::project(&snapshot, &ui, Rect::new(0, 0, w, h));
        let mut buffer = PixelBuffer::new((w as f32 * scale) as u32, (h as f32 * scale) as u32);
        let mut icons = IconCache::new();
        samples.push(bench(name, 64, || {
            let (pixels, bw, bh) = buffer.parts();
            let mut painter = Painter::scaled(pixels, bw, bh, scale);
            paint_frame(
                &mut painter,
                &snapshot,
                &ui,
                &ui_layout,
                font,
                palette,
                &mut icons,
            );
        }));
    }

    // The same scenes repainted through the damage gate, which is what a decay
    // step costs. The pairing with the full frames above is the point: the two
    // numbers side by side are what the gate is worth, and the app counts are
    // what shows it staying flat as the mixer fills up.
    for (name, apps, scale) in [
        ("paint_meters", 2, 1.0),
        ("paint_meters_scale2", 2, 2.0),
        ("paint_meters_24_apps", 24, 1.0),
    ] {
        let (w, h) = WINDOW;
        let app = scene::mixer(apps);
        let snapshot = build_snapshot(&app, |_| None);
        let ui = UiState::new();
        let ui_layout = layout::project(&snapshot, &ui, Rect::new(0, 0, w, h));
        let mut buffer = PixelBuffer::new((w as f32 * scale) as u32, (h as f32 * scale) as u32);
        let mut icons = IconCache::new();
        samples.push(bench(name, 64, || {
            let (pixels, bw, bh) = buffer.parts();
            let mut painter = Painter::scaled(pixels, bw, bh, scale);
            paint_meters(
                &mut painter,
                &snapshot,
                &ui,
                &ui_layout,
                font,
                palette,
                &mut icons,
            );
        }));
    }

    // The per-tick and per-change work around the painter.
    let app = scene::mixer(8);
    let snapshot = build_snapshot(&app, |_| None);
    let ui = UiState::new();
    let ui_layout = layout::project(&snapshot, &ui, Rect::new(0, 0, WINDOW.0, WINDOW.1));

    samples.push(bench("build_snapshot", 512, || {
        black_box(build_snapshot(&app, |_| None));
    }));
    samples.push(bench("layout_project", 512, || {
        black_box(layout::project(
            &snapshot,
            &ui,
            Rect::new(0, 0, WINDOW.0, WINDOW.1),
        ));
    }));

    let mut meters = meters_for(&snapshot);
    samples.push(bench("meter_decay", 4096, || {
        black_box(meters.decay());
    }));

    samples.push(bench("hit_test", 4096, || {
        black_box(hit_sweep(&ui_layout));
    }));

    samples
}

/// Every hit target the pointer could be over, walked once. A single lookup is
/// too small to time honestly, and a sweep is what a drag does anyway.
fn hit_sweep(ui_layout: &UiLayout) -> usize {
    let mut found = 0;
    let mut y = 0;
    while y < WINDOW.1 {
        if ui_layout.hit(WINDOW.0 / 2, y).is_some() {
            found += 1;
        }
        y += 8;
    }
    found
}

/// Meters holding a value for every row a snapshot routes to, which is the
/// state a decay tick actually walks.
fn meters_for(snapshot: &ViewSnapshot) -> MeterState {
    let mut meters = MeterState::new();
    for rows in snapshot.meter_routes.values() {
        for row in rows {
            meters.apply(row, &[0.8, 0.6]);
        }
    }
    meters
}

/// Warm the caches, then take the best per-call time over five rounds. The
/// allocation count comes from one warmed call, which is the steady state a
/// running window is in.
fn bench<T>(name: &'static str, iters: u64, mut f: impl FnMut() -> T) -> Sample {
    for _ in 0..iters.min(16) {
        black_box(f());
    }

    black_box(f());
    alloc::reset();
    black_box(f());
    let allocs = alloc::count();

    let mut best = u64::MAX;
    for _ in 0..5 {
        let start = Instant::now();
        for _ in 0..iters.max(1) {
            black_box(f());
        }
        best = best.min(start.elapsed().as_nanos() as u64 / iters.max(1));
    }
    Sample {
        name,
        ns: best,
        allocs,
    }
}

/// Whether a number is worse than its baseline by at least the gate's ratio.
fn is_regression(current: u64, base: u64) -> bool {
    current as f64 >= (base.max(1) as f64) * REGRESSION_RATIO
}

// ---------------------------------------------------------------------------
// Reporting and the baseline file
// ---------------------------------------------------------------------------

struct Base {
    ns: u64,
    allocs: u64,
}

fn report(samples: &[Sample], baseline: Option<&HashMap<String, Base>>) {
    println!(
        "{:<22} {:>12} {:>9} {:>10} {:>8}",
        "scenario", "ns/op", "vs base", "allocs/op", "vs base"
    );
    for s in samples {
        let base = baseline.and_then(|b| b.get(s.name));
        let (ns_delta, alloc_delta) = match base {
            Some(b) => (delta(s.ns, b.ns), delta(s.allocs, b.allocs)),
            None => ("new".to_string(), "new".to_string()),
        };
        println!(
            "{:<22} {:>12} {:>9} {:>10} {:>8}",
            s.name,
            fmt_ns(s.ns),
            ns_delta,
            s.allocs,
            alloc_delta
        );
    }
}

/// A percentage change, or a dash when it is within a percent either way.
fn delta(current: u64, base: u64) -> String {
    if base == 0 {
        return if current == 0 { "-".into() } else { "+".into() };
    }
    let pct = (current as f64 / base as f64 - 1.0) * 100.0;
    if pct.abs() < 1.0 {
        "-".into()
    } else {
        format!("{pct:+.0}%")
    }
}

/// Nanoseconds, scaled to whatever unit reads without counting zeros.
fn fmt_ns(ns: u64) -> String {
    match ns {
        0..=9_999 => format!("{ns} ns"),
        10_000..=9_999_999 => format!("{:.2} us", ns as f64 / 1_000.0),
        _ => format!("{:.2} ms", ns as f64 / 1_000_000.0),
    }
}

fn load_baseline(path: &str) -> io::Result<Option<HashMap<String, Base>>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut map = HashMap::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let (Some(name), Some(ns), Some(allocs)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let (Ok(ns), Ok(allocs)) = (ns.parse(), allocs.parse()) else {
            continue;
        };
        map.insert(name.to_string(), Base { ns, allocs });
    }
    Ok(Some(map))
}

fn write_baseline(path: &str, samples: &[Sample]) -> io::Result<()> {
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut sorted: Vec<&Sample> = samples.iter().collect();
    sorted.sort_by_key(|s| s.name);
    let mut out = String::from("# bnksound perf: scenario, ns/op, allocs/op (lower is better)\n");
    for s in sorted {
        out.push_str(&format!("{}\t{}\t{}\n", s.name, s.ns, s.allocs));
    }
    std::fs::write(path, out)
}
