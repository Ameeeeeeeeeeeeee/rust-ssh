#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(feature = "desktop")]
fn main() -> eframe::Result<()> {
    #[cfg(windows)]
    rust_ssh::desktop::set_windows_app_user_model_id("Rust-SSH.Client");
    #[cfg(windows)]
    let Some(_instance) = rust_ssh::desktop::SingleInstance::acquire("Local\\Rust-SSH-Client") else {
        return Ok(());
    };

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([620.0, 420.0])
            .with_min_inner_size([560.0, 370.0])
            .with_icon(rust_ssh::desktop::app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "Rust-SSH-Client",
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
