# Architecture

Read this file first. For layer internals, see [simulator.md](simulator.md),
[gui.md](gui.md), and [save-load.md](save-load.md).

## What osmilog is

Osmilog is a digital logic circuit simulator with an egui graphical editor. A circuit
builds two ways: programmatically, or interactively in the GUI.

The simulator propagates combinational signal changes until the state settles
(`settle`). A clock tick advances sequential state (`tick_clock`).

The app targets native (`eframe`) and the browser (WASM). A circuit saves to and loads
from a plain JSON file, extension `.osm`.

The crate is a library (`src/lib.rs`: `pub mod gui / io / sim`) plus a thin binary
(`src/main.rs`) that builds `OsmilogApp` and hands it to `eframe`. Tests live in
`#[cfg(test)]` modules next to the code they test.

## Dependencies

- `slotmap` — generational keys for `Circuit`'s nets.
- `eframe` / `egui` — the GUI framework.
- `serde` / `serde_json` — save and load.
- `rfd` — native and async file dialogs.
- WASM target adds `wasm-bindgen`, `wasm-bindgen-futures`, `js-sys`, `web-sys`.

## Project structure

    src/lib.rs                  crate root: pub mod gui / io / sim
    src/main.rs                 native eframe entry + wasm_bindgen(start) WASM entry

    src/sim.rs                  pub mod circuit / command / component / net / value
    src/sim/value.rs            Value — signal representation
    src/sim/net.rs              Net — a wire connecting component pins
    src/sim/component.rs        Component, Logic/CombLogic/SeqLogic, ComponentSpec, key types
    src/sim/component/*.rs      one file per component kind (gate, mux, demux, reg, adder, ...)
    src/sim/circuit.rs          Circuit — the simulation graph, evaluation engine, tunnels
    src/sim/command.rs          Command/CommandOutput/UndoAction — undo-recordable mutations

    src/gui.rs                  pub mod app / canvas_draw / clipboard / clock / document /
                                 geometry / gui_undo / history / memory_editor /
                                 placed_component / properties / shape / theme / utils / wiring
    src/gui/app.rs               OsmilogApp (eframe::App) — cross-document state, subcircuit
                                 instantiation, save/load, clipboard, menu/palette UI
    src/gui/document.rs         DocId/CircuitDoc/Document — one open circuit's state,
                                 simulation, undo/redo, canvas drawing/interaction
    src/gui/properties.rs       show_properties — mutation-free properties panel
    src/gui/placed_component.rs PlacedComponent — visual record; GUI display methods on ComponentSpec
    src/gui/wiring.rs           Wiring — GUI's connectivity graph (grid nodes + segments)
    src/gui/gui_undo.rs         GuiUndoAction (Wiring delta / drag-move) + edit_wiring/commit_move
    src/gui/history.rs          History — accumulates HistoryEntrys from apply()/edit_wiring()
    src/gui/shape.rs            ComponentShape, PinAnchor, tessellate_path
    src/gui/geometry.rs         per-component-type shape builders + grid/pixel constants
    src/gui/theme.rs            Theme — canvas/signal colors derived from egui Visuals
    src/gui/utils.rs            CanvasCtx — shared canvas-interaction context

    src/io.rs                   ProjectFile/CircuitSnapshot save/load format (JSON), v2→v3 upgrade

## Layering rule

The layering runs one direction only:

    gui  ──depends on──>  sim
    io   ──depends on──>  sim
    sim  ──depends on───  (nothing else in this crate)

`sim` has no knowledge of the GUI. It has its own key types, its own construction API
(`Component::gate(...)`, `Component::mux(...)`, ...), and its own mutation/undo layer
(`sim::command::Command`).

`gui` is the one frontend built on `sim`. It keeps its own connectivity model,
`gui::wiring::Wiring` — grid nodes and segments — as the source of truth for what is
wired together. `sim::circuit::Circuit` is the source of truth for signal values. After
a wiring edit, `Document::rebuild_circuit` clears the circuit's nets and replays them
from `Wiring`: `clear_nets()`, then `link`/`link_tunnel` per connected group, then
`settle()`. `Circuit` never learns pixel or grid geometry. `Wiring` never learns signal
values.

Two types cross the boundary. Both depend on `sim`; `sim` never depends on them.

- **`sim::component::ComponentSpec`** is a construction-params enum, consumed through
  `ComponentSpec::to_component()`. `PlacedComponent`, the GUI's visual record, reuses
  this same enum. `gui::placed_component` adds a second `impl ComponentSpec` block with
  GUI-only display methods (`size`, `label`, `shape`).
- **`sim::command::Command`** is the only path for the GUI to mutate a circuit. Every
  authoritative edit goes through `Document::apply(Command) -> CommandOutput`, which
  calls `Circuit::apply` and pushes the returned `UndoAction` onto
  `gui::history::History`. `gui::gui_undo::GuiUndoAction` is a separate GUI-only undo
  type, for edits `Command` has no notion of: wiring-graph changes, component/tunnel
  moves. Both undo types record onto the same `History` stack, as
  `HistoryEntry::Sim`/`HistoryEntry::Gui`, so one user gesture spanning both domains
  collapses into one `HistoryEntry::Batch`.

`src/io.rs` depends only on `sim` types (`ComponentSpec`, `TunnelRole`) plus plain-data
GUI geometry types. It does not depend on `OsmilogApp`.
