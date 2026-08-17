# GUI (`src/gui/`)

The one egui frontend, built on `sim` (see the layering rule in
[architecture.md](architecture.md)). For simulator internals, see
[simulator.md](simulator.md).

## OsmilogApp (`app.rs`)

`OsmilogApp` implements `eframe::App`, split into `logic` (pre-frame) and `ui`
(painting). It owns only cross-document state; every per-circuit field lives on
`Document` (see Documents below).

- `documents: SlotMap<DocId, CircuitDoc>`, `doc_order: Vec<DocId>`, `active_id: DocId` —
  every open circuit, its display/persisted order, and the active one.
  `active()`/`active_mut()` index `self.documents[self.active_id]`.
- `clipboard: Clipboard` — the last copied selection, decoupled from live `SlotMap`
  keys.
- `io_error: Option<String>` — File > Save/Load errors, separate from a `Document`'s
  own `settle_error`.
- `io: platform::IoState`, `show_profiler: bool`, `new_circuit_dialog: Option<String>`
  — platform file I/O state, the Debug-menu profiler toggle, and the new-circuit-name
  dialog buffer.

`InteractionMode` (a per-document field) is one of `Idle`, `Placing { spec:
ComponentSpec }`, `PlacingTunnel`, `WireDraw`, `ComponentDrag`, `BulkSelect`.

Every circuit mutation goes through `doc.apply(command)`
(`Document::apply`; see the Command layer in [simulator.md](simulator.md)), never a
direct `Circuit` call. Every `Wiring`-graph mutation calls the `Wiring` method directly
and passes its returned `WiringDelta` to `doc.edit_wiring(delta)`, which records it
(see History below).

**Canvas dispatch.** `OsmilogApp::handle_canvas_interaction` reads the active
document's `mode` and dispatches to one `interact_*` method per `InteractionMode`
variant, each taking a `&CanvasCtx` (`gui::utils::CanvasCtx`: the frame's
`egui::Response`/`Painter`/`Context` plus `Camera`/`Theme`). Every variant except
`Placing` lives on `Document`: `interact_idle`/`interact_placing_tunnel`/
`interact_wire_draw`/`interact_component_drag`/`interact_bulk_select`, plus `draw` and
`show_memory_editors`/`show_clock_controls`. `interact_placing` stays on `OsmilogApp`,
since placing a `Subcircuit` spec needs the whole `documents` registry.

## Properties panel (`properties.rs`)

The right-hand per-selection editor is mutation-free: `show_properties(doc: &Document,
ui)` reads the active document and returns the user's intent as an
`Option<PropGuiAction>` (`Reconfigure`/`OpenMemory`/`OpenCircuit`/`RenameTunnel`/
`SetTunnelLabelLive`/`Delete`). `OsmilogApp::apply_prop_gui_action` is the only place
those intents become mutations. `editing_locked`/`value_editing_locked` live on
`Document`; the `OsmilogApp` versions delegate.

`Document::reconfigure_component` is the parameter-swap path every component editor
commits through: `RemoveComponent` plus re-add under the same `PlacedCompKey`, pruning
wires to dropped pins, then rebuild.

## PlacedComponent / PlacedTunnel (`placed_component.rs`, `app.rs`)

`PlacedComponent { key: CompKey, spec: ComponentSpec, grid_pos: GridPos }` and
`PlacedTunnel { key: TunnelKey, label: String, role: TunnelRole, grid_pos: GridPos }`
are the GUI's visual records: a circuit-layer key plus enough to draw and place the
thing. `PlacedTunnel` is the only entity with a user-editable display label;
components use hardcoded per-type/pin labels (`ComponentSpec::label()`).

## Documents / multiple circuits (`document.rs`, `app.rs`)

`OsmilogApp` holds several circuit documents at once, so a subcircuit has something to
reference:

    pub struct Document { circuit, history, components, tunnels, wiring, mode, camera, selected, clock, memory_editor, settle_error }
    pub struct CircuitDoc { name: String, state: Document }

Fields of `Document`:

- `circuit: Circuit` — the simulation graph.
- `history: History` — accumulated undo entries (see History below).
- `components: HashMap<PlacedCompKey, PlacedComponent>`, `tunnels:
  HashMap<PlacedTunnelKey, PlacedTunnel>` — visual records, keyed separately from the
  circuit's own `CompKey`/`TunnelKey` so selection and `Wiring` bindings survive a
  `reconfigure_component`. Delete moves the record into the undo entry.
- `wiring: Wiring` — the GUI's connectivity graph (see Wiring below).
- `mode: InteractionMode` — what the canvas is doing.
- `selected: Option<Selection>` — `Single` (drives the properties panel) or `Bulk`
  (rectangle multi-select).
- `camera: Camera` — the canvas view transform (`geometry::Camera { pan, zoom }`).
  Middle-mouse drag pans; Ctrl+scroll zooms toward the cursor. Not persisted.
- `clock: Clock`, `memory_editor: MemoryEditor` — the clock-run state machine (see
  Clock below) and the ROM/RAM contents editor window's open state.
- `settle_error: Option<String>` — this document's last `settle()`/`tick_clock()`
  error, separate from `OsmilogApp::io_error`.

Most per-document behavior — simulation stepping, undo/redo, wiring queries,
placement/deletion, canvas drawing/interaction, the clock and memory-editor UI — is
implemented as methods on `Document`. `OsmilogApp` keeps only cross-document
operations: subcircuit instantiation, save/load, clipboard, the menu bar and palette.

`OsmilogApp::switch_circuit` reassigns `active_id`. Every document holds its own
settled nets, so switching needs no net rebuild; it does call `refresh_subcircuits()`
(below), since child circuits may have changed while inactive. `doc_order: Vec<DocId>`
fixes the palette and persisted circuit order. There is no UI yet to rename or delete a
circuit document (see [roadmap.md](roadmap.md)).

## Clock (`clock.rs`)

`Clock` is per-`Document` transport state for driving `tick_clock`, independent of a
free-running wall clock: `ClockRun` (`Stopped`/`Playing`/`Paused`), a
`ticks_per_second` rate, and the timestamp of the last auto-tick. `Document`'s
`show_clock_controls` renders Play/Pause/Step/Stop, gated on `ClockRun`. Editing locks
whenever `ClockRun` is not `Stopped`.

`Clock::advance` runs each frame while `Playing`, using a fixed-timestep accumulator
(`ticks_due`) that fires every interval elapsed since the last tick, up to
`MAX_CATCHUP_TICKS`, then resyncs to the current time to avoid replaying a long stall.
A tick that fails to settle auto-pauses. All ticks (`step`, and `stop`'s reset of
sequential state) go through `Circuit::apply` untracked, bypassing the undo stack.

## Subcircuits (`app.rs`)

Placing a document as a component inside another. The GUI is the only place that can
build a subcircuit's real inner `Circuit` (see Subcircuits in
[simulator.md](simulator.md)):

- `OsmilogApp::instantiate(spec)` is the spec-to-component build path used when placing
  or reconfiguring. It matches `spec.to_component()` for every primitive type; for
  `ComponentSpec::Subcircuit` it calls `build_doc_circuit` to build a real inner
  `Circuit`, recursing through nested subcircuits. `would_cycle` refuses a placement
  that would create a reference cycle.
- `build_doc_circuit(doc, visited)` builds a fresh standalone `Circuit` from a
  referenced document's records, the same translation `rebuild_circuit` applies to a
  live document. It returns the inner boundary `Input`/`Output` `CompKey`s ordered
  top-down, then left-to-right by `grid_pos` — this fixes the outer pin order.
- `derive_subcircuit_interface(doc)` returns `(name, input_widths, output_widths)` by
  building the doc's circuit and reading its boundary widths.
- `refresh_subcircuits()`, called on every switch back to a document, reconciles every
  placed `Subcircuit` against its referenced document: a changed pin count goes
  through `reconfigure_component`; an unchanged boundary rebuilds the inner `Circuit`
  in place and refreshes the cached name.

Pin binding is positional: outer pins map to inner boundary `Input`/`Output`s top-down
by grid position. An inner I/O edit that changes the boundary prunes stale wires.

The left panel's "User Created" list shows one entry per document. A single click
enters `Placing` mode with a ghost that follows the cursor. A double click opens that
document for editing via `switch_circuit`. An entry that would create a cycle is
disabled with a tooltip.

## Wiring (`wiring.rs`)

The GUI's connectivity model: a graph of grid-aligned `WireNode`s (`Free`,
`Pin(PlacedCompKey, PinId)`, `Tunnel(PlacedTunnelKey)`) joined by axis-aligned
`WireSegment`s. It knows nothing about `Circuit`. Connectivity derives on demand via
`Wiring::groups()` (union-find over the active segment graph);
`Document::rebuild_circuit` is the only place that translates `Wiring` into `Circuit`
calls (`clear_nets()`, then `link`/`link_tunnel` per group). Wire selection and
deletion are per-segment, not per-group (see [roadmap.md](roadmap.md)).

Nodes and segments live in `HashMap`s keyed by app-assigned `u64` ids
(`WireNodeKey`/`WireSegKey`, never reused). A deleted node or segment is genuinely
removed; the edit's `WiringDelta` carries the removed payload, so undo re-inserts it
under the same key.

## History / GUI undo (`history.rs`, `gui_undo.rs`)

    pub enum HistoryEntry { Sim(UndoAction), Gui(GuiUndoAction), Batch(Vec<HistoryEntry>) }
    fn Document::undo(&mut self) / fn redo(&mut self)

`History` holds two stacks: `undo_stack` and `redo_stack`. One `HistoryEntry`
accumulates per user gesture, from every `Document::apply()` (a `Sim` entry) and
`Document::edit_wiring()` (a `Gui` entry) call. `begin_batch`/`end_batch` collapse a
multi-step operation — for example, deleting a component issues both
`Command::RemoveComponent` and `Wiring::remove_component_nodes` — into one `Batch`
entry. Every fresh edit clears `redo_stack`.

Each `Wiring` mutator returns a `WiringDelta`: an ordered list of invertible
`WiringOp`s, one per changed node/segment slot. `Document::edit_wiring(delta)` records
it as `GuiUndoAction::WiringDelta`; a `forward` flag picks which direction to replay,
so one delta serves both the undo and redo stack. Component/tunnel drag-moves
(`GuiUndoAction::MoveComponent`/`MoveTunnel`) record directly via
`Document::commit_move`. `GuiUndoAction` also carries the GUI-only record deltas
`Command`/`UndoAction` has no notion of: `InsertComponent`/`RemoveComponent`,
`InsertTunnel`/`RemoveTunnel`, `SwapComponentSpec` (reconfigure), `SetTunnelLabel`
(rename).

`Document::undo`/`redo` run one operation in opposite directions, built on
`apply_entry(entry) -> HistoryEntry`: applying an entry reverses it and returns the
entry that reverses that, pushed onto the opposite stack. A `Batch` applies its
children last-first. Afterward, `refresh_after_history` re-syncs wire-node geometry,
clears the selection, and calls `rebuild_circuit`. Exposed as Edit menu Undo/Redo and
`Ctrl/Cmd+Z` / `Ctrl/Cmd+Y` / `Ctrl/Cmd+Shift+Z`. Clock ticks are excluded (see
[roadmap.md](roadmap.md)).

## Shape / geometry / theme (`shape.rs`, `geometry.rs`, `theme.rs`)

`ComponentShape` (outline, pin anchors, labels, in normalized `[0,1]²` coordinates) is
the visual description of one component instance, returned by
`ComponentSpec::shape()`. `geometry.rs` holds the per-type shape builders plus
grid/pixel constants. `theme.rs` derives canvas and signal colors from the ambient egui
`Visuals`, so light/dark tracks the OS live. Every component type specifies its own pin
geometry.
