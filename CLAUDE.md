# osmilog

A digital logic circuit simulator in Rust with an egui graphical editor. Circuits are built
either programmatically (constructing `Component`s and wiring them with `Circuit::link`) or
interactively in the GUI. The simulator propagates combinational signal changes through the
circuit graph until stable (`settle`), and advances sequential state on an explicit clock tick
(`tick_clock`). The app targets both desktop (native window, via `eframe`) and the browser
(WASM), and circuits save to / load from a plain JSON file (`.osm`).

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
    src/sim/component.rs              Component, Logic/CombLogic/SeqLogic, ComponentSpec, pin/key types
    src/sim/component/*.rs            one file per component kind (gate, mux, demux, splitter, reg, adder, ...)
    src/sim/circuit.rs                Circuit - the simulation graph, evaluation engine, tunnels
    src/sim/command.rs                Command/CommandOutput/UndoAction - the undo-recordable mutation layer

    src/gui.rs                       pub mod app / document / geometry / gui_undo / history / placed_component / shape / theme / wiring
    src/gui/app.rs                    OsmilogApp (eframe::App) - state, interaction modes, rendering, menu
    src/gui/document.rs               DocId/CircuitDoc/DocState - the multiple-open-circuits model
    src/gui/placed_component.rs       PlacedComponent - visual record; GUI-only display methods on ComponentSpec
    src/gui/wiring.rs                 Wiring - GUI's own connectivity graph (grid nodes + segments), + WiringDelta undo
    src/gui/gui_undo.rs               GuiUndoAction (Wiring delta / drag-move) + OsmilogApp::edit_wiring/commit_move
    src/gui/history.rs                History - accumulates HistoryEntrys (Sim + Gui) from apply()/edit_wiring()
    src/gui/shape.rs                  ComponentShape, PinAnchor, tessellate_path - visual shape primitives
    src/gui/geometry.rs               per-component-type shape builders + grid/pixel geometry constants
    src/gui/theme.rs                  Theme - canvas/signal colors derived from ambient egui Visuals

    src/io.rs                        ProjectFile/CircuitSnapshot save/load format (JSON), v2->v3 upgrade

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

- **`sim::component::ComponentSpec`** is a plain construction-params enum, consumed via
  `ComponentSpec::to_component()`; the GUI's `PlacedComponent` uses it as its "what to construct"
  record. The GUI reuses this *exact* enum, unmodified, as `PlacedComponent`'s
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
    fn write_rom(&mut self, comp: CompKey, index: usize, value: u32)
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

    pub enum Command { AddComponent(Box<Component>), Link { .. }, RemoveComponent(CompKey), .. }
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
    pub enum Logic { Comb(LogicComb), Seq(LogicSeq), Sub(SubCircuit) }

    pub trait CombLogic {
        fn n_inputs(&self) -> usize;
        fn n_outputs(&self) -> usize;
        fn evaluate(&self, inputs: &[Value]) -> Vec<Value>;
        fn input_width(&self, i: usize) -> Option<u8>;
        fn output_width(&self, i: usize) -> Option<u8>;
    }

Every combinational component type (`Gate`, `Mux`, `Demux`, `Splitter`/`Combine`, `Encoder`,
`Adder`, `Subtractor`, `Multiplier`, `Divider`, `Comparator`, `Rom`, `Constant` - one struct per
file under `component/`) implements `CombLogic`, bundling its construction params, pin arity, and
evaluation logic in one place so they can't drift apart. `Input` and `Output` are the two
sourceless/sinkless special cases.

`Rom` (read-only memory: one address input "A", one data output "D") is the one combinational type
carrying *bulk* state - its `Vec<u32>` contents (length `2^address_width`) are the construction
params, embedded whole in both `ComponentSpec::Rom(Rom)` and `LogicComb::Rom(Rom)` and thus persisted
by serializing the spec. It stays combinational because `evaluate()` is a pure read (address indexes
the table); the contents only change through an explicit `Circuit::write_rom`, which mutates the
contents *in place* (masked to `data_width`) and re-evaluates - deliberately bypassing the
Command/undo layer, exactly like `set_input`/clock ticks, so contents edits are not undoable.
Parameter (width) changes *do* go through the normal undoable `reconfigure_component` path, resizing
the table preserve-and-fit (`Rom::resized`: zero-extend/truncate on address_width, mask on
data_width).

**One shared copy, not two.** The contents can be tens of MiB, so the placed spec and the live
circuit component must not each hold their own `Vec`. `Rom::data` is therefore an
`Rc<RefCell<Vec<u32>>>`, and `ComponentSpec::to_component()` shares the handle (`Rom::shared`, an
`Rc` bump) instead of copying - the *one* deliberate place a spec and its live component alias state,
so a `write_rom` through the component is visible to the spec (what the editor reads and what's
saved) with no mirror write. This forces a hand-written `Clone` for `Rom` that **deep-copies** the
buffer (a fresh `Rc`): the codebase treats `ComponentSpec: Clone` as "independent copy" everywhere
that matters (paste, undo snapshots, save via `clipboard.rs`/`CircuitSnapshot`), and a shallow `Rc`-bump
clone would make a pasted ROM alias the original's contents. So: `shared()` = alias (exactly one
seam), `clone()` = independent (everywhere else). On disk the `Rc<RefCell<..>>` is transparent -
it serializes as a plain word array (needs serde's `rc` feature), so the save format is unchanged.
`Rc` (not `Arc`) suffices because app state is single-threaded; interior mutability means
`set_word`/`write_rom` need only `&self`.

The GUI reads/writes contents through the shared handle (`OsmilogApp::write_rom_cell` just calls
`Circuit::write_rom` + `settle`; the spec updates for free). The properties panel special-cases
`Rom` *before* its generic per-frame `spec.clone()` so it never deep-copies the buffer just to read
the widths. The contents editor is a virtualized `egui::Window` (`OsmilogApp::show_rom_editor`,
hex-dump rows via `ScrollArea::show_rows`) opened from the properties panel, and is the app's first
free-floating window.

    pub trait SeqLogic {
        fn n_inputs(&self) -> usize;
        fn n_outputs(&self) -> usize;
        fn tick(&mut self, inputs: &[Value]) -> Vec<Value>;
        fn apply_async(&mut self, inputs: &[Value]);
        fn observe(&self) -> Vec<Value>;
        fn snapshot(&self) -> SeqState;
        fn input_width(&self, i: usize) -> Option<u8>;
        fn output_width(&self, i: usize) -> Option<u8>;
    }

Sequential component types (`Logic::Seq`: `Reg`, `ShiftReg`, `DFlipFlop`, `TFlipFlop`,
`JKFlipFlop`, `SRFlipFlop`, `Counter`) implement `SeqLogic` instead, and each one splits in two: a
`*Conf` struct (`RegConf`) holding only static construction params, and a runtime struct (`Reg`)
that wraps a
`conf: RegConf` plus the mutable latched `Value`. This mirrors `CombLogic`'s "one struct, config +
logic together" idea while keeping the params embeddable in `ComponentSpec` (see below) without
runtime state riding along - `LogicSeq::Reg(Reg)` holds the runtime struct; `ComponentSpec::Reg`
holds only the bare `RegConf`. A sequential component has *two* ways its latched state can change,
and `observe()` is a pure read of that state (no inputs). `tick()` is the clocked update, driven only
by `tick_clock()`. `apply_async()` is the asynchronous, level-sensitive update: `settle()` runs it on
every evaluation of a sequential component (`eval_component`), so an input can mutate latched state
*without a clock tick* - e.g. an async reset that clears the value the instant its pin is held.
Because it runs inside the fixpoint loop, `apply_async` must be **idempotent** (re-applying it with
the same inputs is a no-op after the first), which is what keeps `settle()` convergent despite now
mutating sequential state; `settle()` re-evaluates sequential sinks like any other on an input change
for exactly this reason (the old "sequential components sit out of settle()" rule is gone). The
register and all four flip-flops carry an async reset pin (label "0", bottom-right): `apply_async`
destructively clears the latch while it's held (exactly `Value::ONE`), and `tick` treats the same pin
as dominant so a clock edge while it's asserted can't write anything else. The clear is destructive
and *not* undoable - like clock ticks (see the Command layer / In-Progress notes), async state
changes happen in `settle()`/derived-rebuild rather than through a recorded `Command`. See each file
under `src/sim/component/` for a given type's specific behavior.

### Subcircuits (`Logic::Sub` / `SubCircuit`, `component.rs`)

A whole other `Circuit` simulated as one component - a third `Logic` variant alongside `Comb` and
`Seq`, because it both propagates combinationally (its inner circuit needs `&mut` to `settle()`)
and holds clocked state (it forwards clock ticks inward):

    pub enum Logic { Comb(LogicComb), Seq(LogicSeq), Sub(SubCircuit) }
    pub struct SubCircuit { pub inner: Circuit, pub inputs: Vec<CompKey>, pub outputs: Vec<CompKey> }

`inputs`/`outputs` are the inner boundary `Input`/`Output` component keys, in the pin order this
component exposes outward (the GUI derives that order top-down from grid position). `SubCircuit`
reuses the same `apply_async`-then-`evaluate` shape `Seq` components use: `apply_async`
(`drive_and_settle`, `&mut`) drives the boundary `Input`s via `Circuit::drive_input` (injects a
`Value` onto an `Input`'s `out_cache`; idempotent - re-driving the same values marks nothing dirty)
and calls `inner.settle()`; `evaluate`/`observe` (`&self`) then just reads the boundary `Output`s,
already settled. `tick`/`reset` forward to `inner.tick_clock()`/`inner.reset_sequential()`.
`Component::is_stateful()` (`Seq(_) | Sub(_)`) is what the engine's whole-component sweeps
(`eval_component`'s `apply_async`, `tick_clock`, `reset_sequential`) key on rather than
`is_sequential()` (`Seq(_)` only), since a subcircuit needs the same per-settle/per-tick treatment
a sequential component does despite not being one.

`Component::subcircuit(inner, inputs, outputs)` builds a real one; `Component::
subcircuit_placeholder(n_inputs, n_outputs)` builds a correctly-pinned one with an empty inner
`Circuit` (settles to all-`Floating` outputs) - the safe fallback `ComponentSpec::to_component()`
uses on a `Subcircuit` spec, since building the *real* inner circuit needs the GUI's document
registry that `sim` doesn't have (see `gui::app::instantiate` below, the actual path the GUI uses).

`DocId` (a `slotmap` key) is defined in `sim::component` rather than the GUI, so `ComponentSpec`
can embed it without `sim` depending on `gui` - it's re-exported from `gui::document`, where the
document registry (`SlotMap<DocId, CircuitDoc>`) actually lives. The simulator never dereferences
it.

### ComponentSpec (`component.rs`)

    pub enum ComponentSpec { Input(Input), Gate(Gate), Mux(Mux), Reg(RegConf), .. }
    fn ComponentSpec::to_component(&self) -> Component

The canonical "construction params" record - everything needed to build an equivalent
`Component`, without any live wiring or runtime state (a `Reg`'s latched value, a live
`Component`'s `NetKey`s, are never part of a `ComponentSpec`) - which is why a sequential variant
like `ComponentSpec::Reg` holds the bare `RegConf`, never the runtime `Reg`. It's the GUI's
`PlacedComponent` record (see Simulator/GUI separation above for how the GUI attaches its own
methods to this same type). There is no `Component -> ComponentSpec` inverse: undo tombstones live
components rather than snapshotting them into specs, so nothing needs to reconstruct a spec from a
live `Component`.

`ComponentSpec::Subcircuit { doc: DocId, name: String, input_widths: Vec<u8>, output_widths:
Vec<u8> }` is the one variant that can't build its live `Component` from its own fields alone (see
Subcircuits above) - `doc` is the link, and `name`/the widths are a *cached derived interface*
(mirroring how `Rom` caches its bulk contents in the spec) so every `&self` spec method
(`n_inputs`/`size`/`shape`/...) works with no document registry in scope. `doc` is `#[serde(skip)]`
since a `DocId` is an ephemeral slotmap key with no cross-reload meaning - see Save/Load below for
how the cross-circuit link actually persists. The cache is refreshed against the referenced
document by `gui::app::refresh_subcircuits` (called on every switch back to a document, so edits
made to a child circuit while it was active show up in its parent's placed instances).

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
- `camera: Camera` - the canvas view transform (`geometry::Camera { pan, zoom }`). `pan` (screen-px
  offset) and `zoom` scale factor funnel through `Camera::grid_to_screen`/`screen_to_grid`/`scale`;
  every draw/hit function takes a `Camera`, not a bare `pan`. Middle-mouse drag pans, Ctrl+scroll
  (egui's `zoom_delta`, cursor-anchored, clamped `[ZOOM_MIN, ZOOM_MAX]`) zooms - both applied in
  `handle_camera_input` before drawing. Not persisted (like the old `pan`).
- `documents: SlotMap<DocId, CircuitDoc>`, `doc_order: Vec<DocId>`, `active: DocId` - every open
  circuit document and which one is live (see Documents / multiple circuits below). All of the
  fields above (`circuit`, `history`, `components`, `tunnels`, `wiring`, `mode`, `camera`, `selected`,
  `clock`, `rom_editor_open`) are *per-document* state; they hold the active document's state
  directly, and every other document parks the same fields in a `DocState`.

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

### Documents / multiple circuits (`document.rs`, `app.rs`)

`OsmilogApp` can hold several circuit documents in memory at once, so a subcircuit has something
to reference. Exactly one is *active*: its state lives directly in the live per-circuit fields
listed under OsmilogApp above; every other document parks the same set of fields in a `DocState`:

    pub struct DocState { circuit, history, components, tunnels, wiring, mode, camera, selected, clock, rom_editor_open }
    pub struct CircuitDoc { name: String, state: Option<DocState> }   // state is None iff active

Switching (`OsmilogApp::switch_circuit`) is a pair of `std::mem` moves - `take_active_state` empties
the live fields into a `DocState` parked on the outgoing document, `put_active_state` installs the
incoming document's parked `DocState` into the live fields - never serialization, so switching
never deep-copies a `ComponentSpec` or a ROM's contents. Because a parked circuit already holds its
settled nets, no net rebuild is needed on switch; what *is* needed is `refresh_subcircuits()` (see
below), since child circuits may have changed while this document was inactive. `doc_order: Vec<
DocId>` fixes the display order for the palette and the persisted circuit order (`SlotMap`
iteration order is unspecified); `create_circuit_doc` parks the current document, allocates a new
blank one via `DocState::blank()`, and appends it to `doc_order`. There is currently no UI to
rename or delete a circuit document (see In-Progress).

### Subcircuits (`app.rs`)

Placing a document as a component inside another. The GUI is the *only* place that can build a
subcircuit's real inner `Circuit` (see the Simulator's Subcircuits section above for why
`ComponentSpec::to_component()` alone can't - it has no document registry):

- `OsmilogApp::instantiate(spec)` is the one spec->component build path the GUI itself uses
  (`place_component`, `reconfigure_component`) - identical to `spec.to_component()` for every
  primitive type, but for `ComponentSpec::Subcircuit` it calls `build_doc_circuit` to build a real
  inner `Circuit` instead of a placeholder. `instantiate_with`'s `visited: &mut Vec<DocId>` breaks
  an accidental reference cycle during the recursive build (real cycles are already refused at
  placement time by `would_cycle`) by yielding an empty placeholder instead of recursing forever.
- `build_doc_circuit(doc, visited)` builds a fresh standalone `Circuit` from a referenced
  document's parked records (components/tunnels/wiring), translating them the same way
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

The top-level on-disk unit (v3) is `ProjectFile { version, active: usize, circuits:
Vec<CircuitEntry> }` - the *whole workspace*, every circuit document, not just the active one -
so subcircuits round-trip (see below). Each `CircuitEntry { name, #[serde(flatten)]
snapshot: CircuitSnapshot, subcircuits: Vec<SubcircuitRef> }` names one document and carries its
records as a `CircuitSnapshot { components: Vec<ComponentEntry>, tunnels: Vec<TunnelEntry>, nodes:
Vec<NodeEntry>, segments: Vec<SegEntry> }` - one document's GUI visual state (placed
components/tunnels + the `Wiring` graph), not `Circuit`'s internal `SlotMap`s - every
cross-reference a plain `usize` index into one of the snapshot's own vectors, since slotmap keys
are ephemeral and not worth persisting. `CircuitSnapshot` is the reusable payload the clipboard
(`gui::clipboard`) also holds. That indexing convention is exactly how cross-*circuit* links persist too: a
placed subcircuit's `ComponentSpec::Subcircuit::doc` (a runtime-only, serde-skipped `DocId`) is
emitted as a `SubcircuitRef { component, circuit }` (component index within the entry → circuit
index within the project) and re-bound to a freshly-allocated `DocId` on load. `active` records
which document was open.

`version` is bumped whenever the shape changes incompatibly; `ProjectFile::validate()` checks
version, `active`, and every index bound before a load replaces the current app state.
`ProjectFile::from_json` transparently upgrades a legacy **v2** single-circuit file (deserialized
via `io::LegacyV2File`, a versioned `CircuitSnapshot`) into a one-circuit project named "Main". The
App↔file conversion lives in `gui::app` (`to_project_file`/`load_project_file`, plus
`extract_records` / `install_circuit_records` shared with the single-doc `to_snapshot`/`load_snapshot`
helpers). Native and WASM get separate
submodules (`platform::native`, `platform::web`) for the actual file I/O, since blocking `rfd`
dialogs and browser Promise-based APIs are different enough mechanically to not share one
`#[cfg]`-sprinkled function; both stay `OsmilogApp`-agnostic (they take/return `ProjectFile`, not
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
- **Canvas pan/zoom**: implemented (`OsmilogApp::camera: geometry::Camera`, see OsmilogApp above) -
  middle-mouse drag pans, Ctrl+scroll zooms toward the cursor. No keyboard/scrollbar pan and no
  "reset view" affordance yet.
- **Whole-wire-run selection**: selecting/deleting a wire is still per-segment. `Wiring::groups()`
  already computes the connected sets a "select the whole net" gesture would need.
- **Pin-index bounds checking**: `Component::net_of`/`Circuit::net_of`/`link` don't bounds-check
  pin indices, so an out-of-range pin (including from a hand-edited save file, which
  `CircuitSnapshot::validate()` doesn't check against a component's actual arity) can panic
  downstream.
- **`set_input` error handling**: silently no-ops on a non-`Input` component instead of returning
  a `Result` (marked with a `TODO` in `circuit.rs`).
- **Circuit document management**: subcircuits (see the Documents / Subcircuits sections under GUI)
  are implemented end-to-end, including project-file persistence - but there's no UI yet to rename
  or delete a circuit document once created (`create_circuit_doc` is the only mutator). Undo/redo
  is also scoped per-document (`History` lives inside each `DocState`), so it does not span a
  circuit switch - undoing in a parent after editing and switching back out of a child only undoes
  the parent's own edits, never reaches into the child's history.
