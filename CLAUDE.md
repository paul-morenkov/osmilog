# osmilog

A digital logic circuit simulator in Rust with an egui graphical editor. Circuits are built
either programmatically (adding components and linking their pins with nets) or interactively
in the GUI. The simulator propagates combinational signal changes through the graph until
stable (settle) and advances sequential state on an explicit clock tick. The app targets both
desktop (native window) and the browser (WASM), and circuits can be saved to / loaded from a
plain JSON file.

The crate is both a library (`src/lib.rs`, modules `gui`/`io`/`sim`) and a thin binary
(`src/main.rs`); tests live in `#[cfg(test)]` modules inside `circuit.rs` and `app.rs`.

Dependencies: slotmap 1.1.1 (stable generational arena keys), eframe/egui 0.35.0 (GUI),
serde + serde_json (save/load), rfd 0.15 (native + async file dialogs). WASM adds
wasm-bindgen / wasm-bindgen-futures / js-sys / web-sys.


## Module Map

    src/lib.rs            crate root: pub mod gui / io / sim
    src/sim/value.rs      Value enum - signal representation
    src/sim/net.rs        Net struct - a wire connecting component pins
    src/sim/component.rs  Component, Logic/LogicComb/LogicSeq, GateOp, Splitter, FanDirection, Pins, PinId, key types
    src/sim/circuit.rs    Circuit - the simulation graph and evaluation engine; Tunnel, TunnelRole, SettleError
    src/io.rs             CircuitFile JSON save/load format + native/wasm file-dialog submodules
    src/gui/shape.rs      ComponentShape, ShapeCmd, PinAnchor, ComponentLabel, tessellate_path - visual shape system
    src/gui/geometry.rs   ComponentDef shape builders (gate_shape, mux_shape, splitter_shape, ...) and geometry constants (GRID_SIZE, COMP_WIDTH, ...)
    src/gui/theme.rs      Theme - canvas colors derived from the ambient egui Visuals (light/dark responsive)
    src/gui/placed_component.rs  PlacedComponent, ComponentDef - visual/construction record for a placed component
    src/gui/app.rs        OsmilogApp - eframe/egui GUI (PlacedTunnel, Wire, TunnelWire, Selected, DragSource, InteractionMode)
    src/main.rs           entry point: native eframe::run_native, plus a wasm_bindgen(start) WASM entry


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

Nets are identified by NetKey (slotmap generational key). Multiple component sources ending up
on the same net is still an unresolved case (merge() has a documented bug — see the
`test_link_merge_keeps_original_source_documents_bug` test). Conflicting *tunnel* drivers, by
contrast, are detected: settle() returns `SettleError::TunnelConflict`.

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

### Logic (component.rs)

`Logic` splits combinational and sequential behavior at the type level:

    pub enum Logic { Comb(LogicComb), Seq(LogicSeq) }

    pub enum LogicComb { Input { bits, width }, Output, Gate { op, width },
                         Mux { .. }, Demux { .. }, Splitter(Splitter) }

    pub enum LogicSeq  { Reg { value: Value, data_width: u8 } }

`is_sequential()` is `matches!(logic, Logic::Seq(_))`. Combinational components compute via
`evaluate(&nets)`; sequential components hold latched state and advance only via `tick(&inputs)`
(see Evaluation Model).

### Circuit (circuit.rs)

    pub struct Circuit {
        pub(crate) nets:       SlotMap<NetKey, Net>,
        pub(crate) components: SlotMap<CompKey, Component>,
        pub(crate) dirty:      VecDeque<NetKey>,
        queued:                SecondaryMap<NetKey, bool>,
        pub(crate) tunnels:    SlotMap<TunnelKey, Tunnel>,
        tunnel_labels:         HashMap<String, Vec<TunnelKey>>,
    }

The dirty queue drives propagation; queued prevents duplicate entries. Tunnels are a second
connectivity mechanism layered on top of nets (see Tunnels below): `tunnels` holds each
`Tunnel { label, role, net }`, and `tunnel_labels` indexes them by shared label so a label
group can be resolved and re-dirtied together.

### Tunnels (circuit.rs)

A Tunnel is a named "net label" / off-page connector: all tunnels sharing a `label` form one
virtual net without a drawn wire between them.

    pub enum TunnelRole { Feed, Pull }
    pub struct Tunnel { pub label: String, pub role: TunnelRole, pub net: Option<NetKey> }

`Pull` tunnels read their attached net's value and contribute it to the shared label group;
`Feed` tunnels drive their attached net from the group's resolved value. Conflicting Pull
values within a group surface as `SettleError::TunnelConflict`. Managed via `add_tunnel`,
`link_tunnel`, `detach_tunnel`, `remove_tunnel`, and `rename_tunnel`.

### PinId (component.rs)

    pub enum PinId { In(InIdx), Out(OutIdx) }

    PinId::input(i)   -- shorthand for PinId::In(InIdx(i))
    PinId::output(i)  -- shorthand for PinId::Out(OutIdx(i))

Pin indices are 0-based u8 values.


## Component Types and Pin Conventions

All constructors are on Component. Logic enum variants are listed with their pin layouts.

### Input

    Component::input(bits: u32, width: u8)
    outputs: [0] driven value

Source node. Updated at runtime via circuit.set_input(key, bits, width).

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

### Splitter / Combine

    Component::splitter(arm_bits: Vec<Vec<u8>>, direction: FanDirection)

A bit re-router (no computation). `arm_bits[j]` lists the data-bit indices routed to arm `j`,
e.g. `[[0, 2], [1, 3]]` sends bits 0,2 to arm 0 and bits 1,3 to arm 1. The trunk bus width is
derived as `max(bit) + 1`; a bit claimed by multiple arms is won by the later arm.
`direction` picks which side is the input:

- `FanDirection::Right` (Splitter): inputs [0] = trunk bus; outputs [0..arms] = arms.
- `FanDirection::Left`  (Combine):  inputs [0..arms] = arms; outputs [0] = trunk bus.

In Combine mode every arm that owns ≥1 bit must be driven at exactly its owned width or the
whole merged output is Floating (mirroring Value's width-mismatch convention). The
`Splitter { arms, direction, routing, arm_width }` struct precomputes a per-data-bit
`routing: Vec<Option<(arm, slot)>>` once at construction so `evaluate()` doesn't recount.

### Reg

    Component::reg(data_width: u8)
    inputs:  [0] data (data_width bits)
             [1] write_enable (1 bit)
    outputs: [0] latched value (data_width bits)

The one sequential component (`Logic::Seq(LogicSeq::Reg { value, data_width })`). `evaluate()`
just reports the currently latched `value`; state changes only on `tick()`: when
write_enable is `Fixed { bits: 1, width: 1 }`, `value` is replaced with the data input.
Advanced through the circuit via `tick_clock()`.


## Evaluation Model

### settle()

`settle() -> Result<(), SettleError>`. Call it after any structural change (link) or input
change (set_input). It drains the dirty queue in a BFS loop.

For each dirty net:
1. resolve_net(net) -- copies out_cache[i] from the source component into net.value (also
   folding in tunnel Feed/Pull contributions for that net's label group); returns true if the
   value changed.
2. If changed, find all combinational sink components (is_sequential() == false) and call
   eval_component on each.

Instead of a single fixed iteration cap, non-convergence is detected two ways and returned as
`SettleError::Oscillation` rather than panicking: a per-net `REVISIT_THRESHOLD` (16 value
changes on one net) and a whole-call `ITERATION_BUDGET_PER_NET` (64) backstop scaled to
circuit size. Tunnel label-group conflicts return `SettleError::TunnelConflict`.

### eval_component()

Calls component.evaluate(&nets) to compute new output values from current input net values,
then writes results into out_cache. If any cache slot changes and the output pin is
connected, its net is marked dirty.

### Combinational vs Sequential — tick_clock()

is_sequential() returns true only for `Logic::Seq` (currently just Reg). Sequential components
are skipped during settle propagation; they advance via `tick_clock() -> Result<(),
SettleError>`, which uses snapshot semantics: it first reads every sequential component's input
values (`read_inputs`), then applies `tick()` to all of them against that snapshot (so chained
registers shift by one stage per tick rather than racing), writes the new outputs, and finally
`settle()`s the resulting combinational fan-out.

### add_component / link / set_input / remove_component

These call eval_component or mark_dirty automatically, so partial evaluation happens
incrementally. settle() is still needed to fully propagate.

`remove_component(key)` deletes a component: it removes the nets it drives (nulling each sink's
input pin), detaches it from nets it receives, detaches any tunnels left dangling, resets
propagation state, and re-evaluates the sinks that lost their driver (so their now-Floating
inputs propagate on the caller's following settle() rather than leaving stale downstream
values). `remove_tunnel(key)` likewise dirties the tunnel's net and its label group's feed
nets. `clear_nets()` disconnects all pins and removes all nets (and detaches all tunnels) while
keeping components in place.


## GUI Architecture

The GUI lives in `src/gui/app.rs` and is driven by `eframe::run_native` in `main.rs`
(1200 × 800 initial viewport) on native, and by `eframe::WebRunner` mounted on a
`#the_canvas_id` element via a `#[wasm_bindgen(start)]` entry on WASM. `OsmilogApp` implements
`eframe::App` via two methods: `logic` (pre-frame logic) and `ui` (painting). Canvas colors
come from `Theme::from_visuals(ui.visuals())`, recomputed each frame so the app tracks the
ambient egui (and OS) light/dark theme live.

### OsmilogApp state

    circuit: Circuit                                       simulation graph — source of truth for signal values
    components: SlotMap<PlacedCompKey, PlacedComponent>     visual records: CompKey + ComponentDef + grid_pos
    tunnels: SlotMap<PlacedTunnelKey, PlacedTunnel>          visual records: TunnelKey + label + role + grid_pos
    wires: Vec<Wire>                                        visual records: src/dst CompKey + pin indices
    tunnel_wires: Vec<TunnelWire>                           visual records: TunnelKey + CompKey + PinId
    mode: InteractionMode                                   Idle | Placing | PlacingTunnel | WireDrag | ComponentDrag
    pan: Vec2                                               canvas pan offset in pixels
    selected: Option<Selected>                              currently selected component or tunnel
    last_settle_error: Option<String>                       transient status: last settle()/tick_clock() error OR Save/Load I/O error (shown red in the menu bar)
    pending_load: PendingLoad (wasm only)                    slot where an async browser file-load delivers its result, polled each frame

`PlacedComponent` mirrors the circuit's `Component` but carries display-only data.
`ComponentDef` re-expresses the `Logic` variants with all parameters needed for both
display (`label`, `n_inputs`, `n_outputs`) and construction (`make_component()`).

`components` and `tunnels` are `SlotMap`s keyed by their own generational key types
(`PlacedCompKey`, `PlacedTunnelKey` — distinct from the circuit's own `CompKey`/
`TunnelKey`), not a `Vec`. This lets `Selected`, `DragSource`, and
`InteractionMode::ComponentDrag` hold a stable key directly — `Selected` is
`Component(PlacedCompKey) | Tunnel(PlacedTunnelKey)` — instead of a `Vec` index
(which would shift on removal) or a raw `CompKey`/`TunnelKey` that then has to be
linearly searched for in a `Vec` to find its visual record. The hit-testing
functions `pin_at_pos`/`tunnel_pin_at_pos` (see Pin positions below) return these
same `PlacedCompKey`/`PlacedTunnelKey` values, so callers can index straight into
`self.components`/`self.tunnels` without an extra search.

### Rendering pipeline (each frame)

1. Menu bar — "File" (Save/Load), "Add" (Gates / Input / Output / Mux / Demux / Splitter /
   Memory→Register / Tunnel→Feed|Pull) populating `mode = Placing`/`PlacingTunnel`, a "Tick
   Clock" button, and a red status label for `last_settle_error`.
2. Optional properties side panel for the current `selected` (edits component params or a
   tunnel label — see Properties panel below).
3. `allocate_painter` fills the remaining area; `Sense::click_and_drag` captures all input.
4. `draw_grid` — dot grid at GRID_SIZE (20 px) intervals, offset by `pan`.
5. Wires — L-shaped elbow at the horizontal midpoint; stroke from `value_stroke(theme, value)`
   (color = signal state, thickness = 1-bit vs multi-bit bus).
6. Components / tunnels — tessellated bezier shapes with labels; pin circles colored by signal
   value. Shape and pin positions come from `ComponentDef::shape()` (see Component Shape System).
7. Mode overlay — ghost component/tunnel while placing; rubber-band line while dragging a wire.

### Interaction modes (`InteractionMode`)

- **Idle**: drag from an output pin (or a Feed tunnel) → enters WireDrag; drag a component/
  tunnel body → ComponentDrag; click a body → selects it; click an Input component body →
  toggles its 1-bit value and calls `circuit.set_input` + `circuit.settle`.
- **Placing { def }**: shows a ghost at the snapped grid cell; click places the component via
  `place_component`, which calls `circuit.add_component` and inserts into `self.components`.
- **PlacingTunnel { role }**: same, for a Feed/Pull tunnel via `place_tunnel`.
- **WireDrag { src, current_end }**: `src` is a `DragSource` (a component output pin or a
  Feed tunnel). Tracks `current_end`; on `drag_stopped`, if the cursor is within 2 × PIN_RADIUS
  of a valid input pin (component input pin, or a Pull tunnel), calls `circuit.link` /
  `circuit.link_tunnel` + `circuit.settle` and records the `Wire`/`TunnelWire`. Escape → Idle.
- **ComponentDrag { key, drag_origin, original_grid_pos }**: moves the selected component or
  tunnel, re-snapping `grid_pos` as the cursor moves.

Regardless of mode, **Backspace** (when no text field is focused) deletes the current selection
(see Deleting components and tunnels below).

### Properties panel

When something is selected, a side panel edits it. For a **tunnel** it edits the `label`
(committed to `circuit.rename_tunnel` + settle on Enter). For a **component** it edits the
type's parameters via `DragValue`/`ComboBox`/`selectable_value` widgets (e.g. Input bits/width,
Gate n_inputs/width, Mux/Demux data_width/sel_width, Reg data_width, Splitter width/arms/
direction and per-bit arm assignment). Any edit calls `reconfigure_component`, which builds a
new `ComponentDef`, `remove_component`s the old circuit component, and re-places a fresh one
(the `PlacedCompKey` is stable, so `Selected` doesn't need updating). The panel also has a
**Delete** button at the bottom that removes the selected component/tunnel.

### Deleting components and tunnels

A selected component or tunnel is removed either via the properties panel's **Delete** button or
by pressing **Backspace** while it is selected (the Backspace handler, next to the Escape handler
in the canvas block, is gated on `!ctx.memory(|m| m.focused().is_some())` so a Backspace aimed at
the tunnel-label text field edits text instead of deleting). Both paths call the App-level
`delete_component(PlacedCompKey)` / `delete_tunnel(PlacedTunnelKey)`, which invoke
`circuit.remove_component` / `remove_tunnel` (net/tunnel teardown + downstream re-evaluation),
then drop the visual records that referenced the removed key (`retain` over `wires`/
`tunnel_wires`, remove from `components`/`tunnels` — same "touches this key" filter as
`reconfigure_component`), clear `selected` if it pointed at the removed item, and settle.

### Save / Load (io.rs)

Save/Load serialize the GUI's visual state (not the sim SlotMaps) to a versioned JSON
`CircuitFile { version, components, tunnels, wires, tunnel_wires }`. Every cross-reference is a
plain `usize` index into the file's `components`/`tunnels` vectors (slotmap keys are ephemeral
and not persisted). `OsmilogApp::to_circuit_file` / `load_circuit_file` do the App↔file
conversion; `CircuitFile::validate()` checks the version and index bounds before a load
replaces the current circuit. File I/O is split into `io::native` (blocking rfd dialogs +
`std::fs`) and `io::wasm` (Blob download for save; async `rfd::AsyncFileDialog` for load,
delivering into `pending_load`, polled each frame by `apply_pending_load`).

### Component Shape System (shape.rs + app.rs)

Every visual component type is described by a `ComponentShape` value returned from
`ComponentDef::shape()`. Nothing hard-codes "inputs on left, outputs on right" — each shape
specifies its own geometry.

    pub struct ComponentShape {
        size: Vec2,                       // bounding box in pixels (W × H)
        outline: Vec<ShapeCmd>,           // closed path in normalized [0,1]² coords
        fill_outline: Option<Vec<ShapeCmd>>, // convex-only fallback for filling concave outlines
        input_anchors: Vec<PinAnchor>,    // one per circuit input pin, in circuit pin order
        output_anchors: Vec<PinAnchor>,   // one per circuit output pin, in circuit pin order
        extra_strokes: Vec<Vec<ShapeCmd>>,// open strokes drawn on top (e.g. XOR arc)
        output_bubbles: Vec<bool>,        // true → draw inversion bubble on that output
        labels: Vec<ComponentLabel>,      // hardcoded, non-editable pin/section labels
        dynamic_label_pos: Vec2,          // position for a single externally-supplied editable label
    }

    pub struct PinAnchor {
        norm_pos: Vec2,     // position in [0,1]² relative to bounding box
        wire_dir: Vec2,     // unit vector the wire exits toward (away from component)
        pixel_offset: f32,  // extra pixel shift along wire_dir (non-zero for bubble outputs)
    }

    pub struct ComponentLabel {
        text: &'static str, // hardcoded label text
        pos: Vec2,           // position in [0,1]², same convention as PinAnchor.norm_pos
    }

**Labels: Components vs. Tunnels.** `ComponentDef::shape()` bakes zero or more hardcoded,
non-editable `ComponentLabel`s into `labels` — most component types have none; `Reg` labels its
two input pins `"D"` and `"WE"` next to their anchors. There is no per-instance component label
anymore (the old free-form `PlacedComponent.label: String` and its properties-panel "Label:"
field were removed — components don't need unique names). `PlacedTunnel` is the one exception:
it keeps a single user-editable `label: String` (edited in the properties panel, committed to
`circuit.rename_tunnel` on Enter), drawn at `shape.dynamic_label_pos` — this is why
`ComponentShape` still carries one dynamic position field alongside the new hardcoded `labels`
list: `dynamic_label_pos` is meaningful only for Tunnels, `labels` only for Components.

`ShapeCmd` is `MoveTo(Vec2) | LineTo(Vec2) | CubicTo(Vec2, Vec2, Vec2)`, all in normalized
coords. `tessellate_path(cmds, rect)` converts a `&[ShapeCmd]` into a `Vec<Pos2>` suitable
for `egui::epaint::PathShape`, approximating cubic beziers with 16 line segments each.

Normalized coords are scaled to `rect` as: `pos2(rect.left + n.x * rect.width, rect.top + n.y * rect.height)`.
Values outside [0,1] are valid and draw outside the bounding box (used by the XOR extra arc).

**Pin positions** are computed by `comp_pin_pos(shape: &ComponentShape, grid_pos, pan,
PinId)` for components and `tunnel_pin_pos(pt: &PlacedTunnel, pan)` for tunnels:

    base = rect.topleft + anchor.norm_pos * rect.size
    pin_pos = base + anchor.wire_dir * anchor.pixel_offset

The reverse lookup — "which pin, if any, sits under this screen position" — is done
by `pin_at_pos` and `tunnel_pin_at_pos`, which return the owning `PlacedCompKey`/
`PlacedTunnelKey` (paired with a `PinId` for `pin_at_pos`) rather than a raw
`CompKey`/`TunnelKey`, so callers can index straight into `self.components`/
`self.tunnels`.

For bubble outputs (`output_bubbles[i] == true`), `draw_component` draws a filled circle
at `base + wire_dir * BUBBLE_R` and the pin dot (and wire terminus) sits `BUBBLE_R * 2`
further along `wire_dir`.

**Shape per component type:**

| Variant | Body shape | Inputs | Outputs | Labels | Notes |
|---|---|---|---|---|---|
| Input | Rectangle | — | right center | — | COMP_MIN_WIDTH × COMP_MIN_HEIGHT (20 × 20) |
| Output | Rectangle | left center | — | — | 20 × 20 |
| AND / NAND | Flat left + semicircle right | left edge, evenly spaced | right center | — | NAND adds bubble |
| OR / NOR | Three-cubic closed curve | left edge (x = 0) | right tip | — | NOR adds bubble |
| XOR / XNOR | Same as OR + extra concave arc at x ≈ −0.15 (extra_strokes) | left edge | right tip | — | XNOR adds bubble |
| NOT | Triangle (3 vertices) | left center | right tip + bubble | — | |
| Mux | Trapezoid, wider left | left edge = data [1..]; bottom center = selector [0] | right center | — | |
| Demux | Trapezoid, wider right | left center = data [0]; bottom center = selector [1] | right edge, evenly spaced | — | |
| Reg | Rectangle | left edge: [0] data (y=0.25), [1] write_enable (y=0.75) | right center | `"D"` next to input[0], `"WE"` next to input[1] | height uses a fixed `(2+3) * COMP_HEIGHT_PER_PIN`, not the branches-based formula below |
| Splitter (FanDirection::Right) | Narrow comb: thin vertical spine + trunk/tooth strokes | trunk bus on left | one arm per output on right | — | drawn narrow (SPLITTER_WIDTH 20) to read as a connector, not a block |
| Combine (FanDirection::Left) | Same comb, mirrored | one arm per input on left | trunk bus on right | — | same geometry as Splitter |

The selector pin anchor for Mux and Demux sits at the midpoint of the bottom slanted edge
`(0.5, 1.0 − T/2)` with `wire_dir = (0, 1)` (exits downward), where `T = 0.2` is the taper
fraction. Component height scales with `(branches + 1) * COMP_HEIGHT_PER_PIN`.

Hit testing for click-to-select uses the bounding rect (`component_bounding_rect`), not the
actual shape polygon. This is a known approximation.

### Geometry constants

In geometry.rs:

    GRID_SIZE            20 px — canvas grid spacing
    COMP_WIDTH           40 px — default component box width
    COMP_MIN_WIDTH       20 px — used by Input/Output
    COMP_HEIGHT_PER_PIN  10 px — height contributed by each pin slot
    COMP_MIN_HEIGHT      20 px — floor on component box height
    SPLITTER_WIDTH       20 px — splitter/combine body width (drawn narrow)

In app.rs:

    PIN_RADIUS             3 px — drawn radius; hit radius is 2 ×
    BUBBLE_R               4 px — inversion bubble radius (from shape.rs)
    WIRE_THICKNESS_THIN    2 px — 1-bit (or floating) wire
    WIRE_THICKNESS_THICK   4 px — multi-bit bus wire
    LABEL_FONT_SIZE        8 px

### Signal color coding

Colors come from `Theme`; the first three below track the theme, the low/high signal colors are
intentionally fixed (they encode circuit data, not UI chrome). Wire thickness encodes bus width
(see `value_stroke`).

    Floating                 theme.value_floating (muted gray)
    Fixed { bits: 0, .. }   dark blue (logic low)
    Fixed { .. }            green (logic high / non-zero bus)
    width == 1               thin wire; width > 1  thick wire

### Window close (macOS)

`logic()` checks `ctx.input(|i| i.viewport().close_requested())` and calls
`std::process::exit(0)`. This bypasses eframe's double `save_and_destroy` cleanup sequence
(a bug in eframe 0.35 on macOS that panics on the second GPU painter destroy) which would
otherwise trigger Apple's crash reporter.


## Known Limitations / TODOs

- No conflict detection when multiple *component* sources end up on the same net (merge() has a
  documented bug); tunnel-driver conflicts *are* detected via `SettleError::TunnelConflict`
- set_input and read_output do not return errors for wrong component type (silent no-op / panic)
- width is not verified to be nonzero in Value::Fixed
- Add/Sub Value ops exist but are unused by any component; may overflow, behavior unspecified
- `net_of`/`link` do not bounds-check pin indices, so an out-of-range pin (incl. from a
  hand-edited saved file, which `validate()` does not check) can panic downstream
- Click-to-select uses the bounding rect, not the actual shape polygon (affects triangles/curves)
- No canvas panning/zooming interaction yet (a `pan` offset field exists but is never mutated)
- No wire-only delete gesture (components/tunnels can be deleted, but individual wires can only
  go away by deleting an endpoint or reconfiguring)
- `Wire`/`TunnelWire` still store raw `CompKey`/`TunnelKey` rather than
  `PlacedCompKey`/`PlacedTunnelKey`, so drawing a wire still does a linear
  `.values().find(...)` over `components`/`tunnels` (see FIXME comments in the draw loop)


## Future Directions

- More component types: arithmetic units (adders/ALU), decoders, memories, additional
  sequential elements
- Extend the GUI: delete individual wires, canvas pan & zoom, multi-select, undo/redo
- Subcircuits / hierarchical components
- Save/load format evolution (bump `CircuitFile` version as its shape changes)
