# In-Progress / Not Yet Implemented

Known scope limits, deliberate gaps, and one class of latent panic.
Cross-references point into [simulator.md](simulator.md) and [gui.md](gui.md).

- **Undo/redo** is fully implemented (see History / GUI undo in [gui.md](gui.md)).
  Two scope limits remain: clock ticks are excluded from undo
  (`Command::TickClock` is issued untracked; `RestoreSeqState` replay is
  unimplemented), and undo/redo re-derives nets via `rebuild_circuit` rather than
  reversing net mutations.
- **Canvas pan/zoom** is implemented (`Document::camera: geometry::Camera`).
  Middle-mouse drag pans; Ctrl+scroll zooms toward the cursor. No keyboard or scrollbar
  pan, and no "reset view" affordance.
- **Whole-wire-run selection** is missing: selecting or deleting a wire is still
  per-segment. `Wiring::groups()` already computes the connected sets a "select the
  whole net" gesture would need.
- **Pin-index bounds checking** is missing: `Component::net_of`/`Circuit::net_of`/`link`
  do not bounds-check pin indices. An out-of-range pin — including from a hand-edited
  save file, which `CircuitSnapshot::validate()` does not check against a component's
  actual arity — can panic downstream.
- **`set_input` error handling** silently no-ops on a non-`Input` component instead of
  returning a `Result` (marked with a `TODO` in `circuit.rs`).
- **Circuit document management**: subcircuits are implemented end-to-end, including
  project-file persistence, but there is no UI to rename or delete a circuit document
  once created (`create_circuit_doc` is the only mutator). Undo/redo is scoped per
  document (`History` lives inside each `Document`) and does not span a circuit switch:
  undoing in a parent after editing and switching back out of a child undoes only the
  parent's own edits.
