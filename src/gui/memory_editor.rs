//! The ROM/RAM contents editor windows and their open-state.
//!
//! `MemoryEditor` owns which memory editor windows are open (a ROM and/or a RAM
//! can be open at once) and draws them; it reads a placed component's contents
//! read-only (`&components`) and returns the edits the user made as a `Vec` for
//! the app to apply through its `write_rom_cell`/`write_ram_cell` (which need
//! `&mut Circuit` + settle and stay on the app). ROM and RAM share one hex-dump
//! layout, differing only in which spec variant supplies the dimensions/words -
//! captured by `MemKind`.

use std::collections::HashMap;

use crate::gui::app::PlacedCompKey;
use crate::gui::placed_component::PlacedComponent;
use crate::sim::component::ComponentSpec;

const WORDS_PER_ROW: usize = 8;

/// Which kind of memory a window is editing. Selects the spec variant to read
/// (and, for the app applying edits, which write path to use).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MemKind {
    Rom,
    Ram,
}

impl MemKind {
    fn title(self) -> &'static str {
        match self {
            MemKind::Rom => "ROM contents",
            MemKind::Ram => "RAM contents",
        }
    }

    /// `(data_width, len)` for this memory kind, or `None` if `spec` isn't that
    /// kind (component deleted / reconfigured to another type -> close window).
    fn dims(self, spec: &ComponentSpec) -> Option<(u8, usize)> {
        match (self, spec) {
            (MemKind::Rom, ComponentSpec::Rom(r)) => Some((r.data_width, r.len())),
            (MemKind::Ram, ComponentSpec::Ram(r)) => Some((r.data_width, r.len())),
            _ => None,
        }
    }

    fn word(self, spec: &ComponentSpec, i: usize) -> u32 {
        match (self, spec) {
            (MemKind::Rom, ComponentSpec::Rom(r)) => r.word(i),
            (MemKind::Ram, ComponentSpec::Ram(r)) => r.word(i),
            _ => 0,
        }
    }
}

/// One word edit made in an editor window, for the app to apply.
pub struct MemEdit {
    pub pc: PlacedCompKey,
    pub kind: MemKind,
    pub index: usize,
    pub value: u32,
}

/// Open-state of the memory contents editor windows. A ROM and a RAM window can
/// each be open independently.
#[derive(Default)]
pub struct MemoryEditor {
    pub(crate) rom_open: Option<PlacedCompKey>,
    pub(crate) ram_open: Option<PlacedCompKey>,
}

impl MemoryEditor {
    /// Opens the editor for `pc` of the given kind (from the properties panel).
    pub(crate) fn open(&mut self, pc: PlacedCompKey, kind: MemKind) {
        match kind {
            MemKind::Rom => self.rom_open = Some(pc),
            MemKind::Ram => self.ram_open = Some(pc),
        }
    }

    /// Draws whichever windows are open and returns the word edits the user
    /// made. Closes a window whose component is gone or is no longer the right
    /// memory kind. `value_locked` dims (but keeps visible) the fields while a
    /// clock run is actively Playing.
    pub(crate) fn show(
        &mut self,
        ctx: &egui::Context,
        components: &HashMap<PlacedCompKey, PlacedComponent>,
        value_locked: bool,
    ) -> Vec<MemEdit> {
        let mut edits = Vec::new();
        if let Some(pc) = self.rom_open {
            if !show_window(ctx, pc, MemKind::Rom, components, value_locked, &mut edits) {
                self.rom_open = None;
            }
        }
        if let Some(pc) = self.ram_open {
            if !show_window(ctx, pc, MemKind::Ram, components, value_locked, &mut edits) {
                self.ram_open = None;
            }
        }
        edits
    }
}

// Draws one memory contents editor window (hex-dump layout: one row per
// WORDS_PER_ROW words, a base-address label, then a hex DragValue per word). The
// row list is virtualized (show_rows) so a 2^24-word memory only builds the
// handful of rows actually on screen. Collected edits are pushed onto `edits`.
// Returns whether the window should stay open (false = close: the ✕ was hit or
// the component went away / changed type).
fn show_window(
    ctx: &egui::Context,
    pc: PlacedCompKey,
    kind: MemKind,
    components: &HashMap<PlacedCompKey, PlacedComponent>,
    value_locked: bool,
    edits: &mut Vec<MemEdit>,
) -> bool {
    // Close if the component was deleted or undone away (or the key now names
    // something else after a reconfigure to a different type).
    let dims = match components.get(&pc) {
        Some(c) => kind.dims(&c.spec),
        _ => None,
    };
    let Some((data_width, len)) = dims else {
        return false;
    };

    let word_nibbles = data_width.div_ceil(4) as usize;
    let addr_nibbles = (usize::BITS - (len.max(1) - 1).leading_zeros())
        .div_ceil(4)
        .max(1) as usize;
    let mask = if data_width >= 32 {
        u32::MAX
    } else {
        (1u32 << data_width) - 1
    };
    let total_rows = len.div_ceil(WORDS_PER_ROW);

    // Contents are a value edit: pokeable while Paused, frozen while Playing.
    // The window can only be *opened* while value edits are allowed, but one
    // left open from Stopped/Paused survives into Play, so gate the fields (they
    // stay visible for observation, just dimmed).
    let values_enabled = !value_locked;
    let mut open = true;
    egui::Window::new(kind.title())
        .open(&mut open)
        .default_size([440.0, 480.0])
        .resizable(true)
        .show(ctx, |ui| {
            // DragValue renders with `drag_value_text_style` (defaults to the
            // proportional Button style), so its digit widths vary and columns
            // drift out of alignment across rows. Force it monospace to keep the
            // hex grid aligned; scoped to this Ui so it doesn't affect
            // DragValues elsewhere in the app.
            ui.style_mut().drag_value_text_style = egui::TextStyle::Monospace;
            let row_height = ui.spacing().interact_size.y;
            ui.add_enabled_ui(values_enabled, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_rows(ui, row_height, total_rows, |ui, range| {
                        for row in range {
                            let base = row * WORDS_PER_ROW;
                            ui.horizontal(|ui| {
                                ui.monospace(format!("0x{base:0addr_nibbles$X}:"));
                                for col in 0..WORDS_PER_ROW {
                                    let i = base + col;
                                    if i >= len {
                                        break;
                                    }
                                    let mut val = kind.word(&components[&pc].spec, i);
                                    let resp = ui.add(
                                        egui::DragValue::new(&mut val)
                                            .range(0..=mask)
                                            .hexadecimal(word_nibbles, false, true),
                                    );
                                    if resp.changed() {
                                        edits.push(MemEdit {
                                            pc,
                                            kind,
                                            index: i,
                                            value: val,
                                        });
                                    }
                                }
                            });
                        }
                    });
            });
        });

    open
}
