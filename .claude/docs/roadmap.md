# In-Progress / Not Yet Implemented

Known scope limits, deliberate gaps, and one class of latent panic. Cross-references point into
[simulator.md](simulator.md) and [gui.md](gui.md).

- **Undo/redo** is fully implemented and wired (see History / GUI undo in [gui.md](gui.md)) -
  `Document::undo`/`redo` consume the two-stack `History` via a symmetric `apply_entry`, exposed as
  an Edit menu (Undo / Redo, the latter `add_enabled`-gated on `can_redo`) and `Ctrl/Cmd+Z` /
  `Ctrl/Cmd+Y` / `Ctrl/Cmd+Shift+Z`. Deletes genuinely remove and move the payload into the history
  entry (no tombstones, no GC to run - memory is reclaimed immediately, bounded only by the capped
  history). Two deliberate scope limits, not gaps: **clock ticks are excluded from undo**
  (`Command::TickClock` is issued untracked, bypassing `apply`, since its `RestoreSeqState` replay is
  unimplemented), and undo/redo re-derives nets via `rebuild_circuit` rather than reversing net
  mutations.
- **Canvas pan/zoom**: implemented (`Document::camera: geometry::Camera`, see Documents / multiple
  circuits in [gui.md](gui.md)) - middle-mouse drag pans, Ctrl+scroll zooms toward the cursor. No
  keyboard/scrollbar pan and no "reset view" affordance yet.
- **Whole-wire-run selection**: selecting/deleting a wire is still per-segment. `Wiring::groups()`
  already computes the connected sets a "select the whole net" gesture would need.
- **Pin-index bounds checking**: `Component::net_of`/`Circuit::net_of`/`link` don't bounds-check
  pin indices, so an out-of-range pin (including from a hand-edited save file, which
  `CircuitSnapshot::validate()` doesn't check against a component's actual arity) can panic
  downstream.
- **`set_input` error handling**: silently no-ops on a non-`Input` component instead of returning
  a `Result` (marked with a `TODO` in `circuit.rs`).
- **Circuit document management**: subcircuits (see the Documents / Subcircuits sections in
  [gui.md](gui.md)) are implemented end-to-end, including project-file persistence - but there's no
  UI yet to rename or delete a circuit document once created (`create_circuit_doc` is the only
  mutator). Undo/redo is also scoped per-document (`History` lives inside each `Document`), so it
  does not span a circuit switch - undoing in a parent after editing and switching back out of a
  child only undoes the parent's own edits, never reaches into the child's history.
