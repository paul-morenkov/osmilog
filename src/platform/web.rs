// Web (WASM/browser) I/O backend: an async file-picker, a Blob-based download,
// and an in-app "Save As" modal (browsers have no native save picker to name a
// download). Mirrors the native backend's `IoState` interface exactly - see
// platform/native.rs and platform.rs for the swap. Both stay OsmilogApp-driven
// but self-contained: they own the whole Save/Load orchestration so the GUI
// call sites are cfg-free one-liners.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use web_sys::{Blob, BlobPropertyBag, HtmlAnchorElement, Url};

use crate::gui::app::OsmilogApp;
use crate::io::{ProjectFile, CIRCUIT_FILE_EXT};

// The browser has no synchronous file dialogs, so a spawned load task delivers
// its outcome into this shared slot; `poll_pending_load` drains it on a later
// frame.
type PendingLoad = Rc<RefCell<Option<Result<ProjectFile, String>>>>;

// Web-only IO state: the async-load delivery slot plus the in-app "Save As"
// modal's contents. native::IoState is a ZST with this same method surface -
// there, poll/drive are no-ops.
pub struct IoState {
    pending_load: PendingLoad,
    // Some(name) while the "Save As" modal is open, holding the text field's
    // current contents; None when closed.
    save_as_dialog: Option<String>,
}

impl Default for IoState {
    fn default() -> Self {
        Self {
            pending_load: Rc::new(RefCell::new(None)),
            save_as_dialog: None,
        }
    }
}

impl IoState {
    // File > Save: the browser can't name a download from a native picker, so
    // open our own filename modal instead of downloading immediately (see
    // `drive_save_dialog`, which completes it on a later frame).
    pub fn request_save(&mut self, _app: &mut OsmilogApp) {
        self.save_as_dialog = Some("circuit".to_string());
    }

    // File > Load: kicks off the async pick + read; the result lands in
    // `pending_load` and is installed by `poll_pending_load` on a later frame.
    pub fn request_load(&mut self, _app: &mut OsmilogApp) {
        let slot = self.pending_load.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let outcome = async {
                let handle = rfd::AsyncFileDialog::new()
                    .add_filter("osmilog circuit", &[CIRCUIT_FILE_EXT])
                    .pick_file()
                    .await
                    .ok_or_else(|| "no file selected".to_string())?;
                let bytes = handle.read().await;
                let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
                let file = ProjectFile::from_json(&text).map_err(|e| e.to_string())?;
                file.validate().map_err(|e| e.to_string())?;
                Ok(file)
            }
            .await;
            *slot.borrow_mut() = Some(outcome);
        });
    }

    // Installs a File > Load result a spawned task has delivered, if any is
    // waiting. No-op most frames.
    pub fn poll_pending_load(&mut self, app: &mut OsmilogApp) {
        let Some(outcome) = self.pending_load.borrow_mut().take() else {
            return;
        };
        match outcome {
            Ok(file) => {
                if let Err(e) = app.load_project_file(&file) {
                    app.last_settle_error = Some(format!("load failed: {e}"));
                }
            }
            Err(e) => app.last_settle_error = Some(format!("load failed: {e}")),
        }
    }

    // Draws the filename modal opened by `request_save`; on confirm, serializes
    // and triggers the browser download. No-op unless it's currently open.
    pub fn drive_save_dialog(&mut self, ctx: &egui::Context, app: &mut OsmilogApp) {
        let Some(name) = &mut self.save_as_dialog else {
            return;
        };
        let mut open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        egui::Window::new("Save As")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let resp = ui.text_edit_singleline(name);
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        confirmed = true;
                    }
                    ui.label(format!(".{CIRCUIT_FILE_EXT}"));
                });
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        confirmed = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                });
            });

        if confirmed {
            let filename = save_filename(name);
            match app.to_project_file().to_json() {
                Ok(json) => trigger_download(&json, &filename),
                Err(e) => app.last_settle_error = Some(format!("save failed: {e}")),
            }
        }
        if !open || confirmed || cancelled {
            self.save_as_dialog = None;
        }
    }
}

// The browser canvas has no process to exit on a window-close request; the
// native counterpart calls `std::process::exit`.
pub fn quit() {}

// Turns a user-typed base name (from the Save As modal) into a well-formed
// download filename: trims whitespace, falls back to "circuit" if empty, and
// appends the `.osm` extension unless the user already typed it.
fn save_filename(base: &str) -> String {
    let base = base.trim();
    let base = if base.is_empty() { "circuit" } else { base };
    let suffix = format!(".{CIRCUIT_FILE_EXT}");
    if base.ends_with(&suffix) {
        base.to_string()
    } else {
        format!("{base}{suffix}")
    }
}

// Triggers a browser download via Blob + object URL + synthetic `<a download>`
// click, rather than the File System Access API's save picker (which `rfd`'s
// wasm backend uses) - that API is Chromium-only. `filename` is used as-is (see
// `save_filename` for turning a user-typed base name into a well-formed one).
fn trigger_download(contents: &str, filename: &str) {
    let window = web_sys::window().expect("no window");
    let document = window.document().expect("no document");

    let parts = js_sys::Array::new();
    parts.push(&wasm_bindgen::JsValue::from_str(contents));
    let opts = BlobPropertyBag::new();
    opts.set_type("application/json");
    let blob =
        Blob::new_with_str_sequence_and_options(&parts, &opts).expect("failed to build blob");
    let url = Url::create_object_url_with_blob(&blob).expect("failed to create object url");

    let anchor: HtmlAnchorElement = document
        .create_element("a")
        .expect("failed to create anchor")
        .dyn_into()
        .expect("created element is not an anchor");
    anchor.set_href(&url);
    anchor.set_download(filename);

    // Firefox requires the anchor be attached to the document for a synthetic
    // click to trigger a download; attach, click, detach.
    let body = document.body().expect("document has no body");
    body.append_child(&anchor).expect("failed to attach anchor");
    anchor.click();
    body.remove_child(&anchor).expect("failed to detach anchor");
    // Object URL intentionally left un-revoked: a one-off leaked blob URL per
    // Save click is harmless for an interactive session, and revoking
    // immediately risks racing the browser's download start.
}
