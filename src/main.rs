use osmilog::gui::app::OsmilogApp;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!(
        "../assets/osmilog_icons/osmilog_256.png"
    ))
    .expect("valid icon png");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("osmilog v{}", env!("CARGO_PKG_VERSION")))
            .with_inner_size([1200.0, 800.0])
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native(
        "osmilog",
        options,
        Box::new(|cc| Ok(Box::new(OsmilogApp::new(cc)))),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(target_arch = "wasm32")]
mod wasm_entry {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen(start)]
    pub fn start() {
        wasm_bindgen_futures::spawn_local(async {
            let canvas = web_sys::window()
                .expect("no window")
                .document()
                .expect("no document")
                .get_element_by_id("the_canvas_id")
                .expect("canvas #the_canvas_id not found")
                .dyn_into::<web_sys::HtmlCanvasElement>()
                .expect("element is not a canvas");

            eframe::WebRunner::new()
                .start(
                    canvas,
                    eframe::WebOptions::default(),
                    Box::new(|cc| Ok(Box::new(osmilog::gui::app::OsmilogApp::new(cc)))),
                )
                .await
                .expect("failed to start eframe");
        });
    }
}
