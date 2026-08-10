# Simulator (`src/sim/`)

The simulation core. Depends on nothing else in the crate (see the layering rule in
[architecture.md](architecture.md)). If you are changing how signals propagate, how circuits
mutate/undo, or adding a component kind, this is the file — and to add a new component kind,
use the `add-component-type` skill.

## Value (`value.rs`)

    pub enum Value { Floating, Fixed { bits: u32, width: u8 }, Invalid }

The signal representation everywhere in the simulator. `Floating` is "unconnected/undefined" and
is absorbing through every operator. `Invalid` means the *wiring itself* is structurally wrong
(a short, or a width mismatch) - it's never produced by component logic, only by `Circuit`, and
it never propagates past the one net where it's flagged.

## Net (`net.rs`) and Circuit (`circuit.rs`)

A `Net` (keyed by `NetKey`) connects one or more component pins: `sources: Vec<(CompKey,
OutIdx)>`, `sinks: Vec<(CompKey, InIdx)>`. `Circuit` owns all `Net`s, `Component`s, and `Tunnel`s
(a "net label" mechanism - components sharing a tunnel label are wired together without a drawn
connection): `Net`s in a `SlotMap<NetKey, _>` (nets are derived state, cleared and rebuilt wholesale
every edit and never delete-then-undone, so a generational key suits them), `Component`s and
`Tunnel`s in `HashMap`s keyed by stable app-assigned `u64` ids (`CompKey`/`TunnelKey`, from `next_comp`/
`next_tunnel` counters) so undo can re-insert a removed one under its original key. Plus a dirty-net
queue that drives propagation.

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
them itself (see the Simulator/GUI separation in [architecture.md](architecture.md)).

## Command layer (`command.rs`)

    pub enum Command { AddComponent(Box<Component>), Link { .. }, RemoveComponent(CompKey), .. }
    fn Circuit::apply(&mut self, command: Command) -> (CommandOutput, UndoAction)

One `Command` variant per structural mutation `Circuit` supports. `apply` dispatches to the
matching `Circuit` method and returns both the output and the `UndoAction` that reverses that one
command (callers that don't want the undo take `.0`). This is the seam the GUI's undo/redo is built
on (see History in [gui.md](gui.md)); `Circuit::apply_undo(UndoAction) -> UndoAction` is what
*consumes* one - applying it reverses the recorded command and returns the inverse to record on
the opposite stack.

The `UndoAction`s are deliberately minimal, because **the circuit's net structure is derived
state**: the GUI rebuilds every net from its authoritative `Wiring`/component/tunnel records after
any edit (`Document::rebuild_circuit`), so undo restores those records and re-derives the nets
rather than reversing net mutations. Hence `ClearNets`/`Link`/`LinkTunnel`/`DetachTunnel` capture
`NoOp` (no net snapshots, no `NetKey`s), and only the *authoritative* commands capture a real
inverse. Component and tunnel removal **genuinely removes** the entity and *moves the owned
`Component`/`Tunnel` into the undo action*: `remove_component`/`remove_tunnel` `HashMap::remove` and
hand the payload back, and undo (`InsertComponent`/`InsertTunnel`) re-inserts it under its original
key. Keys stay stable across undo not because the entity lingers but because `CompKey`/`TunnelKey`
are **app-assigned `u64`s** (allocated from monotonic counters on `Circuit`, never reused), so a
re-insert reuses the exact key every history/wiring reference already holds - nothing needs
remapping. A removed `Reg`'s latched state rides along *inside* the moved `Component` (`remove_component`
nulls its pins first so it carries no dangling `NetKey`s, but the latch is kept apart from pins), so
an undone deletion restores it - which a spec-based re-creation could not. The entity ping-pongs
between the live map and exactly one (capped) history entry via a plain move, so no `Clone` is
needed and deleted memory is reclaimed immediately. The pairs are `RemoveComponent(CompKey)` ⇄
`InsertComponent(CompKey, Box<Component>)` and `RemoveTunnel(TunnelKey)` ⇄
`InsertTunnel(TunnelKey, Box<Tunnel>)`; the remaining real inverses are `SetInput`, `RenameTunnel`,
and `RestoreSeqState` (undo of `TickClock`).

## Component model (`component.rs` + `component/*.rs`)

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

The GUI reads/writes contents through the shared handle (`Document::write_rom_cell` just calls
`Circuit::write_rom` + `settle`; the spec updates for free). The properties panel matches the spec
*by reference* (never a per-frame clone), so it never deep-copies a ROM/RAM buffer just to read the
widths. The contents editor is a virtualized `egui::Window` (`gui::memory_editor::MemoryEditor`,
hex-dump rows via `ScrollArea::show_rows`) opened from the panel via `PropGuiAction::OpenMemory`, and
is the app's first free-floating window.

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
and *not* undoable - like clock ticks (see the Command layer above and [roadmap.md](roadmap.md)),
async state changes happen in `settle()`/derived-rebuild rather than through a recorded `Command`.
See each file under `src/sim/component/` for a given type's specific behavior.

## Subcircuits (`Logic::Sub` / `SubCircuit`, `component.rs`)

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
registry that `sim` doesn't have (see `gui::app::instantiate` in [gui.md](gui.md), the actual path
the GUI uses).

`DocId` (a `slotmap` key) is defined in `sim::component` rather than the GUI, so `ComponentSpec`
can embed it without `sim` depending on `gui` - it's re-exported from `gui::document`, where the
document registry (`SlotMap<DocId, CircuitDoc>`) actually lives. The simulator never dereferences
it.

## ComponentSpec (`component.rs`)

    pub enum ComponentSpec { Input(Input), Gate(Gate), Mux(Mux), Reg(RegConf), .. }
    fn ComponentSpec::to_component(&self) -> Component

The canonical "construction params" record - everything needed to build an equivalent
`Component`, without any live wiring or runtime state (a `Reg`'s latched value, a live
`Component`'s `NetKey`s, are never part of a `ComponentSpec`) - which is why a sequential variant
like `ComponentSpec::Reg` holds the bare `RegConf`, never the runtime `Reg`. It's the GUI's
`PlacedComponent` record (see the Simulator/GUI separation in [architecture.md](architecture.md)
for how the GUI attaches its own methods to this same type). There is no `Component -> ComponentSpec`
inverse: undo of a deletion moves the live `Component` itself into the history entry and re-inserts
it, rather than reconstructing it from a spec, so nothing needs to rebuild a spec from a live
`Component` (and a `Reg`'s latch survives the round trip).

`ComponentSpec::Subcircuit { doc: DocId, name: String, input_widths: Vec<u8>, output_widths:
Vec<u8> }` is the one variant that can't build its live `Component` from its own fields alone (see
Subcircuits above) - `doc` is the link, and `name`/the widths are a *cached derived interface*
(mirroring how `Rom` caches its bulk contents in the spec) so every `&self` spec method
(`n_inputs`/`size`/`shape`/...) works with no document registry in scope. `doc` is `#[serde(skip)]`
since a `DocId` is an ephemeral slotmap key with no cross-reload meaning - see
[save-load.md](save-load.md) for how the cross-circuit link actually persists. The cache is
refreshed against the referenced document by `gui::app::refresh_subcircuits` (called on every switch
back to a document, so edits made to a child circuit while it was active show up in its parent's
placed instances).
