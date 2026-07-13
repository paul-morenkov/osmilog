// Native (desktop) I/O backend: blocking `rfd` file dialogs and a real process
// exit. The web counterpart (platform/web.rs) mirrors this exact interface with
// async browser APIs; see platform.rs for how the two are swapped.

use crate::gui::app::OsmilogApp;
use crate::io::{ProjectFile, CIRCUIT_FILE_EXT};

// Native dialogs are synchronous, so there's no cross-frame IO state to hold -
// this is a zero-sized placeholder that mirrors web::IoState's method surface.
// `poll_pending_load`/`drive_save_dialog` are no-ops here because nothing async
// is ever left to finish on a later frame.
// TODO: Figure out how to stop the clippy warning of this having a Default impl, since the web
// version requires it.
#[derive(Default)]
pub struct IoState;

impl IoState {
    // File > Save: opens the OS "Save As" dialog (which carries its own
    // filename field) and writes the serialized circuit. Cancelling is a
    // silent no-op; a serialize/write failure surfaces in the menu-bar status.
    pub fn request_save(&mut self, app: &mut OsmilogApp) {
        let json = match app.to_project_file().to_json() {
            Ok(json) => json,
            Err(e) => {
                app.last_settle_error = Some(format!("save failed: {e}"));
                return;
            }
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("osmilog circuit", &[CIRCUIT_FILE_EXT])
            .set_file_name(format!("circuit.{CIRCUIT_FILE_EXT}"))
            .save_file()
        else {
            return; // user cancelled
        };
        if let Err(e) = std::fs::write(path, json) {
            app.last_settle_error = Some(format!("save failed: {e}"));
        }
    }

    // File > Load: opens the OS "Open" dialog, then reads + parses + validates
    // and installs the chosen file. Synchronous, so nothing is left for
    // `poll_pending_load` to finish.
    pub fn request_load(&mut self, app: &mut OsmilogApp) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("osmilog circuit", &[CIRCUIT_FILE_EXT])
            .pick_file()
        else {
            return; // user cancelled
        };
        let loaded = (|| {
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let file = ProjectFile::from_json(&text).map_err(|e| e.to_string())?;
            file.validate().map_err(|e| e.to_string())?;
            Ok::<_, String>(file)
        })();
        match loaded {
            Ok(file) => {
                if let Err(e) = app.load_project_file(&file) {
                    app.last_settle_error = Some(format!("load failed: {e}"));
                }
            }
            Err(e) => app.last_settle_error = Some(format!("load failed: {e}")),
        }
    }

    // No async load to complete on native (`request_load` is synchronous).
    pub fn poll_pending_load(&mut self, _app: &mut OsmilogApp) {}

    // No in-app save modal on native - the OS "Save As" dialog in
    // `request_save` names the file itself.
    pub fn drive_save_dialog(&mut self, _ctx: &egui::Context, _app: &mut OsmilogApp) {}
}

// Ends the process on a window-close request. The web build has no process to
// exit (the canvas simply stops), so its counterpart is a no-op.
pub fn quit() {
    std::process::exit(0);
}
