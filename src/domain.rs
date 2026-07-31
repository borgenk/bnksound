use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    /// Application playback stream `media.class = "Stream/Output/Audio"`.
    Application,
    /// Output device `media.class = "Audio/Sink"`. Treated as a "master"
    /// volume control via its channelVolumes.
    Sink,
    /// Input device `media.class = "Audio/Source"`. The capture-side
    /// mirror of [`Self::Sink`] (microphone, line-in).
    Source,
}

/// The three column groups in the strip, each toggled by an action-bar
/// filter button. Visibility is stored per profile in [`SectionFilter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Outputs,
    Inputs,
    Apps,
}

/// Per-section visibility backing the IN/OUT/APP filter. A hidden section
/// is skipped in the layout but its streams keep flowing, so re-showing is
/// instant. Persisted on each [`crate::profile::Profile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionFilter {
    pub outputs: bool,
    pub inputs: bool,
    pub apps: bool,
}

impl Default for SectionFilter {
    fn default() -> Self {
        Self {
            outputs: true,
            inputs: true,
            apps: true,
        }
    }
}

impl SectionFilter {
    /// Whether `section` is currently shown.
    pub fn shows(&self, section: Section) -> bool {
        match section {
            Section::Outputs => self.outputs,
            Section::Inputs => self.inputs,
            Section::Apps => self.apps,
        }
    }

    /// Flip one section's visibility in place.
    pub fn toggle(&mut self, section: Section) {
        match section {
            Section::Outputs => self.outputs = !self.outputs,
            Section::Inputs => self.inputs = !self.inputs,
            Section::Apps => self.apps = !self.apps,
        }
    }
}

/// Resolved physical form of a sink. Drives icon choice and sort order.
/// `None` on `Stream` means we don't have enough info yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkForm {
    /// Bluetooth headset or any output that also exposes a mic (HFP/HSP).
    Headset,
    /// Headphones without a mic, or an A2DP-only Bluetooth profile.
    Headphones,
    /// Built-in speakers, analog line-out, or a USB DAC (indistinguishable
    /// from speakers, software just sees an analog port).
    Speaker,
    /// HDMI audio output, usually a monitor or TV.
    Hdmi,
    /// S/PDIF digital output.
    Spdif,
    /// Anything else — null sinks, loopbacks, unknown port types.
    Generic,
}

impl SinkForm {
    /// Stable sort key so the UI can list headsets before speakers.
    pub fn sort_key(self) -> u8 {
        match self {
            SinkForm::Headset => 0,
            SinkForm::Headphones => 1,
            SinkForm::Speaker => 2,
            SinkForm::Hdmi => 3,
            SinkForm::Spdif => 4,
            SinkForm::Generic => 5,
        }
    }

    /// Uppercase display label for the sink column's type heading
    /// (`OUTPUT` for the catch-all Generic form).
    pub fn display_label(self) -> &'static str {
        match self {
            SinkForm::Headset => "HEADSET",
            SinkForm::Headphones => "HEADPHONES",
            SinkForm::Speaker => "SPEAKER",
            SinkForm::Hdmi => "HDMI",
            SinkForm::Spdif => "S/PDIF",
            SinkForm::Generic => "OUTPUT",
        }
    }

    /// Resolve from a PipeWire `device.form-factor` prop value (set by
    /// udev / WirePlumber; Bluetooth devices almost always have one).
    pub fn from_form_factor(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "headset" => Some(Self::Headset),
            "headphone" | "headphones" => Some(Self::Headphones),
            "speaker" | "internal" | "computer" => Some(Self::Speaker),
            "tv" | "television" => Some(Self::Hdmi),
            _ => None,
        }
    }

    /// Resolve from a Route's `port.type` field (distinguishes laptop
    /// speakers from the headphones jack).
    pub fn from_port_type(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "speaker" => Some(Self::Speaker),
            "headphones" => Some(Self::Headphones),
            "headset" => Some(Self::Headset),
            "hdmi" => Some(Self::Hdmi),
            "spdif" => Some(Self::Spdif),
            // Generic ports: we can't know what's plugged in downstream, so
            // treat as Speaker for a meaningful icon instead of a blank row.
            "analog" | "usb" | "line" => Some(Self::Speaker),
            _ => None,
        }
    }
}

/// Resolved physical form of an input device, mirroring [`SinkForm`] on
/// the capture side. `None` on `Stream` means not enough info yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceForm {
    /// A microphone of any kind (built-in, USB, headset/HFP, webcam, or a
    /// generic analog capture port). On the capture side all of these read
    /// as "a microphone"; the output-side headset distinction is meaningless.
    Microphone,
    /// Line-in / aux analog capture.
    LineIn,
    /// Anything else — null sources, loopbacks, unknown capture ports.
    Generic,
}

impl SourceForm {
    /// Stable sort key so the UI lists plain microphones before line-in
    /// and unknown capture sources.
    pub fn sort_key(self) -> u8 {
        match self {
            SourceForm::Microphone => 0,
            SourceForm::LineIn => 1,
            SourceForm::Generic => 2,
        }
    }

    /// Uppercase display label used as the source column's type heading.
    /// Falls back to `INPUT` for the catch-all Generic form.
    pub fn display_label(self) -> &'static str {
        match self {
            SourceForm::Microphone => "MICROPHONE",
            SourceForm::LineIn => "LINE-IN",
            SourceForm::Generic => "INPUT",
        }
    }

    /// Resolve from a PipeWire `device.form-factor` prop value.
    pub fn from_form_factor(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "headset" | "microphone" | "mic" | "webcam" => Some(Self::Microphone),
            _ => None,
        }
    }

    /// Resolve from a capture Route's `port.type` field.
    pub fn from_port_type(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            // Mic, headset mic, and generic analog/usb capture ports all read
            // as a microphone (mirrors SinkForm's analog→Speaker fallback).
            "mic" | "microphone" | "headset" | "analog" | "usb" => Some(Self::Microphone),
            "line" | "aux" => Some(Self::LineIn),
            _ => None,
        }
    }
}

/// A device's resolved form, tagged by direction: output devices carry a
/// [`SinkForm`], input devices a [`SourceForm`]. Lets the shared `Stream`
/// hold one `form` field that renders correctly for either kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceForm {
    Output(SinkForm),
    Input(SourceForm),
}

impl DeviceForm {
    /// Stable in-section sort key (the UI only sorts within one kind, so
    /// output and input keys never compare against each other).
    pub fn sort_key(self) -> u8 {
        match self {
            DeviceForm::Output(f) => f.sort_key(),
            DeviceForm::Input(f) => f.sort_key(),
        }
    }

    /// Uppercase column heading ("SPEAKER", "MICROPHONE", …).
    pub fn display_label(self) -> &'static str {
        match self {
            DeviceForm::Output(f) => f.display_label(),
            DeviceForm::Input(f) => f.display_label(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub id: u32,
    pub kind: StreamKind,
    pub name: String,
    pub app_id: Option<String>,
    pub binary: Option<String>,
    pub pid: Option<String>,
    pub node_name: Option<String>,
    pub media_name: Option<String>,
    pub media_role: Option<String>,
    pub channel_volumes: Vec<f32>,
    pub muted: bool,
    /// Resolved from the icon themes by name. `None` for device streams and
    /// for apps whose name matches no installed icon.
    pub icon_path: Option<PathBuf>,
    /// Resolved physical form, set for `Sink`/`Source` streams via
    /// [`DeviceForm`]. `None` until the Device info + Route params arrive
    /// (permanently `None` for software-only devices and app streams).
    pub form: Option<DeviceForm>,
    /// True for the device that's currently the system default for its
    /// direction (`default.audio.sink` / `default.audio.source`). Drives the
    /// active label coloring and top-of-list sort. `false` for app streams.
    pub is_default: bool,
    /// User-set output target for application streams (`target.object` on the
    /// `default` metadata). `None` follows the default sink, `Some(node_name)`
    /// pins to a sink. Always `None` for device streams.
    pub target_sink_name: Option<String>,
}

impl Stream {
    /// Best display name: what the app calls itself, minus a trailing generic
    /// product word.
    pub fn display_name(&self) -> &str {
        clean_name(&self.name)
    }

    /// Average linear gain across channels, clamped to [0.0, MAX_VOLUME].
    pub fn average_volume(&self) -> f32 {
        if self.channel_volumes.is_empty() {
            return 0.0;
        }
        let sum: f32 = self.channel_volumes.iter().sum();
        sum / self.channel_volumes.len() as f32
    }

    /// Overwrite every channel with the same linear gain, preserving the
    /// channel count (defaulting to stereo before the first Props event).
    /// The shared write behind slider drags, profile applies, and rescales.
    pub fn set_uniform_volume(&mut self, linear: f32) {
        let channels = self.channel_volumes.len().max(2);
        self.channel_volumes = vec![linear; channels];
    }

    /// Stable identity for "which app this stream belongs to", used to match
    /// across destroy/recreate and to key profile entries. `None` for sinks
    /// and for app streams missing every hint. The tag prefix (`app:` /
    /// `bin:`) keeps different sources from colliding. Deliberately not keyed
    /// on the display name, which the Client props rewrite mid-life.
    pub fn app_identity(&self) -> Option<String> {
        if !matches!(self.kind, StreamKind::Application) {
            return None;
        }
        if let Some(a) = self.app_id.as_deref() {
            return Some(format!("app:{a}"));
        }
        self.binary.as_deref().map(|b| format!("bin:{b}"))
    }
}

/// Generic product words an app tacks onto its own name. Dropped for display
/// when something is left in front, so "Helium Browser" reads as "Helium"
/// while an app calling itself only "Browser" keeps the name. Longest first,
/// so "Web Browser" goes before "Browser".
const NAME_SUFFIXES: [&str; 6] = [
    "web browser",
    "media player",
    "music player",
    "browser",
    "launcher",
    "player",
];

/// Trim a trailing product word, bare ("Helium Browser") or parenthesised
/// ("Spotify (Launcher)"). Returns a slice of the input, so no display path
/// allocates for this.
fn clean_name(name: &str) -> &str {
    let trimmed = name.trim_end();
    for suffix in NAME_SUFFIXES {
        let stem = strip_ci(trimmed, suffix).or_else(|| {
            trimmed
                .strip_suffix(')')
                .and_then(|inner| strip_ci(inner, suffix))
                .and_then(|inner| inner.strip_suffix('('))
        });
        // Only a whole trailing word counts, so whitespace has to have been
        // trimmed off the stem. Without that, "Webbrowser" would lose its tail.
        if let Some(stem) = stem {
            let kept = stem.trim_end();
            if !kept.is_empty() && kept.len() < stem.len() {
                return kept;
            }
        }
    }
    trimmed
}

/// `strip_suffix`, ignoring ASCII case.
fn strip_ci<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    let cut = text.len().checked_sub(suffix.len())?;
    if !text.is_char_boundary(cut) {
        return None;
    }
    text[cut..]
        .eq_ignore_ascii_case(suffix)
        .then(|| &text[..cut])
}

/// Slider range: 0% to 150% on the perceptual (cubic) scale, matching
/// pavucontrol / wpctl / desktop volume sliders.
pub const MAX_VOLUME: f32 = 1.5;

/// Map PipeWire's raw linear `channelVolumes` gain to the perceptual cubic
/// scale every other audio UI (pavucontrol, wpctl, GNOME, KDE) uses, since
/// human loudness is roughly logarithmic. Cubic conversion lives at the UI
/// boundary only; the worker stays linear end-to-end.
pub fn linear_to_cubic(linear: f32) -> f32 {
    linear.max(0.0).cbrt()
}

/// Inverse of [`linear_to_cubic`], to write back into channelVolumes.
pub fn cubic_to_linear(cubic: f32) -> f32 {
    let c = cubic.max(0.0);
    c * c * c
}

/// A [`Stream`] with every optional field unset, for tests to build on:
///
/// ```ignore
/// Stream { name: "Speaker".into(), ..sample_stream(1, StreamKind::Sink) }
/// ```
///
/// The struct has no `Default` because none of its fields have a meaningful
/// zero outside a test, and spelling all of them out at each fixture turns
/// every added field into a sweep across the crate.
#[cfg(test)]
pub(crate) fn sample_stream(id: u32, kind: StreamKind) -> Stream {
    Stream {
        id,
        kind,
        name: format!("Stream {id}"),
        app_id: None,
        binary: None,
        pid: None,
        node_name: None,
        media_name: None,
        media_role: None,
        channel_volumes: vec![0.5, 0.5],
        muted: false,
        icon_path: None,
        form: None,
        is_default: false,
        target_sink_name: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_form_from_port_type_maps_capture_ports() {
        assert_eq!(
            SourceForm::from_port_type("mic"),
            Some(SourceForm::Microphone)
        );
        assert_eq!(
            SourceForm::from_port_type("Microphone"),
            Some(SourceForm::Microphone)
        );
        // A headset's capture endpoint is just a microphone, no HEADSET form.
        assert_eq!(
            SourceForm::from_port_type("headset"),
            Some(SourceForm::Microphone)
        );
        assert_eq!(SourceForm::from_port_type("line"), Some(SourceForm::LineIn));
        assert_eq!(SourceForm::from_port_type("aux"), Some(SourceForm::LineIn));
        // Generic analog/usb capture ports fall back to a microphone icon.
        assert_eq!(
            SourceForm::from_port_type("analog"),
            Some(SourceForm::Microphone)
        );
        assert_eq!(
            SourceForm::from_port_type("usb"),
            Some(SourceForm::Microphone)
        );
        assert_eq!(SourceForm::from_port_type("hdmi"), None);
    }

    #[test]
    fn source_form_from_form_factor() {
        // Headset and webcam both resolve to a microphone.
        assert_eq!(
            SourceForm::from_form_factor("headset"),
            Some(SourceForm::Microphone)
        );
        assert_eq!(
            SourceForm::from_form_factor("webcam"),
            Some(SourceForm::Microphone)
        );
        assert_eq!(SourceForm::from_form_factor("speaker"), None);
    }

    #[test]
    fn source_form_sort_key_orders_microphone_first() {
        assert!(SourceForm::Microphone.sort_key() < SourceForm::LineIn.sort_key());
        assert!(SourceForm::LineIn.sort_key() < SourceForm::Generic.sort_key());
    }

    #[test]
    fn device_form_display_label_dispatches_by_direction() {
        assert_eq!(
            DeviceForm::Output(SinkForm::Speaker).display_label(),
            "SPEAKER"
        );
        assert_eq!(
            DeviceForm::Output(SinkForm::Generic).display_label(),
            "OUTPUT"
        );
        assert_eq!(
            DeviceForm::Input(SourceForm::Microphone).display_label(),
            "MICROPHONE"
        );
        assert_eq!(
            DeviceForm::Input(SourceForm::Generic).display_label(),
            "INPUT"
        );
    }

    #[test]
    fn device_form_sort_key_matches_wrapped_form() {
        assert_eq!(
            DeviceForm::Output(SinkForm::Hdmi).sort_key(),
            SinkForm::Hdmi.sort_key()
        );
        assert_eq!(
            DeviceForm::Input(SourceForm::LineIn).sort_key(),
            SourceForm::LineIn.sort_key()
        );
    }

    /// The names measured off real streams and desktop entries, plus the
    /// shapes that must survive untouched.
    #[test]
    fn a_trailing_product_word_is_dropped_for_display() {
        for (raw, shown) in [
            ("Helium Browser", "Helium"),
            ("Spotify (Launcher)", "Spotify"),
            ("mpv Media Player", "mpv"),
            ("Firefox Web Browser", "Firefox"),
            ("Rhythmbox Music Player", "Rhythmbox"),
            ("VLC media player", "VLC"),
            ("Helium Browser   ", "Helium"),
        ] {
            let mut s = app_stream();
            s.name = raw.to_string();
            assert_eq!(s.display_name(), shown, "{raw}");
        }
    }

    #[test]
    fn a_name_that_is_only_the_product_word_keeps_it() {
        for raw in ["Browser", "Launcher", "Player"] {
            let mut s = app_stream();
            s.name = raw.to_string();
            assert_eq!(s.display_name(), raw, "nothing would be left");
        }
    }

    #[test]
    fn the_word_has_to_stand_alone() {
        for raw in ["Webbrowser", "Multiplayer", "Relauncher", "Helium"] {
            let mut s = app_stream();
            s.name = raw.to_string();
            assert_eq!(s.display_name(), raw, "not a trailing word");
        }
    }

    fn app_stream() -> Stream {
        Stream {
            name: String::new(),
            ..sample_stream(1, StreamKind::Application)
        }
    }
}
