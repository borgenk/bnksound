//! Application-wide visual / behavioral settings, loaded from
//! `settings.conf`. Missing file means first-run defaults; a malformed
//! line is an error. Format: one `field value` per line. Unknown fields
//! are tolerated for forward-compat. Atomic save (temp file + rename).

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

const FILENAME: &str = "settings.conf";

/// Visual / behavioral toggles. Add fields here and a parse arm in
/// `parse_lines`; missing fields fall back to the [`Default`] value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Whether the window draws a hairline border + drop shadow.
    pub show_window_border: bool,
    /// Whether the left action-bar (IN/OUT/APP + M/R strip) is shown.
    pub show_sidebar: bool,
    /// Per-button visibility for the action bar. Each hides just that one
    /// button; the sidebar itself stays unless `show_sidebar` is false.
    pub show_input_button: bool,
    pub show_output_button: bool,
    pub show_apps_button: bool,
    pub show_mute_button: bool,
    pub show_reset_button: bool,
    /// Large volume `%` readout above each slider. Independent of
    /// [`Self::percent_on_slider`].
    pub percent_above: bool,
    /// Volume `%` drawn on the slider knob itself.
    pub percent_on_slider: bool,
    /// How the window's top chrome is drawn. See [`TitlebarMode`].
    pub titlebar: TitlebarMode,
}

/// Strategy for the window's top chrome. `HeaderBar` is the default;
/// `GTK_CSD=0` forces `Strip` at the call site regardless of the setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TitlebarMode {
    /// `gtk::HeaderBar` as the window titlebar.
    #[default]
    HeaderBar,
    /// No GTK titlebar; profile selector in an in-window strip.
    Strip,
}

impl TitlebarMode {
    /// Config-file token for this mode, the inverse of [`Self::parse`].
    fn as_str(self) -> &'static str {
        match self {
            Self::HeaderBar => "headerbar",
            Self::Strip => "strip",
        }
    }

    /// Parse a config-file token. `None` for anything unrecognized.
    fn parse(s: &str) -> Option<Self> {
        match s {
            "headerbar" => Some(Self::HeaderBar),
            "strip" => Some(Self::Strip),
            _ => None,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        // Not derived: the sidebar and its buttons default on (only
        // `show_window_border` defaults off).
        Self {
            show_window_border: false,
            show_sidebar: true,
            show_input_button: true,
            show_output_button: true,
            show_apps_button: true,
            show_mute_button: true,
            show_reset_button: true,
            percent_above: false,
            percent_on_slider: true,
            titlebar: TitlebarMode::HeaderBar,
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
    NoParentDir(PathBuf),
    NoSettingsPath,
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
            Self::NoParentDir(p) => write!(f, "settings path has no parent: {}", p.display())?,
            Self::NoSettingsPath => {
                f.write_str("no XDG_CONFIG_HOME or HOME cannot persist settings")?
            }
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

/// Atomic save: write to a sibling temp file, fsync, rename over the
/// target. A crash mid-write leaves the previous good file intact.
pub fn save_to(path: &Path, settings: &Settings) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::NoParentDir(path.to_path_buf()))?;
    fs::create_dir_all(dir).map_err(|source| Error::Io {
        op: "mkdir",
        path: dir.to_path_buf(),
        source,
    })?;

    let body = encode(settings);
    let tmp = path.with_extension("conf.tmp");
    crate::atomic_write(path, &tmp, body.as_bytes()).map_err(|e| Error::Io {
        op: e.op,
        path: e.path,
        source: e.source,
    })?;
    Ok(())
}

fn encode(settings: &Settings) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "show_window_border {}\n",
        settings.show_window_border
    ));
    out.push_str(&format!("show_sidebar {}\n", settings.show_sidebar));
    out.push_str(&format!(
        "show_input_button {}\n",
        settings.show_input_button
    ));
    out.push_str(&format!(
        "show_output_button {}\n",
        settings.show_output_button
    ));
    out.push_str(&format!("show_apps_button {}\n", settings.show_apps_button));
    out.push_str(&format!("show_mute_button {}\n", settings.show_mute_button));
    out.push_str(&format!(
        "show_reset_button {}\n",
        settings.show_reset_button
    ));
    out.push_str(&format!("percent_above {}\n", settings.percent_above));
    out.push_str(&format!(
        "percent_on_slider {}\n",
        settings.percent_on_slider
    ));
    out.push_str(&format!("titlebar {}\n", settings.titlebar.as_str()));
    out
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
        let want_titlebar = |value: &str| -> Result<TitlebarMode> {
            TitlebarMode::parse(value).ok_or_else(|| Error::BadLine {
                line_no,
                content: raw.to_string(),
                reason: format!("expected `headerbar` or `strip`, got `{value}`"),
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
            "percent_above" => settings.percent_above = want_bool(value)?,
            "percent_on_slider" => settings.percent_on_slider = want_bool(value)?,
            "titlebar" => settings.titlebar = want_titlebar(value)?,
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
    fn roundtrip_through_encode_and_parse() {
        for original in [
            Settings::default(),
            Settings {
                show_window_border: true,
                ..Settings::default()
            },
            Settings {
                show_window_border: false,
                show_sidebar: false,
                show_input_button: false,
                show_output_button: true,
                show_apps_button: false,
                show_mute_button: true,
                show_reset_button: false,
                percent_above: true,
                percent_on_slider: false,
                titlebar: TitlebarMode::Strip,
            },
        ] {
            let encoded = encode(&original);
            let decoded = parse_lines(&encoded).expect("re-parses");
            assert_eq!(decoded, original);
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
    fn titlebar_defaults_to_headerbar() {
        assert_eq!(Settings::default().titlebar, TitlebarMode::HeaderBar);
        assert_eq!(parse_lines("").unwrap().titlebar, TitlebarMode::HeaderBar);
    }

    #[test]
    fn parses_titlebar_mode() {
        for (input, expected) in [
            ("headerbar", TitlebarMode::HeaderBar),
            ("strip", TitlebarMode::Strip),
        ] {
            let s = parse_lines(&format!("titlebar {input}")).unwrap();
            assert_eq!(s.titlebar, expected, "input `{input}`");
        }
        for bad in ["HeaderBar", "STRIP", "bar", "true", "none", "1"] {
            let err =
                parse_lines(&format!("titlebar {bad}")).expect_err("should reject unknown mode");
            assert!(matches!(err, Error::BadLine { .. }), "input `{bad}`");
        }
    }

    #[test]
    fn load_from_missing_file_returns_default() {
        let path = std::env::temp_dir().join("bnksound_test_does_not_exist.conf");
        let _ = std::fs::remove_file(&path);
        let s = load_from(&path).expect("missing file ok");
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let tmpdir = std::env::temp_dir().join("bnksound_settings_roundtrip");
        std::fs::create_dir_all(&tmpdir).expect("mkdir");
        let path = tmpdir.join("settings.conf");
        let original = Settings {
            show_window_border: true,
            show_sidebar: false,
            ..Settings::default()
        };
        save_to(&path, &original).expect("save");
        let loaded = load_from(&path).expect("load");
        assert_eq!(loaded, original);
        std::fs::remove_file(&path).expect("cleanup");
    }
}
