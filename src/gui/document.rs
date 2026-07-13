//! Circuit documents: the multiple-circuits-in-memory model.
//!
//! `OsmilogApp` holds several circuits at once but keeps exactly one *active*:
//! that document's per-circuit state lives directly in the app's live fields
//! (`circuit`/`history`/`components`/…), and its `CircuitDoc::state` is None.
//! Every other document parks its state in a `DocState` here until switched to.
//! Swapping is a set of `std::mem` moves between the live fields and a
//! `DocState` (see `OsmilogApp::take_active_state`/`put_active_state`) - no
//! serialization, so no `ComponentSpec`/ROM deep-copy on a switch.

use egui::Vec2;
use slotmap::SlotMap;

use crate::gui::app::{
    ClockControl, InteractionMode, PlacedCompKey, PlacedTunnel, PlacedTunnelKey, Selection,
};
use crate::gui::history::History;
use crate::gui::placed_component::PlacedComponent;
use crate::gui::wiring::Wiring;
use crate::sim::circuit::Circuit;

/// Stable identity of a circuit document, independent of display order, and the
/// handle `ComponentSpec::Subcircuit` references. Defined in `sim::component`
/// (so the spec can embed it without a gui dependency) and re-exported here,
/// where the document registry lives.
pub use crate::sim::component::DocId;

/// The per-circuit ("document") state, bundled so an inactive circuit can be
/// parked while another is edited. These are exactly the live per-circuit fields
/// on `OsmilogApp`; the active document keeps them in those fields (and its
/// `CircuitDoc::state` is None), while inactive documents hold a `DocState` here.
pub struct DocState {
    pub(crate) circuit: Circuit,
    pub(crate) history: History,
    pub(crate) components: SlotMap<PlacedCompKey, PlacedComponent>,
    pub(crate) tunnels: SlotMap<PlacedTunnelKey, PlacedTunnel>,
    pub(crate) wiring: Wiring,
    pub(crate) mode: InteractionMode,
    pub(crate) pan: Vec2,
    pub(crate) selected: Option<Selection>,
    pub(crate) clock: ClockControl,
    pub(crate) rom_editor_open: Option<PlacedCompKey>,
}

impl DocState {
    /// A fresh blank document - the same per-circuit initial values `empty()` uses.
    pub(crate) fn blank() -> Self {
        Self {
            circuit: Circuit::new(),
            history: History::default(),
            components: SlotMap::default(),
            tunnels: SlotMap::default(),
            wiring: Wiring::new(),
            mode: InteractionMode::Idle,
            pan: Vec2::ZERO,
            selected: None,
            clock: ClockControl::default(),
            rom_editor_open: None,
        }
    }
}

/// One circuit document: its display name plus its parked state while inactive.
/// `state` is None exactly when this is the active document (its state is live
/// on `OsmilogApp`); Some for every parked/inactive document.
pub struct CircuitDoc {
    pub(crate) name: String,
    pub(crate) state: Option<DocState>,
}

/// Default name suggested for a new circuit, e.g. "Circuit 2" for the second
/// document. Only a suggestion (prefilled into the dialog / used when the user
/// clears the field) - names aren't required to be unique; identity is the `DocId`.
pub(crate) fn default_new_circuit_name(documents: &SlotMap<DocId, CircuitDoc>) -> String {
    format!("Circuit {}", documents.len() + 1)
}
