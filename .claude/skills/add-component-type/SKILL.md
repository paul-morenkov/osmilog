---
name: add-component-type
description: Add a new component kind (gate/mux/register/etc.) to the osmilog simulator. Use when adding, creating, or wiring up a new logic component type end-to-end across sim and gui. Covers the CombLogic/SeqLogic impl, ComponentSpec enum wiring, GUI shape, and palette entry.
---

# Adding a new component kind

A component kind is defined in `sim` (its params + logic) and drawn/placed in `gui`. Adding one
touches ~6 files. Model a **combinational** type on `src/sim/component/gate.rs` and a
**sequential** type on `src/sim/component/reg.rs`. Read `.claude/docs/simulator.md` (Component
model) for the trait contracts before starting.

## Checklist

1. **New file `src/sim/component/<kind>.rs`.** Define one struct holding the construction params
   and impl the right trait:
   - Combinational → `impl CombLogic for <Kind>`: `n_inputs`, `n_outputs`, `evaluate(&[Value]) ->
     Vec<Value>`, `input_width(i)`, `output_width(i)`. Keep params + arity + logic in this one
     struct so they can't drift.
   - Sequential → split into `<Kind>Conf` (static params) + `<Kind>` (runtime = `conf` + latched
     `Value`), and `impl SeqLogic for <Kind>`: `n_inputs`, `n_outputs`, `tick`, `apply_async`
     (must be **idempotent** — it runs every `settle()`), `observe`, `snapshot`, widths. Add the
     async-reset pin if the type should support one (see `reg.rs`).
   - Add the module to the `mod` list at the top of `src/sim/component.rs` (and re-export as the
     siblings do).

2. **Register the logic enum arm** in `src/sim/component.rs`: add a variant to `enum LogicComb`
   (`component.rs:673`) or `enum LogicSeq` (`component.rs:820`), and handle it in the `match`es
   that dispatch over that enum (they won't compile until you do — let the compiler list them).

3. **Add the `ComponentSpec` variant + build path** in `src/sim/component.rs`: a variant on
   `enum ComponentSpec` (`component.rs:365`) carrying the params (the `*Conf` for a sequential
   type, never the runtime struct), and a `match` arm in `fn to_component` (`component.rs:487`)
   that builds the live `Component`. Also add a `Component::<kind>(...)` constructor if that
   matches the sibling pattern.

4. **GUI shape** in `src/gui/geometry.rs`: add a `<kind>_shape(...) -> ComponentShape` builder
   (model on `gate_shape`/`reg_shape`) defining outline, pin anchors, and labels in normalized
   `[0,1]²` coords. This is the *only* place pin geometry is specified.

5. **GUI display arms** in the `impl ComponentSpec` block in `src/gui/placed_component.rs`
   (`placed_component.rs:43`): add match arms for `fn size` (:46), `fn label` (:76), and
   `fn shape` (:123) — `shape` calls your new builder from step 4.

6. **Palette entry** in `src/gui/app.rs`: add a button to `fn show_component_palette`
   (`app.rs:1082`) that sets `InteractionMode::Placing { spec: <the new ComponentSpec> }`, the
   same shape as the existing entries.

7. **Verify:** `cargo test`. Add a `#[cfg(test)]` unit test for `evaluate`/`tick` in the new
   component file, next to the code (the convention across `src/sim/component/`).

## Notes

- Never let `sim` reference `gui` types — the shape/size/label live in `gui` precisely to keep
  the boundary one-directional (`.claude/docs/architecture.md`).
- A `ComponentSpec` is construction params only: no live `NetKey`s, no runtime latch.
- Bulk-state types (like `Rom`) alias one buffer between spec and component via
  `Rc<RefCell<..>>`; only reach for that pattern if the type carries large contents.
