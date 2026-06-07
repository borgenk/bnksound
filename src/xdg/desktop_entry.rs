//! Minimal reader for freedesktop `.desktop` entries.
//!
//! v1 parses only the `[Desktop Entry]` group, a handful of keys, and no
//! locale or escape-sequence handling. Files that fail to read or parse are
//! silently skipped so a single broken entry never blocks lookup.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;

pub struct DesktopEntry {
    pub path: PathBuf,
    id: String,
    name: Option<String>,
    icon: Option<String>,
    exec: Option<String>,
    startup_wm_class: Option<String>,
}

impl DesktopEntry {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn icon(&self) -> Option<&str> {
        self.icon.as_deref()
    }

    pub fn exec(&self) -> Option<&str> {
        self.exec.as_deref()
    }

    pub fn startup_wm_class(&self) -> Option<&str> {
        self.startup_wm_class.as_deref()
    }

    pub fn matches_id(&self, candidate: &str) -> bool {
        self.id.eq_ignore_ascii_case(candidate)
    }
}

pub fn desktop_entries() -> Vec<DesktopEntry> {
    let mut entries = Vec::new();
    for dir in application_dirs() {
        let Ok(read_dir) = fs::read_dir(&dir) else {
            continue;
        };
        for file in read_dir.flatten() {
            let path = file.path();
            if path.extension() == Some(OsStr::new("desktop"))
                && let Some(entry) = parse(path)
            {
                entries.push(entry);
            }
        }
    }
    entries
}

/// The `applications` subdir of `$XDG_DATA_HOME` (or `$HOME/.local/share`)
/// followed by the `applications` subdir of each `$XDG_DATA_DIRS` segment.
fn application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(home) = non_empty_var("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(home).join("applications"));
    } else if let Some(home) = non_empty_var("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }

    let data_dirs =
        non_empty_var("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    for segment in data_dirs.split(':').filter(|s| !s.is_empty()) {
        dirs.push(PathBuf::from(segment).join("applications"));
    }

    dirs
}

fn non_empty_var(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

fn parse(path: PathBuf) -> Option<DesktopEntry> {
    let id = path.file_stem()?.to_str()?.to_string();
    let contents = fs::read_to_string(&path).ok()?;

    let mut in_group = false;
    let mut name = None;
    let mut icon = None;
    let mut exec = None;
    let mut startup_wm_class = None;

    for line in contents.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(header) = group_header(trimmed) {
            in_group = header == "Desktop Entry";
            continue;
        }

        if !in_group {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // Locale-suffixed keys (e.g. `Name[fr]`) are ignored in v1.
        let value = value.trim_start().to_string();
        match key.trim() {
            "Name" => name = Some(value),
            "Icon" => icon = Some(value),
            "Exec" => exec = Some(value),
            "StartupWMClass" => startup_wm_class = Some(value),
            _ => {}
        }
    }

    Some(DesktopEntry {
        path,
        id,
        name,
        icon,
        exec,
        startup_wm_class,
    })
}

/// A group header is a line bracketed as `[Group Name]`; returns the name.
fn group_header(line: &str) -> Option<&str> {
    line.strip_prefix('[')?.strip_suffix(']')
}
