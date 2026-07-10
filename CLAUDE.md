# osmilog

A digital logic circuit simulator in Rust with an egui graphical editor. Circuits are built
either programmatically (constructing `Component`s and wiring them with `Circuit::link`) or
interactively in the GUI. The simulator propagates combinational signal changes through the
circuit graph until stable (`settle`), and advances sequential state on an explicit clock tick
(`tick_clock`). The app targets both desktop (native window, via `eframe`) and the browser
(WASM), and circuits save to / load from a plain JSON file (`.som`).

The crate is a library (`src/lib.rs`: `pub mod gui / io / sim`) plus a thin binary
(`src/main.rs`) that just constructs `OsmilogApp` and hands it to `eframe`. Tests live in
`#[cfg(test)]` modules alongside the code they test.

Dependencies: `slotmap` (generational-arena keys for nets/components/tunnels/placed-GUI-records),
`eframe`/`egui` (GUI), `serde`/`serde_json` (save/load), `rfd` (native + async file dialogs).
WASM adds `wasm-bindgen`/`wasm-bindgen-futures`/`js-sys`/`web-sys`.

## Project Structure

    src/lib.rs                       crate root: pub mod gui / io / sim
    src/main.rs                       native eframe::run_native entry, and a wasm_bindgen(start) WASM entry

    src/sim.rs                       pub mod circuit / command / component / net / value
    src/sim/value.rs                  Value - signal representation (Floating / Fixed / Invalid)
    src/sim/net.rs                    Net - a wire connecting component pins
    src/sim/component.rs              Component, Logic/CombLogic, ComponentSpec, pin/key types
    src/sim/component/*.rs            one file per component kind (gate, mux, demux, splitter, reg, adder, ...)
    src/sim/circuit.rs                Circuit - the simulation graph, evaluation engine, tunnels
    src/sim/command.rs                Command/CommandOutput/UndoAction - the undo-recordable mutation layer

    src/gui.rs                       pub mod app / geometry / gui_undo / history / placed_component / shape / theme / wiring
    src/gui/app.rs                    OsmilogApp (eframe::App) - state, interaction modes, rendering, menu
    src/gui/placed_component.rs       PlacedComponent - visual record; GUI-only display methods on ComponentSpec
    src/gui/wiring.rs                 Wiring - GUI's own connectivity graph (grid nodes + segments), + WiringDelta undo
    src/gui/gui_undo.rs               GuiUndoAction (Wiring delta / drag-move) + OsmilogApp::edit_wiring/commit_move
    src/gui/history.rs                History - accumulates HistoryEntrys (Sim + Gui) from apply()/edit_wiring()
    src/gui/shape.rs                  ComponentShape, PinAnchor, tessellate_path - visual shape primitives
    src/gui/geometry.rs               per-component-type shape builders + grid/pixel geometry constants
    src/gui/theme.rs                  Theme - canvas/signal colors derived from ambient egui Visuals

    src/io.rs                        CircuitFile save/load format (JSON) + native/wasm file-dialog submodules

## Simulator / GUI separation

This is the load-bearing architectural boundary in the codebase, and it only runs one direction:

    gui  ──depends on──>  sim
    io   ──depends on──>  sim
    sim  ──depends on───  (nothing in this crate)

`sim` has no knowledge that a GUI exists. It has its own key types, its own construction API
(`Component::gate(...)`, `Component::mux(...)`, ...), and its own mutation/undo layer
(`sim::command::Command`). It could drive a headless simulation (a test suite, a CLI, a future
non-egui frontend) with zero changes.

`gui` is the one egui-based frontend built on top of `sim`. It keeps its own connectivity model
(`gui::wiring::Wiring` - grid nodes and segments) as the *source of truth for what's visually
wired together*, entirely separate from `sim::circuit::Circuit`, which is the *source of truth
for signal values*. After any wiring edit, the GUI throws away the circuit's nets and replays
them from `Wiring` (`OsmilogApp::rebuild_circuit`: `clear_nets()` + `link`/`link_tunnel` per
connected group, then `settle()`). The `Circuit` never learns about pixel/grid geometry; `Wiring`
never learns about signal values.

Two things deliberately cross the boundary, and both do it by depending on `sim`, never the
reverse:

- **`sim::component::ComponentSpec`** is a plain construction-params enum, defined by `sim` as the
  inverse of `to_component()` (`Component::spec()`; the GUI's `PlacedComponent` uses it as its
  "what to construct" record). The GUI reuses this *exact* enum, unmodified, as `PlacedComponent`'s
  own record - `gui::placed_component` adds a second `impl ComponentSpec` block with GUI-only
  display methods (`size`, `label`, `shape`) that depend on `gui::geometry`/`gui::shape` types
  `sim` must never depend on. Rust allows an inherent impl of a crate-local type from any module
  in the crate, so this needs no wrapper/newtype - one enum, one save-file representation, two
  impl blocks in two layers.
- **`sim::command::Command`** is how the GUI mutates the circuit at all. `OsmilogApp` never calls
  `Circuit::add_component`/`link`/`remove_component`/etc. directly; every *authoritative* edit goes
  through `OsmilogApp::apply(Command) -> CommandOutput`, which calls `Circuit::apply` (returning
  `(CommandOutput, UndoAction)`) and pushes the `UndoAction` onto `gui::history::History`. Edits
  that only reconstruct *derived* net state (`ClearNets`/`Link`/`LinkTunnel`, all issued from
  `rebuild_circuit`) bypass that wrapper and call `self.circuit.apply(..).0` untracked - undo
  re-derives the nets rather than reversing them (see the Command layer section below). This makes
  every GUI-issued authoritative mutation undo-recordable without the GUI needing to know how to
  reverse anything itself.
  `gui::gui_undo::GuiUndoAction` is the GUI-only undo counterpart for edits `Command` has no
  notion of (wiring-graph changes, component/tunnel moves) - a wholly separate type since
  `Wiring`/`GridPos`/`PlacedCompKey` must stay out of `sim`, but recorded onto the *same*
  `History` stack (as `HistoryEntry::Sim`/`HistoryEntry::Gui`) so a GUI edit and the `Command`s it
  triggers (e.g. drawing a wire also relinks nets via `rebuild_circuit`) collapse into one
  `HistoryEntry::Batch` instead of two disconnected entries. Unlike the sim side there is no
  "GuiCommand" enum: every `Wiring` mutator's inverse is just "replay its delta backwards", so
  `OsmilogApp` calls the `Wiring` method directly and hands the returned `WiringDelta` to
  `OsmilogApp::edit_wiring` - no command-as-data indirection.

`src/io.rs` (save/load) also depends only on `sim` types (`ComponentSpec`, `TunnelRole`) plus a
couple of GUI-defined-but-plain-data geometry types - it does not depend on `OsmilogApp` itself.

## Simulator (`src/sim/`)

### Value (`value.rs`)

    pub enum Value { Floating, Fixed { bits: u32, width: u8 }, Invalid }

The signal representation everywhere in the simulator. `Floating` is "unconnected/undefined" and
is absorbing through every operator. `Invalid` means the *wiring itself* is structurally wrong
(a short, or a width mismatch) - it's never produced by component logic, only by `Circuit`, and
it never propagates past the one net where it's flagged.

### Net (`net.rs`) and Circuit (`circuit.rs`)

A `Net` (keyed by `NetKey`) connects one or more component pins: `sources: Vec<(CompKey,
OutIdx)>`, `sinks: Vec<(CompKey, InIdx)>`. `Circuit` owns all `Net`s, `Component`s, and `Tunnel`s
(a "net label" mechanism - components sharing a tunnel label are wired together without a drawn
connection) in `SlotMap`s, plus a dirty-net queue that drives propagation.

Circuit's public interface:

    fn add_component(&mut self, comp: Component) -> CompKey
    fn link(&mut self, a: CompKey, a_pin: PinId, b: CompKey, b_pin: PinId) -> NetKey
    fn remove_component(&mut self, key: CompKey)
    fn add_tunnel(&mut self, label: String, role: TunnelRole) -> TunnelKey
    fn link_tunnel(&mut self, tunnel: TunnelKey, comp: CompKey, pin: PinId) -> NetKey
    fn detach_tunnel(&mut self, tunnel: TunnelKey)
    fn remove_tunnel(&mut self, tunnel: TunnelKey)
    fn rename_tunnel(&mut self, tunnel: TunnelKey, new_label: String)
    fn set_input(&mut self, comp: CompKey, bits: u32, width: u8)
    fn read_output(&self, comp: CompKey) -> Value
    fn clear_nets(&mut self)
    fn settle(&mut self) -> Result<(), SettleError>
    fn tick_clock(&mut self) -> Result<(), SettleError>

`settle()` drains the dirty queue, recomputing net values and re-evaluating combinational sinks
until nothing changes, and returns `SettleError::Oscillation` or `SettleError::TunnelConflict`
rather than looping forever or panicking. `tick_clock()` snapshots every sequential component's
inputs, advances them all one step, then calls `settle()` to propagate the result.

These methods are the layer `sim::command::Command` dispatches to (see below) and what direct
sim-layer tests call; they're still `pub` and used directly within `sim`, but the GUI never calls
them itself (see Simulator/GUI separation above).

### Command layer (`command.rs`)

    pub enum Command { AddComponent(Component), Link { .. }, RemoveComponent(CompKey), .. }
    fn Circuit::apply(&mut self, command: Command) -> (CommandOutput, UndoAction)

One `Command` variant per structural mutation `Circuit` supports. `apply` dispatches to the
matching `Circuit` method and returns both the output and the `UndoAction` that reverses that one
command (callers that don't want the undo take `.0`). This is the seam the GUI's undo/redo is built
on (see History below); `Circuit::apply_undo(UndoAction) -> UndoAction` is what *consumes* one -
applying it reverses the recorded command and returns the inverse to record on the opposite stack.

The `UndoAction`s are deliberately minimal, because **the circuit's net structure is derived
state**: the GUI rebuilds every net from its authoritative `Wiring`/component/tunnel records after
any edit (`gui::app::rebuild_circuit`), so undo restores those records and re-derives the nets
rather than reversing net mutations. Hence `ClearNets`/`Link`/`LinkTunnel`/`DetachTunnel` capture
`NoOp` (no net snapshots, no `NetKey`s), and only the *authoritative* commands capture a real
inverse. Component and tunnel removal **tombstones** (an `active: bool` on `Component`/`Tunnel`,
mirroring `Wiring`) rather than deleting: `remove_component`/`remove_tunnel` flip `active` off (the
engine's whole-component sweeps - `tick_clock`, `clear_nets`'s re-eval - skip inactive), and undo is
a stable-key `reactivate_component`/`reactivate_tunnel` (`ReactivateComponent`/`ReactivateTunnel`).
This keeps `CompKey`/`TunnelKey`s stable across undo (nothing else in the history needs remapping)
and preserves a removed `Reg`'s latched state for reactivation - which a spec-based re-creation
could not. `remove_unreferenced_tombstones` (unwired) reclaims tombstones no history entry
references. The remaining real inverses: `DeactivateComponent`/`DeactivateTunnel` (undo of an add),
`SetInput`, `RenameTunnel`, `RestoreSeqState` (undo of `TickClock`).

### Component model (`component.rs` + `component/*.rs`)

    pub struct Component { pub pins: Pins, pub logic: Logic }
    pub enum Logic { Comb(LogicComb), Seq(LogicSeq) }

    pub trait CombLogic {
        fn n_inputs(&self) -> usize;
        fn n_outputs(&self) -> usize;
        fn evaluate(&self, inputs: &[Value]) -> Vec<Value>;
        fn input_width(&self, i: usize) -> Option<u8>;
        fn output_width(&self, i: usize) -> Option<u8>;
    }

Every combinational component type (`Gate`, `Mux`, `Demux`, `Splitter`/`Combine`, `Encoder`,
`Adder`, `Subtractor`, `Multiplier`, `Divider`, `Comparator` - one struct per file under
`component/`) implements `CombLogic`, bundling its construction params, pin arity, and evaluation
logic in one place so they can't drift apart. `Input` and `Output` are the two sourceless/sinkless
special cases. `Reg` is the one sequential component type (`Logic::Seq`); sequential components
sit out of `settle()`'s propagation and only change state via `tick_clock()`. See each file under
`src/sim/component/` for a given type's specific behavior.

### ComponentSpec (`component.rs`)

    pub enum ComponentSpec { Input(Input), Gate(Gate), Mux(Mux), .. }
    fn ComponentSpec::to_component(&self) -> Component
    fn Component::spec(&self) -> ComponentSpec   // pub(crate)

The canonical "construction params" record - everything needed to build an equivalent
`Component`, without any live wiring or runtime state (a `Reg`'s latched value, a live
`Component`'s `NetKey`s, are never part of a `ComponentSpec`). It's the GUI's `PlacedComponent`
record (see Simulator/GUI separation above for how the GUI attaches its own methods to this same
type). `Component::spec()` (the inverse of `to_component()`) has no production caller now that undo
tombstones live components rather than snapshotting them into specs; it's retained, guarded by
`test_component_spec_round_trips_pin_arity`, for the arity invariant the GUI relies on.

## GUI (`src/gui/`)

### OsmilogApp (`app.rs`)

The `eframe::App` implementation, split into `logic` (pre-frame) and `ui` (painting). Owns:

- `circuit: Circuit` - the simulation graph.
- `history: History` - accumulated undo entries (see History below).
- `components: SlotMap<PlacedCompKey, PlacedComponent>`, `tunnels: SlotMap<PlacedTunnelKey,
  PlacedTunnel>` - visual records, keyed by their own generational keys (distinct from the
  circuit's own `CompKey`/`TunnelKey`) so selection/drag state and `Wiring`'s node bindings stay
  valid across a `reconfigure_component` (which changes the underlying `CompKey`).
- `wiring: Wiring` - the GUI's connectivity graph (see Wiring below).
- `mode: InteractionMode` - what the canvas is currently doing.
- `selected: Option<Selected>`, `bulk_selection: Vec<Selected>` - single selection (drives the
  properties panel) and rectangle multi-select, kept separate.
- `pan: Vec2` - canvas pan offset (present but not yet wired to any interaction - see In-Progress).

`InteractionMode` covers `Idle`, `Placing { spec: ComponentSpec }`, `PlacingTunnel`, `WireDraw`
(hybrid drag-elbow / click-polyline wire drawing), `ComponentDrag`, and `BulkSelect` (rubber-band
rectangle select, populating `bulk_selection`).

Every circuit mutation goes through `self.apply(command)` (see Command layer above), never a
direct `Circuit` method call. Every `Wiring`-graph mutation calls the `Wiring` method directly and
passes the `WiringDelta` it returns to `self.edit_wiring(delta)` (see History / GUI undo below),
which records it - that's what makes GUI edits undo-recordable in both domains.

### PlacedComponent / PlacedTunnel (`placed_component.rs`, `app.rs`)

`PlacedComponent { key: CompKey, spec: ComponentSpec, grid_pos: GridPos }` and `PlacedTunnel {
key: TunnelKey, label: String, role: TunnelRole, grid_pos: GridPos }` are the GUI's visual
records - a circuit-layer key plus enough to draw and place the thing. `PlacedTunnel` is the one
entity with a user-editable display label; components only have hardcoded, non-editable
per-type/pin labels (`ComponentSpec::label()`, `ComponentShape::labels`).

### Wiring (`wiring.rs`)

The GUI's own connectivity model: a graph of grid-aligned `WireNode`s (`Free`, `Pin(PlacedCompKey,
PinId)`, or `Tunnel(PlacedTunnelKey)`) joined by axis-aligned `WireSegment`s. Deliberately knows
nothing about `Circuit` - connectivity is derived on demand via `Wiring::groups()` (union-find
over the active segment graph), and `OsmilogApp::rebuild_circuit` is the only place that translates
a `Wiring` state into `Circuit` calls (`clear_nets()` then `link`/`link_tunnel` per group). Wire
selection/deletion is currently per-segment, not per-group (see In-Progress).

**Tombstoning.** Editing never `remove()`s from the node/segment `SlotMap`s: a "deleted"
`WireNode`/`WireSegment` is flagged `active = false` instead, so its key stays valid forever and an
edit's undo record can be a compact list of `active`-bit flips rather than a whole-graph clone (see
GUI undo below). Consequently every connectivity/hit/drawing/save read iterates `Wiring::
active_nodes()`/`active_segments()`, never the raw maps (raw indexing `nodes[k]` is still fine - a
tombstone is simply never iterated). Tombstones accumulate with cumulative edits (not circuit
size); `Wiring::remove_unreferenced_tombstones` reclaims any not referenced by the live history
(keys gathered by `History::referenced_wire_keys`) - defined but not yet called.

### History / GUI undo (`history.rs`, `gui_undo.rs`, `wiring.rs`)

    pub enum HistoryEntry { Sim(UndoAction), Gui(GuiUndoAction), Batch(Vec<HistoryEntry>) }
    pub fn History::push_sim(&mut self, action: UndoAction)
    pub fn History::push_gui(&mut self, action: GuiUndoAction)
    pub fn History::begin_batch(&mut self) / fn end_batch(&mut self)
    pub fn History::pop_undo/pop_redo/push_undo/push_redo/can_undo/can_redo
    fn OsmilogApp::undo(&mut self) / fn redo(&mut self)

`History` holds **two** stacks: `undo_stack` grows as edits are recorded (via `push_sim`/`push_gui`/
`end_batch`), `redo_stack` holds entries popped by `undo` so `redo` can replay them. Recording
accumulates one `HistoryEntry` per user gesture from every `OsmilogApp::apply()` (Circuit mutations,
via `push_sim`) and `OsmilogApp::edit_wiring()` (`Wiring`-graph mutations, via `push_gui`) call.
`begin_batch`/`end_batch` collapse a multi-step GUI operation (e.g. deleting a component, which
issues both a tracked `Command::RemoveComponent` and a `Wiring::remove_component_nodes`) into one
`HistoryEntry` - a `Batch` when it's more than one sub-entry, unwrapped to the bare entry when it's
exactly one. Every *fresh* edit funnels through `History::commit`, which also clears `redo_stack`
(the standard branch-invalidation); `pop_*`/`push_*` are the engine's stack moves and deliberately
do **not** clear the opposite stack. `rebuild_circuit` is history-free, so it contributes nothing to
a batch (its net reconstruction is untracked derived state).

The `Wiring` mutators (`add_route`, `delete_segment`, `remove_component_nodes`,
`remove_tunnel_nodes`, `prune_stale_pins`) each **return** a `gui::wiring::WiringDelta` - an ordered
list of invertible `WiringOp`s (`NodeActive`/`SegActive` bit flips, `SetAttach` swaps) whose stored
size is proportional to the entries that edit touched, not the whole graph. Because deletes
tombstone rather than remove, undo/redo are just `Wiring::undo_delta`/`redo_delta` replaying those
flips (keys never move, so `add_route`'s mid-wire split - which the old whole-graph snapshot existed
to sidestep - is captured precisely). `OsmilogApp::edit_wiring(delta)` records a non-empty delta as
`GuiUndoAction::WiringDelta { delta, forward: false }` (the `forward` flag picks `undo_delta` vs
`redo_delta` so one delta serves both directions across the two stacks); there is no "GuiCommand"
enum, since unlike `sim::command::Command` every `Wiring` edit's inverse is uniform. Component/tunnel
drag-moves (`GuiUndoAction::MoveComponent`/`MoveTunnel`) are recorded directly
(`OsmilogApp::commit_move`), bypassing `edit_wiring`, because `grid_pos` is written every drag frame
for live visual feedback - by the time the drag ends there's no "before" state left in the field to
read, only the `original_grid_pos` captured once at drag-start. `GuiUndoAction` additionally carries
the **GUI-authoritative record deltas** the sim-side `Command`/`UndoAction` path has no notion of:
`SetComponentActive`/`SetTunnelActive` (place/delete tombstone toggle), `SwapComponentDef`
(reconfigure's whole-record swap), `SetTunnelLabel` (properties-panel rename) - all swap-style, i.e.
they store the value to restore and return the value they displaced.

**Consuming the stack.** `OsmilogApp::undo`/`redo` are one symmetric operation in opposite
directions, built on `apply_entry(entry) -> HistoryEntry`: applying an entry performs the reversal
*and returns the entry that reverses that* (pushed onto the opposite stack). It dispatches
`Sim(a) -> Sim(circuit.apply_undo(a))`, `Gui(a) -> Gui(self.apply_gui_undo(a))`, and a `Batch` by
applying its children last-first and collecting their inverses (so redo of an undone batch replays
it forward). `Circuit::apply_undo` and `OsmilogApp::apply_gui_undo` only touch authoritative state
(active flags, input values, tunnel labels, records) - never nets; afterward
`OsmilogApp::refresh_after_history` re-syncs every live record's wire-node geometry (needed for a
move-undo, which carries no wiring delta), clears the selection, and `rebuild_circuit`s (re-deriving
nets + settling). Exposed as an Edit menu (Undo / Redo, the latter `add_enabled`-gated on
`can_redo`) and `Ctrl/Cmd+Z` (undo) / `Ctrl/Cmd+Y` / `Ctrl/Cmd+Shift+Z` (redo), all guarded by the
same widget-focus check as Delete so the shortcuts don't fire while editing a text field. Clock
ticks are excluded (see In-Progress below).

### Shape / geometry / theme (`shape.rs`, `geometry.rs`, `theme.rs`)

`ComponentShape` (outline + pin anchors + labels, in normalized `[0,1]²` coordinates) is the
visual description of one component instance, returned by `ComponentSpec::shape()`; `geometry.rs`
holds the per-type shape builders plus grid/pixel constants; `theme.rs` derives canvas and signal
colors from the ambient egui `Visuals` so light/dark tracks the OS live. Nothing hardcodes "inputs
on the left" anywhere outside these shape builders - every component type specifies its own pin
geometry.

## Save / Load (`src/io.rs`)

`CircuitFile { version, components: Vec<ComponentEntry>, tunnels: Vec<TunnelEntry>, nodes:
Vec<NodeEntry>, segments: Vec<SegEntry> }` mirrors the GUI's visual state (placed components/
tunnels + the `Wiring` graph), not `Circuit`'s internal `SlotMap`s - every cross-reference is a
plain `usize` index into one of the file's own vectors, since slotmap keys are ephemeral and not
worth persisting. `version` is bumped whenever the shape changes incompatibly; `validate()` checks
version and index bounds before a load replaces the current app state. Native and WASM get
separate submodules (`io::native`, `io::wasm`) for the actual file I/O, since blocking `rfd`
dialogs and browser Promise-based APIs are different enough mechanically to not share one
`#[cfg]`-sprinkled function; both stay `OsmilogApp`-agnostic (they take/return `CircuitFile`, not
app state).

## In-Progress / Not Yet Implemented

- **Undo/redo tombstone GC**: undo/redo itself is **implemented and wired** (see the "History / GUI
  undo" section under GUI for the full mechanism) - `OsmilogApp::undo`/`redo` consume the two-stack
  `History` via a symmetric `apply_entry`, exposed as an Edit menu (Undo / Redo, the latter
  `add_enabled`-gated on `can_redo`) and `Ctrl/Cmd+Z` / `Ctrl/Cmd+Y` / `Ctrl/Cmd+Shift+Z`. What
  remains here is only the **tombstone garbage collection**: deletes and wiring edits tombstone
  (`active: bool`) rather than remove, so tombstones accumulate with cumulative edit count. The GC
  primitives (`Wiring::remove_unreferenced_tombstones`, `Circuit::remove_unreferenced_tombstones`,
  fed by `History::referenced_wire_keys`) exist and are tested but are **still not called anywhere** -
  nothing prunes tombstones the live history no longer references. Two deliberate scope limits, not
  gaps: **clock ticks are excluded from undo** (`Command::TickClock` is issued untracked, bypassing
  `apply`, since its `RestoreSeqState` replay is unimplemented), and undo/redo re-derives nets via
  `rebuild_circuit` rather than reversing net mutations.
- **Canvas pan/zoom**: `OsmilogApp::pan` exists and is read everywhere geometry is drawn, but
  nothing currently mutates it - there's no pan gesture, and no zoom at all.
- **Whole-wire-run selection**: selecting/deleting a wire is still per-segment. `Wiring::groups()`
  already computes the connected sets a "select the whole net" gesture would need.
- **Pin-index bounds checking**: `Component::net_of`/`Circuit::net_of`/`link` don't bounds-check
  pin indices, so an out-of-range pin (including from a hand-edited save file, which
  `CircuitFile::validate()` doesn't check against a component's actual arity) can panic
  downstream.
- **`set_input` error handling**: silently no-ops on a non-`Input` component instead of returning
  a `Result` (marked with a `TODO` in `circuit.rs`).
- **More component types**: decoders, memories, additional sequential elements beyond `Reg`.
- **Subcircuits / hierarchical components**: not started.

Multi-select (rectangle select + bulk delete, `InteractionMode::BulkSelect`/`bulk_selection`) is
already implemented, despite appearing as a future item in older notes - don't assume anything
described as "not yet done" elsewhere is still accurate without checking the code.
