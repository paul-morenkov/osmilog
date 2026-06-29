# osmilog

A digital logic circuit simulator in Rust. Circuits are built programmatically by adding
components and linking their pins with nets. The simulator propagates signal changes through
the graph until stable (settle). Planned future work: more component types and an egui
graphical editor targeting both desktop and WASM.

Dependencies: slotmap 1.1.1 (stable generational arena keys), egui 0.35.0 (not yet wired in).


## Module Map

    src/value.rs      Value enum - signal representation
    src/net.rs        Net struct - a wire connecting component pins
    src/component.rs  Component, Logic, Pins, PinId, key types
    src/circuit.rs    Circuit - the simulation graph and evaluation engine
    src/main.rs       entry point and integration tests


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
- egui graphical editor for building circuits visually, targeting desktop and WASM
- Project stays as a single binary crate; egui rendering will be added directly
