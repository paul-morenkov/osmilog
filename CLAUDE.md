# osmilog

A digital logic circuit simulator in Rust. Circuits are built programmatically by adding
components and linking their pins with nets. The simulator propagates signal changes through
the graph until stable (settle). Planned future work: more component types and an egui
graphical editor targeting both desktop and WASM.

Dependencies: slotmap 1.1.1 (stable generational arena keys), eframe/egui 0.35.0 (native GUI).


## Module Map

    src/value.rs      Value enum - signal representation
    src/net.rs        Net struct - a wire connecting component pins
    src/component.rs  Component, Logic, Pins, PinId, key types
    src/circuit.rs    Circuit - the simulation graph and evaluation engine
    src/app.rs        OsmilogApp - eframe/egui GUI (ComponentDef, PlacedComponent, Wire, InteractionMode)
    src/main.rs       entry point (eframe::run_native) and integration tests


## Core Types

### Value (value.rs)

    pub enum Value {
        Floating,                       // unconnected or undefined
        Fixed { bits: u32, width: u8 } // concrete signal of given bit width
    }

Floating is the default. Binary ops (AND, OR, XOR, NOT, Add, Sub) return Floating when
operands have mismatched widths. NOT masks the result to width bits.

    Value::new(bits, width)  -- construct a Fixed value
    Value::mask(width)       -- bitmask of `width` ones (u32)

### Net (net.rs)

    pub struct Net {
        pub value: Value,
        pub source: Option<(CompKey, OutIdx)>,  // at most one driver
        pub sinks:  Vec<(CompKey, InIdx)>,      // zero or more receivers
    }

Nets are identified by NetKey (slotmap generational key). Multiple sources on the same net
are not yet supported (TODO in merge()).

### Component (component.rs)

    pub struct Component {
        pub pins:  Pins,
        pub logic: Logic,
    }

    pub struct Pins {
        pub inputs:    Vec<Option<NetKey>>,
        pub outputs:   Vec<Option<NetKey>>,
        pub out_cache: Vec<Value>,   // last computed output, parallel to outputs
    }

CompKey is the slotmap key for a Component. out_cache is written by eval_component and read
by resolve_net; it decouples evaluation from net updates.

### Circuit (circuit.rs)

    pub struct Circuit {
        pub(crate) nets:       SlotMap<NetKey, Net>,
        components:            SlotMap<CompKey, Component>,
        pub(crate) dirty:      VecDeque<NetKey>,
        queued:                SecondaryMap<NetKey, bool>,
    }

The dirty queue drives propagation. queued prevents duplicate entries.

### PinId (component.rs)

    pub enum PinId { In(InIdx), Out(OutIdx) }

    PinId::input(i)   -- shorthand for PinId::In(InIdx(i))
    PinId::output(i)  -- shorthand for PinId::Out(OutIdx(i))

Pin indices are 0-based u8 values.


## Component Types and Pin Conventions

All constructors are on Component. Logic enum variants are listed with their pin layouts.

### Input

    Component::input(value: Value)
    outputs: [0] driven value

Source node. Updated at runtime via circuit.set_input(key, value).

### Output

    Component::output()
    inputs: [0] observed value

Sink node. Read via circuit.read_output(key), which returns the value of its input net.

### Gate

    Component::gate(op: GateOp, n_inputs: usize, width: u8)
    inputs:  [0..n] operands
    outputs: [0]    result

GateOp variants: And, Or, Xor, Xnor, Nand, Nor, Not. All inputs and the output share the
same bit width. NOT ignores n_inputs and only reads input[0].

And/Nand accumulate from all-ones identity; Or/Nor/Xor/Xnor accumulate from zero identity.

### Mux

    Component::mux(data_width: u8, sel_width: u8)
    inputs:  [0]         selector (sel_width bits)
             [1..2^sel]  data branches (data_width bits each)
    outputs: [0]         selected branch

NOTE: pin ordering is provisional and may change.

sel_width controls how many data inputs exist: 2^sel_width branches. Selector value is used
directly as the index into the data inputs (input[sel + 1]).

### Demux

    Component::demux(data_width: u8, sel_width: u8)
    inputs:  [0]  data (data_width bits)
             [1]  selector (sel_width bits)
    outputs: [0..2^sel]  routed outputs (data_width bits each)

NOTE: pin ordering is provisional and may change.

Selected output carries the data value; all other outputs are zero.

### Reg (not yet implemented)

    Logic::Reg
    evaluate() calls todo!()

Register semantics (clocking model) are not yet decided.


## Evaluation Model

### settle()

Call settle() after any structural change (link) or input change (set_input). It drains the
dirty queue in a BFS loop, capped at MAX_ITERATIONS = 100 (panics if exceeded).

For each dirty net:
1. resolve_net(net) -- copies out_cache[i] from the source component into net.value;
   returns true if the value changed.
2. If changed, find all combinational sink components (is_sequential() == false) and call
   eval_component on each.

### eval_component()

Calls component.evaluate(&nets) to compute new output values from current input net values,
then writes results into out_cache. If any cache slot changes and the output pin is
connected, its net is marked dirty.

### Combinational vs Sequential

is_sequential() returns true only for Logic::Reg. Sequential components are skipped during
settle propagation; they must be advanced separately (mechanism not yet designed).

### add_component / link / set_input

These call eval_component or mark_dirty automatically, so partial evaluation happens
incrementally. settle() is still needed to fully propagate.

clear_nets() disconnects all pins and removes all nets while keeping components in place.


## GUI Architecture

The GUI lives in `src/app.rs` and is driven by `eframe::run_native` in `main.rs`
(1200 × 800 initial viewport). `OsmilogApp` implements `eframe::App` via two methods:
`logic` (pre-frame logic) and `ui` (painting).

### OsmilogApp state

    circuit: Circuit          simulation graph — source of truth for signal values
    components: Vec<PlacedComponent>  visual records: CompKey + ComponentDef + grid_pos + label
    wires: Vec<Wire>          visual records: NetKey + src/dst CompKey + pin indices
    mode: InteractionMode     Idle | Placing { def } | WireDrag { src_comp, src_pin, current_end }
    pan: Vec2                 canvas pan offset in pixels

`PlacedComponent` mirrors the circuit's `Component` but carries display-only data.
`ComponentDef` re-expresses the `Logic` variants with all parameters needed for both
display (`label`, `n_inputs`, `n_outputs`) and construction (`make_component()`).

### Rendering pipeline (each frame)

1. Menu bar — "Add" menu populates `mode = Placing { def }` for the chosen component type.
2. `allocate_painter` fills the remaining area; `Sense::click_and_drag` captures all input.
3. `draw_grid` — dot grid at GRID_SIZE (20 px) intervals, offset by `pan`.
4. Wires — L-shaped elbow at the horizontal midpoint; color from `value_color(net.value)`.
5. Components — rounded rectangles with a label; input pins on the left edge, output pins
   on the right edge, each a filled circle colored by the current signal value.
6. Mode overlay — ghost component while placing; rubber-band line while dragging a wire.

### Interaction modes

- **Idle**: drag from an output pin → enters WireDrag; click on an Input component body →
  toggles its 1-bit value and calls `circuit.set_input` + `circuit.settle`.
- **Placing**: shows a ghost at the snapped grid cell; click places the component via
  `place_component`, which calls `circuit.add_component` and appends to `self.components`.
- **WireDrag**: tracks `current_end` while dragging; on `drag_stopped`, if the cursor is
  within 2 × PIN_RADIUS of an input pin on a different component, calls `circuit.link` +
  `circuit.settle` and appends to `self.wires`. Escape returns to Idle.

### Geometry constants

    GRID_SIZE            20 px — canvas grid spacing
    COMP_WIDTH           80 px — fixed component box width
    COMP_HEIGHT_PER_PIN  20 px — height contributed by each pin slot
    COMP_MIN_HEIGHT      40 px — floor on component box height
    PIN_RADIUS            5 px — drawn radius; hit radius is 2 ×

### Signal color coding

    Floating                 gray
    Fixed { bits: 0, .. }   dark blue (logic low)
    Fixed { .. }            green (logic high / non-zero bus)

### Window close (macOS)

`logic()` checks `ctx.input(|i| i.viewport().close_requested())` and calls
`std::process::exit(0)`. This bypasses eframe's double `save_and_destroy` cleanup sequence
(a bug in eframe 0.35 on macOS that panics on the second GPU painter destroy) which would
otherwise trigger Apple's crash reporter.


## Known Limitations / TODOs

- Logic::Reg is a placeholder; clocking semantics not decided
- Mux/Demux pin ordering is provisional and may change
- No conflict detection when multiple sources end up on the same net (merge() has a TODO)
- settle() panics after 100 iterations; no graceful error return
- set_input and read_output do not return errors for wrong component type
- width is not verified to be nonzero in Value::Fixed
- Add/Sub may overflow; behavior is unspecified


## Future Directions

- More component types: sequential elements, arithmetic units, decoders, etc.
- Extend the GUI: delete components/wires, multi-bit input editing, zoom, save/load
- WASM target: compile egui app to wasm32 for browser use
- Project stays as a single binary crate
