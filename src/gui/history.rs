use std::collections::HashSet;

use crate::gui::gui_undo::GuiUndoAction;
use crate::gui::wiring::{WireNodeKey, WireSegKey};
use crate::sim::command::UndoAction;

// One entry in the undo history: a Circuit-level UndoAction, a GUI-level
// GuiUndoAction, or a Batch of either/both collapsed into one user-visible
// step. Sim and GUI actions stay separate types (Circuit has no notion of
// grid_pos/Wiring) but share one interleaved stack, so a gesture touching
// both (e.g. drawing a wire also relinks nets) stays one entry.
#[derive(Debug)]
pub enum HistoryEntry {
    Sim(UndoAction),
    Gui(GuiUndoAction),
    Batch(Vec<HistoryEntry>),
}

// Accumulates HistoryEntrys from every OsmilogApp::apply()/edit_wiring() call.
// Lives on the GUI side, not Circuit, since batching boundaries ("this delete
// is one undo step") are a GUI-level concept.
//
// begin_batch/end_batch use a depth counter, not a flag, because top-level
// methods that issue multiple apply()/edit_wiring() calls can themselves be
// called from inside other such methods (e.g. delete_component calls
// apply(RemoveComponent) then rebuild_circuit()) - without depth-counting, an
// inner batch would close before the outer edit was accounted for, splitting
// one user gesture into two undo entries.
//
// Two stacks: `undo_stack` grows via push_sim/push_gui/end_batch; `redo_stack`
// holds entries popped by undo() so redo() can replay them. Any fresh edit
// clears redo_stack (standard branch-invalidation). pop_*/push_* deliberately
// do NOT clear the opposite stack.
#[derive(Default)]
pub struct History {
    undo_stack: Vec<HistoryEntry>,
    redo_stack: Vec<HistoryEntry>,
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
            self.commit(entry);
        }
    }

    // Commits one finished top-level entry onto the undo stack and invalidates
    // any pending redo branch. Every new edit funnels through here (directly, or
    // via end_batch's collapse), so this is the single place redo_stack is
    // cleared.
    fn commit(&mut self, entry: HistoryEntry) {
        self.undo_stack.push(entry);
        self.redo_stack.clear();
    }

    pub fn begin_batch(&mut self) {
        self.depth += 1;
    }

    pub fn end_batch(&mut self) {
        debug_assert!(self.depth > 0, "end_batch without matching begin_batch");
        self.depth = self.depth.saturating_sub(1);
        if self.depth == 0 && !self.pending.is_empty() {
            let batch = std::mem::take(&mut self.pending);
            self.commit(if batch.len() == 1 {
                batch.into_iter().next().unwrap()
            } else {
                HistoryEntry::Batch(batch)
            });
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn pop_undo(&mut self) -> Option<HistoryEntry> {
        self.undo_stack.pop()
    }

    pub fn pop_redo(&mut self) -> Option<HistoryEntry> {
        self.redo_stack.pop()
    }

    // Pushes an entry produced by the undo/redo engine onto the opposite stack.
    // Unlike a fresh edit, these must NOT clear the other stack - undoing must
    // leave the rest of the redo branch intact, and vice versa.
    pub fn push_redo(&mut self, entry: HistoryEntry) {
        self.redo_stack.push(entry);
    }

    pub fn push_undo(&mut self, entry: HistoryEntry) {
        self.undo_stack.push(entry);
    }

    /// Keep-set for tombstone GC: every wire node/segment key referenced by
    /// any WiringDelta in the history, including a pending open batch. See
    /// `Wiring::remove_unreferenced_tombstones`.
    pub fn referenced_wire_keys(&self) -> (HashSet<WireNodeKey>, HashSet<WireSegKey>) {
        let mut nodes = HashSet::new();
        let mut segs = HashSet::new();
        fn walk(
            entry: &HistoryEntry,
            nodes: &mut HashSet<WireNodeKey>,
            segs: &mut HashSet<WireSegKey>,
        ) {
            match entry {
                HistoryEntry::Gui(GuiUndoAction::WiringDelta { delta, .. }) => {
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
        for entry in self
            .undo_stack
            .iter()
            .chain(self.redo_stack.iter())
            .chain(self.pending.iter())
        {
            walk(entry, &mut nodes, &mut segs);
        }
        (nodes, segs)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.undo_stack.len()
    }

    #[cfg(test)]
    pub(crate) fn last(&self) -> Option<&HistoryEntry> {
        self.undo_stack.last()
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
