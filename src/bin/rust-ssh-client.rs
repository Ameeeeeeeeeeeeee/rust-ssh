#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(feature = "desktop")]
fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([620.0, 390.0])
            .with_min_inner_size([560.0, 340.0])
            .with_icon(rust_ssh::desktop::app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "rust-ssh client",
        options,
        Box::new(|creation_context| {
            Ok(Box::new(rust_ssh::desktop::ClientApp::new(
                creation_context,
            )))
        }),
    )
}

#[cfg(not(feature = "desktop"))]
fn main() {
    eprintln!(
        "rust-ssh-client requires: cargo run --release --features desktop --bin rust-ssh-client"
    );
}
