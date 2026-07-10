use crate::gui::app::{OsmilogApp, PlacedCompKey, PlacedTunnelKey};
use crate::gui::geometry::GridPos;
use crate::gui::wiring::{NodeAttach, WiringDelta};
use crate::sim::component::{CompKey, ComponentSpec};

// Undo data for a GUI-level (Wiring/geometry) edit - the counterpart of
// sim::command::UndoAction, but recorded onto the same gui::history::History
// stack (as HistoryEntry::Gui) so a GUI edit can share a Batch with the Sim
// UndoActions it triggers (e.g. a wiring edit followed by rebuild_circuit's
// net relink). Deliberately separate from UndoAction because Wiring/GridPos/
// PlacedCompKey are GUI-only types that must never leak into sim.
//
// There is no "GuiCommand" enum: unlike the sim side, where each Command
// variant produces a genuinely different fine-grained inverse, every Wiring
// mutator's inverse is just "replay this delta backwards". So OsmilogApp calls
// the Wiring methods directly and hands the WiringDelta they return to
// edit_wiring - no command-as-data indirection to re-select a method the
// caller already chose.
#[derive(Debug)]
pub enum GuiUndoAction {
    // Undoes any Wiring-graph edit (add_route / delete_segment /
    // remove_component_nodes / remove_tunnel_nodes / prune_stale_pins): the
    // compact, invertible op list that edit captured. Its stored size is
    // proportional to the entries the edit touched, not the whole graph - see
    // gui::wiring::WiringDelta.
    //
    // `forward` picks the replay direction so the *same* delta serves both undo
    // and redo across the two history stacks: applying it with forward=false
    // runs `undo_delta` (revert), with forward=true runs `redo_delta` (re-apply);
    // apply_gui_undo returns the variant with `forward` flipped. Recorded as
    // forward=false by edit_wiring, since the first application of a recorded
    // edit is always its undo.
    WiringDelta {
        delta: WiringDelta,
        forward: bool,
    },
    // Undoes a component drag-move. Pushed directly (not via edit_wiring) from
    // OsmilogApp::commit_move: grid_pos is written every drag frame for live
    // visual feedback, so by the time the drag ends there's no "before"
    // state left to read - only the original_grid_pos captured at drag-start
    // (already needed for Escape-cancel) still has it.
    MoveComponent {
        key: PlacedCompKey,
        old_pos: GridPos,
    },
    MoveTunnel {
        key: PlacedTunnelKey,
        old_pos: GridPos,
    },
    // GUI-authoritative record deltas: these cover record mutations the sim-side
    // Command/UndoAction path has no notion of. All swap-style - they carry the
    // value to restore, and apply_gui_undo returns the variant carrying the
    // value it displaced, so undo and redo are one symmetric operation.
    //
    // Component/tunnel tombstone toggle (place records active=true then undo
    // sets it false; delete sets false then undo restores true).
    SetComponentActive {
        key: PlacedCompKey,
        active: bool,
    },
    SetTunnelActive {
        key: PlacedTunnelKey,
        active: bool,
    },
    // reconfigure_component swaps the whole underlying record (a new CompKey and
    // ComponentSpec, keeping grid_pos/active); this restores the old pair.
    SwapComponentDef {
        key: PlacedCompKey,
        comp_key: CompKey,
        def: ComponentSpec,
    },
    // Properties-panel tunnel rename edits the record's label field directly.
    SetTunnelLabel {
        key: PlacedTunnelKey,
        label: String,
    },
}

impl OsmilogApp {
    // Records a Wiring edit's delta into history, iff it changed anything.
    // The GUI counterpart of OsmilogApp::apply() for the Command/UndoAction
    // path: the Wiring mutators already return the WiringDelta (see
    // gui::wiring), so this is a plain pusher - callers do
    // `let delta = self.wiring.<edit>(..); self.edit_wiring(delta);`.
    pub(crate) fn edit_wiring(&mut self, delta: WiringDelta) {
        if !delta.is_empty() {
            self.history.push_gui(GuiUndoAction::WiringDelta {
                delta,
                forward: false,
            });
        }
    }

    // Applies one GUI undo action to the records/wiring and returns the action
    // that reverses *this* application (to record on the opposite history stack).
    // The GUI counterpart of Circuit::apply_undo; see GuiUndoAction for the
    // swap-style contract. Does not settle or rebuild nets - the undo/redo engine
    // (refresh_after_history) does that once after the whole entry is applied.
    pub(crate) fn apply_gui_undo(&mut self, action: GuiUndoAction) -> GuiUndoAction {
        match action {
            GuiUndoAction::WiringDelta { delta, forward } => {
                if forward {
                    self.wiring.redo_delta(&delta);
                } else {
                    self.wiring.undo_delta(&delta);
                }
                GuiUndoAction::WiringDelta {
                    delta,
                    forward: !forward,
                }
            }
            GuiUndoAction::MoveComponent { key, old_pos } => {
                let current = self.components[key].grid_pos;
                self.components[key].grid_pos = old_pos;
                GuiUndoAction::MoveComponent {
                    key,
                    old_pos: current,
                }
            }
            GuiUndoAction::MoveTunnel { key, old_pos } => {
                let current = self.tunnels[key].grid_pos;
                self.tunnels[key].grid_pos = old_pos;
                GuiUndoAction::MoveTunnel {
                    key,
                    old_pos: current,
                }
            }
            GuiUndoAction::SetComponentActive { key, active } => {
                let current = self.components[key].active;
                self.components[key].active = active;
                GuiUndoAction::SetComponentActive {
                    key,
                    active: current,
                }
            }
            GuiUndoAction::SetTunnelActive { key, active } => {
                let current = self.tunnels[key].active;
                self.tunnels[key].active = active;
                GuiUndoAction::SetTunnelActive {
                    key,
                    active: current,
                }
            }
            GuiUndoAction::SwapComponentDef {
                key,
                comp_key,
                def,
            } => {
                let pc = &mut self.components[key];
                let prev_comp_key = pc.key;
                let prev_def = std::mem::replace(&mut pc.def, def);
                pc.key = comp_key;
                GuiUndoAction::SwapComponentDef {
                    key,
                    comp_key: prev_comp_key,
                    def: prev_def,
                }
            }
            GuiUndoAction::SetTunnelLabel { key, label } => {
                let prev = std::mem::replace(&mut self.tunnels[key].label, label);
                GuiUndoAction::SetTunnelLabel { key, label: prev }
            }
        }
    }

    // Records a completed component/tunnel drag-move, if it actually moved.
    // Called once from ComponentDrag's drag_stopped handling; see
    // GuiUndoAction::MoveComponent/MoveTunnel for why this bypasses edit_wiring.
    pub(crate) fn commit_move(&mut self, key: crate::gui::app::Selected, old_pos: GridPos) {
        use crate::gui::app::Selected;
        match key {
            Selected::Component(k) => {
                if let Some(pc) = self.components.get(k) {
                    if pc.grid_pos != old_pos {
                        self.history
                            .push_gui(GuiUndoAction::MoveComponent { key: k, old_pos });
                    }
                }
            }
            Selected::Tunnel(k) => {
                if let Some(pt) = self.tunnels.get(k) {
                    if pt.grid_pos != old_pos {
                        self.history
                            .push_gui(GuiUndoAction::MoveTunnel { key: k, old_pos });
                    }
                }
            }
            Selected::Wire(_) => {}
        }
    }

    // Draws a wire route and relinks the circuit as one undo entry: the
    // Wiring-graph change and rebuild_circuit's resulting net relink are
    // batched together so they collapse into one HistoryEntry::Batch mixing
    // a Gui(WiringDelta) with the Sim(_) entries from ClearNets/Link/
    // LinkTunnel, rather than landing as two separate stack entries.
    pub(crate) fn commit_wire_route(
        &mut self,
        points: Vec<GridPos>,
        start_attach: NodeAttach,
        end_attach: NodeAttach,
    ) {
        self.history.begin_batch();
        let delta = self.wiring.add_route(&points, start_attach, end_attach);
        self.edit_wiring(delta);
        self.rebuild_circuit();
        self.history.end_batch();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::history::HistoryEntry;
    use crate::gui::wiring::Wiring;
    use crate::sim::component::PinId;
    use slotmap::SlotMap;

    fn comp_keys(n: usize) -> Vec<PlacedCompKey> {
        let mut sm: SlotMap<PlacedCompKey, ()> = SlotMap::with_key();
        (0..n).map(|_| sm.insert(())).collect()
    }

    #[test]
    fn edit_wiring_pushes_one_delta_when_changed() {
        let mut app = OsmilogApp::empty();
        let c = comp_keys(2);
        let delta = app.wiring.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c[0], PinId::output(0)),
            NodeAttach::Pin(c[1], PinId::input(0)),
        );
        app.edit_wiring(delta);
        assert_eq!(app.history.len(), 1);
        assert!(matches!(
            app.history.last(),
            Some(HistoryEntry::Gui(GuiUndoAction::WiringDelta { .. }))
        ));
    }

    #[test]
    fn edit_wiring_pushes_nothing_for_empty_delta() {
        let mut app = OsmilogApp::empty();
        // A sub-two-point route is a no-op, producing an empty delta.
        let delta = app.wiring.add_route(
            &[GridPos::new(0, 0)],
            NodeAttach::Free,
            NodeAttach::Free,
        );
        app.edit_wiring(delta);
        assert_eq!(app.history.len(), 0);
    }

    #[test]
    fn edit_wiring_delete_missing_segment_pushes_nothing() {
        let mut app = OsmilogApp::empty();
        let missing = {
            let mut w = Wiring::new();
            let c = comp_keys(2);
            w.add_route(
                &[GridPos::new(0, 0), GridPos::new(10, 0)],
                NodeAttach::Pin(c[0], PinId::output(0)),
                NodeAttach::Pin(c[1], PinId::input(0)),
            );
            let seg = w.active_segments().next().unwrap().0;
            seg
        };
        // The segment key belongs to a different Wiring; app.wiring is empty, so
        // delete produces an empty delta.
        let delta = app.wiring.delete_segment(missing);
        app.edit_wiring(delta);
        assert_eq!(app.history.len(), 0);
    }

    #[test]
    fn remove_component_nodes_on_wired_component_pushes_delta() {
        let mut app = OsmilogApp::empty();
        let c = comp_keys(2);
        app.wiring.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c[0], PinId::output(0)),
            NodeAttach::Pin(c[1], PinId::input(0)),
        );
        let delta = app.wiring.remove_component_nodes(c[0]);
        app.edit_wiring(delta);
        assert_eq!(app.history.len(), 1);
        assert!(matches!(
            app.history.last(),
            Some(HistoryEntry::Gui(GuiUndoAction::WiringDelta { .. }))
        ));
    }
}
