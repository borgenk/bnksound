//! Per-stream peak metering. Each bound Node gets a passive PipeWire
//! capture stream that taps its audio and folds per-channel peaks into a
//! shared [`crate::meter::PeakPool`] slot the UI decays ~60Hz.

use std::io::Cursor;
use std::sync::Arc;

use libspa::param::ParamType;
use libspa::param::audio::{AudioFormat, AudioInfoRaw};
use libspa::pod::serialize::PodSerializer;
use libspa::pod::{Object, Pod, Value};
use libspa::utils::{Direction, SpaTypes};
use pipewire as pw;
use pw::core::CoreRc;
use pw::properties::properties;
use pw::stream::{StreamFlags, StreamListener, StreamRc};

use crate::bus::Sender as BusSender;
use crate::domain::StreamKind;
use crate::meter::{MeterSlot, PeakPool};
use crate::pipewire_worker::Event;

/// Capture stream + listener kept alongside a bound Node so `drop(Bound)`
/// tears the meter down (returning the `MeterSlot` to the pool).
pub(crate) struct MonitorState {
    _stream: StreamRc,
    _listener: StreamListener<MonitorData>,
}

/// User-data for the stream callbacks: `param_changed` fills `n_channels`
/// after format negotiation, `process` folds per-channel peaks into `slot`.
struct MonitorData {
    /// Channel count, 0 until the format is negotiated. While 0 the
    /// process callback skips folding (can't split interleaved samples).
    n_channels: u32,
    /// Peak-pool slot, or `None` if the pool was full at monitor start.
    slot: Option<MeterSlot>,
}

/// Spawn a capture stream tapping `node_id`'s audio and report its peak
/// level through `tx`. `None` on any setup failure (the meter is a
/// nicety, not load-bearing).
///
/// Three properties keep this from disturbing the audio graph:
///
/// - `stream.monitor = "true"`: WirePlumber's bluetooth autoswitch
///   excludes monitor streams, so our meter won't drop an A2DP headset
///   to HFP mic mode.
/// - `node.dont-reconnect = "true"`: when the target goes away, stop
///   rather than auto-link elsewhere.
/// - `stream.capture.sink = "true"` (sinks only): request the sink's
///   monitor port. App streams produce output, so we tap them directly.
pub(crate) fn start_monitor_stream(
    core: &CoreRc,
    node_id: u32,
    kind: StreamKind,
    object_serial: Option<&str>,
    node_name: Option<&str>,
    tx: BusSender<Event>,
    pool: &Arc<PeakPool>,
) -> Option<MonitorState> {
    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "DSP",
        *pw::keys::APP_NAME => "bnksound (meter)",
        *pw::keys::NODE_NAME => "bnksound-meter",
        *pw::keys::NODE_DONT_RECONNECT => "true",
        *pw::keys::STREAM_MONITOR => "true",
        // NOT setting `node.passive = "true"`: the producer side is
        // already passive, so a passive input link means no buffers arrive.
    };
    if matches!(kind, StreamKind::Sink) {
        props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
    }
    // `target.object` auto-link key accepts a node.name or object.serial
    // (NOT the registry global id). Prefer object.serial: it's unique, so
    // app streams sharing a node.name (every Chromium browser tab) get
    // distinct targets. node_id is a wrong last resort that at least fails
    // loudly instead of silently colliding.
    let target = object_serial
        .map(str::to_string)
        .or_else(|| node_name.map(str::to_string))
        .unwrap_or_else(|| node_id.to_string());
    props.insert("target.object", target);

    let report_err = |stage: &str, e: pw::Error| {
        let _ = tx.send(Event::Error(format!("meter {stage} {node_id}: {e}")));
    };

    let stream = match StreamRc::new(core.clone(), "bnksound-meter", props) {
        Ok(s) => s,
        Err(e) => {
            report_err("create", e);
            return None;
        }
    };

    let slot = pool.claim(node_id);
    if slot.is_none() {
        eprintln!("meter: peak pool full, node {node_id} gets no level meter");
    }
    let user_data = MonitorData {
        n_channels: 0,
        slot,
    };

    let listener = match stream
        .add_local_listener_with_user_data(user_data)
        .param_changed(|_, data: &mut MonitorData, id, param| {
            // Only the negotiated Format param tells us the channel count.
            if id != ParamType::Format.as_raw() {
                return;
            }
            let Some(param) = param else { return };
            let mut info = AudioInfoRaw::new();
            if info.parse(param).is_ok() {
                data.n_channels = info.channels();
            }
        })
        .process(|stream, data: &mut MonitorData| {
            let n = data.n_channels as usize;
            if n == 0 {
                // Format not negotiated yet; drop the buffer (can't split
                // channels without n_channels).
                let _ = stream.dequeue_buffer();
                return;
            }

            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }

            // F32LE-interleaved: sample `i` belongs to channel `i % n`.
            // Stack-buffer up to 8 channels; more collapse to slot 0.
            let mut peaks_buf = [0.0_f32; 8];
            let track = n.min(peaks_buf.len());
            let collapse_extra = n > peaks_buf.len();
            for d in datas.iter_mut() {
                let chunk_size = d.chunk().size() as usize;
                let Some(bytes) = d.data() else { continue };
                let bytes = &bytes[..chunk_size.min(bytes.len())];
                for (idx, sample_bytes) in
                    bytes.chunks_exact(std::mem::size_of::<f32>()).enumerate()
                {
                    let s = f32::from_le_bytes([
                        sample_bytes[0],
                        sample_bytes[1],
                        sample_bytes[2],
                        sample_bytes[3],
                    ]);
                    let a = s.abs();
                    let ch = if collapse_extra { 0 } else { idx % track };
                    if a > peaks_buf[ch] {
                        peaks_buf[ch] = a;
                    }
                }
            }

            // Fold per-channel maxima into the shared slot via atomic max
            // (no alloc/lock); the GTK decay tick reads-and-clears ~60Hz.
            if let Some(slot) = &data.slot {
                slot.fold(&peaks_buf[..track]);
            }
        })
        .register()
    {
        Ok(l) => l,
        Err(e) => {
            report_err("listener", e);
            return None;
        }
    };

    // EnumFormat requesting F32LE; rate/channels unset so PipeWire picks
    // the source's native format.
    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values = match PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(obj)) {
        Ok((cursor, _)) => cursor.into_inner(),
        Err(_) => return None,
    };
    let pod = Pod::from_bytes(&values)?;
    let mut params = [pod];

    if let Err(e) = stream.connect(
        Direction::Input,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
        &mut params,
    ) {
        report_err("connect", e);
        return None;
    }

    Some(MonitorState {
        _stream: stream,
        _listener: listener,
    })
}
