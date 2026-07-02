use egui::{Color32, Visuals};

// Canvas colors derived from the ambient egui theme so light/dark switches
// (including live OS theme changes) are picked up automatically. Recompute
// once per frame via `Theme::from_visuals` rather than caching - it's just a
// handful of Color32 copies.
#[derive(Clone, Copy)]
pub struct Theme {
    pub canvas_bg: Color32,
    pub grid_dot: Color32,
    pub component_fill: Color32,
    pub tunnel_fill: Color32,
    pub outline_default: Color32,
    pub outline_selected: Color32,
    pub label_text: Color32,
    pub error_text: Color32,
    pub ghost_preview: Color32,
    pub wire_drag_preview: Color32,
    pub value_floating: Color32,
    // Signal colors are fixed rather than theme-derived: they encode circuit
    // data (logic low/high), not UI chrome, so they stay constant across
    // light/dark. Kept here anyway so every color decision lives in one place.
    pub value_low: Color32,
    pub value_high: Color32,
}

impl Theme {
    pub fn from_visuals(visuals: &Visuals) -> Self {
        Theme {
            canvas_bg: visuals.extreme_bg_color,
            grid_dot: visuals.widgets.noninteractive.bg_stroke.color,
            component_fill: visuals.widgets.inactive.bg_fill,
            tunnel_fill: visuals.widgets.open.bg_fill,
            outline_default: visuals.widgets.hovered.bg_stroke.color,
            outline_selected: visuals.selection.stroke.color,
            label_text: visuals.text_color(),
            error_text: visuals.error_fg_color,
            ghost_preview: visuals.weak_text_color(),
            wire_drag_preview: visuals.widgets.active.fg_stroke.color,
            value_floating: visuals.widgets.noninteractive.fg_stroke.color,
            value_low: Color32::from_rgb(40, 40, 80),
            value_high: Color32::from_rgb(50, 200, 80),
        }
    }
}
