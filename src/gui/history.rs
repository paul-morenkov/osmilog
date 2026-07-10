use std::collections::HashSet;

use crate::gui::gui_undo::GuiUndoAction;
use crate::gui::wiring::{WireNodeKey, WireSegKey};
use crate::sim::command::UndoAction;

// One entry in the undo history: either a Circuit-level UndoAction (from
// OsmilogApp::apply()), a GUI-level GuiUndoAction (from OsmilogApp::edit_wiring()
// or a direct push after a drag-move), or a Batch of either/both collapsed
// into one user-visible step. Sim and GUI actions are deliberately two
// separate types - Circuit has no notion of grid_pos/Wiring - but they share
// one interleaved stack so a single user gesture that touches both (e.g.
// drawing a wire also relinks nets) stays one entry instead of two.
#[derive(Debug)]
pub enum HistoryEntry {
    Sim(UndoAction),
    Gui(GuiUndoAction),
    Batch(Vec<HistoryEntry>),
}

// Accumulates HistoryEntrys from every OsmilogApp::apply()/edit_wiring() call
// (see app.rs). Lives on the GUI side, not on Circuit, since batching
// boundaries ("this delete is one undo step") are a GUI-level concept
// Circuit has no visibility into.
//
// begin_batch/end_batch use a depth counter rather than a single flag
// because top-level App methods that issue multiple apply()/edit_wiring() calls
// (e.g. rebuild_circuit) are themselves called from inside other top-level
// methods that issue their own apply() calls first (e.g. delete_component
// calls apply(RemoveComponent) then rebuild_circuit()) - without
// depth-counting, a batch opened only inside the inner call would close
// before the outer method's own edit was accounted for, splitting one
// user-visible action into two undo entries. Nesting is safe to do
// uniformly: a single-call method wrapped in begin_batch/end_batch produces
// the same one stack entry as not wrapping it at all.
//
// Track-only for now: nothing reads `stack` back yet (that's the next
// step's undo()/redo(), which will consume it).
#[derive(Default)]
#[allow(dead_code)]
pub struct History {
    stack: Vec<HistoryEntry>,
    pending: Vec<HistoryEntry>,
    depth: u32,
}

impl History {
    pub fn push_sim(&mut self, action: UndoAction) {
        if matches!(action, UndoAction::NoOp) {
            return;
        }
        self.push_entry(HistoryEntry::Sim(action));
    }

    pub fn push_gui(&mut self, action: GuiUndoAction) {
        self.push_entry(HistoryEntry::Gui(action));
    }

    fn push_entry(&mut self, entry: HistoryEntry) {
        if self.depth > 0 {
            self.pending.push(entry);
        } else {
            self.stack.push(entry);
        }
    }

    pub fn begin_batch(&mut self) {
        self.depth += 1;
    }

    pub fn end_batch(&mut self) {
        debug_assert!(self.depth > 0, "end_batch without matching begin_batch");
        self.depth = self.depth.saturating_sub(1);
        if self.depth == 0 && !self.pending.is_empty() {
            let batch = std::mem::take(&mut self.pending);
            self.stack.push(if batch.len() == 1 {
                batch.into_iter().next().unwrap()
            } else {
                HistoryEntry::Batch(batch)
            });
        }
    }

    /// The union of every wire node/segment key referenced by any WiringDelta
    /// anywhere in the history (recursing into Batches, and including a pending
    /// open batch). This is the keep-set a tombstone GC pass consumes - see
    /// `Wiring::remove_unreferenced_tombstones`. Unwired for now, alongside the
    /// GC it feeds.
    #[allow(dead_code)]
    pub fn referenced_wire_keys(&self) -> (HashSet<WireNodeKey>, HashSet<WireSegKey>) {
        let mut nodes = HashSet::new();
        let mut segs = HashSet::new();
        fn walk(entry: &HistoryEntry, nodes: &mut HashSet<WireNodeKey>, segs: &mut HashSet<WireSegKey>) {
            match entry {
                HistoryEntry::Gui(GuiUndoAction::WiringDelta(delta)) => {
                    delta.collect_keys(nodes, segs);
                }
                HistoryEntry::Batch(entries) => {
                    for e in entries {
                        walk(e, nodes, segs);
                    }
                }
                HistoryEntry::Sim(_) | HistoryEntry::Gui(_) => {}
            }
        }
        for entry in self.stack.iter().chain(self.pending.iter()) {
            walk(entry, &mut nodes, &mut segs);
        }
        (nodes, segs)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.stack.len()
    }

    #[cfg(test)]
    pub(crate) fn last(&self) -> Option<&HistoryEntry> {
        self.stack.last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::app::PlacedCompKey;
    use crate::gui::geometry::GridPos;
    use crate::sim::component::CompKey;
    use slotmap::SlotMap;

    fn comp_key() -> CompKey {
        let mut sm: SlotMap<CompKey, ()> = SlotMap::with_key();
        sm.insert(())
    }

    fn placed_comp_key() -> PlacedCompKey {
        let mut sm: SlotMap<PlacedCompKey, ()> = SlotMap::with_key();
        sm.insert(())
    }

    #[test]
    fn push_sim_unbatched_produces_one_entry() {
        let mut h = History::default();
        h.push_sim(UndoAction::DeactivateComponent(comp_key()));
        assert_eq!(h.len(), 1);
        assert!(matches!(h.last(), Some(HistoryEntry::Sim(_))));
    }

    #[test]
    fn push_gui_unbatched_produces_one_entry() {
        let mut h = History::default();
        h.push_gui(GuiUndoAction::MoveComponent {
            key: placed_comp_key(),
            old_pos: GridPos::new(0, 0),
        });
        assert_eq!(h.len(), 1);
        assert!(matches!(h.last(), Some(HistoryEntry::Gui(_))));
    }

    #[test]
    fn push_sim_noop_pushes_nothing() {
        let mut h = History::default();
        h.push_sim(UndoAction::NoOp);
        assert_eq!(h.len(), 0);
    }

    #[test]
    fn batch_with_mixed_pushes_collapses_to_one_batch_entry() {
        let mut h = History::default();
        h.begin_batch();
        h.push_sim(UndoAction::DeactivateComponent(comp_key()));
        h.push_gui(GuiUndoAction::MoveComponent {
            key: placed_comp_key(),
            old_pos: GridPos::new(0, 0),
        });
        h.end_batch();
        assert_eq!(h.len(), 1);
        match h.last() {
            Some(HistoryEntry::Batch(entries)) => {
                assert_eq!(entries.len(), 2);
                assert!(matches!(entries[0], HistoryEntry::Sim(_)));
                assert!(matches!(entries[1], HistoryEntry::Gui(_)));
            }
            other => panic!("expected Batch, got {other:?}"),
        }
    }

    #[test]
    fn nested_batches_collapse_to_one_entry() {
        let mut h = History::default();
        h.begin_batch();
        h.push_sim(UndoAction::DeactivateComponent(comp_key()));
        h.begin_batch();
        h.push_gui(GuiUndoAction::MoveComponent {
            key: placed_comp_key(),
            old_pos: GridPos::new(0, 0),
        });
        h.end_batch();
        h.end_batch();
        assert_eq!(h.len(), 1);
        assert!(matches!(h.last(), Some(HistoryEntry::Batch(_))));
    }

    #[test]
    fn single_push_batch_unwraps_to_bare_entry() {
        let mut h = History::default();
        h.begin_batch();
        h.push_sim(UndoAction::DeactivateComponent(comp_key()));
        h.end_batch();
        assert_eq!(h.len(), 1);
        assert!(matches!(h.last(), Some(HistoryEntry::Sim(_))));
    }
}
