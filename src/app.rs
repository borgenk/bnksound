use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

use crate::bus::{self, Sender};
use crate::pipewire_worker::Event as WorkerEvent;
use crate::state::Message;
use crate::ui;
use crate::{meter, pipewire_worker, settings, state};

/// How many messages each main-loop channel can hold.
const BUS_CAPACITY: usize = 4096;

// The running UI's shared handles. The closures driving the main loop
// (autosave, meter tick, the two bus drains) each hold a clone, so the UI
// lives as long as any one of them: state, widgets, peak pool, sender.
struct Session {
    state: Rc<RefCell<state::App>>,
    widgets: Rc<RefCell<ui::Widgets>>,
    peaks: Arc<meter::PeakPool>,
    tx: Sender<Message>,
}

pub fn activate(app: &gtk::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }

    // Two main-loop channels: one for UI/state messages, one for PipeWire
    // worker events. Both drained on the main thread below; senders cloned
    // out to producers.
    let (msg_tx, msg_rx) = bus::channel::<Message>(BUS_CAPACITY);
    let (evt_tx, evt_rx) = bus::channel::<WorkerEvent>(BUS_CAPACITY);

    // Shared peak-meter storage. The worker's audio callbacks write each
    // buffer's levels in, the meter tick below reads them back out to paint
    // the bars.
    let peak_pool = Arc::new(meter::PeakPool::new());

    pipewire_worker::init(evt_tx, Arc::clone(&peak_pool));

    // MPRIS metadata the UI queries for titles. Inert when the session bus is
    // unavailable; the UI just falls back to its label ladder. Owned by the
    // Widgets below, so it lives as long as the window.
    let mpris = crate::mpris::init(msg_tx.clone());

    let state = Rc::new(RefCell::new(state::boot()));
    let initial_geometry = state.borrow().geometry;

    let widgets = Rc::new(RefCell::new(ui::Widgets::build(
        app,
        msg_tx.clone(),
        initial_geometry,
        &settings::load(),
        mpris,
    )));

    // Initial paint, then show.
    {
        let s = state.borrow();
        widgets.borrow_mut().refresh(&s, &msg_tx);
    }
    widgets.borrow().window.present();

    // The one owner. Each long-lived closure below captures an `Rc<Session>`;
    // those clones are what keep the UI alive once this binding drops.
    let session = Rc::new(Session {
        state,
        widgets,
        peaks: peak_pool,
        tx: msg_tx,
    });

    // Autosave debounce tick: coarse enough to collapse a slider drag into
    // one write, fine enough to survive a near-immediate window close.
    {
        let session = Rc::clone(&session);
        glib::timeout_add_local(Duration::from_millis(500), move || {
            if session.tx.send(state::Message::AutoSaveTick).is_err() {
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    // Meter tick: read the latest peaks from the pool and decay every bar.
    // Peaks aren't events; a silent node decays to zero on its own here.
    {
        let session = Rc::clone(&session);
        glib::timeout_add_local(ui::meter::PEAK_DECAY_INTERVAL, move || {
            let s = session.state.borrow();
            session.widgets.borrow().pump_meters(&s, &session.peaks);
            glib::ControlFlow::Continue
        });
    }

    // Two buses, one dispatch path: drain each on the main loop until it
    // closes. Worker events are wrapped so state::update sees a unified
    // Message stream. The tasks may be `!Send` (they hold the `Rc` state)
    // and detach for the process lifetime.
    {
        let session = Rc::clone(&session);
        glib::spawn_future_local(async move {
            while let Some(msg) = msg_rx.recv().await {
                dispatch(&session, msg);
            }
        });
    }
    {
        let session = Rc::clone(&session);
        glib::spawn_future_local(async move {
            while let Some(evt) = evt_rx.recv().await {
                dispatch(&session, state::Message::Worker(Box::new(evt)));
            }
        });
    }
}

/// Reduce one message through `state::update` and reconcile the widgets.
/// Two cases skip the refresh to avoid rebuilding the tree (which churns
/// focus/hover): `AutoSaveTick` on a clean state, and `GeometryChanged`
/// (resize/maximize never change what's drawn inside). Dirty ticks still
/// refresh since a save failure may set `state.status`.
fn dispatch(session: &Session, msg: state::Message) {
    let needs_refresh = {
        let mut s = session.state.borrow_mut();
        let skip = match &msg {
            state::Message::AutoSaveTick => !s.dirty && !s.geometry_dirty,
            state::Message::GeometryChanged { .. } => true,
            _ => false,
        };
        state::update(&mut s, msg);
        !skip
    };
    if needs_refresh {
        let s = session.state.borrow();
        session.widgets.borrow_mut().refresh(&s, &session.tx);
    }
}
