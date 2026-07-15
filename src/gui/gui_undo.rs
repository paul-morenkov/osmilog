use crate::gui::app::{PlacedCompKey, PlacedTunnel, PlacedTunnelKey};
use crate::gui::document::Document;
use crate::gui::geometry::GridPos;
use crate::gui::placed_component::PlacedComponent;
use crate::gui::wiring::{NodeAttach, WireNodeKey, WiringDelta};
use crate::sim::component::{CompKey, ComponentSpec};

// Undo data for a GUI-level (Wiring/geometry) edit, recorded onto the same
// gui::history::History stack. Every Wiring mutator's inverse is just "replay
// this delta backwards", so OsmilogApp calls the Wiring methods directly and
// hands the returned WiringDelta to edit_wiring.
#[derive(Debug)]
pub enum GuiUndoAction {
    // Undoes any Wiring-graph edit via the delta it captured.
    // `forward` picks the replay direction so the same
    // delta serves undo (false, runs undo_delta) and redo (true, runs
    // redo_delta) across the two stacks; apply_gui_undo flips it each time.
    WiringDelta {
        delta: WiringDelta,
        forward: bool,
    },
    //  `grid_pos` is overwritten every drag frame, so by drag-end
    // there's no "before" state left except the drag-start original_grid_pos.
    MoveComponent {
        key: PlacedCompKey,
        old_pos: GridPos,
    },
    MoveTunnel {
        key: PlacedTunnelKey,
        old_pos: GridPos,
    },
    // Free-attached wire node dragged along with a bulk selection (see
    // OsmilogApp::free_wire_nodes/interact_component_drag) - same
    // overwritten-every-frame rationale as MoveComponent/MoveTunnel above.
    MoveWireNode {
        key: WireNodeKey,
        old_pos: GridPos,
    },

    // Place/delete of a component record. The record is genuinely
    // inserted/removed; `InsertComponent` carries the moved-out payload so
    // undo/redo shuttle it between the map and the history entry.
    InsertComponent {
        key: PlacedCompKey,
        comp: Box<PlacedComponent>,
    },
    RemoveComponent {
        key: PlacedCompKey,
    },
    // Place/delete of a tunnel record (see InsertComponent/RemoveComponent).
    InsertTunnel {
        key: PlacedTunnelKey,
        tunnel: Box<PlacedTunnel>,
    },
    RemoveTunnel {
        key: PlacedTunnelKey,
    },
    // reconfigure_component swaps the whole underlying record (a new CompKey and
    // ComponentSpec, keeping grid_pos/active); this restores the old pair.
    SwapComponentSpec {
        key: PlacedCompKey,
        comp_key: CompKey,
        spec: ComponentSpec,
    },
    // Properties-panel tunnel rename edits the record's label field directly.
    SetTunnelLabel {
        key: PlacedTunnelKey,
        label: String,
    },
}

impl Document {
    // Records a Wiring edit's delta into history, iff it changed anything.
    // The GUI counterpart of Document::apply() for the Command path.
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
                let pc = self.components.get_mut(&key).unwrap();
                let current = pc.grid_pos;
                pc.grid_pos = old_pos;
                GuiUndoAction::MoveComponent {
                    key,
                    old_pos: current,
                }
            }
            GuiUndoAction::MoveTunnel { key, old_pos } => {
                let pt = self.tunnels.get_mut(&key).unwrap();
                let current = pt.grid_pos;
                pt.grid_pos = old_pos;
                GuiUndoAction::MoveTunnel {
                    key,
                    old_pos: current,
                }
            }
            GuiUndoAction::MoveWireNode { key, old_pos } => {
                let n = self.wiring.nodes.get_mut(&key).unwrap();
                let current = n.pos;
                n.pos = old_pos;
                GuiUndoAction::MoveWireNode {
                    key,
                    old_pos: current,
                }
            }
            GuiUndoAction::RemoveComponent { key } => {
                let comp = self
                    .components
                    .remove(&key)
                    .expect("undo removes a live component record");
                GuiUndoAction::InsertComponent {
                    key,
                    comp: Box::new(comp),
                }
            }
            GuiUndoAction::InsertComponent { key, comp } => {
                self.components.insert(key, *comp);
                GuiUndoAction::RemoveComponent { key }
            }
            GuiUndoAction::RemoveTunnel { key } => {
                let tunnel = self
                    .tunnels
                    .remove(&key)
                    .expect("undo removes a live tunnel record");
                GuiUndoAction::InsertTunnel {
                    key,
                    tunnel: Box::new(tunnel),
                }
            }
            GuiUndoAction::InsertTunnel { key, tunnel } => {
                self.tunnels.insert(key, *tunnel);
                GuiUndoAction::RemoveTunnel { key }
            }
            GuiUndoAction::SwapComponentSpec {
                key,
                comp_key,
                spec,
            } => {
                let pc = self.components.get_mut(&key).unwrap();
                let prev_comp_key = pc.key;
                let prev_spec = std::mem::replace(&mut pc.spec, spec);
                pc.key = comp_key;
                GuiUndoAction::SwapComponentSpec {
                    key,
                    comp_key: prev_comp_key,
                    spec: prev_spec,
                }
            }
            GuiUndoAction::SetTunnelLabel { key, label } => {
                let prev =
                    std::mem::replace(&mut self.tunnels.get_mut(&key).unwrap().label, label);
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
                if let Some(pc) = self.components.get(&k) {
                    if pc.grid_pos != old_pos {
                        self.history
                            .push_gui(GuiUndoAction::MoveComponent { key: k, old_pos });
                    }
                }
            }
            Selected::Tunnel(k) => {
                if let Some(pt) = self.tunnels.get(&k) {
                    if pt.grid_pos != old_pos {
                        self.history
                            .push_gui(GuiUndoAction::MoveTunnel { key: k, old_pos });
                    }
                }
            }
            Selected::Wire(_) => {}
        }
    }

    // Records a completed Free-wire-node drag-move, if it actually moved.
    // The wire-node counterpart of commit_move, for the Free elbow nodes
    // interact_component_drag carries along with a bulk selection.
    pub(crate) fn commit_wire_node_move(&mut self, key: WireNodeKey, old_pos: GridPos) {
        if self.wiring.nodes[&key].pos != old_pos {
            self.history
                .push_gui(GuiUndoAction::MoveWireNode { key, old_pos });
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
    use crate::gui::app::OsmilogApp;
    use crate::gui::history::HistoryEntry;
    use crate::gui::wiring::Wiring;
    use crate::sim::component::PinId;

    // These keys are only used as Wiring pin attachments in tests; any distinct
    // values suffice.
    fn comp_keys(n: usize) -> Vec<PlacedCompKey> {
        (0..n as u64).map(PlacedCompKey).collect()
    }

    #[test]
    fn edit_wiring_pushes_one_delta_when_changed() {
        let mut app = OsmilogApp::empty();
        let c = comp_keys(2);
        let delta = app.active_mut().wiring.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c[0], PinId::output(0)),
            NodeAttach::Pin(c[1], PinId::input(0)),
        );
        app.active_mut().edit_wiring(delta);
        assert_eq!(app.active().history.len(), 1);
        assert!(matches!(
            app.active().history.last(),
            Some(HistoryEntry::Gui(GuiUndoAction::WiringDelta { .. }))
        ));
    }

    #[test]
    fn edit_wiring_pushes_nothing_for_empty_delta() {
        let mut app = OsmilogApp::empty();
        // A sub-two-point route is a no-op, producing an empty delta.
        let delta = app.active_mut().wiring.add_route(
            &[GridPos::new(0, 0)],
            NodeAttach::Free,
            NodeAttach::Free,
        );
        app.active_mut().edit_wiring(delta);
        assert_eq!(app.active().history.len(), 0);
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
        let delta = app.active_mut().wiring.delete_segment(missing);
        app.active_mut().edit_wiring(delta);
        assert_eq!(app.active().history.len(), 0);
    }

    #[test]
    fn remove_component_nodes_on_wired_component_pushes_delta() {
        let mut app = OsmilogApp::empty();
        let c = comp_keys(2);
        app.active_mut().wiring.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c[0], PinId::output(0)),
            NodeAttach::Pin(c[1], PinId::input(0)),
        );
        let delta = app.active_mut().wiring.remove_component_nodes(c[0]);
        app.active_mut().edit_wiring(delta);
        assert_eq!(app.active().history.len(), 1);
        assert!(matches!(
            app.active().history.last(),
            Some(HistoryEntry::Gui(GuiUndoAction::WiringDelta { .. }))
        ));
    }
}
