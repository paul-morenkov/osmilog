//! The right-hand properties panel: per-selection editors (component / tunnel /
//! wire). A mutation-free renderer - it takes a read-only `&Document` and
//! *returns* the edit the user requested this frame as a `PropGuiAction`,
//! which `OsmilogApp::apply_prop_gui_action` (in `gui::app`) then applies. The
//! panel never mutates app state itself.

use crate::gui::app::{PlacedCompKey, PlacedTunnelKey, Selected, Selection};
use crate::gui::document::{DocId, Document};
use crate::gui::memory_editor::MemKind;
use crate::sim::circuit::TunnelRole;
use crate::sim::component::*;
use crate::sim::value::Value;

/// One user intent collected from the (mutation-free) properties panel, applied
/// afterward by `OsmilogApp::apply_prop_gui_action` once the panel's `&Document`
/// borrow ends. The panel only *describes* what to do; it never mutates app
/// state itself, which is what keeps `show_properties` a pure render function
/// over a bare `&Document`. Named for its origin: these are GUI-panel actions,
/// distinct from the sim-layer `Command`/`GuiUndoAction` machinery each one may
/// ultimately drive.
pub(crate) enum PropGuiAction {
    /// Swap a component's construction params (widths, arity, ...).
    Reconfigure(PlacedCompKey, ComponentSpec),
    /// Open the virtualized contents editor for a ROM or RAM.
    OpenMemory(PlacedCompKey, MemKind),
    /// Switch the active document to a placed subcircuit's referenced circuit.
    OpenCircuit(DocId),
    /// Commit a tunnel label rename (on focus loss) - relinks nets, undoable.
    RenameTunnel(PlacedTunnelKey, String),
    /// Persist an in-progress tunnel label edit (on each keystroke). Must be
    /// applied the same frame: the text buffer is re-cloned from the record
    /// every frame, so a dropped write loses the edit. Not a committed rename.
    SetTunnelLabelLive(PlacedTunnelKey, String),
    /// Delete the selected component / tunnel / wire.
    Delete(Selected),
}

/// Renders the properties panel for the current selection and returns the edit
/// the user requested this frame, if any. Deliberately takes a read-only
/// `&Document` (not `&mut OsmilogApp`): every value it needs to draw is
/// document-scoped, so all it can do is collect intent - the caller applies it.
pub(crate) fn show_properties(doc: &Document, ui: &mut egui::Ui) -> Option<PropGuiAction> {
    let sel = match &doc.selected {
        None => {
            ui.label("Click a component or tunnel to select it.");
            return None;
        }
        Some(Selection::Bulk(items)) => {
            ui.heading("SELECTION");
            ui.separator();
            ui.label(format!("{} items selected.", items.len()));
            ui.label("Press Backspace or Delete to remove them.");
            return None;
        }
        Some(Selection::Single(sel)) => *sel,
    };
    // A run session makes structural edits read-only, but value edits
    // (an Input's bits, ROM/RAM contents) stay live while Paused. Rather
    // than blanket-disabling the panel, each per-component editor gates its
    // own widgets - structural ones on editing_locked(), value ones on
    // value_editing_locked() - so the carve-out lands on exactly those.
    let mut action = match sel {
        Selected::Component(key) => show_component_properties(doc, key, ui),
        Selected::Tunnel(key) => show_tunnel_properties(doc, key, ui),
        Selected::Wire(_) => {
            ui.heading("WIRE");
            ui.label("A wire segment. Press Backspace or Delete to remove it.");
            None
        }
    };

    ui.separator();
    // Delete is structural: disabled for the whole run session.
    ui.add_enabled_ui(!doc.editing_locked(), |ui| {
        if ui.button("Delete").clicked() {
            action = Some(PropGuiAction::Delete(sel));
        }
    });
    action
}

pub(crate) fn show_tunnel_properties(
    doc: &Document,
    key: PlacedTunnelKey,
    ui: &mut egui::Ui,
) -> Option<PropGuiAction> {
    let role = doc.tunnels[&key].role;
    let tunnel_key = doc.tunnels[&key].key;

    ui.heading(match role {
        TunnelRole::Feed => "TUNNEL (FEED)",
        TunnelRole::Pull => "TUNNEL (PULL)",
    });
    ui.separator();
    let mut action = None;
    // A tunnel's label is structural (it rewires nets): read-only for the
    // whole run session.
    ui.add_enabled_ui(!doc.editing_locked(), |ui| {
        ui.label("Label:");
        let mut label = doc.tunnels[&key].label.clone();
        let response = ui.text_edit_singleline(&mut label);
        if response.changed() {
            // Persist the in-progress edit back to the record this frame (the
            // buffer is re-cloned from it next frame). Applied by the caller,
            // but still same-frame - see SetTunnelLabelLive.
            action = Some(PropGuiAction::SetTunnelLabelLive(key, label.clone()));
        }

        // Commit on any focus loss (Enter/Tab/click-away), not just Enter. The
        // record label is updated live above, but the circuit's isn't; the
        // caller reads the old label from the circuit to detect a real change
        // and capture undo's restore value. (rebuild_circuit also reconciles.)
        if response.lost_focus()
            && doc
                .circuit
                .tunnels
                .get(&tunnel_key)
                .map(|t| t.label.as_str())
                != Some(label.as_str())
        {
            action = Some(PropGuiAction::RenameTunnel(key, label));
        }
    });
    action
}

// Draws "<label> [DragValue]" in a horizontal row and returns whether the
// value changed - the recurring widget idiom nearly every ComponentSpec
// arm below uses for one numeric parameter. Generic over the DragValue's
// numeric type since fields vary between u8/u32/usize.
fn labeled_drag<Num: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Num,
    range: std::ops::RangeInclusive<Num>,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        changed = ui.add(egui::DragValue::new(value).range(range)).changed();
    });
    changed
}

// Draws the "bits" widget shared by Input and Constant: a checkbox when
// width == 1 (a single bit reads as on/off), else a labeled DragValue
// clamped to the value's width. Returns whether it changed. Callers
// choose which enable-gate wraps this (Input gates it on value_ok,
// Constant on structural_ok - see each arm).
fn bits_widget(ui: &mut egui::Ui, bits: &mut u32, width: u8) -> bool {
    if width == 1 {
        let mut high = *bits != 0;
        if ui.checkbox(&mut high, "Toggle").clicked() {
            *bits = high as u32;
            return true;
        }
        false
    } else {
        labeled_drag(ui, "Bits:", bits, 0..=Value::mask(width))
    }
}

pub(crate) fn show_component_properties(
    doc: &Document,
    key: PlacedCompKey,
    ui: &mut egui::Ui,
) -> Option<PropGuiAction> {
    let comp_key = doc.components[&key].key;

    ui.heading(doc.components[&key].spec.label());
    ui.separator();

    // A run session locks *structural* edits (widths, arity, wiring) for its
    // whole duration, but carves out live *value* edits - an Input's bits and
    // ROM/RAM contents - which stay pokeable while Paused (blocked only while
    // actively Playing). Every editable widget below is gated on whichever
    // predicate applies; read-only value displays stay ungated so a running
    // circuit's state remains observable.
    let structural_ok = !doc.editing_locked();
    let value_ok = !doc.value_editing_locked();

    // The spec is matched *by reference*: a ROM/RAM spec carries its whole
    // contents buffer (up to tens of MiB), so cloning it every frame just to
    // own the match scrutinee is out. Borrowing it (and taking only `&Document`)
    // means the arms can't mutate anything, so each records a deferred
    // PropGuiAction the caller applies once this borrow ends.
    let mut edit: Option<PropGuiAction> = None;

    let fmt_val = |v: Value| match v {
        Value::Fixed { bits, width } => format!("0x{:X} ({}b)", bits, width),
        Value::Floating => "Floating".to_string(),
        Value::Invalid => "Invalid (width mismatch)".to_string(),
    };

    match &doc.components[&key].spec {
        ComponentSpec::Input(Input {
            mut bits,
            mut width,
        }) => {
            let mut changed = false;
            ui.label(format!("Value: 0x{:X}", bits));
            // `bits` is the live value: editable while Paused.
            ui.add_enabled_ui(value_ok, |ui| {
                changed |= bits_widget(ui, &mut bits, width);
            });
            // `width` is structural: locked for the whole run session.
            ui.add_enabled_ui(structural_ok, |ui| {
                changed |= labeled_drag(ui, "Width:", &mut width, 1..=32);
            });
            if changed {
                bits &= Value::mask(width); // In case width was changed below max `bits` value
                edit = Some(PropGuiAction::Reconfigure(
                    key,
                    ComponentSpec::Input(Input { bits, width }),
                ));
            }
        }
        ComponentSpec::Constant(Constant {
            mut bits,
            mut width,
        }) => {
            let mut changed = false;
            ui.label(format!("Value: 0x{:X}", bits));
            ui.add_enabled_ui(structural_ok, |ui| {
                changed |= bits_widget(ui, &mut bits, width);
                changed |= labeled_drag(ui, "Width:", &mut width, 1..=32);
            });
            if changed {
                bits &= Value::mask(width); // In case width was changed below max `bits` value
                edit = Some(PropGuiAction::Reconfigure(
                    key,
                    ComponentSpec::Constant(Constant { bits, width }),
                ));
            }
        }
        ComponentSpec::Output => {
            let val = doc.circuit.read_output(comp_key);
            ui.label(format!("Value: {}", fmt_val(val)));
        }
        ComponentSpec::Gate(Gate {
            op,
            mut n_inputs,
            mut width,
        }) => {
            let op = *op;
            let mut changed = false;
            ui.add_enabled_ui(structural_ok, |ui| {
                if op != GateOp::Not {
                    changed |= labeled_drag(ui, "Inputs:", &mut n_inputs, 2..=8);
                }
                changed |= labeled_drag(ui, "Width:", &mut width, 1..=32);
            });
            if changed {
                edit = Some(PropGuiAction::Reconfigure(
                    key,
                    ComponentSpec::Gate(Gate {
                        op,
                        n_inputs,
                        width,
                    }),
                ));
            }
        }
        ComponentSpec::Mux(Mux {
            mut data_width,
            mut sel_width,
        }) => {
            let mut changed = false;
            ui.add_enabled_ui(structural_ok, |ui| {
                changed |= labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                changed |= labeled_drag(ui, "Sel width:", &mut sel_width, 1..=4);
            });
            if changed {
                edit = Some(PropGuiAction::Reconfigure(
                    key,
                    ComponentSpec::Mux(Mux {
                        data_width,
                        sel_width,
                    }),
                ));
            }
        }
        ComponentSpec::Demux(Demux {
            mut data_width,
            mut sel_width,
        }) => {
            let mut changed = false;
            ui.add_enabled_ui(structural_ok, |ui| {
                changed |= labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                changed |= labeled_drag(ui, "Sel width:", &mut sel_width, 1..=4);
            });
            if changed {
                edit = Some(PropGuiAction::Reconfigure(
                    key,
                    ComponentSpec::Demux(Demux {
                        data_width,
                        sel_width,
                    }),
                ));
            }
        }
        ComponentSpec::Reg(RegConf { mut data_width }) => {
            let mut changed = false;
            ui.add_enabled_ui(structural_ok, |ui| {
                changed |= labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
            });
            if changed {
                edit = Some(PropGuiAction::Reconfigure(
                    key,
                    ComponentSpec::Reg(RegConf { data_width }),
                ));
            }

            let cur = doc.circuit.components[&comp_key].pins.out_cache[0];
            ui.label(format!("Value: {}", fmt_val(cur)));
        }
        ComponentSpec::ShiftReg(ShiftRegConf {
            mut data_width,
            mut num_stages,
            mut parallel_load,
        }) => {
            let mut changed = false;
            ui.add_enabled_ui(structural_ok, |ui| {
                changed |= labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                changed |= labeled_drag(ui, "Stages:", &mut num_stages, 1..=16);
                ui.horizontal(|ui| {
                    changed |= ui.checkbox(&mut parallel_load, "Parallel load").changed();
                });
            });
            if changed {
                edit = Some(PropGuiAction::Reconfigure(
                    key,
                    ComponentSpec::ShiftReg(ShiftRegConf {
                        data_width,
                        num_stages,
                        parallel_load,
                    }),
                ));
            }

            for (i, v) in doc.circuit.components[&comp_key]
                .pins
                .out_cache
                .iter()
                .enumerate()
            {
                ui.label(format!("Stage {i}: {}", fmt_val(*v)));
            }
        }
        ComponentSpec::Counter(CounterConf {
            mut data_width,
            mut max_value,
            mut overflow_action,
        }) => {
            let mut changed = false;
            ui.add_enabled_ui(structural_ok, |ui| {
                changed |= labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                changed |= labeled_drag(
                    ui,
                    "Max value:",
                    &mut max_value,
                    0..=Value::mask(data_width),
                );
                ui.horizontal(|ui| {
                    ui.label("Overflow action:");
                    egui::ComboBox::from_id_salt(key)
                        .selected_text(format!("{overflow_action:?}"))
                        .show_ui(ui, |ui| {
                            for action in [
                                OverflowAction::Wrap,
                                OverflowAction::StayMax,
                                OverflowAction::PassMax,
                                OverflowAction::LoadNext,
                            ] {
                                changed |= ui
                                    .selectable_value(
                                        &mut overflow_action,
                                        action,
                                        format!("{action:?}"),
                                    )
                                    .changed();
                            }
                        });
                });
            });
            if changed {
                max_value = max_value.min(Value::mask(data_width)); // Re-cap in case data_width shrank below max_value
                edit = Some(PropGuiAction::Reconfigure(
                    key,
                    ComponentSpec::Counter(CounterConf {
                        data_width,
                        max_value,
                        overflow_action,
                    }),
                ));
            }

            let q = doc.circuit.components[&comp_key].pins.out_cache[0];
            let carry = doc.circuit.components[&comp_key].pins.out_cache[1];
            ui.label(format!("Q: {}", fmt_val(q)));
            ui.label(format!("Carry: {}", fmt_val(carry)));
        }
        ComponentSpec::DFlipFlop(_)
        | ComponentSpec::TFlipFlop(_)
        | ComponentSpec::JKFlipFlop(_)
        | ComponentSpec::SRFlipFlop(_) => {
            let cur = doc.circuit.components[&comp_key].pins.out_cache[0];
            ui.label(format!("Value: {}", fmt_val(cur)));
        }
        ComponentSpec::Encoder(Encoder { mut sel_width }) => {
            let mut changed = false;
            ui.add_enabled_ui(structural_ok, |ui| {
                changed |= labeled_drag(ui, "Sel width:", &mut sel_width, 0..=4);
            });
            if changed {
                edit = Some(PropGuiAction::Reconfigure(
                    key,
                    ComponentSpec::Encoder(Encoder { sel_width }),
                ));
            }
        }
        ComponentSpec::Adder(Adder { mut data_width }) => {
            let mut changed = false;
            ui.add_enabled_ui(structural_ok, |ui| {
                changed |= labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
            });
            if changed {
                edit = Some(PropGuiAction::Reconfigure(
                    key,
                    ComponentSpec::Adder(Adder { data_width }),
                ));
            }
        }
        ComponentSpec::Subtractor(Subtractor { mut data_width }) => {
            let mut changed = false;
            ui.add_enabled_ui(structural_ok, |ui| {
                changed |= labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
            });
            if changed {
                edit = Some(PropGuiAction::Reconfigure(
                    key,
                    ComponentSpec::Subtractor(Subtractor { data_width }),
                ));
            }
        }
        ComponentSpec::Multiplier(Multiplier { mut data_width }) => {
            let mut changed = false;
            ui.add_enabled_ui(structural_ok, |ui| {
                changed |= labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
            });
            if changed {
                edit = Some(PropGuiAction::Reconfigure(
                    key,
                    ComponentSpec::Multiplier(Multiplier { data_width }),
                ));
            }
        }
        ComponentSpec::Divider(Divider { mut data_width }) => {
            let mut changed = false;
            ui.add_enabled_ui(structural_ok, |ui| {
                changed |= labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
            });
            if changed {
                edit = Some(PropGuiAction::Reconfigure(
                    key,
                    ComponentSpec::Divider(Divider { data_width }),
                ));
            }
        }
        ComponentSpec::Comparator(Comparator { mut data_width }) => {
            let mut changed = false;
            ui.add_enabled_ui(structural_ok, |ui| {
                changed |= labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
            });
            if changed {
                edit = Some(PropGuiAction::Reconfigure(
                    key,
                    ComponentSpec::Comparator(Comparator { data_width }),
                ));
            }
        }
        // A ROM's contents buffer is huge, so its spec is matched by
        // reference here (never cloned per-frame) - the whole reason the spec
        // match above borrows rather than owns. Widths are structural;
        // rom.resized() preserve-and-fits the contents into a fresh owned
        // buffer, and editing the contents is a value edit (live while Paused).
        ComponentSpec::Rom(
            rom @ Rom {
                mut data_width,
                mut address_width,
                ..
            },
        ) => {
            let mut changed = false;
            ui.add_enabled_ui(structural_ok, |ui| {
                changed |= labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                changed |= labeled_drag(
                    ui,
                    "Address width:",
                    &mut address_width,
                    1..=MAX_ADDRESS_WIDTH,
                );
                ui.label(format!("{} words", 1usize << address_width));
            });
            if changed {
                edit = Some(PropGuiAction::Reconfigure(
                    key,
                    ComponentSpec::Rom(rom.resized(data_width, address_width)),
                ));
            }
            ui.add_enabled_ui(value_ok, |ui| {
                if ui.button("Edit contents…").clicked() {
                    edit = Some(PropGuiAction::OpenMemory(key, MemKind::Rom));
                }
            });
        }
        // Same reasoning as Rom, above (huge contents buffer, matched by
        // reference); read behavior joins the widths as structural.
        ComponentSpec::Ram(
            ram @ Ram {
                mut data_width,
                mut address_width,
                mut read_behavior,
                ..
            },
        ) => {
            let mut changed = false;
            ui.add_enabled_ui(structural_ok, |ui| {
                changed |= labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                changed |= labeled_drag(
                    ui,
                    "Address width:",
                    &mut address_width,
                    1..=MAX_ADDRESS_WIDTH,
                );
                ui.label(format!("{} words", 1usize << address_width));
                ui.horizontal(|ui| {
                    ui.label("Read behavior:");
                    egui::ComboBox::from_id_salt(key)
                        .selected_text(format!("{read_behavior:?}"))
                        .show_ui(ui, |ui| {
                            for rb in [ReadBehavior::ReadAfterWrite, ReadBehavior::WriteAfterRead] {
                                changed |= ui
                                    .selectable_value(&mut read_behavior, rb, format!("{rb:?}"))
                                    .changed();
                            }
                        });
                });
            });
            if changed {
                let mut resized = ram.resized(data_width, address_width);
                resized.read_behavior = read_behavior;
                edit = Some(PropGuiAction::Reconfigure(key, ComponentSpec::Ram(resized)));
            }
            ui.add_enabled_ui(value_ok, |ui| {
                if ui.button("Edit contents…").clicked() {
                    edit = Some(PropGuiAction::OpenMemory(key, MemKind::Ram));
                }
            });

            let cur = doc.circuit.components[&comp_key].pins.out_cache[0];
            ui.label(format!("DO: {}", fmt_val(cur)));
        }
        ComponentSpec::Splitter {
            mut width,
            arm_bits,
            mut direction,
        } => {
            // let mut width = *width;
            let mut arm_bits = arm_bits.clone();
            let mut changed = false;
            ui.add_enabled_ui(structural_ok, |ui| {
                let before_dir = direction;
                ui.horizontal(|ui| {
                    ui.label("Fan Direction:");
                    ui.selectable_value(&mut direction, FanDirection::Right, "Split");
                    ui.selectable_value(&mut direction, FanDirection::Left, "Combine");
                });
                changed |= direction != before_dir;

                changed |= labeled_drag(ui, "Data width:", &mut width, 1..=32);
                let mut arms = arm_bits.len() as u8;
                changed |= labeled_drag(ui, "Arms:", &mut arms, 1..=16);

                // Apply width/arms bookkeeping before rendering bit rows below,
                // so a shrink is reflected the same frame; truncating drops
                // any bits assigned to a removed arm.
                arm_bits.resize_with(arms as usize, Vec::new);
                for list in &mut arm_bits {
                    list.retain(|&b| b < width);
                }

                for bit in 0..width {
                    let mut current_arm = arm_bits
                        .iter()
                        .position(|list| list.contains(&bit))
                        .map(|i| i as u8);
                    let before = current_arm;
                    ui.horizontal(|ui| {
                        ui.label(format!("Bit {bit}:"));
                        egui::ComboBox::from_id_salt((key, bit))
                            .selected_text(match current_arm {
                                Some(a) => format!("Arm {a}"),
                                None => "None".to_string(),
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut current_arm, None, "None");
                                for a in 0..arms {
                                    ui.selectable_value(
                                        &mut current_arm,
                                        Some(a),
                                        format!("Arm {a}"),
                                    );
                                }
                            });
                    });
                    if current_arm != before {
                        for list in &mut arm_bits {
                            list.retain(|&b| b != bit);
                        }
                        if let Some(a) = current_arm {
                            arm_bits[a as usize].push(bit);
                        }
                        changed = true;
                    }
                }
            });
            if changed {
                edit = Some(PropGuiAction::Reconfigure(
                    key,
                    ComponentSpec::Splitter {
                        width,
                        arm_bits,
                        direction,
                    },
                ));
            }
        }
        // Read-only: a subcircuit's interface is derived from the referenced
        // document, not edited here. Offer a jump to edit that document
        // (mirrors ROM's "Edit contents…" affordance); interface changes
        // are picked up on switch-back (refresh_subcircuits).
        ComponentSpec::Subcircuit {
            doc,
            name,
            input_widths,
            output_widths,
        } => {
            let doc = *doc;
            ui.label(format!("Circuit: {name}"));
            ui.label(format!(
                "{} input(s), {} output(s)",
                input_widths.len(),
                output_widths.len()
            ));
            // Navigating into the child circuit is a structural action
            // (it switches the active document): locked during a run.
            ui.add_enabled_ui(structural_ok, |ui| {
                if ui.button("Open circuit").clicked() {
                    edit = Some(PropGuiAction::OpenCircuit(doc));
                }
            });
        }
    }

    edit
}
