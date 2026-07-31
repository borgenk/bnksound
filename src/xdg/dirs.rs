//! Where icons are looked for.
//!
//! The XDG variables answer this on a normal session. In a Flatpak sandbox
//! they answer for the sandbox, but the answer stays usable anyway: Flatpak
//! binds the host's icon themes at /run/host/share/icons and
//! /run/host/user-share/icons and names both in XDG_DATA_DIRS, for every app,
//! with no permission asked. So the host's icons arrive through the ordinary
//! XDG_DATA_DIRS walk and nothing here needs to know it is sandboxed.

use std::env;
use std::path::PathBuf;

/// The XDG data roots to search, highest priority first, deduplicated. Roots
/// that do not exist stay in the list; callers already skip what they cannot
/// read.
pub(crate) fn data_roots() -> Vec<PathBuf> {
    roots(&Dirs {
        data_home: non_empty_var("XDG_DATA_HOME"),
        home: non_empty_var("HOME"),
        data_dirs: non_empty_var("XDG_DATA_DIRS"),
    })
}

/// What the roots are built from, taken apart from the environment so the
/// order can be tested without one.
struct Dirs {
    data_home: Option<String>,
    home: Option<String>,
    data_dirs: Option<String>,
}

/// The spec's default when XDG_DATA_DIRS is unset or empty.
const DEFAULT_DATA_DIRS: &str = "/usr/local/share:/usr/share";

/// Where Flatpak exports the entries and icons of installed apps. A desktop
/// session usually has both in XDG_DATA_DIRS already; naming them keeps them
/// searched when it does not, and from inside a sandbox where XDG_DATA_DIRS is
/// the runtime's.
const FLATPAK_SYSTEM_EXPORTS: &str = "/var/lib/flatpak/exports/share";
const FLATPAK_USER_EXPORTS: &str = ".local/share/flatpak/exports/share";

fn roots(dirs: &Dirs) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |path: PathBuf| {
        if !out.contains(&path) {
            out.push(path);
        }
    };

    if let Some(data_home) = &dirs.data_home {
        push(PathBuf::from(data_home));
    }
    if let Some(home) = &dirs.home {
        push(PathBuf::from(home).join(".local/share"));
    }

    let data_dirs = dirs.data_dirs.as_deref().unwrap_or(DEFAULT_DATA_DIRS);
    for segment in data_dirs.split(':').filter(|s| !s.is_empty()) {
        push(PathBuf::from(segment));
    }

    if let Some(home) = &dirs.home {
        push(PathBuf::from(home).join(FLATPAK_USER_EXPORTS));
    }
    push(PathBuf::from(FLATPAK_SYSTEM_EXPORTS));

    out
}

fn non_empty_var(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn session() -> Dirs {
        Dirs {
            data_home: Some("/home/u/.local/share".into()),
            home: Some("/home/u".into()),
            data_dirs: Some("/usr/share".into()),
        }
    }

    #[test]
    fn the_data_home_leads_and_repeats_are_dropped() {
        let roots = roots(&session());
        assert_eq!(roots.first(), Some(&PathBuf::from("/home/u/.local/share")));
        assert_eq!(
            roots
                .iter()
                .filter(|r| *r == Path::new("/home/u/.local/share"))
                .count(),
            1,
            "the data home and the default under HOME are the same path here",
        );
    }

    /// A sandbox points XDG_DATA_HOME at the app's private dir, so the real
    /// one has to be reached through HOME instead.
    #[test]
    fn a_redirected_data_home_keeps_the_real_one_searched() {
        let mut dirs = session();
        dirs.data_home = Some("/home/u/.var/app/io.github.borgenk.BnkSound/data".into());

        let roots = roots(&dirs);
        assert!(roots.contains(&PathBuf::from("/home/u/.local/share")));
    }

    /// Flatpak names its own bind mounts in XDG_DATA_DIRS, so the host's icons
    /// arrive with no special casing.
    #[test]
    fn the_hosts_icon_dirs_ride_in_on_the_data_dirs() {
        let mut dirs = session();
        dirs.data_dirs = Some("/app/share:/usr/share:/run/host/user-share:/run/host/share".into());

        let roots = roots(&dirs);
        for host in ["/run/host/share", "/run/host/user-share"] {
            assert!(roots.contains(&PathBuf::from(host)), "{host}");
        }
    }

    #[test]
    fn flatpak_exports_are_searched() {
        let roots = roots(&session());
        assert!(roots.contains(&PathBuf::from(FLATPAK_SYSTEM_EXPORTS)));
        assert!(roots.contains(&PathBuf::from("/home/u").join(FLATPAK_USER_EXPORTS)));
    }

    #[test]
    fn an_absent_environment_still_yields_the_spec_defaults() {
        let roots = roots(&Dirs {
            data_home: None,
            home: None,
            data_dirs: None,
        });
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
                PathBuf::from(FLATPAK_SYSTEM_EXPORTS),
            ]
        );
    }
}
