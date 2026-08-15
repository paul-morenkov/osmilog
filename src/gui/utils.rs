//! Small shared types with no other natural home - kept separate from
//! `geometry.rs` (grid/pixel geometry constants and shape builders).

use egui::Painter;

use crate::gui::geometry::Camera;
use crate::gui::theme::Theme;

// Built fresh each frame in `OsmilogApp::ui`, never stored - bundles values that
// would otherwise be repeated as individual parameters across interact_* methods.
pub(crate) struct CanvasCtx<'a> {
    pub(crate) response: &'a egui::Response,
    pub(crate) painter: &'a Painter,
    pub(crate) ctx: &'a egui::Context,
    pub(crate) camera: Camera,
    pub(crate) theme: Theme,
}
