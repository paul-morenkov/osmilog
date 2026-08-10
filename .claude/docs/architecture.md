# Architecture

*Read this first.* It covers the crate layout, the file map, and the one load-bearing rule
that governs the whole codebase: the `sim` / `gui` / `io` layering. For the internals of a
given layer, follow the pointers to [simulator.md](simulator.md), [gui.md](gui.md), and
[save-load.md](save-load.md).

## What osmilog is

A digital logic circuit simulator in Rust with an egui graphical editor. Circuits are built
either programmatically (constructing `Component`s and wiring them with `Circuit::link`) or
interactively in the GUI. The simulator propagates combinational signal changes through the
circuit graph until stable (`settle`), and advances sequential state on an explicit clock tick
(`tick_clock`). The app targets both desktop (native window, via `eframe`) and the browser
(WASM), and circuits save to / load from a plain JSON file (`.osm`).

The crate is a library (`src/lib.rs`: `pub mod gui / io / sim`) plus a thin binary
(`src/main.rs`) that just constructs `OsmilogApp` and hands it to `eframe`. Tests live in
`#[cfg(test)]` modules alongside the code they test.

## Dependencies

`slotmap` (generational-arena keys for `Circuit`'s nets - the one entity kind that is
never delete-then-undone, so a generational key is fine; every other keyed entity uses stable
app-assigned `u64` ids in plain `HashMap`s so undo can re-insert under the same key),
`eframe`/`egui` (GUI), `serde`/`serde_json` (save/load), `rfd` (native + async file dialogs).
WASM adds `wasm-bindgen`/`wasm-bindgen-futures`/`js-sys`/`web-sys`.

## Project structure

    src/lib.rs                       crate root: pub mod gui / io / sim
    src/main.rs                       native eframe::run_native entry, and a wasm_bindgen(start) WASM entry

    src/sim.rs                       pub mod circuit / command / component / net / value
    src/sim/value.rs                  Value - signal representation (Floating / Fixed / Invalid)
    src/sim/net.rs                    Net - a wire connecting component pins
    src/sim/component.rs              Component, Logic/CombLogic/SeqLogic, ComponentSpec, pin/key types
    src/sim/component/*.rs            one file per component kind (gate, mux, demux, splitter, reg, adder, ...)
    src/sim/circuit.rs               Circuit - the simulation graph, evaluation engine, tunnels
    src/sim/command.rs               Command/CommandOutput/UndoAction - the undo-recordable mutation layer

    src/gui.rs                       pub mod app / canvas_draw / clipboard / clock / document / geometry / gui_undo / history / memory_editor / placed_component / properties / shape / theme / utils / wiring
    src/gui/app.rs                    OsmilogApp (eframe::App) - cross-document state; subcircuit instantiation, save/load, clipboard, menu/palette UI
    src/gui/document.rs               DocId/CircuitDoc/Document - the multiple-open-circuits model; per-document state, simulation, undo/redo, and canvas drawing/interaction methods
    src/gui/properties.rs             show_properties - mutation-free properties panel returning a PropGuiAction
    src/gui/placed_component.rs       PlacedComponent - visual record; GUI-only display methods on ComponentSpec
    src/gui/wiring.rs                 Wiring - GUI's own connectivity graph (grid nodes + segments), + WiringDelta undo
    src/gui/gui_undo.rs               GuiUndoAction (Wiring delta / drag-move) + Document::edit_wiring/commit_move
    src/gui/history.rs                History - accumulates HistoryEntrys (Sim + Gui) from apply()/edit_wiring()
    src/gui/shape.rs                  ComponentShape, PinAnchor, tessellate_path - visual shape primitives
    src/gui/geometry.rs               per-component-type shape builders + grid/pixel geometry constants
    src/gui/theme.rs                  Theme - canvas/signal colors derived from ambient egui Visuals
    src/gui/utils.rs                  CanvasCtx - shared canvas-interaction context, used by both app.rs and document.rs

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
them from `Wiring` (`Document::rebuild_circuit`: `clear_nets()` + `link`/`link_tunnel` per
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
- **`sim::command::Command`** is how the GUI mutates the circuit at all. Neither `OsmilogApp` nor
  `Document` ever calls `Circuit::add_component`/`link`/`remove_component`/etc. directly; every
  *authoritative* edit goes through `Document::apply(Command) -> CommandOutput`, which calls
  `Circuit::apply` (returning `(CommandOutput, UndoAction)`) and pushes the `UndoAction` onto
  `gui::history::History`. Edits that only reconstruct *derived* net state (`ClearNets`/`Link`/
  `LinkTunnel`, all issued from `rebuild_circuit`) bypass that wrapper and call
  `self.circuit.apply(..).0` untracked - undo re-derives the nets rather than reversing them (see
  the Command layer section in [simulator.md](simulator.md)). This makes every GUI-issued
  authoritative mutation undo-recordable without the GUI needing to know how to reverse anything
  itself. `gui::gui_undo::GuiUndoAction` is the GUI-only undo counterpart for edits `Command` has
  no notion of (wiring-graph changes, component/tunnel moves) - a wholly separate type since
  `Wiring`/`GridPos`/`PlacedCompKey` must stay out of `sim`, but recorded onto the *same*
  `History` stack (as `HistoryEntry::Sim`/`HistoryEntry::Gui`) so a GUI edit and the `Command`s it
  triggers (e.g. drawing a wire also relinks nets via `rebuild_circuit`) collapse into one
  `HistoryEntry::Batch` instead of two disconnected entries. Unlike the sim side there is no
  "GuiCommand" enum: every `Wiring` mutator's inverse is just "replay its delta backwards", so
  `Document` calls the `Wiring` method directly and hands the returned `WiringDelta` to
  `Document::edit_wiring` - no command-as-data indirection.

`src/io.rs` (save/load) also depends only on `sim` types (`ComponentSpec`, `TunnelRole`) plus a
couple of GUI-defined-but-plain-data geometry types - it does not depend on `OsmilogApp` itself.
