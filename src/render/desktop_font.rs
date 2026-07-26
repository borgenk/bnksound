//! The desktop's configured UI font.
//!
//! GTK gets this from the settings portal on Wayland, so asking the portal is
//! what makes the two shells agree on a typeface. The answer is a Pango font
//! description, "Family Style Size", where everything before a trailing number
//! is the family.
//!
//! Every step is optional. No portal, no session bus, or no setting all land on
//! the same generic family, which fontconfig then resolves to whatever the
//! system considers its sans-serif.

use crate::dbus::connection::Connection;
use crate::dbus::wire::{MethodCall, Value};

/// Used when nothing configured is readable. fontconfig maps it to the
/// system's default sans face.
pub const FALLBACK_FAMILY: &str = "sans-serif";

const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PORTAL_IFACE: &str = "org.freedesktop.portal.Settings";
const SETTINGS_NAMESPACE: &str = "org.gnome.desktop.interface";
const FONT_KEY: &str = "font-name";
const FONT_KEY_INI: &str = "gtk-font-name";
const DPI_KEY_INI: &str = "gtk-xft-dpi";

/// The font DPI a session assumes when nothing configures one.
pub const DEFAULT_DPI: f32 = 96.0;

/// gtk-xft-dpi holds the DPI scaled by this, so 96 DPI is stored as 98304.
const XFT_DPI_SCALE: f32 = 1024.0;

/// The configured UI font family, or [`FALLBACK_FAMILY`].
pub fn family() -> String {
    portal_font_name()
        .or_else(gtk_settings_font_name)
        .as_deref()
        .map(parse_family)
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| FALLBACK_FAMILY.to_string())
}

/// Read font-name from the settings portal.
fn portal_font_name() -> Option<String> {
    let mut conn = Connection::session().ok()?;
    let reply = conn
        .call(&MethodCall {
            destination: PORTAL_NAME,
            path: PORTAL_PATH,
            interface: PORTAL_IFACE,
            member: "Read",
            args: &[SETTINGS_NAMESPACE, FONT_KEY],
        })
        .ok()?;
    // The reply nests the value in a variant twice; as_str sees through both.
    reply
        .body_values()?
        .first()
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The desktop's font DPI.
///
/// Turning a pixel size into points needs this, and points are what a variable
/// font's optical size axis is measured in.
pub fn dpi() -> f32 {
    gtk_setting(DPI_KEY_INI)
        .and_then(|value| value.parse::<f32>().ok())
        .map(|scaled| scaled / XFT_DPI_SCALE)
        .filter(|dpi| *dpi > 0.0)
        .unwrap_or(DEFAULT_DPI)
}

/// Read gtk-font-name from the GTK settings file, which is where a session
/// without a portal usually keeps it.
fn gtk_settings_font_name() -> Option<String> {
    gtk_setting(FONT_KEY_INI)
}

/// Read one key from the GTK settings file.
fn gtk_setting(key: &str) -> Option<String> {
    let path = crate::config_path_in("gtk-4.0", "settings.ini")?;
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .filter_map(|line| line.split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, value)| value.trim().to_string())
}

/// Take the family out of a Pango font description.
///
/// The description ends with an optional size and may carry style words before
/// it, e.g. "Cantarell 11" or "Segoe UI Variable Bold 10". Trailing numbers and
/// style words are dropped; what remains is the family.
fn parse_family(description: &str) -> String {
    // A comma separates the family list from the style and size, and may also
    // separate alternative families. The first entry is the one that was asked
    // for; leaving the comma attached makes the name match nothing, and the
    // lookup falls back to a default sans without saying so.
    let description = description.split(',').next().unwrap_or(description);
    let mut words: Vec<&str> = description.split_whitespace().collect();

    // A trailing size, which may be fractional.
    if let Some(last) = words.last()
        && last.parse::<f32>().is_ok()
    {
        words.pop();
    }

    // Style words Pango would have parsed as attributes rather than family.
    const STYLES: &[&str] = &[
        "thin",
        "ultralight",
        "extralight",
        "light",
        "semilight",
        "book",
        "regular",
        "medium",
        "semibold",
        "demibold",
        "bold",
        "ultrabold",
        "extrabold",
        "heavy",
        "black",
        "italic",
        "oblique",
        "condensed",
        "expanded",
    ];
    while let Some(last) = words.last()
        && STYLES.contains(&last.to_ascii_lowercase().as_str())
    {
        words.pop();
    }

    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_family_and_size_splits_on_the_size() {
        assert_eq!(parse_family("Cantarell 11"), "Cantarell");
        assert_eq!(parse_family("Inter 10"), "Inter");
    }

    /// A comma ends the family. Keeping it attached yields a name fontconfig
    /// matches nothing for, and the lookup then falls back to a default sans
    /// without reporting anything, so the app quietly draws in the wrong
    /// typeface.
    #[test]
    fn a_comma_ends_the_family() {
        assert_eq!(parse_family("Segoe UI Variable,  10"), "Segoe UI Variable");
        assert_eq!(parse_family("Cantarell, 11"), "Cantarell");
        assert_eq!(parse_family("Inter,Bold 10"), "Inter");
        // No comma is still handled by the size and style stripping.
        assert_eq!(parse_family("Segoe UI Variable 10"), "Segoe UI Variable");
    }

    #[test]
    fn a_multi_word_family_keeps_all_its_words() {
        // The real value on the machine this was written for, double space and
        // all, which split_whitespace collapses.
        assert_eq!(parse_family("Segoe UI Variable  10"), "Segoe UI Variable");
        assert_eq!(parse_family("Noto Sans Display 12"), "Noto Sans Display");
    }

    #[test]
    fn a_fractional_size_is_still_a_size() {
        assert_eq!(parse_family("Cantarell 11.5"), "Cantarell");
    }

    #[test]
    fn style_words_are_not_part_of_the_family() {
        assert_eq!(
            parse_family("Segoe UI Variable Bold 10"),
            "Segoe UI Variable"
        );
        assert_eq!(parse_family("Cantarell Italic 11"), "Cantarell");
        assert_eq!(parse_family("Inter Semibold"), "Inter");
    }

    #[test]
    fn a_family_with_no_size_is_taken_whole() {
        assert_eq!(parse_family("Cantarell"), "Cantarell");
        assert_eq!(parse_family("Segoe UI Variable"), "Segoe UI Variable");
    }

    #[test]
    fn a_family_whose_name_ends_in_a_number_keeps_it_only_when_it_is_not_the_size() {
        // "Source Sans 3 11" is the family "Source Sans 3" at size 11. Dropping
        // one trailing number is what separates them.
        assert_eq!(parse_family("Source Sans 3 11"), "Source Sans 3");
    }

    #[test]
    fn an_empty_description_yields_nothing_so_the_caller_falls_back() {
        assert_eq!(parse_family(""), "");
        assert_eq!(parse_family("   "), "");
    }

    #[test]
    fn family_always_names_something_usable() {
        // Whatever this machine reports, the result is never empty, which is
        // what keeps font loading from having a no-family case.
        assert!(!family().is_empty());
    }
}
