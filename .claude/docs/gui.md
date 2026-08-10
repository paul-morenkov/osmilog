# GUI (`src/gui/`)

The one egui-based frontend, built on top of `sim` (see the layering rule and the two
boundary-crossing types in [architecture.md](architecture.md)). For simulator-side internals
those methods dispatch to, see [simulator.md](simulator.md).

## OsmilogApp (`app.rs`)

The `eframe::App` implementation, split into `logic` (pre-frame) and `ui` (painting). Owns the
*cross-document* state only - every per-circuit field lives on `Document` instead (see Documents /
multiple circuits below):

- `documents: SlotMap<DocId, CircuitDoc>`, `doc_order: Vec<DocId>`, `active_id: DocId` - every open
  circuit document, the stable display order for the palette and persisted circuit order (`SlotMap`
  iteration order is unspecified), and which one is currently active. `active()`/`active_mut()` are
  the sole accessors onto the active document's state (`&self.documents[self.active_id].state`) -
  there is no separate set of "live" fields to keep in sync with a parked copy.
- `clipboard: Clipboard` - snapshot of the last copied selection, decoupled from live `SlotMap`
  keys so it survives undo/redo and further edits to the copied originals.
- `io_error: Option<String>` - File > Save/Load I/O errors, distinct from a `Document`'s own
  `settle_error` (a simulation-side problem); the menu bar shows whichever is set, I/O first.
- `io: platform::IoState`, `show_profiler: bool`, `new_circuit_dialog: Option<String>` - platform
  file-I/O orchestration, the Debug-menu puffin viewer toggle, and the new-circuit-name dialog
  (`Some(buffer)` while open).

`InteractionMode` (one of `Document`'s per-circuit fields) covers `Idle`, `Placing { spec:
ComponentSpec }`, `PlacingTunnel`, `WireDraw` (hybrid drag-elbow / click-polyline wire drawing),
`ComponentDrag`, and `BulkSelect` (rubber-band rectangle select, populating a `Selection::Bulk`).

Every circuit mutation goes through `doc.apply(command)` (`Document::apply`, see the Command layer
in [simulator.md](simulator.md)), never a direct `Circuit` method call. Every `Wiring`-graph
mutation calls the `Wiring` method directly and passes the `WiringDelta` it returns to
`doc.edit_wiring(delta)` (`Document::edit_wiring`, see History / GUI undo below), which records it -
that's what makes GUI edits undo-recordable in both domains. `doc` here is `self.active_mut()` from
`OsmilogApp`, or `&mut self` from a method already defined on `Document`.

**Canvas dispatch.** `OsmilogApp::handle_canvas_interaction` reads the active document's `mode`
and dispatches to one `interact_*` method per `InteractionMode` variant, each taking a `&CanvasCtx`
(`gui::utils::CanvasCtx` - the frame's `egui::Response`/`Painter`/`Context` plus `Camera`/`Theme`,
built fresh each frame in `ui()`, never stored). Every variant except `Placing` needs nothing but
the active document, so `interact_idle`/`interact_placing_tunnel`/`interact_wire_draw`/
`interact_component_drag`/`interact_bulk_select` (and `draw`, the canvas painter, and
`show_memory_editors`/`show_clock_controls`, the two free-floating-window/transport UI blocks) all
live directly on `Document` - `OsmilogApp` just calls `self.active_mut().interact_idle(cc,
pointer)` etc. `interact_placing` is the one exception and stays on `OsmilogApp`: placing a
component has to build the live `Component` via `instantiate`, which (for a `Subcircuit` spec)
needs the whole `documents` registry a bare `Document` doesn't have - the same
registry-vs-document split `reconfigure_component` uses (see Properties panel below).

## Properties panel (`properties.rs`)

The right-hand per-selection editor is a **mutation-free renderer**: `show_properties(doc:
&Document, ui)` reads only the active document (all its inputs are document-scoped) and *returns*
the user's intent as an `Option<PropGuiAction>` (`Reconfigure`/`OpenMemory`/`OpenCircuit`/
`RenameTunnel`/`SetTunnelLabelLive`/`Delete`) instead of mutating anything. The caller in
`app::ui` applies it via `OsmilogApp::apply_prop_gui_action`, the one place those intents become
mutations - so the panel stays decoupled from `OsmilogApp`. (The two edit-lock predicates it gates
widgets on, `editing_locked`/`value_editing_locked`, live on `Document`; the `OsmilogApp` ones
delegate.)

`reconfigure_component` (the parameter-swap path every component editor commits through:
`RemoveComponent` + re-add under the same `PlacedCompKey`, prune wires to dropped pins, rebuild)
lives on `Document`, alongside the record/wiring/undo bookkeeping it does. `OsmilogApp::
reconfigure_component` is a thin wrapper that pre-builds the new `Component` via `instantiate` (the
one step needing the whole `documents` registry) and hands it to the document method.

## PlacedComponent / PlacedTunnel (`placed_component.rs`, `app.rs`)

`PlacedComponent { key: CompKey, spec: ComponentSpec, grid_pos: GridPos }` and `PlacedTunnel {
key: TunnelKey, label: String, role: TunnelRole, grid_pos: GridPos }` are the GUI's visual
records - a circuit-layer key plus enough to draw and place the thing. `PlacedTunnel` is the one
entity with a user-editable display label; components only have hardcoded, non-editable
per-type/pin labels (`ComponentSpec::label()`, `ComponentShape::labels`).

## Documents / multiple circuits (`document.rs`, `app.rs`)

`OsmilogApp` can hold several circuit documents in memory at once, so a subcircuit has something
to reference. Every document's per-circuit state - active or not - lives directly in its own
`Document`, stored in `OsmilogApp::documents: SlotMap<DocId, CircuitDoc>`:

    pub struct Document { circuit, history, components, tunnels, wiring, mode, camera, selected, clock, memory_editor, settle_error }
    pub struct CircuitDoc { name: String, state: Document }

This is a single source of truth with no "parked vs. live" distinction - an earlier design (still
visible in old commit history) kept the active document's fields inline on `OsmilogApp` and moved
them into a `DocState` parked on `CircuitDoc` around every switch; that's gone. `Document`'s fields
are:

- `circuit: Circuit` - the simulation graph.
- `history: History` - accumulated undo entries (see History below).
- `components: HashMap<PlacedCompKey, PlacedComponent>`, `tunnels: HashMap<PlacedTunnelKey,
  PlacedTunnel>` (with sibling `next_placed_comp`/`next_placed_tunnel` `u64` id counters) - visual
  records, keyed by their own app-assigned `u64` ids (distinct from the circuit's own
  `CompKey`/`TunnelKey`) so selection/drag state and `Wiring`'s node bindings stay valid across a
  `reconfigure_component` (which changes the underlying `CompKey`). Delete removes the record and
  moves it into the undo entry; undo re-inserts under the same key (see History / GUI undo).
- `wiring: Wiring` - the GUI's connectivity graph (see Wiring below).
- `mode: InteractionMode` - what the canvas is currently doing.
- `selected: Option<Selection>` - `Selection::Single` (drives the properties panel) or
  `Selection::Bulk` (rectangle multi-select); `None` is the one "nothing selected" representation.
- `camera: Camera` - the canvas view transform (`geometry::Camera { pan, zoom }`). `pan` (screen-px
  offset) and `zoom` scale factor funnel through `Camera::grid_to_screen`/`screen_to_grid`/`scale`;
  every draw/hit function takes a `Camera`, not a bare `pan`. Middle-mouse drag pans, Ctrl+scroll
  (egui's `zoom_delta`, cursor-anchored, clamped `[ZOOM_MIN, ZOOM_MAX]`) zooms - both applied in
  `handle_camera_input` before drawing. Not persisted.
- `clock: Clock`, `memory_editor: MemoryEditor` - the clock-run state machine (see Clock below) and
  the free-floating ROM/RAM contents editor window's open/closed state.
- `settle_error: Option<String>` - this document's last `settle()`/`tick_clock()` error surface (an
  oscillation, a tunnel conflict, ...), distinct from `OsmilogApp::io_error`.

`OsmilogApp::active()`/`active_mut()` reach the active document's fields by indexing
`documents[active_id]`; most per-document behavior (simulation stepping, undo/redo, wiring
queries, placement/deletion, `apply`/`edit_wiring`, canvas drawing/interaction, the clock-transport
and memory-editor UI) is implemented as methods directly on `Document` rather than on `OsmilogApp`,
which only keeps the cross-document operations (subcircuit instantiation, save/load, clipboard
copy/paste, the menu bar and component palette UI) that need the whole `documents` registry or the
app-wide `clipboard` in scope.

Switching (`OsmilogApp::switch_circuit`) is just reassigning `active_id` - no `std::mem` moves, no
serialization, so it never deep-copies a `ComponentSpec` or a ROM's contents. Because every
document already holds its own settled nets regardless of whether it's active, no net rebuild is
needed on switch; what *is* needed is `refresh_subcircuits()` (see below), since child circuits may
have changed while this document was inactive. `doc_order: Vec<DocId>` fixes the display order for
the palette and the persisted circuit order (`SlotMap` iteration order is unspecified);
`create_circuit_doc` inserts a new blank `Document`, appends it to `doc_order`, and makes it
active. There is currently no UI to rename or delete a circuit document (see [roadmap.md](roadmap.md)).

## Subcircuits (`app.rs`)

Placing a document as a component inside another. The GUI is the *only* place that can build a
subcircuit's real inner `Circuit` (see the Subcircuits section in [simulator.md](simulator.md) for
why `ComponentSpec::to_component()` alone can't - it has no document registry):

- `OsmilogApp::instantiate(spec)` is the one spec->component build path the GUI itself uses
  (`place_component`, `reconfigure_component`) - identical to `spec.to_component()` for every
  primitive type, but for `ComponentSpec::Subcircuit` it calls `build_doc_circuit` to build a real
  inner `Circuit` instead of a placeholder. `instantiate_with`'s `visited: &mut Vec<DocId>` breaks
  an accidental reference cycle during the recursive build (real cycles are already refused at
  placement time by `would_cycle`) by yielding an empty placeholder instead of recursing forever.
- `build_doc_circuit(doc, visited)` builds a fresh standalone `Circuit` from a referenced
  document's records (components/tunnels/wiring), translating them the same way
  `rebuild_circuit` translates the *live* document - but into a new `Circuit`, untracked, and
  recursing through `instantiate_with` for nested subcircuits. It returns the inner boundary
  `Input`/`Output` `CompKey`s ordered top-down (then left-to-right by `grid_pos`), which fixes the
  outer pin order a placed subcircuit exposes.
- `derive_subcircuit_interface(doc)` returns `(name, input_widths, output_widths)` by actually
  building the doc's circuit and reading its boundary widths - the source of truth
  `ComponentSpec::Subcircuit`'s cache is refreshed from.
- `refresh_subcircuits()` (called by `switch_circuit` on every switch *back* to a document) walks
  every placed `Subcircuit` in the now-active document and reconciles it against its referenced
  document: if the boundary (pin count) changed, it goes through the normal undoable
  `reconfigure_component` path (prunes wires to dropped pins, same positional binding as any other
  reconfigure); if the boundary is unchanged, it just rebuilds the inner `Circuit` in place
  (`rebuild_subcircuit_inner`) and refreshes the cached name, then does one final
  `rebuild_circuit()` so the re-derived inner outputs settle outward.
- `doc_references(doc)`/`would_cycle(target)` walk placed-`Subcircuit` references (transitively) to
  refuse placing a document into itself, directly or through a chain of subcircuits - checked
  before every placement, in the component palette.

**Pin binding is positional**, like any other reconfigure: outer pins map to inner boundary
`Input`/`Output`s top-down by grid position, and an inner I/O edit that changes the boundary prunes
stale wires exactly like `reconfigure_component` does for any other type.

**Placement UX**: the left panel's "User Created" list (`show_component_palette`) shows one
selectable entry per document. A single click enters `InteractionMode::Placing` with a
`subcircuit_spec(doc)` ghost that follows the cursor - nothing is placed until a canvas click, same
as any other palette item. A double click instead calls `switch_circuit` to open that document for
editing (cancelling any placement the double-click's first click started, so a double-click never
also drops a stray component). An entry that would create a cycle is disabled with a tooltip
instead of removed, so the list's shape doesn't shift as you edit.

## Wiring (`wiring.rs`)

The GUI's own connectivity model: a graph of grid-aligned `WireNode`s (`Free`, `Pin(PlacedCompKey,
PinId)`, or `Tunnel(PlacedTunnelKey)`) joined by axis-aligned `WireSegment`s. Deliberately knows
nothing about `Circuit` - connectivity is derived on demand via `Wiring::groups()` (union-find
over the active segment graph), and `Document::rebuild_circuit` is the only place that translates
a `Wiring` state into `Circuit` calls (`clear_nets()` then `link`/`link_tunnel` per group). Wire
selection/deletion is currently per-segment, not per-group (see [roadmap.md](roadmap.md)).

**Stable keys, move-based undo.** Nodes and segments live in plain `HashMap`s keyed by app-assigned
`u64` ids (`WireNodeKey`/`WireSegKey`, from monotonic counters on `Wiring`, never reused). A
"deleted" `WireNode`/`WireSegment` is genuinely `remove()`d; the edit's `WiringDelta` op carries the
removed payload so undo re-inserts it under the *same* key (keys never dangle because the app owns
them). `Wiring::active_nodes()`/`active_segments()` are thin accessors that just iterate the whole
map yielding owned keys (the old slotmap-style interface - callers unchanged); there are no
tombstones to filter and no GC to run, so deleted memory is reclaimed on the spot. `insert_node_untracked`/
`insert_segment_untracked` mint keys without recording an op, for the history-free snapshot/clipboard
install paths.

## History / GUI undo (`history.rs`, `gui_undo.rs`, `wiring.rs`)

    pub enum HistoryEntry { Sim(UndoAction), Gui(GuiUndoAction), Batch(Vec<HistoryEntry>) }
    pub fn History::push_sim(&mut self, action: UndoAction)
    pub fn History::push_gui(&mut self, action: GuiUndoAction)
    pub fn History::begin_batch(&mut self) / fn end_batch(&mut self)
    pub fn History::pop_undo/pop_redo/push_undo/push_redo/can_undo/can_redo
    fn Document::undo(&mut self) / fn redo(&mut self)

`History` holds **two** stacks: `undo_stack` grows as edits are recorded (via `push_sim`/`push_gui`/
`end_batch`), `redo_stack` holds entries popped by `undo` so `redo` can replay them. Recording
accumulates one `HistoryEntry` per user gesture from every `Document::apply()` (Circuit mutations,
via `push_sim`) and `Document::edit_wiring()` (`Wiring`-graph mutations, via `push_gui`) call.
`begin_batch`/`end_batch` collapse a multi-step GUI operation (e.g. deleting a component, which
issues both a tracked `Command::RemoveComponent` and a `Wiring::remove_component_nodes`) into one
`HistoryEntry` - a `Batch` when it's more than one sub-entry, unwrapped to the bare entry when it's
exactly one. Every *fresh* edit funnels through `History::commit`, which also clears `redo_stack`
(the standard branch-invalidation); `pop_*`/`push_*` are the engine's stack moves and deliberately
do **not** clear the opposite stack. `rebuild_circuit` is history-free, so it contributes nothing to
a batch (its net reconstruction is untracked derived state).

The `Wiring` mutators (`add_route`, `delete_segment`, `remove_component_nodes`,
`remove_tunnel_nodes`, `prune_stale_pins`) each **return** a `gui::wiring::WiringDelta` - an ordered
list of invertible `WiringOp`s (`SetNode`/`SetSeg`, each carrying the `before`/`after` `Option<..>`
value of one node/segment slot; `None` = absent, so one op uniformly covers insert, remove, and
attach-change) whose stored size is proportional to the entries that edit touched, not the whole
graph. `undo_delta`/`redo_delta` just install each op's `before`/`after` value; because keys are
stable app-ids, a removed entry re-inserts under its own key, so `add_route`'s mid-wire split (remove
the original segment, add a mid node + two halves) reverses precisely. `Document::edit_wiring(delta)`
records a non-empty delta as
`GuiUndoAction::WiringDelta { delta, forward: false }` (the `forward` flag picks `undo_delta` vs
`redo_delta` so one delta serves both directions across the two stacks); there is no "GuiCommand"
enum, since unlike `sim::command::Command` every `Wiring` edit's inverse is uniform. Component/tunnel
drag-moves (`GuiUndoAction::MoveComponent`/`MoveTunnel`) are recorded directly
(`Document::commit_move`), bypassing `edit_wiring`, because `grid_pos` is written every drag frame
for live visual feedback - by the time the drag ends there's no "before" state left in the field to
read, only the `original_grid_pos` captured once at drag-start. `GuiUndoAction` additionally carries
the **GUI-authoritative record deltas** the sim-side `Command`/`UndoAction` path has no notion of:
`InsertComponent`/`RemoveComponent` and `InsertTunnel`/`RemoveTunnel` (place/delete of a `PlacedComponent`/
`PlacedTunnel` record - the record is genuinely removed and its payload moved into the `Insert*`
entry, mirroring the sim side; the GUI record maps are the same app-assigned-`u64`-keyed `HashMap`s),
`SwapComponentSpec` (reconfigure's whole-record swap), `SetTunnelLabel` (properties-panel rename) -
the swap-style ones store the value to restore and return the value they displaced.

**Consuming the stack.** `Document::undo`/`redo` are one symmetric operation in opposite
directions, built on `apply_entry(entry) -> HistoryEntry`: applying an entry performs the reversal
*and returns the entry that reverses that* (pushed onto the opposite stack). It dispatches
`Sim(a) -> Sim(circuit.apply_undo(a))`, `Gui(a) -> Gui(self.apply_gui_undo(a))`, and a `Batch` by
applying its children last-first and collecting their inverses (so redo of an undone batch replays
it forward). `Circuit::apply_undo` and `Document::apply_gui_undo` only touch authoritative state
(record insert/remove, input values, tunnel labels, specs) - never nets; afterward
`Document::refresh_after_history` re-syncs every live record's wire-node geometry (needed for a
move-undo, which carries no wiring delta), clears the selection, and `rebuild_circuit`s (re-deriving
nets + settling). Exposed as an Edit menu (Undo / Redo, the latter `add_enabled`-gated on
`can_redo`) and `Ctrl/Cmd+Z` (undo) / `Ctrl/Cmd+Y` / `Ctrl/Cmd+Shift+Z` (redo), all guarded by the
same widget-focus check as Delete so the shortcuts don't fire while editing a text field. Clock
ticks are excluded (see [roadmap.md](roadmap.md)).

## Shape / geometry / theme (`shape.rs`, `geometry.rs`, `theme.rs`)

`ComponentShape` (outline + pin anchors + labels, in normalized `[0,1]²` coordinates) is the
visual description of one component instance, returned by `ComponentSpec::shape()`; `geometry.rs`
holds the per-type shape builders plus grid/pixel constants; `theme.rs` derives canvas and signal
colors from the ambient egui `Visuals` so light/dark tracks the OS live. Nothing hardcodes "inputs
on the left" anywhere outside these shape builders - every component type specifies its own pin
geometry.
