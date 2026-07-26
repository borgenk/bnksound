//! Application-wide visual / behavioral settings, read from settings.conf.
//! Missing file means first-run defaults; a malformed line is an error. Format
//! is one "field value" per line, and unknown fields are tolerated so a config
//! written by a later build still loads. Hand-edited: nothing writes it back.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

const FILENAME: &str = "settings.conf";

/// Visual / behavioral toggles. Add fields here and a parse arm in
/// `parse_lines`; missing fields fall back to the [`Default`] value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Whether the window draws a hairline border around its edge.
    pub show_window_border: bool,
    /// Whether the toolbar (IN/OUT/APP filters + M/R actions) is shown.
    pub show_sidebar: bool,
    /// Per-button visibility for the toolbar. Each hides just that one button;
    /// the toolbar itself stays unless `show_sidebar` is false.
    pub show_input_button: bool,
    pub show_output_button: bool,
    pub show_apps_button: bool,
    pub show_mute_button: bool,
    pub show_reset_button: bool,
    /// Who draws the window's chrome. See [`Decorations`].
    pub decorations: Decorations,
    /// Which colours the GTK build's own chrome takes. See [`GtkChrome`]. The
    /// native shell paints no GTK widgets and ignores it.
    pub gtk_chrome: GtkChrome,
}

/// Which colours GTK's chrome takes in the GTK build: the header bar, the
/// window, and the menus.
///
/// `Palette` is the default: the chrome takes the mixer's own colours, so the
/// two shells look like one app rather than the GTK one looking like whatever
/// desktop it happens to be on. `Theme` hands the chrome back to the desktop
/// for anyone who would rather the window matched their other GTK apps. There
/// is no third option that gets both, so this is the switch between them.
///
/// The colours are dark whichever desktop this runs on, since the palette has
/// no light variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GtkChrome {
    #[default]
    Palette,
    Theme,
}

impl GtkChrome {
    /// Parse a config-file token. `None` for anything unrecognized.
    fn parse(s: &str) -> Option<Self> {
        match s {
            "theme" => Some(Self::Theme),
            "palette" => Some(Self::Palette),
            _ => None,
        }
    }
}

/// Who draws the window's titlebar and borders.
///
/// `Server` asks the compositor for them and is the default; `Client` draws them
/// in-window. A compositor that refuses server-side decorations gets `Client`
/// regardless, since the alternative is a window with no chrome at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Decorations {
    #[default]
    Server,
    Client,
}

impl Decorations {
    /// Parse a config-file token. `None` for anything unrecognized.
    fn parse(s: &str) -> Option<Self> {
        match s {
            "server" => Some(Self::Server),
            "client" => Some(Self::Client),
            _ => None,
        }
    }

    /// Read the retired `titlebar` field. A GTK header bar was the app drawing
    /// its own chrome; dropping it for an in-window strip left the chrome to
    /// the compositor.
    fn from_titlebar(s: &str) -> Option<Self> {
        match s {
            "headerbar" => Some(Self::Client),
            "strip" => Some(Self::Server),
            _ => None,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        // Not derived: the toolbar and its buttons default on (only
        // `show_window_border` defaults off).
        Self {
            show_window_border: false,
            show_sidebar: true,
            show_input_button: true,
            show_output_button: true,
            show_apps_button: true,
            show_mute_button: true,
            show_reset_button: true,
            decorations: Decorations::default(),
            gtk_chrome: GtkChrome::default(),
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Io {
        op: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    /// A line couldn't be parsed. `line_no` is 1-indexed.
    BadLine {
        line_no: usize,
        content: String,
        reason: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { op, path, .. } => write!(f, "{op} {}", path.display())?,
            Self::BadLine {
                line_no,
                content,
                reason,
            } => write!(
                f,
                "settings line {line_no}: {reason} (got `{}`)",
                content.trim()
            )?,
        }
        if f.alternate()
            && let Some(src) = std::error::Error::source(self)
        {
            write!(f, ": {src}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Path to the settings file, `None` when no config dir resolves.
pub fn settings_path() -> Option<PathBuf> {
    crate::config_path(FILENAME)
}

/// Load settings from the standard config location, never failing. No
/// config dir, an unreadable file, or a malformed line falls back to the
/// defaults (logging the latter), so a typo can't keep the app from starting.
pub fn load() -> Settings {
    let Some(path) = settings_path() else {
        return Settings::default();
    };
    load_from(&path).unwrap_or_else(|e| {
        eprintln!("settings: {e:#}; falling back to defaults");
        Settings::default()
    })
}

/// Read settings from `path`. Missing file returns the defaults; a
/// malformed line is an error.
pub fn load_from(path: &Path) -> Result<Settings> {
    match fs::read_to_string(path) {
        Ok(text) => parse_lines(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(source) => Err(Error::Io {
            op: "read settings",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn parse_lines(text: &str) -> Result<Settings> {
    let mut settings = Settings::default();
    for (i, raw) in text.lines().enumerate() {
        let line_no = i + 1;
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // First whitespace run separates field from value.
        let (field, value) =
            line.split_once(char::is_whitespace)
                .ok_or_else(|| Error::BadLine {
                    line_no,
                    content: raw.to_string(),
                    reason: "missing value (expected `field value`)".to_string(),
                })?;
        let value = value.trim();
        // Strict bool, mapping failure to a BadLine on this line.
        let want_bool = |value: &str| -> Result<bool> {
            parse_bool(value).ok_or_else(|| Error::BadLine {
                line_no,
                content: raw.to_string(),
                reason: format!("expected `true` or `false`, got `{value}`"),
            })
        };
        // Same shape as `want_bool`, but for the one enum-valued field.
        let want_decorations = |value: &str| -> Result<Decorations> {
            Decorations::parse(value).ok_or_else(|| Error::BadLine {
                line_no,
                content: raw.to_string(),
                reason: format!("expected `server` or `client`, got `{value}`"),
            })
        };
        // Same shape again, for the GTK build's chrome colours.
        let want_gtk_chrome = |value: &str| -> Result<GtkChrome> {
            GtkChrome::parse(value).ok_or_else(|| Error::BadLine {
                line_no,
                content: raw.to_string(),
                reason: format!("expected `theme` or `palette`, got `{value}`"),
            })
        };
        // Each known field gets one arm; unknown fields are tolerated
        // for forward-compat.
        match field {
            "show_window_border" => settings.show_window_border = want_bool(value)?,
            "show_sidebar" => settings.show_sidebar = want_bool(value)?,
            "show_input_button" => settings.show_input_button = want_bool(value)?,
            "show_output_button" => settings.show_output_button = want_bool(value)?,
            "show_apps_button" => settings.show_apps_button = want_bool(value)?,
            "show_mute_button" => settings.show_mute_button = want_bool(value)?,
            "show_reset_button" => settings.show_reset_button = want_bool(value)?,
            "decorations" => settings.decorations = want_decorations(value)?,
            "gtk_chrome" => settings.gtk_chrome = want_gtk_chrome(value)?,
            // Retired in favour of `decorations`. Still read so an existing
            // config keeps the chrome it asked for; an unreadable value falls
            // back to the default rather than refusing to start.
            "titlebar" => {
                if let Some(mode) = Decorations::from_titlebar(value) {
                    settings.decorations = mode;
                }
            }
            _ => {}
        }
    }
    Ok(settings)
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_returns_defaults() {
        assert_eq!(parse_lines("").unwrap(), Settings::default());
    }

    #[test]
    fn blank_lines_are_ignored() {
        let txt = "

            show_window_border true

        ";
        let s = parse_lines(txt).unwrap();
        assert!(s.show_window_border);
    }

    #[test]
    fn only_literal_true_and_false_accepted() {
        let ok_cases = [("true", true), ("false", false)];
        for (input, expected) in ok_cases {
            let s = parse_lines(&format!("show_window_border {input}")).unwrap();
            assert_eq!(s.show_window_border, expected, "input `{input}`");
        }
        for bad in ["True", "FALSE", "yes", "no", "1", "0", "on", "off", "maybe"] {
            let err = parse_lines(&format!("show_window_border {bad}"))
                .expect_err("should reject non-literal");
            matches!(err, Error::BadLine { .. });
        }
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        let txt = "
            future_option whatever
            show_window_border true
            mystery 42
        ";
        let s = parse_lines(txt).unwrap();
        assert!(s.show_window_border);
    }

    #[test]
    fn missing_fields_fall_back_to_default() {
        let txt = "unrelated something\n";
        let s = parse_lines(txt).unwrap();
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn line_with_only_a_field_is_an_error() {
        let err = parse_lines("show_window_border").expect_err("should reject");
        match err {
            Error::BadLine { line_no, .. } => assert_eq!(line_no, 1),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn multiple_spaces_between_field_and_value_are_tolerated() {
        for spacing in [
            "show_window_border true",
            "show_window_border  true",
            "show_window_border\ttrue",
            "   show_window_border   true   ",
        ] {
            let s = parse_lines(spacing).unwrap();
            assert!(s.show_window_border, "input `{spacing}`");
        }
    }

    #[test]
    fn sidebar_and_buttons_default_on() {
        let d = Settings::default();
        assert!(d.show_sidebar);
        assert!(d.show_input_button);
        assert!(d.show_output_button);
        assert!(d.show_apps_button);
        assert!(d.show_mute_button);
        assert!(d.show_reset_button);
    }

    #[test]
    fn parses_sidebar_button_toggles() {
        let txt = "
            show_sidebar false
            show_input_button false
            show_mute_button false
        ";
        let s = parse_lines(txt).unwrap();
        assert!(!s.show_sidebar);
        assert!(!s.show_input_button);
        assert!(!s.show_mute_button);
        // Unmentioned toggles keep their default (on).
        assert!(s.show_output_button);
        assert!(s.show_apps_button);
        assert!(s.show_reset_button);
    }

    #[test]
    fn decorations_default_to_server_side() {
        assert_eq!(Settings::default().decorations, Decorations::Server);
        assert_eq!(parse_lines("").unwrap().decorations, Decorations::Server);
    }

    #[test]
    fn parses_decorations() {
        for (input, expected) in [
            ("server", Decorations::Server),
            ("client", Decorations::Client),
        ] {
            let s = parse_lines(&format!("decorations {input}")).unwrap();
            assert_eq!(s.decorations, expected, "input `{input}`");
        }
        for bad in ["Server", "CLIENT", "headerbar", "true", "none", "1"] {
            let err =
                parse_lines(&format!("decorations {bad}")).expect_err("should reject unknown mode");
            assert!(matches!(err, Error::BadLine { .. }), "input `{bad}`");
        }
    }

    /// The two shells look like one app out of the box, so handing the chrome
    /// back to the desktop takes an explicit `theme`.
    #[test]
    fn gtk_chrome_defaults_to_the_mixer_palette() {
        assert_eq!(Settings::default().gtk_chrome, GtkChrome::Palette);
        assert_eq!(parse_lines("").unwrap().gtk_chrome, GtkChrome::Palette);
    }

    #[test]
    fn parses_gtk_chrome() {
        for (input, expected) in [("theme", GtkChrome::Theme), ("palette", GtkChrome::Palette)] {
            let s = parse_lines(&format!("gtk_chrome {input}")).unwrap();
            assert_eq!(s.gtk_chrome, expected, "input `{input}`");
        }
        for bad in ["Theme", "PALETTE", "native", "true", "dark", "1"] {
            let err =
                parse_lines(&format!("gtk_chrome {bad}")).expect_err("should reject unknown mode");
            assert!(matches!(err, Error::BadLine { .. }), "input `{bad}`");
        }
    }

    /// A config written before `decorations` existed still says what chrome the
    /// user wanted, so it keeps working without them editing anything.
    #[test]
    fn the_retired_titlebar_field_migrates_to_decorations() {
        // A header bar was chrome the app drew; a bare strip left it to the
        // compositor. Getting this backwards hands the window the wrong chrome.
        for (input, expected) in [
            ("headerbar", Decorations::Client),
            ("strip", Decorations::Server),
        ] {
            let s = parse_lines(&format!("titlebar {input}")).unwrap();
            assert_eq!(s.decorations, expected, "input `{input}`");
        }
        // An unreadable legacy value is tolerated, not fatal.
        let s = parse_lines("titlebar nonsense").expect("legacy junk does not refuse to start");
        assert_eq!(s.decorations, Decorations::Server);
        // An explicit `decorations` line wins when both appear.
        let s = parse_lines("titlebar headerbar\ndecorations server").unwrap();
        assert_eq!(s.decorations, Decorations::Server);
    }

    #[test]
    fn load_from_missing_file_returns_default() {
        let path = std::env::temp_dir().join("bnksound_test_does_not_exist.conf");
        let _ = std::fs::remove_file(&path);
        let s = load_from(&path).expect("missing file ok");
        assert_eq!(s, Settings::default());
    }
}
