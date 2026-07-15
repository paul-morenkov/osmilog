use std::collections::{HashSet, VecDeque};

use crate::gui::gui_undo::GuiUndoAction;
use crate::gui::wiring::{WireNodeKey, WireSegKey};
use crate::sim::command::UndoAction;

// Default cap on undo_stack/redo_stack length. A VecDeque (not Vec) backs
// both stacks so the oldest entries can be evicted from the front in O(1)
// when a push exceeds the cap, and so the cap itself can be changed mid-run
// (see History::set_limit) without rebuilding the stack.
pub const DEFAULT_HISTORY_LIMIT: usize = 100;

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
//
// Both stacks are capped at `limit` entries (default DEFAULT_HISTORY_LIMIT):
// every push evicts from the front until the stack fits, so the oldest edits
// age out first. `limit` isn't a const because it's meant to be adjustable
// mid-run (e.g. a future settings UI) via set_limit, which re-trims both
// stacks immediately to the new value.
pub struct History {
    undo_stack: VecDeque<HistoryEntry>,
    redo_stack: VecDeque<HistoryEntry>,
    pending: Vec<HistoryEntry>,
    depth: u32,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self {
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            pending: Vec::new(),
            depth: 0,
            limit: DEFAULT_HISTORY_LIMIT,
        }
    }
}

// Pushes onto a capped stack, evicting from the front until it fits `limit`.
fn push_capped(stack: &mut VecDeque<HistoryEntry>, entry: HistoryEntry, limit: usize) {
    stack.push_back(entry);
    while stack.len() > limit {
        stack.pop_front();
    }
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
        push_capped(&mut self.undo_stack, entry, self.limit);
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
        self.undo_stack.pop_back()
    }

    pub fn pop_redo(&mut self) -> Option<HistoryEntry> {
        self.redo_stack.pop_back()
    }

    // Pushes an entry produced by the undo/redo engine onto the opposite stack.
    // Unlike a fresh edit, these must NOT clear the other stack - undoing must
    // leave the rest of the redo branch intact, and vice versa. Still capped:
    // in practice this can't exceed `limit` (it only ever replays entries
    // popped off a stack that's itself capped), but capping here too keeps the
    // invariant self-evidently true rather than relying on that reasoning.
    pub fn push_redo(&mut self, entry: HistoryEntry) {
        push_capped(&mut self.redo_stack, entry, self.limit);
    }

    pub fn push_undo(&mut self, entry: HistoryEntry) {
        push_capped(&mut self.undo_stack, entry, self.limit);
    }

    /// Current cap on undo_stack/redo_stack length.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Changes the cap, trimming from the front of both stacks immediately if
    /// it shrinks below their current length. Lets the limit be adjusted
    /// mid-run (e.g. from a future settings UI) without losing the invariant
    /// that neither stack exceeds `limit`.
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
        while self.undo_stack.len() > limit {
            self.undo_stack.pop_front();
        }
        while self.redo_stack.len() > limit {
            self.redo_stack.pop_front();
        }
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
        self.undo_stack.back()
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

    fn placed_tunnel_key() -> crate::gui::app::PlacedTunnelKey {
        let mut sm: SlotMap<crate::gui::app::PlacedTunnelKey, ()> = SlotMap::with_key();
        sm.insert(())
    }

    // Pushes a uniquely-labeled entry, to distinguish push order in eviction tests.
    fn push_labeled(h: &mut History, label: &str) {
        h.push_gui(GuiUndoAction::SetTunnelLabel {
            key: placed_tunnel_key(),
            label: label.to_string(),
        });
    }

    fn label_of(entry: &HistoryEntry) -> &str {
        match entry {
            HistoryEntry::Gui(GuiUndoAction::SetTunnelLabel { label, .. }) => label,
            other => panic!("expected SetTunnelLabel, got {other:?}"),
        }
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

    #[test]
    fn default_limit_is_default_history_limit() {
        let h = History::default();
        assert_eq!(h.limit(), DEFAULT_HISTORY_LIMIT);
    }

    #[test]
    fn push_beyond_limit_evicts_oldest_from_undo_stack() {
        let mut h = History::default();
        h.set_limit(3);
        for label in ["a", "b", "c", "d", "e"] {
            push_labeled(&mut h, label);
        }
        assert_eq!(h.len(), 3);
        // Oldest ("a", "b") evicted; "c", "d", "e" remain, most recent last.
        let remaining: Vec<HistoryEntry> = std::iter::from_fn(|| h.pop_undo()).collect();
        let labels: Vec<&str> = remaining.iter().rev().map(label_of).collect();
        assert_eq!(labels, vec!["c", "d", "e"]);
    }

    #[test]
    fn set_limit_trims_existing_stack_from_front() {
        let mut h = History::default();
        for label in ["a", "b", "c", "d"] {
            push_labeled(&mut h, label);
        }
        assert_eq!(h.len(), 4);
        h.set_limit(2);
        assert_eq!(h.len(), 2);
        let remaining: Vec<HistoryEntry> = std::iter::from_fn(|| h.pop_undo()).collect();
        let labels: Vec<&str> = remaining.iter().rev().map(label_of).collect();
        assert_eq!(labels, vec!["c", "d"]);
    }

    #[test]
    fn set_limit_trims_redo_stack_too() {
        let mut h = History::default();
        for label in ["a", "b", "c"] {
            push_labeled(&mut h, label);
        }
        // Move all three onto the redo stack via pop_undo/push_redo, mirroring
        // what Document::undo does.
        while let Some(entry) = h.pop_undo() {
            h.push_redo(entry);
        }
        assert!(h.can_redo()); // sanity: redo has entries
        h.set_limit(1);
        assert!(h.pop_redo().is_some());
        assert!(h.pop_redo().is_none());
    }

    #[test]
    fn increasing_limit_does_not_drop_entries() {
        let mut h = History::default();
        h.set_limit(2);
        for label in ["a", "b"] {
            push_labeled(&mut h, label);
        }
        h.set_limit(5);
        assert_eq!(h.len(), 2);
        for label in ["c", "d", "e"] {
            push_labeled(&mut h, label);
        }
        assert_eq!(h.len(), 5);
    }
}
