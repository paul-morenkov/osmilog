# osmilog

A digital logic circuit simulator in Rust with an egui editor, targeting native + WASM, saving
circuits to a plain-JSON `.osm` file.

## Core rules

1. **Layering is one-directional:** `gui` and `io` depend on `sim`; `sim` depends on nothing
   in-crate. Never make `sim` aware of the GUI.
2. **The GUI mutates the circuit only through `Document::apply(Command)`** — never call
   `Circuit::add_component`/`link`/etc. directly.
3. **`Wiring` is the source of truth for connectivity, `Circuit` for signal values;** after any
   wiring edit, nets are re-derived via `Document::rebuild_circuit`.

## Style rules
1. Only add comments to explain unusual or tricky behavior; avoid narrating every code block.
2. Do not split `impl` blocks across multiple files just to reduce file size.
3. **Verify with `cargo check` / `cargo test`.** Tests live in `#[cfg(test)]` modules next to code.
4. Never check for formatting after editing, just run `cargo fmt`.

## Where to read (open based on task)

- `.claude/docs/architecture.md` — **read first:** the layering boundary, file map, deps.
- `.claude/docs/simulator.md` — changing `sim`: values, nets, the engine, commands, components.
- `.claude/docs/gui.md` — changing the editor: documents, wiring, undo/redo, drawing, properties.
- `.claude/docs/save-load.md` — touching the `.osm` save format.
- `.claude/docs/roadmap.md` — unimplemented areas + known panics.

To add a new component kind, invoke the `add-component-type` skill.
To cut a release, the `release` skill.
