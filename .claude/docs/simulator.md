# Simulator (`src/sim/`)

The simulation core. It depends on nothing else in the crate (see the layering rule in
[architecture.md](architecture.md)). To add a new component kind, use the
`add-component-type` skill.

## Value (`value.rs`)

    pub enum Value { Floating, Fixed { bits: u32, width: u8 }, Invalid }

`Floating` means unconnected or undefined; it is absorbing through every operator.
`Invalid` means the wiring is structurally wrong: a short, or a width mismatch. Only
`Circuit` produces `Invalid`. It never propagates past the net where it is flagged.

## Net and Circuit (`net.rs`, `circuit.rs`)

A `Net`, keyed by `NetKey`, connects component pins: `sources: Vec<(CompKey, OutIdx)>`,
`sinks: Vec<(CompKey, InIdx)>`.

`Circuit` owns all `Net`s, `Component`s, and `Tunnel`s. A `Tunnel` is a net-label
mechanism: components sharing a label are wired together with no drawn connection.
`Net`s live in a `SlotMap<NetKey, _>`. `Component`s and `Tunnel`s live in `HashMap`s
keyed by stable, app-assigned `u64` ids (`CompKey`/`TunnelKey`), so undo can re-insert a
removed entry under its original key. A dirty-net queue drives propagation.

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

`settle()` drains the dirty queue, recomputing net values and re-evaluating
combinational sinks until nothing changes. It returns `SettleError::Oscillation` or
`SettleError::TunnelConflict` instead of looping or panicking. `tick_clock()` snapshots
every sequential component's inputs, advances all of them one step, then calls
`settle()`.

`sim::command::Command` dispatches to these methods. They stay `pub`, but the GUI never
calls them directly — see [architecture.md](architecture.md).

## Command layer (`command.rs`)

    pub enum Command { AddComponent(Box<Component>), Link { .. }, RemoveComponent(CompKey), .. }
    fn Circuit::apply(&mut self, command: Command) -> (CommandOutput, UndoAction)

One `Command` variant exists per structural mutation. `apply` dispatches to the matching
`Circuit` method and returns the output plus an `UndoAction`. `Circuit::apply_undo(UndoAction)
-> UndoAction` reverses the recorded command and returns the inverse, for the opposite
undo/redo stack (see History in [gui.md](gui.md)).

Net structure is derived state: the GUI rebuilds every net from `Wiring` after any edit
(`Document::rebuild_circuit`). Undo restores records and re-derives nets, rather than
reversing net mutations. `ClearNets`/`Link`/`LinkTunnel`/`DetachTunnel` capture `NoOp`.

Component and tunnel removal takes the entity out of its `HashMap` and moves it into the
undo action. Undo (`InsertComponent`/`InsertTunnel`) re-inserts it under its original
key — `CompKey`/`TunnelKey` are app-assigned `u64`s, never reused, so re-insertion needs
no remapping. `remove_component` nulls the removed `Component`'s pins first. A `Reg`'s
latched state stays intact, so an undone deletion restores it. The pairs are
`RemoveComponent(CompKey)` ⇄ `InsertComponent(CompKey, Box<Component>)` and
`RemoveTunnel(TunnelKey)` ⇄ `InsertTunnel(TunnelKey, Box<Tunnel>)`. The remaining real
inverses are `SetInput`, `RenameTunnel`, and `RestoreSeqState` (undo of `TickClock`).

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

Every combinational type implements `CombLogic`, one struct per file under
`component/`: `Gate`, `Mux`, `Demux`, `Splitter`/`Combine`, `Encoder`, `Adder`,
`Subtractor`, `Multiplier`, `Divider`, `Comparator`, `Rom`, `Constant`. `Input` and
`Output` are the sourceless/sinkless special cases.

`Rom` (address input "A", data output "D") holds a `Vec<u32>` of length
`2^address_width`, embedded in `ComponentSpec::Rom(Rom)` and persisted through the spec.
`evaluate()` is a pure read. Contents change only through `Circuit::write_rom`, which
mutates the buffer in place (masked to `data_width`) and re-evaluates; this bypasses the
Command/undo layer, so contents edits are not undoable, matching `set_input` and clock
ticks. Width changes go through `reconfigure_component` and are undoable; `Rom::resized`
zero-extends/truncates on `address_width` and masks on `data_width`.

`Rom::data` is `Rc<RefCell<Vec<u32>>>`, shared (`Rom::shared`) between the placed spec
and the live component, so a `write_rom` is visible to the spec with no mirror write.
`Clone` on `Rom` is hand-written and deep-copies the buffer under a fresh `Rc` — paste,
undo snapshots, and save all need an independent copy. On disk the buffer serializes as
a plain word array (serde's `rc` feature).

The GUI reads and writes contents through the shared handle
(`Document::write_rom_cell`: `Circuit::write_rom` then `settle`). The properties panel
matches the spec by reference, never cloning the buffer. The contents editor is a
virtualized `egui::Window` (`gui::memory_editor::MemoryEditor`), opened via
`PropGuiAction::OpenMemory`.

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

Sequential types implement `SeqLogic`: `Reg`, `ShiftReg`, `DFlipFlop`, `TFlipFlop`,
`JKFlipFlop`, `SRFlipFlop`, `Counter`. Each splits into a `*Conf` struct (`RegConf`,
static params only) and a runtime struct (`Reg`, wrapping `conf: RegConf` plus the
latched `Value`). `LogicSeq::Reg(Reg)` holds the runtime struct; `ComponentSpec::Reg`
holds only `RegConf`.

`observe()` reads current state with no inputs. `tick()` is the clocked update, driven
by `tick_clock()`. `apply_async()` is the asynchronous, level-sensitive update:
`settle()` runs it on every evaluation of a sequential component, so an input can mutate
latched state without a clock tick (for example, an async reset). `apply_async` must be
idempotent — re-applying the same inputs is a no-op after the first call — which keeps
`settle()` convergent.

The register and all four flip-flops carry an async reset pin (label "0",
bottom-right). `apply_async` clears the latch while the pin holds `Value::ONE`. `tick`
treats the same pin as dominant, so a clock edge during an asserted reset writes
nothing else. The clear is destructive and not undoable, like clock ticks (see
[roadmap.md](roadmap.md)). See each file under `src/sim/component/` for a type's
specific behavior.

## Subcircuits (`Logic::Sub` / `SubCircuit`, `component.rs`)

A subcircuit is a whole other `Circuit`, simulated as one component:

    pub enum Logic { Comb(LogicComb), Seq(LogicSeq), Sub(SubCircuit) }
    pub struct SubCircuit { pub inner: Circuit, pub inputs: Vec<CompKey>, pub outputs: Vec<CompKey> }

`Logic::Sub` is a third `Logic` variant: a subcircuit both propagates combinationally
and holds clocked state. `inputs`/`outputs` are the inner boundary `Input`/`Output`
keys, in the pin order the subcircuit exposes outward — derived top-down from grid
position in the GUI.

`SubCircuit` follows the same `apply_async`-then-`evaluate` shape as `Seq` components.
`apply_async` (`drive_and_settle`) drives the boundary `Input`s via
`Circuit::drive_input` (idempotent) and calls `inner.settle()`. `evaluate`/`observe`
read the settled boundary `Output`s. `tick`/`reset` forward to
`inner.tick_clock()`/`inner.reset_sequential()`. `Component::is_stateful()`
(`Seq(_) | Sub(_)`) is what whole-component sweeps key on, not `is_sequential()`
(`Seq(_)` only), since a subcircuit needs the same per-settle/per-tick treatment.

`Component::subcircuit(inner, inputs, outputs)` builds a real subcircuit.
`Component::subcircuit_placeholder(n_inputs, n_outputs)` builds a correctly-pinned one
with an empty inner `Circuit` (settles to all-`Floating` outputs) — the fallback
`ComponentSpec::to_component()` uses, since building the real inner circuit needs the
GUI's document registry. See `gui::app::instantiate` in [gui.md](gui.md) for the path
the GUI actually uses.

`DocId`, a `slotmap` key, is defined in `sim::component` so `ComponentSpec` can embed it
without `sim` depending on `gui`. It is re-exported from `gui::document`, where the
document registry (`SlotMap<DocId, CircuitDoc>`) lives. The simulator never dereferences
it.

## ComponentSpec (`component.rs`)

    pub enum ComponentSpec { Input(Input), Gate(Gate), Mux(Mux), Reg(RegConf), .. }
    fn ComponentSpec::to_component(&self) -> Component

`ComponentSpec` is the construction-params record: everything needed to build an
equivalent `Component`, with no live wiring or runtime state. It is also the GUI's
`PlacedComponent` record — see [architecture.md](architecture.md) for the GUI's
attached methods on this type.

There is no `Component -> ComponentSpec` inverse. Undo of a deletion moves the live
`Component` into the history entry and re-inserts it, so a `Reg`'s latch survives the
round trip.

`ComponentSpec::Subcircuit { doc: DocId, name: String, input_widths: Vec<u8>,
output_widths: Vec<u8> }` cannot build its `Component` from its own fields alone. `doc`
links to the referenced document; `name` and the widths are a cached derived interface,
so every `&self` spec method works with no document registry in scope. `doc` is
`#[serde(skip)]` — see [save-load.md](save-load.md) for how the cross-circuit link
persists on disk. The cache refreshes against the referenced document via
`gui::app::refresh_subcircuits`, called on every switch back to a document.
