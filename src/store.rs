//! Unified on-disk state: a binary blob at `state.bin`
//! holding window geometry + active profile, then one page per profile
//! (device presets plus its section filter). Writes go through a
//! temp-file + rename so a crash mid-write can't truncate the store.
//!
//! On-disk format (little-endian, no padding):
//! ```text
//!   magic            4 bytes "BNKZ"
//!   version          u8 (currently 3)
//!   --- header (general state) ---
//!   window width     u32
//!   window height    u32
//!   window maximized u8
//!   active           u8 tag (0 = None, 1 = Some) + u16 len + utf-8
//!   --- profile pages ---
//!   profile count    u32
//!   per profile:
//!       name            u16 len + utf-8
//!       default_sink    u8 tag + (u16 len + utf-8) if Some
//!       sink count      u32
//!       per sink:       u16 len + utf-8 key, f32 volume, u8 muted
//!       app count       u32
//!       per app:        u16 len + utf-8 key, f32 volume, u8 muted,
//!                       u8 tag + (u16 len + utf-8) target_sink_name if Some
//!       default_source  u8 tag + (u16 len + utf-8) if Some
//!       source count    u32
//!       per source:     u16 len + utf-8 key, f32 volume, u8 muted
//!       section filter  3 × u8 (outputs, inputs, apps; 0 = hidden)
//! ```
//!
//! All strings are bounded at `u16::MAX` bytes; the writer fails loudly
//! if the bound is exceeded so the round-trip stays lossless.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::domain::SectionFilter;
use crate::geometry::Geometry;
use crate::profile::{AppSettings, DeviceSettings, Profile, ProfileStore};

const FILENAME: &str = "state.bin";
const MAGIC: &[u8; 4] = b"BNKZ";
const VERSION: u8 = 3;

/// Everything that persists across a restart: window geometry plus the
/// profile store. The active-profile pointer lives in [`ProfileStore::active`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub window: Geometry,
    pub profiles: ProfileStore,
}

/// Errors from loading or saving the state file. Codec variants carry
/// offsets, counts, and profile names to pinpoint a malformed file.
#[derive(Debug)]
pub enum Error {
    Io {
        op: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    NoParentDir(PathBuf),
    NoStatePath,
    InvalidMagic([u8; 4]),
    UnsupportedVersion {
        got: u8,
        want: u8,
    },
    OffsetOverflow,
    UnexpectedEnd {
        pos: usize,
        need: usize,
        have: usize,
    },
    InvalidOptionalTag {
        tag: u8,
        offset: usize,
    },
    InvalidUtf8 {
        offset: usize,
        source: std::str::Utf8Error,
    },
    TrailingBytes(usize),
    TooManyProfiles(usize),
    TooManyEntries {
        profile: String,
        count: usize,
    },
    StringTooLong(usize),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { op, path, .. } => write!(f, "{op} {}", path.display())?,
            Self::NoParentDir(p) => write!(f, "state path has no parent: {}", p.display())?,
            Self::NoStatePath => f.write_str("no XDG_CONFIG_HOME or HOME cannot persist state")?,
            Self::InvalidMagic(b) => write!(f, "invalid state magic: {b:?}")?,
            Self::UnsupportedVersion { got, want } => {
                write!(f, "unsupported state version {got} (expected {want})")?
            }
            Self::OffsetOverflow => f.write_str("state codec offset overflow")?,
            Self::UnexpectedEnd { pos, need, have } => write!(
                f,
                "unexpected end of state data at offset {pos} (need {need} more bytes, have {have})"
            )?,
            Self::InvalidOptionalTag { tag, offset } => {
                write!(f, "invalid optional tag {tag} at offset {offset}")?
            }
            Self::InvalidUtf8 { offset, .. } => {
                write!(f, "invalid utf-8 in state string at offset {offset}")?
            }
            Self::TrailingBytes(n) => write!(f, "trailing {n} bytes after state data")?,
            Self::TooManyProfiles(n) => write!(f, "too many profiles: {n}")?,
            Self::TooManyEntries { profile, count } => {
                write!(f, "too many entries in profile {profile}: {count}")?
            }
            Self::StringTooLong(n) => write!(f, "string too long for state codec: {n} bytes")?,
        }
        // Alternate-mode (`{:#}`) walks the source chain.
        if f.alternate() {
            let mut cur = std::error::Error::source(self);
            while let Some(e) = cur {
                write!(f, ": {e}")?;
                cur = e.source();
            }
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidUtf8 { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Path to the state file, `None` when no config dir resolves.
pub fn state_path() -> Option<PathBuf> {
    crate::config_path(FILENAME)
}

/// Read the state from disk. Missing file returns the default state; a
/// corrupt or wrong-version file is an error (we never silently nuke it).
pub fn load_from(path: &Path) -> Result<State> {
    match fs::read(path) {
        Ok(bytes) => decode(&bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(source) => Err(Error::Io {
            op: "read state",
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Atomic save: write to a sibling temp file, fsync, rename over the
/// target. A crash mid-write leaves the previous good file intact.
pub fn save_to(path: &Path, state: &State) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::NoParentDir(path.to_path_buf()))?;
    fs::create_dir_all(dir).map_err(|source| Error::Io {
        op: "mkdir",
        path: dir.to_path_buf(),
        source,
    })?;

    let bytes = encode(state)?;
    let tmp = path.with_extension("bin.tmp");
    crate::atomic_write(path, &tmp, &bytes).map_err(|e| Error::Io {
        op: e.op,
        path: e.path,
        source: e.source,
    })?;
    Ok(())
}

fn encode(state: &State) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(VERSION);

    // Header: window geometry, then the active-profile pointer.
    out.extend_from_slice(&state.window.width.to_le_bytes());
    out.extend_from_slice(&state.window.height.to_le_bytes());
    out.push(u8::from(state.window.maximized));
    write_opt_string(&mut out, state.profiles.active.as_deref())?;

    let count = u32::try_from(state.profiles.profiles.len())
        .map_err(|_| Error::TooManyProfiles(state.profiles.profiles.len()))?;
    out.extend_from_slice(&count.to_le_bytes());
    for p in &state.profiles.profiles {
        write_string(&mut out, &p.name)?;
        write_opt_string(&mut out, p.default_sink.as_deref())?;
        write_device_map(&mut out, &p.name, &p.sinks)?;
        write_app_map(&mut out, &p.name, &p.apps)?;
        write_opt_string(&mut out, p.default_source.as_deref())?;
        write_device_map(&mut out, &p.name, &p.sources)?;
        out.push(u8::from(p.section_filter.outputs));
        out.push(u8::from(p.section_filter.inputs));
        out.push(u8::from(p.section_filter.apps));
    }
    Ok(out)
}

fn write_device_map(
    out: &mut Vec<u8>,
    profile: &str,
    map: &std::collections::BTreeMap<String, DeviceSettings>,
) -> Result<()> {
    let count = u32::try_from(map.len()).map_err(|_| Error::TooManyEntries {
        profile: profile.to_string(),
        count: map.len(),
    })?;
    out.extend_from_slice(&count.to_le_bytes());
    for (key, settings) in map {
        write_string(out, key)?;
        out.extend_from_slice(&settings.volume.to_le_bytes());
        out.push(u8::from(settings.muted));
    }
    Ok(())
}

fn write_app_map(
    out: &mut Vec<u8>,
    profile: &str,
    map: &std::collections::BTreeMap<String, AppSettings>,
) -> Result<()> {
    let count = u32::try_from(map.len()).map_err(|_| Error::TooManyEntries {
        profile: profile.to_string(),
        count: map.len(),
    })?;
    out.extend_from_slice(&count.to_le_bytes());
    for (key, settings) in map {
        write_string(out, key)?;
        out.extend_from_slice(&settings.volume.to_le_bytes());
        out.push(u8::from(settings.muted));
        write_opt_string(out, settings.target_sink_name.as_deref())?;
    }
    Ok(())
}

fn decode(data: &[u8]) -> Result<State> {
    let mut r = Reader::new(data);
    let magic = r.take(4)?;
    if magic != MAGIC {
        return Err(Error::InvalidMagic([
            magic[0], magic[1], magic[2], magic[3],
        ]));
    }
    let version = r.u8()?;
    // No back-compat: only the current format is accepted.
    if version != VERSION {
        return Err(Error::UnsupportedVersion {
            got: version,
            want: VERSION,
        });
    }

    let width = r.u32()?;
    let height = r.u32()?;
    let maximized = r.u8()? != 0;
    let window = Geometry::clamped(width, height, maximized);

    let active = r.opt_string()?;
    let profile_count = r.u32()? as usize;
    let mut profiles = Vec::with_capacity(profile_count);
    for _ in 0..profile_count {
        let name = r.string()?;
        let default_sink = r.opt_string()?;
        let sinks = read_device_map(&mut r)?;
        let apps = read_app_map(&mut r)?;
        let default_source = r.opt_string()?;
        let sources = read_device_map(&mut r)?;
        let section_filter = SectionFilter {
            outputs: r.u8()? != 0,
            inputs: r.u8()? != 0,
            apps: r.u8()? != 0,
        };
        profiles.push(Profile {
            name,
            sinks,
            apps,
            sources,
            default_sink,
            default_source,
            section_filter,
        });
    }
    r.expect_end()?;
    Ok(State {
        window,
        profiles: ProfileStore { profiles, active },
    })
}

fn read_device_map(r: &mut Reader) -> Result<std::collections::BTreeMap<String, DeviceSettings>> {
    let count = r.u32()? as usize;
    let mut map = std::collections::BTreeMap::new();
    for _ in 0..count {
        let key = r.string()?;
        let volume = r.f32()?;
        let muted = r.u8()? != 0;
        map.insert(key, DeviceSettings { volume, muted });
    }
    Ok(map)
}

fn read_app_map(r: &mut Reader) -> Result<std::collections::BTreeMap<String, AppSettings>> {
    let count = r.u32()? as usize;
    let mut map = std::collections::BTreeMap::new();
    for _ in 0..count {
        let key = r.string()?;
        let volume = r.f32()?;
        let muted = r.u8()? != 0;
        let target_sink_name = r.opt_string()?;
        map.insert(
            key,
            AppSettings {
                volume,
                muted,
                target_sink_name,
            },
        );
    }
    Ok(map)
}

fn write_string(out: &mut Vec<u8>, s: &str) -> Result<()> {
    let len = u16::try_from(s.len()).map_err(|_| Error::StringTooLong(s.len()))?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

fn write_opt_string(out: &mut Vec<u8>, s: Option<&str>) -> Result<()> {
    match s {
        None => {
            out.push(0);
            Ok(())
        }
        Some(v) => {
            out.push(1);
            write_string(out, v)
        }
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::OffsetOverflow)?;
        if end > self.data.len() {
            return Err(Error::UnexpectedEnd {
                pos: self.pos,
                need: n,
                have: self.data.len() - self.pos,
            });
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn f32(&mut self) -> Result<f32> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn string(&mut self) -> Result<String> {
        let len = self.u16()? as usize;
        let start = self.pos;
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes)
            .map(str::to_string)
            .map_err(|source| Error::InvalidUtf8 {
                offset: start,
                source,
            })
    }

    fn opt_string(&mut self) -> Result<Option<String>> {
        let tag_pos = self.pos;
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.string()?)),
            tag => Err(Error::InvalidOptionalTag {
                tag,
                offset: tag_pos,
            }),
        }
    }

    fn expect_end(self) -> Result<()> {
        if self.pos != self.data.len() {
            return Err(Error::TrailingBytes(self.data.len() - self.pos));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state() -> State {
        let mut p = Profile {
            name: "Work".into(),
            default_sink: Some("alsa_output.usb".into()),
            default_source: Some("alsa_input.xlr".into()),
            section_filter: SectionFilter {
                outputs: true,
                inputs: false,
                apps: true,
            },
            ..Profile::default()
        };
        p.sinks.insert(
            "alsa_output.usb".into(),
            DeviceSettings {
                volume: 0.75,
                muted: false,
            },
        );
        p.apps.insert(
            "bin:firefox".into(),
            AppSettings {
                volume: 0.5,
                muted: true,
                target_sink_name: Some("bluez_output.headset".into()),
            },
        );
        p.sources.insert(
            "alsa_input.xlr".into(),
            DeviceSettings {
                volume: 0.6,
                muted: true,
            },
        );
        State {
            window: Geometry {
                width: 1024,
                height: 768,
                maximized: true,
            },
            profiles: ProfileStore {
                profiles: vec![p],
                active: Some("Work".into()),
            },
        }
    }

    #[test]
    fn empty_state_round_trips() {
        let bytes = encode(&State::default()).expect("encode");
        let loaded = decode(&bytes).expect("decode");
        assert_eq!(loaded, State::default());
    }

    #[test]
    fn full_state_round_trips() {
        let original = sample_state();
        let bytes = encode(&original).expect("encode");
        let loaded = decode(&bytes).expect("decode");
        assert_eq!(loaded, original);
    }

    #[test]
    fn section_filter_survives_round_trip() {
        let original = sample_state();
        let loaded = decode(&encode(&original).expect("encode")).expect("decode");
        let sf = loaded.profiles.profiles[0].section_filter;
        assert!(sf.outputs);
        assert!(!sf.inputs);
        assert!(sf.apps);
    }

    #[test]
    fn window_geometry_survives_round_trip() {
        let loaded = decode(&encode(&sample_state()).expect("encode")).expect("decode");
        assert_eq!(loaded.window.width, 1024);
        assert_eq!(loaded.window.height, 768);
        assert!(loaded.window.maximized);
    }

    #[test]
    fn encode_starts_with_magic_and_version() {
        let bytes = encode(&State::default()).expect("encode");
        assert_eq!(&bytes[0..4], MAGIC);
        assert_eq!(bytes[4], VERSION);
    }

    #[test]
    fn decode_rejects_bad_magic() {
        let mut bytes = encode(&State::default()).expect("encode");
        bytes[0] = b'X';
        let err = decode(&bytes).unwrap_err();
        assert!(format!("{err}").contains("invalid state magic"));
    }

    #[test]
    fn decode_rejects_unknown_version() {
        let mut bytes = encode(&State::default()).expect("encode");
        bytes[4] = 99;
        let err = decode(&bytes).unwrap_err();
        assert!(format!("{err}").contains("unsupported state version"));
    }

    #[test]
    fn decode_rejects_truncated_input() {
        let bytes = encode(&sample_state()).expect("encode");
        let err = decode(&bytes[..bytes.len() - 2]).unwrap_err();
        assert!(format!("{err}").contains("unexpected end of state data"));
    }

    #[test]
    fn decode_rejects_trailing_garbage() {
        let mut bytes = encode(&State::default()).expect("encode");
        bytes.push(0xAA);
        let err = decode(&bytes).unwrap_err();
        assert!(format!("{err}").contains("trailing 1 bytes"));
    }

    #[test]
    fn decode_clamps_corrupt_window_dims() {
        let s = State {
            window: Geometry {
                width: 5,
                height: 999_999,
                maximized: false,
            },
            ..State::default()
        };
        let loaded = decode(&encode(&s).expect("encode")).expect("decode");
        // Clamped into the sane range by `Geometry::clamped`.
        assert!(loaded.window.width >= 200);
        assert!(loaded.window.height <= 8192);
    }

    #[test]
    fn round_trip_preserves_unicode_keys() {
        let mut p = Profile {
            name: "Café ☕".into(),
            ..Profile::default()
        };
        p.sinks.insert(
            "alsa_output.日本語".into(),
            DeviceSettings {
                volume: 0.42,
                muted: true,
            },
        );
        let original = State {
            window: Geometry::default(),
            profiles: ProfileStore {
                profiles: vec![p],
                active: Some("Café ☕".into()),
            },
        };
        let loaded = decode(&encode(&original).expect("encode")).expect("decode");
        assert_eq!(loaded, original);
    }

    #[test]
    fn load_returns_default_on_missing_file() {
        let path = std::env::temp_dir()
            .join("bnksound_state_test_missing")
            .join("does-not-exist.bin");
        let loaded = load_from(&path).expect("missing file ok");
        assert_eq!(loaded, State::default());
    }

    #[test]
    fn save_then_load_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("bnksound_state_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("state.bin");

        let original = sample_state();
        save_to(&path, &original).expect("save");
        let loaded = load_from(&path).expect("load");
        assert_eq!(loaded, original);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
