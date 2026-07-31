//! Pick an application icon by name.
//!
//! A desktop entry names an app's icon outright, and reading one costs host
//! filesystem access a sandbox is not given. The icon themes themselves come
//! free: Flatpak mounts the host's at /run/host/share/icons whether the app
//! asks or not. So the icon is found by its name instead, matching what the
//! app calls itself against what the icons are called, on letters and digits
//! with everything else dropped.
//!
//! Containment is tested both ways because either side carries the extra
//! word: "helium" against helium-browser, and "google-chrome-stable" against
//! google-chrome. Where several match, the shortest name wins, being the one
//! that added least to the app's own.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

/// How deep an icon tree nests before the walk gives up. Themes disagree on
/// the order: hicolor is theme/48x48/apps, breeze is theme/apps/48.
const MAX_DEPTH: usize = 4;

/// Shorter than this and containment stops meaning anything, since a needle
/// of one or two letters is inside half the icon set.
const MIN_NEEDLE: usize = 3;

/// The icon whose name best fits any of `needles`, or None.
pub(crate) fn icon_name_for(needles: &[&str]) -> Option<&'static str> {
    best_match(names(), needles)
}

fn best_match<'a>(names: &'a [String], needles: &[&str]) -> Option<&'a str> {
    let needles: Vec<String> = needles
        .iter()
        .map(|needle| normalized(needle))
        .filter(|needle| needle.len() >= MIN_NEEDLE)
        .collect();
    if needles.is_empty() {
        return None;
    }

    names
        .iter()
        .filter(|name| {
            let name = normalized(name);
            needles
                .iter()
                .any(|needle| name.contains(needle.as_str()) || needle.contains(name.as_str()))
        })
        // Ties break alphabetically so the answer never depends on the order
        // a directory happened to be read in.
        .min_by_key(|name| (name.len(), name.as_str()))
        .map(String::as_str)
}

/// Every application icon name reachable from the data roots, gathered once.
fn names() -> &'static [String] {
    static CACHE: OnceLock<Vec<String>> = OnceLock::new();
    CACHE.get_or_init(collect)
}

fn collect() -> Vec<String> {
    let mut found = HashSet::new();
    for root in crate::xdg::dirs::data_roots() {
        walk(&root.join("icons"), 0, false, &mut found);
        // The legacy flat dir holds app icons and nothing else, so everything
        // in it counts without looking for an apps subdir first.
        walk(&root.join("pixmaps"), MAX_DEPTH, true, &mut found);
    }
    let mut names: Vec<String> = found.into_iter().collect();
    names.sort();
    names
}

/// Collect icon names under `dir`, descending until an `apps` directory is
/// reached and taking every icon below it. Directories are read rather than
/// stat'd per entry, so a theme with thousands of files costs one pass.
fn walk(dir: &Path, depth: usize, in_apps: bool, found: &mut HashSet<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if depth < MAX_DEPTH {
                let is_apps = in_apps || entry.file_name() == OsStr::new("apps");
                walk(&entry.path(), depth + 1, is_apps, found);
            }
        } else if in_apps && let Some(name) = icon_name(&entry.path()) {
            found.insert(name);
        }
    }
}

fn icon_name(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?;
    if !matches!(extension, "png" | "svg" | "xpm") {
        return None;
    }
    let name = path.file_stem()?.to_str()?;
    // Symbolic icons are monochrome toolbar glyphs, never a recognisable app
    // icon, and they shadow the real one whenever a name matches both.
    if name.ends_with("-symbolic") {
        return None;
    }
    Some(name.to_string())
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names taken from a real desktop: the two shapes that defeat matching in
    /// one direction only, plus the symbolic variant that shadows mpv.
    fn universe() -> Vec<String> {
        [
            "helium-browser",
            "spotify-launcher",
            "mpv",
            "google-chrome",
            "firefox",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn an_icon_may_carry_the_extra_word() {
        let names = universe();
        assert_eq!(best_match(&names, &["Helium"]), Some("helium-browser"));
        assert_eq!(best_match(&names, &["Spotify"]), Some("spotify-launcher"));
    }

    #[test]
    fn so_may_the_app() {
        let names = universe();
        assert_eq!(
            best_match(&names, &["google-chrome-stable"]),
            Some("google-chrome")
        );
    }

    #[test]
    fn the_shortest_match_wins() {
        let names = vec!["mpv".to_string(), "mpv-nightly".to_string()];
        assert_eq!(best_match(&names, &["mpv"]), Some("mpv"));
    }

    #[test]
    fn a_needle_too_short_to_mean_anything_is_ignored() {
        let names = universe();
        assert_eq!(
            best_match(&names, &["m"]),
            None,
            "one letter is inside everything"
        );
        assert_eq!(best_match(&names, &[""]), None);
    }

    #[test]
    fn nothing_matches_nothing() {
        assert_eq!(best_match(&universe(), &["libreoffice"]), None);
    }

    #[test]
    fn punctuation_and_case_do_not_matter() {
        let names = vec!["org.kde.dolphin".to_string()];
        assert_eq!(
            best_match(&names, &["org.kde.Dolphin"]),
            Some("org.kde.dolphin")
        );
        assert_eq!(
            best_match(&names, &["OrgKdeDolphin"]),
            Some("org.kde.dolphin")
        );
    }

    #[test]
    fn symbolic_variants_are_not_app_icons() {
        assert_eq!(icon_name(Path::new("/t/mpv-symbolic.svg")), None);
        assert_eq!(icon_name(Path::new("/t/mpv.svg")).as_deref(), Some("mpv"));
    }

    #[test]
    fn only_image_files_count() {
        assert_eq!(icon_name(Path::new("/t/index.theme")), None);
        assert_eq!(icon_name(Path::new("/t/mpv.png")).as_deref(), Some("mpv"));
        assert_eq!(icon_name(Path::new("/t/mpv.xpm")).as_deref(), Some("mpv"));
    }
}
