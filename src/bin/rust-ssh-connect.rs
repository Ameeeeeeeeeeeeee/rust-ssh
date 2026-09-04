#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(feature = "desktop")]
use anyhow::{anyhow, Context, Result};
#[cfg(feature = "desktop")]
use clap::Parser;
#[cfg(feature = "desktop")]
use std::path::PathBuf;
#[cfg(feature = "desktop")]
use std::process::ExitCode;
#[cfg(feature = "desktop")]
use tokio::runtime::Runtime;

#[cfg(feature = "desktop")]
#[derive(Debug, Parser)]
#[command(name = "Rust-SSH-Connect")]
struct ProxyArgs {
    /// Internal stdin/stdout mode used by OpenSSH ProxyCommand.
    #[arg(long)]
    proxy: bool,
    /// Pairing code file used by the GUI-generated SSH configuration.
    #[arg(long)]
    setup_code_file: Option<PathBuf>,
    /// Legacy/manual relay endpoint.
    #[arg(long)]
    server: Option<String>,
    /// Legacy/manual pinned server public key file.
    #[arg(long)]
    server_key: Option<PathBuf>,
    /// Legacy/manual controller token file.
    #[arg(long)]
    token_file: Option<PathBuf>,
    #[arg(long)]
    target: Option<String>,
}

#[cfg(feature = "desktop")]
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::from(1)
        }
    }
}

#[cfg(feature = "desktop")]
fn run() -> Result<()> {
    if std::env::args().any(|argument| argument == "--proxy") {
        let args = ProxyArgs::parse();
        let target = args.target.context("ProxyCommand 缺少 --target")?;
        let (server, server_key, token) = if let Some(setup_code_file) = args.setup_code_file {
            let code = std::fs::read_to_string(&setup_code_file)
                .with_context(|| format!("读取配置码文件 {}", setup_code_file.display()))?;
            let pairing = rust_ssh::bootstrap::decode(&code)?;
            (pairing.server, pairing.server_key, pairing.token)
        } else {
            let server = args.server.context("ProxyCommand 缺少 --server")?;
            let server_key_path = args.server_key.context("ProxyCommand 缺少 --server-key")?;
            let server_key = rust_ssh::identity::load_public_key(&server_key_path)?;
            let token_file = args.token_file.context("ProxyCommand 缺少 --token-file")?;
            let token = std::fs::read_to_string(&token_file)
                .with_context(|| format!("读取 token 文件 {}", token_file.display()))?
                .trim()
                .to_owned();
            (server, server_key, token)
        };
        let runtime = Runtime::new().context("创建 Tokio runtime")?;
        runtime.block_on(rust_ssh::connect::run(rust_ssh::connect::Config {
            server,
            server_key,
            token,
            target,
        }))?;
        return Ok(());
    }

    #[cfg(windows)]
    rust_ssh::desktop::set_windows_app_user_model_id("Rust-SSH.Connect");
    #[cfg(windows)]
    let Some(_instance) = rust_ssh::desktop::SingleInstance::acquire("Local\\Rust-SSH-Connect") else {
        return Ok(());
    };

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([650.0, 500.0])
            .with_min_inner_size([580.0, 440.0])
            .with_icon(rust_ssh::desktop::app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "Rust-SSH-Connect",
        options,
        Box::new(|creation_context| {
            Ok(Box::new(rust_ssh::desktop::ConnectApp::new(
                creation_context,
            )))
        }),
    )
    .map_err(|error| anyhow!("运行 Rust-SSH-Connect GUI: {error:?}"))
}

#[cfg(not(feature = "desktop"))]
fn main() {
    eprintln!(
        "rust-ssh-connect requires: cargo run --release --features desktop --bin rust-ssh-connect"
    );
}
