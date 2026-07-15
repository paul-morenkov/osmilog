//! Small shared types with no other natural home - kept separate from
//! `geometry.rs` (grid/pixel geometry constants and shape builders).

use egui::Painter;

use crate::gui::geometry::Camera;
use crate::gui::theme::Theme;

// Ambient egui/render handles used by the canvas-interaction dispatch and its
// per-mode methods (`OsmilogApp::handle_canvas_interaction` and its
// `interact_*` methods, some on `OsmilogApp`, some on `Document`). Built fresh
// each frame in `OsmilogApp::ui`, never stored - just a bundle of the 5 values
// that would otherwise be repeated as individual parameters everywhere.
pub(crate) struct CanvasCtx<'a> {
    pub(crate) response: &'a egui::Response,
    pub(crate) painter: &'a Painter,
    pub(crate) ctx: &'a egui::Context,
    pub(crate) camera: Camera,
    pub(crate) theme: Theme,
}
