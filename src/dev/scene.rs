//! Mixer fixtures for the dev tools: no PipeWire, no window, just the state a
//! painter needs.

use crate::domain::{DeviceForm, SinkForm, SourceForm, Stream, StreamKind};
use crate::state;

/// Two sinks and a source, the devices every fixture starts from.
fn devices() -> state::App {
    let mut app = state::empty();
    let rows = [
        (
            1,
            StreamKind::Sink,
            "BNK Headset",
            Some(DeviceForm::Output(SinkForm::Headset)),
            0.33,
            true,
        ),
        (
            2,
            StreamKind::Sink,
            "Schiit Modi+ Analog",
            Some(DeviceForm::Output(SinkForm::Speaker)),
            0.26,
            false,
        ),
        (
            3,
            StreamKind::Source,
            "Yeti Nano",
            Some(DeviceForm::Input(SourceForm::Microphone)),
            0.70,
            false,
        ),
    ];
    for (id, kind, name, form, volume, is_default) in rows {
        app.streams
            .insert(id, stream(id, kind, name, form, volume, is_default));
    }
    app
}

/// One stream, with the fields a fixture never varies already filled in.
fn stream(
    id: u32,
    kind: StreamKind,
    name: &str,
    form: Option<DeviceForm>,
    volume: f32,
    is_default: bool,
) -> Stream {
    Stream {
        id,
        kind,
        name: name.to_string(),
        app_id: None,
        binary: None,
        pid: None,
        node_name: Some(format!("node.{id}")),
        media_name: None,
        media_role: None,
        channel_volumes: vec![volume, volume],
        muted: false,
        xdg: None,
        form,
        is_default,
        target_sink_name: None,
    }
}

/// The mixer as it looks in a screenshot: real device names and two
/// applications playing something.
pub fn showcase() -> state::App {
    let mut app = devices();
    for (id, app_id, name, volume, muted) in [
        (10, "com.spotify.Client", "Kingdom Hearts", 0.84_f32, false),
        (11, "org.mozilla.firefox", "Firefox", 0.55, true),
    ] {
        let mut s = stream(id, StreamKind::Application, name, None, volume, false);
        s.app_id = Some(app_id.to_string());
        s.media_name = Some(name.to_string());
        s.muted = muted;
        app.streams.insert(id, s);
    }
    app
}

/// The devices plus `apps` application streams, each its own application, so the
/// row count is exactly what was asked for.
pub fn mixer(apps: usize) -> state::App {
    let mut app = devices();
    for i in 0..apps {
        let id = 10 + i as u32;
        let volume = 0.3 + (i % 7) as f32 * 0.1;
        let mut s = stream(
            id,
            StreamKind::Application,
            &format!("Application {i}"),
            None,
            volume,
            false,
        );
        s.app_id = Some(format!("com.example.app{i}"));
        s.media_name = Some(format!("Now playing {i}"));
        s.muted = i % 5 == 0;
        app.streams.insert(id, s);
    }
    app
}

/// The same mixer with the applications sharing an id in threes, so they
/// collapse into groups. A grouped row carries a member count and an expand
/// arrow, neither of which [`mixer`] draws.
pub fn grouped(apps: usize) -> state::App {
    let mut app = mixer(apps);
    for (i, stream) in app
        .streams
        .values_mut()
        .filter(|s| s.kind == StreamKind::Application)
        .enumerate()
    {
        stream.app_id = Some(format!("com.example.group{}", i / 3));
    }
    app
}
