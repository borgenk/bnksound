//! Resolve PipeWire audio streams to their freedesktop desktop entries.
//!
//! Apps set PipeWire Node hints inconsistently, so we try the most reliable
//! ones first (app_id, WMClass) and fall back to fuzzier matches.

mod desktop_entry;
mod icons;

use std::path::PathBuf;
use std::sync::OnceLock;

use desktop_entry::DesktopEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XdgInfo {
    pub name: String,
    pub icon_path: Option<PathBuf>,
    pub desktop_path: PathBuf,
}

#[derive(Debug, Default)]
pub struct Hints<'a> {
    pub app_id: Option<&'a str>,
    pub portal_app_id: Option<&'a str>,
    pub binary: Option<&'a str>,
    pub wm_class: Option<&'a str>,
    pub app_name: Option<&'a str>,
}

pub fn lookup(hints: &Hints<'_>) -> Option<XdgInfo> {
    let entries = entries();
    let entry = find_entry(entries, hints)?;
    let raw_name = entry
        .name()
        .map(str::to_string)
        .unwrap_or_else(|| entry.id().to_string());
    let icon_path = entry.icon().and_then(resolve_icon);

    Some(XdgInfo {
        name: clean_name(raw_name),
        icon_path,
        desktop_path: entry.path.clone(),
    })
}

fn find_entry<'a>(entries: &'a [DesktopEntry], hints: &Hints<'_>) -> Option<&'a DesktopEntry> {
    // 1. Exact app_id match (well-behaved apps; portal forwards it too).
    for id in [hints.app_id, hints.portal_app_id].into_iter().flatten() {
        if let Some(e) = entries.iter().find(|e| e.matches_id(id)) {
            return Some(e);
        }
    }

    // 2. StartupWMClass match via window.x11.wm_class.
    if let Some(cls) = hints.wm_class
        && let Some(e) = entries.iter().find(|e| wm_class_eq(e, cls))
    {
        return Some(e);
    }

    // 3. binary as a .desktop id (e.g. "spotify" -> spotify.desktop).
    if let Some(bin) = hints.binary
        && let Some(e) = entries.iter().find(|e| e.matches_id(bin))
    {
        return Some(e);
    }

    // 4. binary or application.name vs StartupWMClass. Catches wrapper packages
    //    whose .desktop id mismatches the binary but StartupWMClass matches
    //    (e.g. spotify-launcher.desktop has StartupWMClass=spotify).
    for cls in [hints.binary, hints.app_name].into_iter().flatten() {
        if let Some(e) = entries.iter().find(|e| wm_class_eq(e, cls)) {
            return Some(e);
        }
    }

    // 5. binary as basename of Exec=.
    if let Some(bin) = hints.binary
        && let Some(e) = entries.iter().find(|e| exec_basename_matches(e, bin))
    {
        return Some(e);
    }

    None
}

fn wm_class_eq(entry: &DesktopEntry, value: &str) -> bool {
    entry
        .startup_wm_class()
        .is_some_and(|c| c.eq_ignore_ascii_case(value))
}

/// Strip trailing "(Launcher)"/"Launcher" so wrapper packages display as just
/// the app name. Only chops the known suffixes and never empties the string.
fn clean_name(name: String) -> String {
    let trimmed = name.trim_end();
    let lower = trimmed.to_ascii_lowercase();
    for suffix in [" (launcher)", " launcher"] {
        if let Some(rest) = lower.strip_suffix(suffix) {
            let candidate = trimmed[..rest.len()].trim_end();
            if !candidate.is_empty() {
                return candidate.to_string();
            }
        }
    }
    name
}

fn exec_basename_matches(entry: &DesktopEntry, binary: &str) -> bool {
    let Some(exec) = entry.exec() else {
        return false;
    };
    // Basename of the first non-flag token (e.g. `/usr/bin/spotify` in
    // `/usr/bin/spotify --no-zygote %U`).
    let cmd = exec.split_whitespace().find(|t| !t.starts_with('-'));
    let Some(cmd) = cmd else { return false };
    let base = std::path::Path::new(cmd)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(cmd);
    base.eq_ignore_ascii_case(binary)
}

fn resolve_icon(name: &str) -> Option<PathBuf> {
    icons::lookup(name, 48)
}

// ---------------------------------------------------------------------------
// Caches
// ---------------------------------------------------------------------------

fn entries() -> &'static [DesktopEntry] {
    static CACHE: OnceLock<Vec<DesktopEntry>> = OnceLock::new();
    CACHE.get_or_init(desktop_entry::desktop_entries).as_slice()
}
