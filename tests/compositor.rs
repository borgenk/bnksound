//! The Wayland protocol code against real compositors.
//!
//! The app speaks the wire protocol directly, so a wrong assumption is only
//! caught by a second implementation. These run the shipped window against
//! three headless ones and assert on what came back:
//!
//! - weston, the reference, and the thinnest of the three. It offers no seat,
//!   no decoration manager, no activation and no fractional scale, which makes
//!   it the compositor that has only the bare minimum.
//! - labwc, wlroots with stacking and server-side decorations. The only one
//!   here with xdg_activation_v1, so the only place the raise path runs.
//! - cage, a wlroots kiosk, which answers the same decoration request with
//!   client mode and so covers the other half of that negotiation.
//!
//! Every test is ignored by default, since a machine without those three has
//! no business failing `cargo test`. Run them with:
//!
//! ```sh
//! make test-compositor
//! ```
//!
//! Each one boots a compositor, runs `bnksound --probe`, and parses the facts
//! the probe prints. Test isolation is XDG_CONFIG_HOME: the geometry the window
//! opens at is seeded there, and the state it saves on the way out is read back
//! from there, so nothing here touches the real configuration. XDG_RUNTIME_DIR
//! is deliberately left alone, because PipeWire is found through it and the
//! one-window lock is already named after the display.

// The probe flag these run on is dev tooling, so without it there is no window
// to ask anything of and the file is empty rather than passing for the wrong
// reason.
#![cfg(feature = "dev")]

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use bnksound::geometry::Geometry;
use bnksound::platform::conn::Connection;
use bnksound::platform::protocol::{WL_DISPLAY, evt, req};
use bnksound::platform::wire::{Arg, encode};
use bnksound::store::{self, State};

/// The window size every test starts from. Small enough to fit the headless
/// output with a titlebar above it, so a compositor has no reason to resize it
/// and the seeded size is what comes back.
const SEED: Geometry = Geometry {
    width: 560,
    height: 600,
    maximized: false,
};

/// The headless output every rig creates. The wlroots pair have no say in this
/// (their backend fixes it), so weston is told the same thing for the sake of
/// comparable numbers.
const OUTPUT: (u32, u32) = (1280, 720);

/// How long a probe holds the window when a test has nothing to do meanwhile.
const PROBE_MS: u64 = 1500;

/// How long a probe holds the window while a second launch is aimed at it.
const HOST_MS: u64 = 4000;

// --- The window comes up at all -------------------------------------------

#[test]
#[ignore = "needs a compositor; run with make test-compositor"]
fn the_window_comes_up_under_every_compositor() {
    for kind in [Compositor::Weston, Compositor::Labwc, Compositor::Cage] {
        let rig = Rig::new("comes-up");
        let report = rig.run(kind, PROBE_MS);

        assert_eq!(report.end(), "ok", "{kind} run did not finish: {report}");
        for global in ["compositor", "shm", "wm_base"] {
            assert!(
                report.flag(global),
                "{kind} must offer {global} or the app cannot run: {report}"
            );
        }
        assert!(
            report.count("configures") >= 1,
            "{kind} configured the toplevel: {report}"
        );
        assert!(
            report.count("frames") >= 1,
            "{kind} took a painted frame: {report}"
        );
    }
}

#[test]
#[ignore = "needs a compositor; run with make test-compositor"]
fn each_compositor_offers_what_the_matrix_says() {
    // The matrix in docs-internal/baseline.md is what every expectation below
    // rests on. When a distribution upgrade moves one of these, this is the
    // test that says so, and the matrix is what gets corrected.
    let expected = [
        (
            Compositor::Weston,
            [
                ("seat", false),
                ("decoration", false),
                ("activation", false),
                ("fractional", false),
                ("viewport", true),
            ],
        ),
        (
            Compositor::Labwc,
            [
                ("seat", true),
                ("decoration", true),
                ("activation", true),
                ("fractional", true),
                ("viewport", true),
            ],
        ),
        (
            Compositor::Cage,
            [
                ("seat", true),
                ("decoration", true),
                ("activation", false),
                ("fractional", false),
                ("viewport", true),
            ],
        ),
    ];
    for (kind, globals) in expected {
        let rig = Rig::new("matrix");
        let report = rig.run(kind, PROBE_MS);
        for (global, want) in globals {
            assert_eq!(
                report.flag(global),
                want,
                "{kind} {global}: the matrix in baseline.md says {want}: {report}"
            );
        }
    }
}

#[test]
fn with_no_compositor_the_app_says_so_and_stops() {
    // Not ignored, and deliberately so: this is the control that says the rest
    // of the file is testing anything at all. It needs no compositor, only the
    // certainty that a socket by this name is not one.
    let rig = Rig::new("no-compositor");
    let (stdout, stderr) = rig.client("bnksound-no-such-compositor", PROBE_MS, &[]);
    let stderr = String::from_utf8_lossy(&stderr);
    assert!(
        stderr.contains("No such file or directory"),
        "the message names the socket that was not there: {stderr}"
    );
    assert!(
        stdout.is_empty(),
        "and nothing was reported, because nothing ran: {}",
        String::from_utf8_lossy(&stdout)
    );
}

// --- Decorations ------------------------------------------------------------

#[test]
#[ignore = "needs a compositor; run with make test-compositor"]
fn a_server_side_compositor_paints_the_chrome() {
    let rig = Rig::new("ssd");
    let report = rig.run(Compositor::Labwc, PROBE_MS);
    assert!(
        report.flag("decoration"),
        "labwc offers the manager: {report}"
    );
    assert_eq!(
        report.text("chrome"),
        "server",
        "labwc took the request, so the window draws no strip of its own: {report}"
    );
}

#[test]
#[ignore = "needs a compositor; run with make test-compositor"]
fn a_compositor_that_refuses_leaves_the_window_to_draw_its_own() {
    // cage offers the manager and answers client mode, which is the branch a
    // compositor that will not decorate takes.
    let rig = Rig::new("csd");
    let report = rig.run(Compositor::Cage, PROBE_MS);
    assert!(
        report.flag("decoration"),
        "cage offers the manager: {report}"
    );
    assert_eq!(
        report.text("chrome"),
        "client",
        "the refusal has to reach the chrome, or the window has none: {report}"
    );
}

#[test]
#[ignore = "needs a compositor; run with make test-compositor"]
fn the_settings_file_can_take_the_chrome_back_from_the_compositor() {
    let rig = Rig::new("csd-setting");
    rig.settings("decorations client\n");
    let report = rig.run(Compositor::Labwc, PROBE_MS);
    assert_eq!(
        report.text("chrome"),
        "client",
        "asked for client decorations, so nothing is negotiated with labwc: {report}"
    );
}

#[test]
#[ignore = "needs a compositor; run with make test-compositor"]
fn with_no_decoration_manager_nothing_is_negotiated() {
    // weston offers no manager at all. What the app does with that is the row
    // recorded in baseline.md: it keeps the server default, so neither side
    // draws a titlebar. Pinned here so the day it changes is a day someone
    // meant it to.
    let rig = Rig::new("no-deco");
    let report = rig.run(Compositor::Weston, PROBE_MS);
    assert!(
        !report.flag("decoration"),
        "weston offers no decoration manager: {report}"
    );
    assert_eq!(report.text("chrome"), "server", "{report}");
}

// --- Activation and the one-window lock ------------------------------------

#[test]
#[ignore = "needs a compositor; run with make test-compositor"]
fn a_second_launch_hands_its_token_over_and_the_window_raises() {
    // The strongest case in the file, and the one the session compositor
    // cannot run at all: labwc is the only compositor here with activation.
    let rig = Rig::new("handover");
    let (host, second) = rig.run_pair(Compositor::Labwc, "token-from-a-launcher");

    assert_eq!(
        second.end(),
        "handed-over",
        "the second launch opens no window of its own: {second}"
    );
    assert!(host.flag("activation"), "labwc offers activation: {host}");
    assert_eq!(
        host.count("handovers"),
        1,
        "the token reached the window that was already up: {host}"
    );
    assert!(
        host.count("tokens") >= 1,
        "and the compositor answered with a token to raise on: {host}"
    );
}

#[test]
#[ignore = "needs a compositor; run with make test-compositor"]
fn without_activation_the_lock_still_spares_the_second_window() {
    // weston and cage have no activation global, which is the shape the session
    // compositor is in. The raise gives up; the one-window rule does not.
    for kind in [Compositor::Weston, Compositor::Cage] {
        let rig = Rig::new("handover-bare");
        let (host, second) = rig.run_pair(kind, "token-from-a-launcher");

        assert_eq!(second.end(), "handed-over", "{kind}: {second}");
        assert!(!host.flag("activation"), "{kind}: {host}");
        assert_eq!(
            host.count("handovers"),
            1,
            "{kind} took the handover even with nothing to raise with: {host}"
        );
        assert_eq!(
            host.count("tokens"),
            0,
            "{kind} has no activation, so no token was ever asked for: {host}"
        );
    }
}

// --- Window states ----------------------------------------------------------

#[test]
#[ignore = "needs a compositor; run with make test-compositor"]
fn a_tiled_window_is_read_as_tiled_and_keeps_its_own_size() {
    let rig = Rig::new("tiled");
    rig.rc_xml(&window_rule(
        r#"<action name="SnapToEdge" direction="left"/>"#,
    ));
    let report = rig.run(Compositor::Labwc, PROBE_MS);

    assert!(
        report.flag_word("tiled"),
        "labwc snapped the window to an edge: {report}"
    );
    assert!(
        !report.flag_word("maximized"),
        "tiling is not maximizing: {report}"
    );
    assert_ne!(
        report.pair("size"),
        (SEED.width as i32, SEED.height as i32),
        "the compositor sized it, or the rule never fired: {report}"
    );
    assert_eq!(
        report.pair("normal"),
        (SEED.width as i32, SEED.height as i32),
        "a tile is the compositor's arrangement, not a size to restore to: {report}"
    );
    assert_eq!(
        rig.saved(),
        SEED,
        "and that is the size that reached the disk"
    );
}

#[test]
#[ignore = "needs a compositor; run with make test-compositor"]
fn a_maximized_window_is_read_as_maximized_and_keeps_its_own_size() {
    // cage is a kiosk: its one client is maximized whether it likes it or not.
    let rig = Rig::new("maximized");
    let report = rig.run(Compositor::Cage, PROBE_MS);

    assert!(report.flag_word("maximized"), "{report}");
    assert!(!report.flag_word("tiled"), "{report}");
    assert_eq!(
        report.pair("size"),
        (OUTPUT.0 as i32, OUTPUT.1 as i32),
        "a kiosk window fills the output: {report}"
    );
    assert_eq!(
        report.pair("normal"),
        (SEED.width as i32, SEED.height as i32),
        "the size to restore to is untouched by it: {report}"
    );
    assert_eq!(
        rig.saved(),
        Geometry {
            maximized: true,
            ..SEED
        },
        "the kiosk's size is not saved, but being maximized is, so a relaunch \
         comes back maximized at the size the user chose"
    );
}

// --- Scale ------------------------------------------------------------------

#[test]
#[ignore = "needs a compositor; run with make test-compositor"]
fn fractional_scaling_is_wired_up_where_it_is_offered() {
    // At an output scale of 1 there is nothing to see in the numbers, so this
    // asserts the wiring: both halves exist, and the buffer is the logical size
    // rather than something the missing viewport left behind.
    let rig = Rig::new("fractional");
    let report = rig.run(Compositor::Labwc, PROBE_MS);

    assert!(report.flag("fractional"), "{report}");
    assert!(report.flag("viewport"), "{report}");
    assert_eq!(report.text("factor"), "1", "{report}");
    assert_eq!(report.pair("buffer"), report.pair("size"), "{report}");
}

#[test]
#[ignore = "needs a compositor; run with make test-compositor"]
fn a_fractional_scale_moves_the_buffer_and_leaves_the_window_alone() {
    // The two paths through the scale code diverge here, and this is the one
    // the session compositor takes. Three scales, because 1.5 alone would pass
    // on an implementation that only ever doubled.
    for scale in [1.25, 1.5, 1.75] {
        let rig = Rig::new("fractional-set");
        let report = rig.run_scaled(Compositor::Labwc, &[scale]);

        assert_eq!(
            report.text("factor"),
            &format!("{scale}"),
            "labwc's preferred scale has to reach the window: {report}"
        );
        assert_eq!(
            report.pair("buffer"),
            scaled(report.pair("size"), scale),
            "the buffer holds device pixels, so it follows the scale: {report}"
        );
        assert_eq!(
            report.count("buffer_scale"),
            1,
            "the integer path stays out of it when a viewport is in hand: {report}"
        );
    }
}

#[test]
#[ignore = "needs a compositor; run with make test-compositor"]
fn without_fractional_scaling_the_integer_output_scale_is_what_runs() {
    // cage has no fractional manager, so the same output scale arrives as a
    // wl_output scale and the buffer is set from that instead.
    let rig = Rig::new("integer-set");
    let report = rig.run_scaled(Compositor::Cage, &[2.0]);

    assert!(!report.flag("fractional"), "{report}");
    assert_eq!(
        report.count("buffer_scale"),
        2,
        "set_buffer_scale carries the output's own scale: {report}"
    );
    assert_eq!(
        report.pair("buffer"),
        scaled(report.pair("size"), 2.0),
        "{report}"
    );
}

#[test]
#[ignore = "needs a compositor; run with make test-compositor"]
fn an_output_that_goes_back_to_one_takes_the_window_with_it() {
    // The scale a window follows is the one its output has now, not the
    // highest it ever had. Two outputs would be the fuller version of this and
    // need a way to move a window between them, which is input.
    let rig = Rig::new("scale-back");
    let report = rig.run_scaled(Compositor::Cage, &[2.0, 1.0]);

    assert_eq!(report.count("buffer_scale"), 1, "{report}");
    assert_eq!(report.pair("buffer"), report.pair("size"), "{report}");
}

/// A logical size in device pixels at `scale`, the way the window works it out.
fn scaled(size: (i32, i32), scale: f64) -> (i32, i32) {
    let px = |v: i32| ((v as f64 * scale).round() as i32).max(1);
    (px(size.0), px(size.1))
}

// --- The rig ----------------------------------------------------------------

/// Which compositor a test runs against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Compositor {
    Weston,
    Labwc,
    Cage,
}

impl Compositor {
    fn binary(self) -> &'static str {
        match self {
            Compositor::Weston => "weston",
            Compositor::Labwc => "labwc",
            Compositor::Cage => "cage",
        }
    }
}

impl std::fmt::Display for Compositor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.binary())
    }
}

/// A test's own configuration home, plus the compositor plumbing that reads it.
///
/// Everything a run touches lives under one directory: the state file the
/// window opens from and saves back to, settings.conf when a test needs one,
/// and labwc's rc.xml. The directory goes when the rig does.
struct Rig {
    name: String,
    config: PathBuf,
}

impl Rig {
    /// A rig of this test's own, seeded with [`SEED`] as the window size.
    fn new(name: &str) -> Rig {
        let config =
            std::env::temp_dir().join(format!("bnksound_compositor_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&config);
        fs::create_dir_all(config.join("bnksound")).expect("create the config home");
        fs::create_dir_all(config.join("labwc")).expect("create the labwc config");
        let rig = Rig {
            name: name.to_string(),
            config,
        };
        // labwc with no configuration file of its own does not size a window
        // the same way twice, so every rig writes one whether the test cares or
        // not.
        rig.rc_xml("<?xml version=\"1.0\"?>\n<labwc_config></labwc_config>\n");
        rig.seed(SEED);
        rig
    }

    /// Write the window geometry the next run opens from.
    fn seed(&self, window: Geometry) {
        let state = State {
            window,
            ..Default::default()
        };
        store::save_to(&self.state_path(), &state).expect("seed the state file");
    }

    /// The geometry the last run left on disk.
    fn saved(&self) -> Geometry {
        store::load_from(&self.state_path())
            .expect("read the state back")
            .window
    }

    fn state_path(&self) -> PathBuf {
        self.config.join("bnksound/state.bin")
    }

    /// Write settings.conf for the run.
    fn settings(&self, text: &str) {
        fs::write(self.config.join("bnksound/settings.conf"), text).expect("write settings.conf");
    }

    /// Write labwc's configuration for the run.
    fn rc_xml(&self, text: &str) {
        fs::write(self.config.join("labwc/rc.xml"), text).expect("write rc.xml");
    }

    /// Run one probe under `kind` and take back what it reported.
    fn run(&self, kind: Compositor, ms: u64) -> Report {
        match kind {
            // weston names its socket, so the probe is our own child.
            Compositor::Weston => {
                let mut weston = self.start_weston(&[]);
                let display = self.weston_display();
                let (stdout, _) = self.client(&display, ms, &[]);
                stop(&mut weston);
                self.unlink_lock(&display);
                Report::parse(&stdout, &self.log_tail())
            }
            // The wlroots pair pick their own socket and terminate with their
            // client, so the probe rides in as that client and its output comes
            // back on the compositor's own.
            _ => {
                let mut child = self.start_wlroots(kind, &self.probe_command(ms));
                let (stdout, _) = wait_for(&mut child, Duration::from_millis(ms) + GRACE);
                self.unlink_lock(&self.marker_display());
                Report::parse(&stdout, &self.log_tail())
            }
        }
    }

    /// Run a probe, set the output scale to each of `scales` in turn once the
    /// window is up, and take back what the window made of the last one.
    fn run_scaled(&self, kind: Compositor, scales: &[f64]) -> Report {
        assert_ne!(kind, Compositor::Weston, "weston sends no surface enter");
        let mut child = self.start_wlroots(kind, &self.probe_command(HOST_MS));
        let display = self.await_marker();
        self.await_lock(&display);
        for scale in scales {
            set_output_scale(&display, *scale);
            // The window has to have painted at one scale before the next, or
            // the run only ever proves the last of them.
            settle();
            settle();
        }
        let (stdout, _) = wait_for(&mut child, Duration::from_millis(HOST_MS) + GRACE);
        self.unlink_lock(&display);
        Report::parse(&stdout, &self.log_tail())
    }

    /// Run a probe, then a second launch against the same compositor while the
    /// first still holds the window, carrying `token` the way a launcher would.
    /// Returns what each of them reported.
    fn run_pair(&self, kind: Compositor, token: &str) -> (Report, Report) {
        let env = [("XDG_ACTIVATION_TOKEN", token)];
        let (mut compositor, display, host) = match kind {
            Compositor::Weston => {
                let weston = self.start_weston(&[]);
                let display = self.weston_display();
                let host = self.client_spawn(&display, HOST_MS, &[]);
                (weston, display, host)
            }
            _ => {
                let child = self.start_wlroots(kind, &self.probe_command(HOST_MS));
                let display = self.await_marker();
                (child, display, None)
            }
        };

        // The lock is what a second launch hands itself over on, so there is no
        // point starting one before the first has taken it.
        self.await_lock(&display);
        let (second, second_err) = self.client(&display, PROBE_MS, &env);

        // Under weston the first probe is our own child; under the others it is
        // the compositor's, and the compositor ends with it.
        let (first, _) = match host {
            Some(mut host) => {
                let out = wait_for(&mut host, Duration::from_millis(HOST_MS) + GRACE);
                stop(&mut compositor);
                out
            }
            None => wait_for(&mut compositor, Duration::from_millis(HOST_MS) + GRACE),
        };
        self.unlink_lock(&display);
        (
            Report::parse(&first, &self.log_tail()),
            Report::parse(&second, &String::from_utf8_lossy(&second_err)),
        )
    }

    // --- Compositors --------------------------------------------------------

    /// Boot weston on a socket named after this rig and wait for it to bind.
    fn start_weston(&self, extra: &[String]) -> Child {
        let display = self.weston_display();
        let socket = runtime_dir().join(&display);
        let _ = fs::remove_file(&socket);
        let _ = fs::remove_file(runtime_dir().join(format!("{display}.lock")));

        let mut command = Command::new(Compositor::Weston.binary());
        command
            .args(["--backend=headless", "--renderer=pixman", "--no-config"])
            .arg(format!("--socket={display}"))
            .arg(format!("--width={}", OUTPUT.0))
            .arg(format!("--height={}", OUTPUT.1))
            .args(extra);
        let mut child = self.spawn(command, Compositor::Weston, Stdio::null());

        let deadline = Instant::now() + STARTUP;
        while !socket.exists() {
            if Instant::now() > deadline {
                stop(&mut child);
                panic!("weston never bound {}", socket.display());
            }
            settle();
        }
        child
    }

    /// Start labwc or cage with `client` as the child it runs and terminates
    /// with. The display it picked is written down before the client starts,
    /// since the name is the compositor's to choose and a test cannot ask.
    ///
    /// The client goes in a script rather than on a command line. cage execs
    /// its arguments and labwc splits its own, so a command written for one is
    /// mangled by the other, and a path to a file is a word either way.
    fn start_wlroots(&self, kind: Compositor, client: &str) -> Child {
        let script = self.config.join("start.sh");
        fs::write(
            &script,
            format!(
                "printf %s \"$WAYLAND_DISPLAY\" > {}\nexec {client}\n",
                self.marker().display()
            ),
        )
        .expect("write the startup script");

        let mut command = Command::new(kind.binary());
        match kind {
            Compositor::Labwc => {
                command
                    .arg("-C")
                    .arg(self.config.join("labwc"))
                    .arg("-S")
                    .arg(format!("sh {}", script.display()));
            }
            _ => {
                command.args(["--", "sh"]).arg(&script);
            }
        }
        command.env("WLR_BACKENDS", "headless");
        self.spawn(command, kind, Stdio::piped())
    }

    /// Spawn a compositor, with the environment every run shares.
    ///
    /// Its logging goes to a file rather than a pipe. A compositor writes more
    /// than a pipe buffer holds and nothing here reads it until the run is
    /// over, which on a pipe is a compositor stuck mid-log.
    fn spawn(&self, mut command: Command, kind: Compositor, out: Stdio) -> Child {
        let log = fs::File::create(self.log_path()).expect("open the compositor log");
        command
            .env("XDG_CONFIG_HOME", &self.config)
            // A compositor started from inside a session would otherwise nest
            // itself in the one running it.
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(Stdio::from(log));
        command.spawn().unwrap_or_else(|e| {
            panic!(
                "cannot start {kind}: {e}\n\
                 the compositor suite needs weston, labwc and cage installed"
            )
        })
    }

    fn log_path(&self) -> PathBuf {
        self.config.join("compositor.log")
    }

    /// The last of the compositor's logging, which is the only part of it that
    /// ever says why a run went wrong.
    fn log_tail(&self) -> String {
        let log = fs::read_to_string(self.log_path()).unwrap_or_default();
        let tail: Vec<&str> = log.lines().rev().take(15).collect();
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    }

    // --- Clients ------------------------------------------------------------

    /// The probe command line, as a compositor's startup command.
    fn probe_command(&self, ms: u64) -> String {
        format!("{} --probe {ms}", env!("CARGO_BIN_EXE_bnksound"))
    }

    /// Run a probe against `display` and wait for it. Returns what it wrote to
    /// each stream.
    fn client(&self, display: &str, ms: u64, env: &[(&str, &str)]) -> (Vec<u8>, Vec<u8>) {
        let mut child = self
            .client_spawn(display, ms, env)
            .expect("the probe binary is there to run");
        wait_for(&mut child, Duration::from_millis(ms) + GRACE)
    }

    /// Start a probe against `display` without waiting for it.
    fn client_spawn(&self, display: &str, ms: u64, env: &[(&str, &str)]) -> Option<Child> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bnksound"));
        command
            .arg("--probe")
            .arg(ms.to_string())
            .env("XDG_CONFIG_HOME", &self.config)
            .env("WAYLAND_DISPLAY", display)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        command.spawn().ok()
    }

    // --- Displays and locks -------------------------------------------------

    /// The socket name weston is told to bind. Kept short, because the lock
    /// socket derived from it has to fit in a unix address.
    fn weston_display(&self) -> String {
        let name: String = self.name.chars().take(12).collect();
        format!("bnk-{}-{name}", std::process::id())
    }

    /// Where a wlroots compositor's startup command writes its display name.
    fn marker(&self) -> PathBuf {
        self.config.join("display")
    }

    fn marker_display(&self) -> String {
        fs::read_to_string(self.marker()).unwrap_or_default()
    }

    /// Wait for the display name a wlroots compositor picked.
    fn await_marker(&self) -> String {
        let deadline = Instant::now() + STARTUP;
        loop {
            let display = self.marker_display();
            if !display.is_empty() {
                return display;
            }
            assert!(
                Instant::now() < deadline,
                "the compositor never started its client"
            );
            settle();
        }
    }

    /// Wait for the one-window lock to be taken, which is the point a second
    /// launch has something to hand itself over to.
    fn await_lock(&self, display: &str) {
        let path = lock_socket(display);
        let deadline = Instant::now() + STARTUP;
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "the first launch never took the lock at {}",
                path.display()
            );
            settle();
        }
    }

    /// Clear the lock socket a killed probe leaves behind. A clean exit unlinks
    /// its own, so this only matters when a run was cut short.
    fn unlink_lock(&self, display: &str) {
        if !display.is_empty() {
            let _ = fs::remove_file(lock_socket(display));
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.config);
    }
}

/// How long a compositor is given to bind its socket and start its client.
const STARTUP: Duration = Duration::from_secs(15);

/// What a process is given past its own deadline before it counts as hung.
const GRACE: Duration = Duration::from_secs(20);

fn settle() {
    std::thread::sleep(Duration::from_millis(50));
}

fn runtime_dir() -> PathBuf {
    PathBuf::from(std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR is set"))
}

/// Where the one-window lock lands for a display, by the same rule the app
/// names it: one path component, cut to fit a unix address.
fn lock_socket(display: &str) -> PathBuf {
    let name: String = display
        .chars()
        .take(32)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    runtime_dir().join(format!("bnksound-{name}.sock"))
}

/// Wait for a process, killing it if it runs past `limit`, and take everything
/// it wrote either way.
fn wait_for(child: &mut Child, limit: Duration) -> (Vec<u8>, Vec<u8>) {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() > deadline => {
                stop(child);
                break;
            }
            Ok(None) => settle(),
            Err(e) => panic!("waiting on a child failed: {e}"),
        }
    }
    (drain(child.stdout.take()), drain(child.stderr.take()))
}

/// Everything left in a pipe, which is all of it once the writer has gone.
fn drain<R: Read>(pipe: Option<R>) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut bytes);
    }
    bytes
}

/// Kill a process and reap it, so no test leaves one behind.
fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// What a probe reported, parsed back into the keys it printed.
///
/// The format is fixed by `Facts` in src/native/app.rs and covered by a unit
/// test there, so a key that goes missing here is a change in the app rather
/// than a compositor doing something new.
struct Report {
    values: HashMap<String, String>,
    end: String,
    /// Everything the run wrote, kept for failure messages.
    output: String,
}

impl Report {
    fn parse(stdout: &[u8], log: &str) -> Report {
        let out = String::from_utf8_lossy(stdout).into_owned();
        let mut values = HashMap::new();
        let mut end = String::new();
        for line in out.lines() {
            let Some(rest) = line.strip_prefix("probe ") else {
                continue;
            };
            let mut words = rest.split_whitespace();
            let section = words.next().unwrap_or_default();
            if section == "end" {
                end = words.next().unwrap_or_default().to_string();
                continue;
            }
            for word in words {
                match word.split_once('=') {
                    Some((key, value)) => {
                        values.insert(key.to_string(), value.to_string());
                    }
                    // The window line leads with its size before the pairs.
                    None if section == "window" => {
                        values.insert("size".to_string(), word.to_string());
                    }
                    None => {}
                }
            }
        }
        let output = format!("\n--- probe said ---\n{out}--- and the last of the log ---\n{log}\n");
        Report {
            values,
            end,
            output,
        }
    }

    /// How the run ended: "ok", "handed-over", or empty when it never got that
    /// far.
    fn end(&self) -> &str {
        &self.end
    }

    /// A global's presence, which the probe prints as 1 or 0.
    fn flag(&self, key: &str) -> bool {
        self.text(key) == "1"
    }

    /// A window state, which the probe prints as true or false.
    fn flag_word(&self, key: &str) -> bool {
        self.text(key) == "true"
    }

    fn count(&self, key: &str) -> u32 {
        self.text(key).parse().unwrap_or_else(|_| {
            panic!("{key} is not a number in this run: {self}");
        })
    }

    /// A size, which the probe prints as WxH.
    fn pair(&self, key: &str) -> (i32, i32) {
        let text = self.text(key);
        let parsed = text
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
        parsed.unwrap_or_else(|| panic!("{key} is not a size in this run: {self}"))
    }

    fn text(&self, key: &str) -> &str {
        self.values.get(key).map_or("", String::as_str)
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.output)
    }
}

/// wlr-output-management-unstable-v1, as much of it as setting a scale needs.
/// Opcodes are declaration order in the protocol, per kind.
mod wlr {
    /// The manager, at the version its scale and configuration events need.
    pub const MANAGER: &str = "zwlr_output_manager_v1";
    pub const MANAGER_VERSION: u32 = 1;

    /// Manager events: a head, and the serial that names this whole state.
    pub const HEAD: u16 = 0;
    pub const DONE: u16 = 1;
    /// Manager request.
    pub const CREATE_CONFIGURATION: u16 = 0;

    /// Configuration requests.
    pub const ENABLE_HEAD: u16 = 0;
    pub const APPLY: u16 = 2;
    /// Configuration events.
    pub const SUCCEEDED: u16 = 0;
    pub const FAILED: u16 = 1;
    pub const CANCELLED: u16 = 2;

    /// Head configuration request.
    pub const SET_SCALE: u16 = 4;
}

/// Set the scale of every enabled output on `display`.
///
/// Neither headless compositor here takes an output scale from a command line
/// or a configuration file, and the tools that would set one are not installed.
/// The protocol is small, and the encoder is the app's own, so the harness asks
/// for the scale itself: bind the manager, take the serial that names the
/// current state, enable every head (one left out of a configuration is one
/// turned off), set the scale, and apply.
fn set_output_scale(display: &str, scale: f64) {
    let mut conn =
        Connection::connect_at(&runtime_dir().join(display)).expect("connect to the compositor");
    let mut next_id = 2;

    let registry = take(&mut next_id);
    encode(
        conn.out(),
        WL_DISPLAY,
        req::DISPLAY_GET_REGISTRY,
        &[Arg::NewId(registry)],
    );
    conn.flush(None).expect("ask for the registry");

    // Bind the manager as soon as it is announced, then take heads until the
    // manager says that is all of them.
    let mut manager = 0;
    let mut heads = Vec::new();
    let mut serial = None;
    let deadline = Instant::now() + STARTUP;
    while serial.is_none() {
        assert!(
            Instant::now() < deadline,
            "{display} never finished announcing its outputs"
        );
        assert!(conn.fill().expect("read from the compositor"), "closed");
        while let Some(msg) = conn.next_message() {
            let mut r = msg.reader();
            match (msg.object, msg.opcode) {
                (obj, evt::REGISTRY_GLOBAL) if obj == registry => {
                    let name = r.u32().unwrap_or(0);
                    let interface = r.string().unwrap_or_default();
                    let version = r.u32().unwrap_or(1);
                    if interface == wlr::MANAGER {
                        manager = take(&mut next_id);
                        encode(
                            conn.out(),
                            registry,
                            req::REGISTRY_BIND,
                            &[
                                Arg::Uint(name),
                                Arg::Bind {
                                    interface: wlr::MANAGER,
                                    version: version.min(wlr::MANAGER_VERSION),
                                    new_id: manager,
                                },
                            ],
                        );
                    }
                }
                (obj, wlr::HEAD) if obj == manager && manager != 0 => {
                    heads.push(r.u32().unwrap_or(0));
                }
                (obj, wlr::DONE) if obj == manager && manager != 0 => {
                    serial = Some(r.u32().unwrap_or(0));
                }
                _ => {}
            }
        }
        conn.flush(None).expect("flush the bind");
        settle();
    }

    assert_ne!(manager, 0, "{display} offers no {}", wlr::MANAGER);
    assert!(!heads.is_empty(), "{display} reported no outputs");
    let serial = serial.unwrap_or_default();

    let config = take(&mut next_id);
    encode(
        conn.out(),
        manager,
        wlr::CREATE_CONFIGURATION,
        &[Arg::NewId(config), Arg::Uint(serial)],
    );
    for head in &heads {
        let head_config = take(&mut next_id);
        encode(
            conn.out(),
            config,
            wlr::ENABLE_HEAD,
            &[Arg::NewId(head_config), Arg::Object(*head)],
        );
        // wl_fixed is 24.8, and there is no Arg for it because the app has
        // never had to send one.
        encode(
            conn.out(),
            head_config,
            wlr::SET_SCALE,
            &[Arg::Int((scale * 256.0).round() as i32)],
        );
    }
    encode(conn.out(), config, wlr::APPLY, &[]);
    conn.flush(None).expect("apply the configuration");

    let deadline = Instant::now() + STARTUP;
    loop {
        assert!(
            Instant::now() < deadline,
            "{display} never answered the configuration"
        );
        assert!(conn.fill().expect("read from the compositor"), "closed");
        while let Some(msg) = conn.next_message() {
            if msg.object != config {
                continue;
            }
            match msg.opcode {
                wlr::SUCCEEDED => return,
                wlr::FAILED => panic!("{display} refused a scale of {scale}"),
                wlr::CANCELLED => panic!("{display} cancelled the configuration"),
                _ => {}
            }
        }
        settle();
    }
}

/// The next object id, which a client allocates as it goes.
fn take(next: &mut u32) -> u32 {
    let id = *next;
    *next += 1;
    id
}

/// A labwc configuration whose only rule fires on the mixer's window.
fn window_rule(action: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?>\n\
         <labwc_config>\n\
         \x20 <windowRules>\n\
         \x20   <windowRule identifier=\"io.github.borgenk.BnkSound\">\n\
         \x20     {action}\n\
         \x20   </windowRule>\n\
         \x20 </windowRules>\n\
         </labwc_config>\n"
    )
}
