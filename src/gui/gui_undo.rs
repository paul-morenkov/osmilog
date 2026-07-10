use crate::gui::app::{OsmilogApp, PlacedCompKey, PlacedTunnelKey};
use crate::gui::geometry::GridPos;
use crate::gui::wiring::{NodeAttach, WiringDelta};
use crate::sim::component::{CompKey, ComponentSpec};

// Undo data for a GUI-level (Wiring/geometry) edit - the counterpart of
// sim::command::UndoAction, recorded onto the same gui::history::History
// stack (as HistoryEntry::Gui) so a GUI edit can share a Batch with the Sim
// UndoActions it triggers. Kept separate from UndoAction because Wiring/
// GridPos/PlacedCompKey must never leak into sim.
//
// No "GuiCommand" enum: every Wiring mutator's inverse is just "replay this
// delta backwards", so OsmilogApp calls the Wiring methods directly and
// hands the returned WiringDelta to edit_wiring.
#[derive(Debug)]
pub enum GuiUndoAction {
    // Undoes any Wiring-graph edit (add_route/delete_segment/
    // remove_component_nodes/remove_tunnel_nodes/prune_stale_pins) via the
    // delta it captured. `forward` picks the replay direction so the same
    // delta serves undo (false, runs undo_delta) and redo (true, runs
    // redo_delta) across the two stacks; apply_gui_undo flips it each time.
    WiringDelta {
        delta: WiringDelta,
        forward: bool,
    },
    // Undoes a component drag-move. Pushed directly from commit_move, not
    // edit_wiring: grid_pos is overwritten every drag frame, so by drag-end
    // there's no "before" state left except the drag-start original_grid_pos.
    MoveComponent {
        key: PlacedCompKey,
        old_pos: GridPos,
    },
    MoveTunnel {
        key: PlacedTunnelKey,
        old_pos: GridPos,
    },
    // GUI-authoritative record deltas the sim-side Command/UndoAction path has
    // no notion of. All swap-style: carry the value to restore, and
    // apply_gui_undo returns the value it displaced.

    // Component/tunnel tombstone toggle.
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
    // The GUI counterpart of OsmilogApp::apply() for the Command path.
    pub(crate) fn edit_wiring(&mut self, delta: WiringDelta) {
        if !delta.is_empty() {
            self.history.push_gui(GuiUndoAction::WiringDelta {
                delta,
                forward: false,
            });
        }
    }

    // Applies one GUI undo action and returns the action that reverses *this*
    // application (recorded on the opposite stack) - the GUI counterpart of
    // Circuit::apply_undo. Does not settle or rebuild nets; refresh_after_history
    // does that once after the whole entry is applied.
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

    // Draws a wire route and relinks the circuit as one undo entry: batches
    // the Wiring-graph change with rebuild_circuit's net relink into one
    // HistoryEntry::Batch instead of two separate entries.
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
