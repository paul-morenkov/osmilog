//! The signal viewer: a bottom panel that shows every placed Probe's current
//! value in a table and its waveform across clock ticks (a small chronogram).
//!
//! `SignalLog` is the recorded history; `Document::sample_probes` appends one
//! sample per clock tick. Both live on `Document` as runtime-only state - they
//! are never saved. History keys on `PlacedCompKey` (GUI-stable), so a probe's
//! waveform survives a rename.

use std::collections::HashMap;

use egui::{Align2, FontId, Rect, Sense, Stroke, Vec2};

use crate::gui::app::PlacedCompKey;
use crate::gui::canvas_draw::value_stroke;
use crate::gui::placed_component::PlacedComponent;
use crate::gui::theme::Theme;
use crate::sim::component::ComponentSpec;
use crate::sim::value::Value;

/// The number base a probe's value is shown in.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Radix {
    #[default]
    Hex,
    Binary,
    Unsigned,
    Signed,
}

impl Radix {
    fn label(self) -> &'static str {
        match self {
            Radix::Hex => "hex",
            Radix::Binary => "bin",
            Radix::Unsigned => "uint",
            Radix::Signed => "int",
        }
    }

    const ALL: [Radix; 4] = [Radix::Hex, Radix::Binary, Radix::Unsigned, Radix::Signed];
}

/// A value's text in the given radix. `Floating` shows as `Z`, `Invalid` as `X`.
pub fn format_value(v: Value, radix: Radix) -> String {
    match v {
        Value::Floating => "Z".to_string(),
        Value::Invalid => "X".to_string(),
        Value::Fixed { bits, width } => match radix {
            Radix::Hex => format!("0x{:X}", bits),
            Radix::Binary => format!("{:0width$b}", bits, width = width as usize),
            Radix::Unsigned => bits.to_string(),
            Radix::Signed => sign_extend(bits, width).to_string(),
        },
    }
}

// Reads `bits` as a two's-complement value of `width` bits.
fn sign_extend(bits: u32, width: u8) -> i64 {
    let w = width as u32;
    if w == 0 || w >= 32 {
        return bits as i32 as i64;
    }
    if bits & (1 << (w - 1)) != 0 {
        bits as i64 - (1i64 << w)
    } else {
        bits as i64
    }
}

/// One probe's recorded waveform, aligned to a shared tick axis. Runtime-only.
#[derive(Default)]
pub struct SignalLog {
    pub traces: HashMap<PlacedCompKey, Vec<Value>>,
    pub ticks: usize,
}

impl SignalLog {
    pub fn clear(&mut self) {
        self.traces.clear();
        self.ticks = 0;
    }

    /// Appends one sample per probe, then advances the tick axis. A probe first
    /// seen mid-run is back-filled with `Floating` so all traces stay aligned.
    pub fn record(&mut self, samples: &[(PlacedCompKey, Value)]) {
        for &(key, val) in samples {
            let trace = self.traces.entry(key).or_default();
            if trace.len() < self.ticks {
                trace.resize(self.ticks, Value::Floating);
            }
            trace.push(val);
        }
        self.ticks += 1;
    }
}

/// The viewer's own UI state: the panel-open flag and each probe's radix.
#[derive(Default)]
pub struct SignalViewer {
    pub open: bool,
    radix: HashMap<PlacedCompKey, Radix>,
}

// Width of the left control column (name + value + radix), in points.
const CONTROL_W: f32 = 220.0;
const ROW_H: f32 = 24.0;

impl SignalViewer {
    /// Draws the table and waveforms. Returns `true` if the user pressed Clear
    /// (the caller clears the log, since the log is a sibling field).
    pub fn show(
        &mut self,
        components: &HashMap<PlacedCompKey, PlacedComponent>,
        log: &SignalLog,
        theme: Theme,
        ui: &mut egui::Ui,
    ) -> bool {
        let mut clear = false;
        ui.horizontal(|ui| {
            ui.strong("Signal Viewer");
            if ui.button("Clear").clicked() {
                clear = true;
            }
            ui.weak(format!("{} ticks", log.ticks));
        });
        ui.separator();

        // Stable order: by name, then by key so equal names never reshuffle.
        let mut probes: Vec<(PlacedCompKey, &str)> = components
            .iter()
            .filter_map(|(&k, pc)| match &pc.spec {
                ComponentSpec::Probe(p) => Some((k, p.name.as_str())),
                _ => None,
            })
            .collect();
        probes.sort_by(|a, b| a.1.cmp(b.1).then(a.0 .0.cmp(&b.0 .0)));

        if probes.is_empty() {
            ui.weak("Place a Probe on a wire to watch its value here.");
            return clear;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            for (key, name) in probes {
                let radix = *self.radix.get(&key).unwrap_or(&Radix::default());
                let current = log
                    .traces
                    .get(&key)
                    .and_then(|t| t.last().copied())
                    .unwrap_or(Value::Floating);

                ui.horizontal(|ui| {
                    // Left: name, current value, radix selector.
                    ui.allocate_ui(Vec2::new(CONTROL_W, ROW_H), |ui| {
                        ui.horizontal(|ui| {
                            ui.set_min_width(CONTROL_W);
                            ui.strong(name);
                            ui.monospace(format_value(current, radix));
                            let mut r = radix;
                            egui::ComboBox::from_id_salt(("probe_radix", key.0))
                                .selected_text(r.label())
                                .width(52.0)
                                .show_ui(ui, |ui| {
                                    for opt in Radix::ALL {
                                        ui.selectable_value(&mut r, opt, opt.label());
                                    }
                                });
                            if r != radix {
                                self.radix.insert(key, r);
                            }
                        });
                    });

                    // Right: the waveform fills the remaining width.
                    let avail = ui.available_size_before_wrap();
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::new(avail.x.max(60.0), ROW_H), Sense::hover());
                    if let Some(trace) = log.traces.get(&key) {
                        draw_waveform(ui, rect, trace, radix, theme);
                    }
                });
            }
        });

        clear
    }
}

// Draws one probe's step waveform inside `rect`. Single-bit traces render as a
// square wave; multi-bit / floating / invalid traces render as a value band
// with the formatted text on each run of equal value.
fn draw_waveform(ui: &egui::Ui, rect: Rect, trace: &[Value], radix: Radix, theme: Theme) {
    let painter = ui.painter_at(rect);
    let n = trace.len();
    if n == 0 {
        return;
    }
    let dx = rect.width() / n as f32;
    let y_high = rect.top() + rect.height() * 0.2;
    let y_low = rect.bottom() - rect.height() * 0.2;
    let y_mid = rect.center().y;

    let is_bus = trace
        .iter()
        .any(|v| matches!(v, Value::Fixed { width, .. } if *width > 1));

    let mut prev_level: Option<f32> = None;
    let mut i = 0;
    while i < n {
        // Group a run of equal values so bus text and band draw once per run.
        let v = trace[i];
        let mut j = i + 1;
        while j < n && trace[j] == v {
            j += 1;
        }
        let x0 = rect.left() + i as f32 * dx;
        let x1 = rect.left() + j as f32 * dx;
        let stroke = Stroke::new(1.5, value_stroke(theme, v).color);

        if is_bus {
            // A band spanning the run, with the value text if it fits.
            let band = Rect::from_min_max(egui::pos2(x0, y_high), egui::pos2(x1, y_low));
            painter.rect_stroke(band, 0.0, stroke, egui::StrokeKind::Inside);
            let text = format_value(v, radix);
            if (x1 - x0) > text.len() as f32 * 7.0 {
                painter.text(
                    band.center(),
                    Align2::CENTER_CENTER,
                    text,
                    FontId::monospace(10.0),
                    theme.label_text,
                );
            }
            prev_level = None;
        } else {
            // Single-bit square wave.
            let y = match v {
                Value::Fixed { bits, .. } if bits != 0 => y_high,
                Value::Fixed { .. } => y_low,
                _ => y_mid,
            };
            if let Some(py) = prev_level {
                if py != y {
                    painter.line_segment([egui::pos2(x0, py), egui::pos2(x0, y)], stroke);
                }
            }
            painter.line_segment([egui::pos2(x0, y), egui::pos2(x1, y)], stroke);
            prev_level = Some(y);
        }
        i = j;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u64) -> PlacedCompKey {
        PlacedCompKey(n)
    }

    #[test]
    fn test_format_value_radices() {
        let v = Value::new(0xB, 4);
        assert_eq!(format_value(v, Radix::Hex), "0xB");
        assert_eq!(format_value(v, Radix::Binary), "1011");
        assert_eq!(format_value(v, Radix::Unsigned), "11");
        // 0b1011 as a signed 4-bit value is -5.
        assert_eq!(format_value(v, Radix::Signed), "-5");
        assert_eq!(format_value(Value::Floating, Radix::Hex), "Z");
        assert_eq!(format_value(Value::Invalid, Radix::Hex), "X");
    }

    #[test]
    fn test_log_records_and_aligns_late_probe() {
        let mut log = SignalLog::default();
        let a = key(1);
        let b = key(2);
        // Two ticks with only probe `a`.
        log.record(&[(a, Value::new(0, 1))]);
        log.record(&[(a, Value::new(1, 1))]);
        // Probe `b` appears on the third tick; its trace back-fills to align.
        log.record(&[(a, Value::new(0, 1)), (b, Value::new(7, 4))]);

        assert_eq!(log.ticks, 3);
        assert_eq!(log.traces[&a].len(), 3);
        assert_eq!(log.traces[&b].len(), 3);
        assert_eq!(log.traces[&b][0], Value::Floating);
        assert_eq!(log.traces[&b][2], Value::new(7, 4));

        log.clear();
        assert_eq!(log.ticks, 0);
        assert!(log.traces.is_empty());
    }
}
