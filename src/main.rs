use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use rust_ssh::{agent, connect, identity, server};
use std::fs;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "rust-ssh", version, about = "SSH over a Noise-encrypted relay")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate the relay's long-lived Noise static identity key pair.
    Keygen(KeygenArgs),
    /// Run the public relay server.
    #[command(name = "relay", visible_alias = "server")]
    Relay(ServerArgs),
    /// Run the controlled-device agent.
    Agent(AgentArgs),
    /// List online controlled devices.
    List(ListArgs),
    /// Connect stdin/stdout to a controlled device; designed for SSH ProxyCommand.
    #[command(name = "controller", visible_alias = "connect")]
    Controller(ConnectArgs),
}

#[derive(Debug, Args)]
struct ServerArgs {
    /// TCP listen address, for example 0.0.0.0:24443.
    #[arg(long, env = "RUST_SSH_LISTEN", default_value = "0.0.0.0:24443")]
    listen: String,
    /// Raw 32-byte Noise static private key; keep this only on the VPS.
    #[arg(long, env = "RUST_SSH_IDENTITY_KEY")]
    identity_key: PathBuf,
    /// Shared bootstrap token. Prefer --token-file in production.
    #[arg(long, env = "RUST_SSH_TOKEN", conflicts_with = "token_file")]
    token: Option<String>,
    /// File containing the shared bootstrap token; surrounding whitespace is removed.
    #[arg(long, env = "RUST_SSH_TOKEN_FILE", conflicts_with = "token")]
    token_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct KeygenArgs {
    /// Output path for the raw 32-byte Noise private key.
    #[arg(long, env = "RUST_SSH_IDENTITY_KEY")]
    identity_key: PathBuf,
    /// Output path for the hex-encoded public key.
    #[arg(long, env = "RUST_SSH_IDENTITY_PUBLIC_KEY")]
    public_key: PathBuf,
}

#[derive(Debug, Args)]
struct ClientCommonArgs {
    /// Relay endpoint as an IP:port, for example 203.0.113.10:24443.
    #[arg(long, env = "RUST_SSH_SERVER")]
    server: String,
    /// Hex-encoded public key pinned to the relay's Noise identity.
    #[arg(long, env = "RUST_SSH_SERVER_KEY")]
    server_key: PathBuf,
    /// Shared bootstrap token. Prefer --token-file in production.
    #[arg(long, env = "RUST_SSH_TOKEN", conflicts_with = "token_file")]
    token: Option<String>,
    /// File containing the shared bootstrap token; surrounding whitespace is removed.
    #[arg(long, env = "RUST_SSH_TOKEN_FILE", conflicts_with = "token")]
    token_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct AgentArgs {
    #[command(flatten)]
    common: ClientCommonArgs,
    /// Stable name used by the controller to select this device.
    #[arg(long, env = "RUST_SSH_DEVICE_ID")]
    device_id: String,
    /// Local target reached by the agent. Keep this on loopback.
    #[arg(long, env = "RUST_SSH_TARGET", default_value = "127.0.0.1:22")]
    target: String,
}

#[derive(Debug, Args)]
struct ListArgs {
    #[command(flatten)]
    common: ClientCommonArgs,
}

#[derive(Debug, Args)]
struct ConnectArgs {
    #[command(flatten)]
    common: ClientCommonArgs,
    /// Device ID registered by the controlled agent.
    #[arg(long, env = "RUST_SSH_TARGET_DEVICE")]
    target: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    match Cli::parse().command {
        Command::Keygen(args) => identity::generate(&args.identity_key, &args.public_key),
        Command::Relay(args) => {
            server::run(server::Config {
                listen: args.listen,
                identity_key: args.identity_key,
                token: resolve_token(args.token, args.token_file)?,
            })
            .await
        }
        Command::Agent(args) => {
            agent::run(agent::Config {
                server: args.common.server,
                server_key: args.common.server_key,
                token: resolve_token(args.common.token, args.common.token_file)?,
                device_id: args.device_id,
                target: args.target,
            })
            .await
        }
        Command::List(args) => {
            let devices = connect::list_devices(connect::ListConfig {
                server: args.common.server,
                server_key: args.common.server_key,
                token: resolve_token(args.common.token, args.common.token_file)?,
            })
            .await?;
            for device in devices {
                println!("{}", device.device_id);
            }
            Ok(())
        }
        Command::Controller(args) => {
            connect::run(connect::Config {
                server: args.common.server,
                server_key: args.common.server_key,
                token: resolve_token(args.common.token, args.common.token_file)?,
                target: args.target,
            })
            .await
        }
    }
}

fn resolve_token(token: Option<String>, token_file: Option<PathBuf>) -> Result<String> {
    let value = match (token, token_file) {
        (Some(token), None) => token,
        (None, Some(path)) => fs::read_to_string(&path)
            .map_err(|error| anyhow::anyhow!("reading token file {}: {error}", path.display()))?,
        (None, None) => {
            return Err(anyhow::anyhow!(
                "a relay token is required; use --token or --token-file"
            ));
        }
        (Some(_), Some(_)) => unreachable!("clap enforces token argument conflicts"),
    };

    let value = value.trim().to_owned();
    if value.len() < 32 {
        return Err(anyhow::anyhow!(
            "relay token must contain at least 32 non-whitespace bytes"
        ));
    }
    if value.len() > 4096 {
        return Err(anyhow::anyhow!("relay token is too large"));
    }
    Ok(value)
}
