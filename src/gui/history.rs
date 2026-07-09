use crate::sim::command::UndoAction;

// Accumulates UndoActions captured by App::apply() (see app.rs). Lives on
// the GUI side, not on Circuit, since batching boundaries ("this delete is
// one undo step") are a GUI-level concept Circuit has no visibility into.
//
// begin_batch/end_batch use a depth counter rather than a single flag
// because top-level App methods that issue multiple apply() calls (e.g.
// rebuild_circuit) are themselves called from inside other top-level methods
// that issue their own apply() calls first (e.g. delete_component calls
// apply(RemoveComponent) then rebuild_circuit()) - without depth-counting, a
// batch opened only inside the inner call would close before the outer
// method's own edit was accounted for, splitting one user-visible action
// into two undo entries. Nesting is safe to do uniformly: a single-call
// method wrapped in begin_batch/end_batch produces the same one stack entry
// as not wrapping it at all.
//
// Track-only for now: nothing reads `stack` back yet (that's the next
// step's undo()/redo(), which will consume it).
#[derive(Default)]
#[allow(dead_code)]
pub struct History {
    stack: Vec<UndoAction>,
    pending: Vec<UndoAction>,
    depth: u32,
}

impl History {
    pub fn push(&mut self, action: UndoAction) {
        if matches!(action, UndoAction::Noop) {
            return;
        }
        if self.depth > 0 {
            self.pending.push(action);
        } else {
            self.stack.push(action);
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
                UndoAction::Batch(batch)
            });
        }
    }
}
