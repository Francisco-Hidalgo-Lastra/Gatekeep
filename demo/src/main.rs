//! Gatekeep screenshot demo: the real UI with fictional data and no backend.
//! Nothing here touches the network, the keychain or the config directory.

mod app;
mod data;
mod theme;

fn main() -> eframe::Result {
    // `--light` / `--dark` force a theme; the default follows the system.
    let theme = std::env::args().skip(1).find_map(|a| match a.as_str() {
        "--light" => Some(eframe::egui::ThemePreference::Light),
        "--dark" => Some(eframe::egui::ThemePreference::Dark),
        _ => None,
    });
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Gatekeep")
            .with_app_id("gatekeep")
            .with_inner_size([1280.0, 780.0])
            .with_min_inner_size([600.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Gatekeep",
        options,
        Box::new(move |cc| Ok(Box::new(app::DemoApp::new(cc, theme)))),
    )
}
