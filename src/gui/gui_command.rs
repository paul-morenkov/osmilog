use crate::gui::app::{OsmilogApp, PlacedCompKey, PlacedTunnelKey};
use crate::gui::geometry::GridPos;
use crate::gui::wiring::{NodeAttach, WireSegKey, Wiring};

// The GUI-side counterpart of sim::command::Command: one variant per
// Wiring-graph mutation the GUI can make. Deliberately separate from Command
// (Wiring/GridPos/PlacedCompKey are GUI-only types that must never leak into
// sim), but recorded onto the same gui::history::History stack via
// HistoryEntry so a GuiCommand's undo data can sit in the same Batch as the
// Sim UndoActions it triggers (e.g. a wiring edit followed by
// rebuild_circuit's net relink).
#[derive(Debug)]
pub enum GuiCommand {
    AddRoute {
        points: Vec<GridPos>,
        start_attach: NodeAttach,
        end_attach: NodeAttach,
    },
    DeleteSegment(WireSegKey),
    RemoveComponentNodes(PlacedCompKey),
    RemoveTunnelNodes(PlacedTunnelKey),
    PruneStalePins {
        key: PlacedCompKey,
        n_inputs: usize,
        n_outputs: usize,
    },
}

// Undo data for a GUI-level edit. No NoOp/Batch variants (unlike
// sim::command::UndoAction): apply_gui returns () to a single caller class
// and simply skips push_gui when nothing changed, and batching already lives
// one level up in HistoryEntry::Batch.
#[derive(Debug)]
pub enum GuiUndoAction {
    // Undoes any of the AddRoute/DeleteSegment/RemoveComponentNodes/
    // RemoveTunnelNodes/PruneStalePins GuiCommands: a full pre-edit clone of
    // Wiring. Diffing precisely which nodes/segments changed is exactly as
    // hard as sim::command::LinkUndo::Split's already-deferred case (add_route
    // can split one segment into a node plus two segments in one call, and
    // reuses existing nodes elsewhere), so this snapshots the whole graph
    // instead - circuits are small, user-drawn diagrams, so cloning a couple
    // of SlotMaps per discrete edit is cheap.
    RestoreWiring(Wiring),
    // Undoes a component drag-move. Pushed directly (not via apply_gui) from
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
}

impl OsmilogApp {
    // Applies a GuiCommand to self.wiring and records its GuiUndoAction into
    // history, mirroring OsmilogApp::apply() for Command/UndoAction. Guards
    // known no-ops before cloning: for these five specific Wiring mutators,
    // AddRoute only ever adds nodes/segments and the other four only ever
    // remove them, so comparing (nodes.len(), segments.len()) before/after is
    // a sound (not heuristic) test of "did anything change" - it would not
    // generalize to a hypothetical mutator that could add and remove in the
    // same call.
    pub(crate) fn apply_gui(&mut self, cmd: GuiCommand) {
        match cmd {
            GuiCommand::AddRoute {
                points,
                start_attach,
                end_attach,
            } => {
                if points.len() < 2 {
                    return;
                }
                let before = self.wiring.clone();
                let before_shape = (before.nodes.len(), before.segments.len());
                self.wiring.add_route(&points, start_attach, end_attach);
                if (self.wiring.nodes.len(), self.wiring.segments.len()) != before_shape {
                    self.history.push_gui(GuiUndoAction::RestoreWiring(before));
                }
            }
            GuiCommand::DeleteSegment(seg) => {
                if !self.wiring.segments.contains_key(seg) {
                    return;
                }
                let before = self.wiring.clone();
                let before_shape = (before.nodes.len(), before.segments.len());
                self.wiring.delete_segment(seg);
                if (self.wiring.nodes.len(), self.wiring.segments.len()) != before_shape {
                    self.history.push_gui(GuiUndoAction::RestoreWiring(before));
                }
            }
            GuiCommand::RemoveComponentNodes(pck) => {
                let before = self.wiring.clone();
                let before_shape = (before.nodes.len(), before.segments.len());
                self.wiring.remove_component_nodes(pck);
                if (self.wiring.nodes.len(), self.wiring.segments.len()) != before_shape {
                    self.history.push_gui(GuiUndoAction::RestoreWiring(before));
                }
            }
            GuiCommand::RemoveTunnelNodes(ptk) => {
                let before = self.wiring.clone();
                let before_shape = (before.nodes.len(), before.segments.len());
                self.wiring.remove_tunnel_nodes(ptk);
                if (self.wiring.nodes.len(), self.wiring.segments.len()) != before_shape {
                    self.history.push_gui(GuiUndoAction::RestoreWiring(before));
                }
            }
            GuiCommand::PruneStalePins {
                key,
                n_inputs,
                n_outputs,
            } => {
                let before = self.wiring.clone();
                let before_shape = (before.nodes.len(), before.segments.len());
                self.wiring.prune_stale_pins(key, n_inputs, n_outputs);
                if (self.wiring.nodes.len(), self.wiring.segments.len()) != before_shape {
                    self.history.push_gui(GuiUndoAction::RestoreWiring(before));
                }
            }
        }
    }

    // Records a completed component/tunnel drag-move, if it actually moved.
    // Called once from ComponentDrag's drag_stopped handling; see
    // GuiUndoAction::MoveComponent/MoveTunnel for why this bypasses apply_gui.
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
    // a Gui(RestoreWiring) with the Sim(_) entries from ClearNets/Link/
    // LinkTunnel, rather than landing as two separate stack entries.
    pub(crate) fn commit_wire_route(
        &mut self,
        points: Vec<GridPos>,
        start_attach: NodeAttach,
        end_attach: NodeAttach,
    ) {
        self.history.begin_batch();
        self.apply_gui(GuiCommand::AddRoute {
            points,
            start_attach,
            end_attach,
        });
        self.rebuild_circuit();
        self.history.end_batch();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::history::HistoryEntry;
    use crate::sim::component::PinId;
    use slotmap::SlotMap;

    fn comp_keys(n: usize) -> Vec<PlacedCompKey> {
        let mut sm: SlotMap<PlacedCompKey, ()> = SlotMap::with_key();
        (0..n).map(|_| sm.insert(())).collect()
    }

    #[test]
    fn add_route_between_pins_pushes_one_restore_wiring() {
        let mut app = OsmilogApp::empty();
        let c = comp_keys(2);
        app.apply_gui(GuiCommand::AddRoute {
            points: vec![GridPos::new(0, 0), GridPos::new(10, 0)],
            start_attach: NodeAttach::Pin(c[0], PinId::output(0)),
            end_attach: NodeAttach::Pin(c[1], PinId::input(0)),
        });
        assert_eq!(app.history.len(), 1);
        assert!(matches!(
            app.history.last(),
            Some(HistoryEntry::Gui(GuiUndoAction::RestoreWiring(_)))
        ));
    }

    #[test]
    fn add_route_with_fewer_than_two_points_pushes_nothing() {
        let mut app = OsmilogApp::empty();
        app.apply_gui(GuiCommand::AddRoute {
            points: vec![GridPos::new(0, 0)],
            start_attach: NodeAttach::Free,
            end_attach: NodeAttach::Free,
        });
        assert_eq!(app.history.len(), 0);
    }

    #[test]
    fn delete_missing_segment_pushes_nothing() {
        let mut app = OsmilogApp::empty();
        let missing: WireSegKey = {
            let mut sm: SlotMap<WireSegKey, ()> = SlotMap::with_key();
            let k = sm.insert(());
            sm.remove(k);
            k
        };
        app.apply_gui(GuiCommand::DeleteSegment(missing));
        assert_eq!(app.history.len(), 0);
    }

    #[test]
    fn remove_component_nodes_on_unwired_component_pushes_nothing() {
        let mut app = OsmilogApp::empty();
        let c = comp_keys(1);
        app.apply_gui(GuiCommand::RemoveComponentNodes(c[0]));
        assert_eq!(app.history.len(), 0);
    }

    #[test]
    fn remove_component_nodes_on_wired_component_pushes_restore_with_prior_nodes() {
        let mut app = OsmilogApp::empty();
        let c = comp_keys(2);
        app.wiring.add_route(
            &[GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(c[0], PinId::output(0)),
            NodeAttach::Pin(c[1], PinId::input(0)),
        );
        let nodes_before = app.wiring.nodes.len();
        app.apply_gui(GuiCommand::RemoveComponentNodes(c[0]));
        assert_eq!(app.history.len(), 1);
        match app.history.last() {
            Some(HistoryEntry::Gui(GuiUndoAction::RestoreWiring(w))) => {
                assert_eq!(w.nodes.len(), nodes_before);
            }
            other => panic!("expected RestoreWiring, got {other:?}"),
        }
    }
}
