use postern::ui::{app::PosternApp, theme};
use std::sync::Arc;

fn main() -> eframe::Result {
    // Window size and column widths are remembered in the application data directory.
    let persistence_path = postern::services::storage::Store::directory()
        .ok()
        .map(|directory| directory.join("window_state.ron"));
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([1400.0, 880.0])
        .with_min_inner_size([1000.0, 660.0])
        .with_title("Postern");
    if let Some(icon) = theme::app_icon() {
        viewport = viewport.with_icon(Arc::new(icon));
    }
    let options = eframe::NativeOptions {
        viewport,
        persistence_path,
        ..Default::default()
    };
    eframe::run_native(
        "Postern",
        options,
        Box::new(|cc| Ok(Box::new(PosternApp::new(cc)?))),
    )
}
