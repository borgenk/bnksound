// Derived from `freedesktop-icons` v0.4.0
//   https://github.com/oknozor/freedesktop-icons
// The upstream crate was vendored into this tree and stripped down to just
// the hicolor name+size lookup path the resolver uses. Its MIT notice:
//
//   MIT License
//
//   Copyright (c) 2022 Paul Delafosse
//
//   Permission is hereby granted, free of charge, to any person obtaining a
//   copy of this software and associated documentation files (the "Software"),
//   to deal in the Software without restriction, including without limitation
//   the rights to use, copy, modify, merge, publish, distribute, sublicense,
//   and/or sell copies of the Software, and to permit persons to whom the
//   Software is furnished to do so, subject to the following conditions:
//
//   The above copyright notice and this permission notice shall be included in
//   all copies or substantial portions of the Software.
//
//   THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//   IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//   FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//   AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//   LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
//   FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
//   DEALINGS IN THE SOFTWARE.

//! Minimal freedesktop icon lookup: app icons in the hicolor theme by name and
//! size. Walks hicolor dirs (exact size match, then nearest), then falls back
//! to `$base/<name>.{png,svg,xpm}` and `/usr/share/pixmaps/`.
//!
//! Spec: https://specifications.freedesktop.org/icon-theme-spec/latest/

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

/// Resolve `name` to an icon path at roughly `size` pixels, or `None`.
pub fn lookup(name: &str, size: u16) -> Option<PathBuf> {
    let key = CacheKey {
        name: name.to_string(),
        size,
    };
    if let Some(hit) = cache().read().ok().and_then(|c| c.get(&key).cloned()) {
        return hit;
    }
    let result = find_uncached(name, size);
    if let Ok(mut c) = cache().write() {
        c.insert(key, result.clone());
    }
    result
}

#[derive(Hash, Eq, PartialEq)]
struct CacheKey {
    name: String,
    size: u16,
}

fn cache() -> &'static RwLock<HashMap<CacheKey, Option<PathBuf>>> {
    static C: OnceLock<RwLock<HashMap<CacheKey, Option<PathBuf>>>> = OnceLock::new();
    C.get_or_init(|| RwLock::new(HashMap::new()))
}

fn find_uncached(name: &str, size: u16) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    if let Some(absolute) = absolute_passthrough(name) {
        return Some(absolute);
    }

    for theme in hicolor_themes() {
        // Exact size match first, then nearest, per the spec.
        let exact: Vec<&IconDir> = theme
            .dirs
            .iter()
            .filter(|d| d.matches_size(size, 1))
            .collect();
        for dir in &exact {
            if let Some(p) = try_extensions(&theme.path.join(dir.name.as_str()), name) {
                return Some(p);
            }
        }

        let mut by_distance: Vec<(&IconDir, i32)> = theme
            .dirs
            .iter()
            .filter(|d| !exact.iter().any(|e| std::ptr::eq(*e, *d)))
            .map(|d| (d, d.size_distance(size, 1)))
            .collect();
        by_distance.sort_by_key(|(_, dist)| *dist);
        for (dir, _) in by_distance {
            if let Some(p) = try_extensions(&theme.path.join(dir.name.as_str()), name) {
                return Some(p);
            }
        }
    }

    for base in icon_base_paths() {
        if let Some(p) = try_extensions(&base, name) {
            return Some(p);
        }
    }
    try_extensions(Path::new("/usr/share/pixmaps"), name)
}

fn absolute_passthrough(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.is_absolute() && p.exists() {
        Some(p.to_path_buf())
    } else {
        None
    }
}

fn try_extensions(dir: &Path, name: &str) -> Option<PathBuf> {
    for ext in ["png", "svg", "xpm"] {
        let candidate = dir.join(format!("{name}.{ext}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Themes
// ---------------------------------------------------------------------------

struct HicolorTheme {
    path: PathBuf,
    dirs: Vec<IconDir>,
}

fn hicolor_themes() -> &'static [HicolorTheme] {
    static C: OnceLock<Vec<HicolorTheme>> = OnceLock::new();
    C.get_or_init(|| {
        let mut out = Vec::new();
        for base in icon_base_paths() {
            let theme = base.join("hicolor");
            let index = theme.join("index.theme");
            if !index.is_file() {
                continue;
            }
            let dirs = parse_index(&index);
            out.push(HicolorTheme { path: theme, dirs });
        }
        out
    })
}

/// Returns the XDG icon base paths in priority order, deduplicated and
/// filtered to those that actually exist.
fn icon_base_paths() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if p.is_dir() && !out.contains(&p) {
            out.push(p);
        }
    };

    if let Some(home) = std::env::var_os("HOME") {
        push(PathBuf::from(&home).join(".icons"));
    }

    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    if let Some(p) = data_home {
        push(p.join("icons"));
    }

    let dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    for d in dirs.split(':') {
        if d.is_empty() {
            continue;
        }
        push(PathBuf::from(d).join("icons"));
    }

    out
}

// ---------------------------------------------------------------------------
// index.theme parsing
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct IconDir {
    name: String,
    size: i32,
    scale: i32,
    type_: DirType,
    min_size: i32,
    max_size: i32,
    threshold: i32,
}

#[derive(Debug, Clone, Copy, Default)]
enum DirType {
    Fixed,
    Scalable,
    #[default]
    Threshold,
}

impl IconDir {
    fn matches_size(&self, size: u16, scale: u16) -> bool {
        let size = size as i32;
        let scale = scale as i32;
        if self.scale != scale {
            return false;
        }
        match self.type_ {
            DirType::Fixed => self.size == size,
            DirType::Scalable => self.min_size <= size && size <= self.max_size,
            DirType::Threshold => {
                self.size - self.threshold <= size && size <= self.size + self.threshold
            }
        }
    }

    /// Distance metric to rank subdirs that didn't match exactly; smaller is
    /// better.
    fn size_distance(&self, size: u16, scale: u16) -> i32 {
        let scaled_size = self.size * self.scale;
        let min_scaled = self.min_size * self.scale;
        let max_scaled = self.max_size * self.scale;
        let scale = scale as i32;
        let size = size as i32;
        let req = size * scale;
        let raw = match self.type_ {
            DirType::Fixed => scaled_size - req,
            DirType::Scalable => {
                if req < min_scaled {
                    min_scaled - req
                } else if req > max_scaled {
                    req - max_scaled
                } else {
                    0
                }
            }
            DirType::Threshold => {
                if req < (self.size - self.threshold) * scale {
                    min_scaled - req
                } else if req > (self.size + self.threshold) * scale {
                    req - max_scaled
                } else {
                    0
                }
            }
        };
        raw.abs()
    }
}

/// Accumulator for one `index.theme` `[<dir>]` section, flushed into an
/// [`IconDir`] at the next section header.
#[derive(Default)]
struct PendingDir<'a> {
    section: Option<&'a str>,
    size: Option<i32>,
    scale: Option<i32>,
    type_: DirType,
    min_size: Option<i32>,
    max_size: Option<i32>,
    threshold: Option<i32>,
}

impl PendingDir<'_> {
    /// Push as an [`IconDir`] if it names a real directory (has a `Size=` and
    /// isn't the `[Icon Theme]` header).
    fn flush(self, out: &mut Vec<IconDir>) {
        if let (Some(name), Some(sz)) = (self.section, self.size)
            && name != "Icon Theme"
        {
            out.push(IconDir {
                name: name.to_string(),
                size: sz,
                scale: self.scale.unwrap_or(1),
                type_: self.type_,
                min_size: self.min_size.unwrap_or(sz),
                max_size: self.max_size.unwrap_or(sz),
                threshold: self.threshold.unwrap_or(2),
            });
        }
    }
}

fn parse_index(path: &Path) -> Vec<IconDir> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let mut cur = PendingDir::default();

    for raw in content.lines() {
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            std::mem::take(&mut cur).flush(&mut out);
            let name = rest.rsplit_once(']').map(|(n, _)| n).unwrap_or(rest);
            cur.section = Some(name);
            continue;
        }
        let Some(name) = cur.section else { continue };
        if name == "Icon Theme" {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "Size" => cur.size = value.parse().ok(),
            "Scale" => cur.scale = value.parse().ok(),
            "MinSize" => cur.min_size = value.parse().ok(),
            "MaxSize" => cur.max_size = value.parse().ok(),
            "Threshold" => cur.threshold = value.parse().ok(),
            "Type" => {
                cur.type_ = match value {
                    "Fixed" => DirType::Fixed,
                    "Scalable" => DirType::Scalable,
                    _ => DirType::Threshold,
                }
            }
            _ => {}
        }
    }
    cur.flush(&mut out);

    // We don't enforce the `Directories=` list under `[Icon Theme]`; every
    // section past it is a declared dir per the spec, and sections without a
    // `Size=` get filtered out above.
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TmpDir(PathBuf);
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fresh_tmp() -> TmpDir {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "bnk_icons_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TmpDir(dir)
    }

    fn write_file(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn parse_index_basic() {
        let tmp = fresh_tmp();
        let path = tmp.0.join("index.theme");
        write_file(
            &path,
            "[Icon Theme]\n\
             Name=hicolor\n\
             Directories=48x48/apps,scalable/apps\n\
             \n\
             [48x48/apps]\n\
             Size=48\n\
             Type=Threshold\n\
             \n\
             [scalable/apps]\n\
             Size=128\n\
             MinSize=8\n\
             MaxSize=512\n\
             Type=Scalable\n",
        );
        let dirs = parse_index(&path);
        assert_eq!(dirs.len(), 2);
        assert_eq!(dirs[0].name, "48x48/apps");
        assert_eq!(dirs[0].size, 48);
        assert!(matches!(dirs[0].type_, DirType::Threshold));
        assert_eq!(dirs[1].name, "scalable/apps");
        assert_eq!(dirs[1].min_size, 8);
        assert_eq!(dirs[1].max_size, 512);
        assert!(matches!(dirs[1].type_, DirType::Scalable));
    }

    #[test]
    fn dir_size_matching() {
        let fixed = IconDir {
            name: "48".into(),
            size: 48,
            scale: 1,
            type_: DirType::Fixed,
            min_size: 48,
            max_size: 48,
            threshold: 2,
        };
        assert!(fixed.matches_size(48, 1));
        assert!(!fixed.matches_size(49, 1));

        let thresh = IconDir {
            name: "32".into(),
            size: 32,
            scale: 1,
            type_: DirType::Threshold,
            min_size: 32,
            max_size: 32,
            threshold: 2,
        };
        assert!(thresh.matches_size(31, 1));
        assert!(thresh.matches_size(34, 1));
        assert!(!thresh.matches_size(48, 1));

        let scal = IconDir {
            name: "any".into(),
            size: 128,
            scale: 1,
            type_: DirType::Scalable,
            min_size: 16,
            max_size: 256,
            threshold: 2,
        };
        assert!(scal.matches_size(16, 1));
        assert!(scal.matches_size(48, 1));
        assert!(!scal.matches_size(512, 1));
    }

    #[test]
    fn try_extensions_prefers_png() {
        let tmp = fresh_tmp();
        let dir = &tmp.0;
        write_file(&dir.join("foo.svg"), "<svg/>");
        write_file(&dir.join("foo.png"), "");
        let got = super::try_extensions(dir, "foo").unwrap();
        assert!(got.ends_with("foo.png"), "got {got:?}");
    }
}
