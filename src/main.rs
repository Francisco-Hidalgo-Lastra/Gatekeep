mod app;
mod auth;
mod config;
mod filter;
mod mail;
mod oauth;
mod model;
mod rules;
mod theme;
mod worker;

fn main() -> eframe::Result {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Gatekeep")
            .with_app_id("gatekeep")
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([600.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Gatekeep",
        options,
        Box::new(|cc| Ok(Box::new(app::GatekeepApp::new(cc)))),
    )
}
