fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("osmilog")
            .with_inner_size([1200.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "osmilog",
        options,
        Box::new(|cc| Ok(Box::new(osmilog::app::OsmilogApp::new(cc)))),
    )
}
