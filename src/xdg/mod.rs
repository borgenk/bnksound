//! Resolve PipeWire audio streams to an application icon.
//!
//! A stream arrives naming itself: an application.name, usually a binary,
//! sometimes an id. What it never carries is an icon, since no app sets
//! application.icon_name in practice. The icon is found by matching those
//! names against the installed icon themes, which is the one part of the
//! desktop a sandbox can read without asking for anything.

mod dirs;
mod guess;
mod icons;

use std::path::PathBuf;

/// What a stream says about itself, in the order it is worth trusting. Every
/// field is a name the app chose, so all of them are tried.
#[derive(Debug, Default)]
pub struct Hints<'a> {
    pub app_id: Option<&'a str>,
    pub portal_app_id: Option<&'a str>,
    pub binary: Option<&'a str>,
    pub wm_class: Option<&'a str>,
    pub app_name: Option<&'a str>,
}

/// Row icons are drawn at 48 logical pixels; the theme lookup takes the exact
/// size where a theme has it and the nearest otherwise.
const ICON_SIZE: u16 = 48;

/// The icon for the app behind a stream, or None when nothing matches.
pub fn icon_for(hints: &Hints<'_>) -> Option<PathBuf> {
    let needles: Vec<&str> = [
        hints.app_id,
        hints.portal_app_id,
        hints.binary,
        hints.wm_class,
        hints.app_name,
    ]
    .into_iter()
    .flatten()
    .collect();

    icons::lookup(guess::icon_name_for(&needles)?, ICON_SIZE)
}
