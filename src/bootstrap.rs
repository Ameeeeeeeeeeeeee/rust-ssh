use crate::identity;
use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::SocketAddr;
use std::path::Path;

pub const CODE_PREFIX: &str = "rssh1:";
const MIN_TOKEN_BYTES: usize = 32;
const MAX_TOKEN_BYTES: usize = 4096;

#[derive(Debug, Clone)]
pub struct Config {
    pub server: String,
    pub server_key: [u8; identity::STATIC_KEY_SIZE],
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct WireConfig {
    server: String,
    server_key: String,
    token: String,
}

pub fn create_code(server: &str, public_key_path: &Path, token_path: &Path) -> Result<String> {
    let server_key = identity::load_public_key(public_key_path)?;
    let token = fs::read_to_string(token_path)
        .with_context(|| format!("reading relay token {}", token_path.display()))?;
    encode(Config {
        server: server.to_owned(),
        server_key,
        token: token.trim().to_owned(),
    })
}

pub fn encode(config: Config) -> Result<String> {
    let server = validate_server(&config.server)?;
    let token = validate_token(&config.token)?;
    let payload = serde_json::to_vec(&WireConfig {
        server,
        server_key: hex::encode(config.server_key),
        token,
    })
    .context("encoding rust-ssh pairing code")?;
    Ok(format!("{CODE_PREFIX}{}", URL_SAFE_NO_PAD.encode(payload)))
}

pub fn decode(code: &str) -> Result<Config> {
    let compact: String = code.split_whitespace().collect();
    let payload = compact
        .strip_prefix(CODE_PREFIX)
        .ok_or_else(|| anyhow!("配对码格式不正确，应以 {CODE_PREFIX} 开头"))?;
    let payload = URL_SAFE_NO_PAD
        .decode(payload)
        .context("decoding rust-ssh pairing code")?;
    let wire: WireConfig = serde_json::from_slice(&payload).context("reading pairing code")?;
    Ok(Config {
        server: validate_server(&wire.server)?,
        server_key: identity::decode_public_key(&wire.server_key)?,
        token: validate_token(&wire.token)?,
    })
}

fn validate_server(server: &str) -> Result<String> {
    let server = server.trim();
    let address: SocketAddr = server
        .parse()
        .map_err(|_| anyhow!("服务器地址必须是公网 IP:端口，例如 203.0.113.10:24443"))?;
    if address.port() == 0 || address.ip().is_unspecified() {
        return Err(anyhow!("服务器地址的 IP 或端口无效"));
    }
    Ok(server.to_owned())
}

fn validate_token(token: &str) -> Result<String> {
    let token = token.trim();
    if token.len() < MIN_TOKEN_BYTES {
        return Err(anyhow!(
            "relay token 至少需要 {MIN_TOKEN_BYTES} 个非空白字节"
        ));
    }
    if token.len() > MAX_TOKEN_BYTES {
        return Err(anyhow!("relay token 超过 {MAX_TOKEN_BYTES} 字节"));
    }
    Ok(token.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_round_trip() {
        let original = Config {
            server: "203.0.113.10:24443".to_owned(),
            server_key: [7_u8; identity::STATIC_KEY_SIZE],
            token: "a".repeat(MIN_TOKEN_BYTES),
        };
        let encoded = encode(original.clone()).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.server, original.server);
        assert_eq!(decoded.server_key, original.server_key);
        assert_eq!(decoded.token, original.token);
    }

    #[test]
    fn code_rejects_domain_name() {
        let result = encode(Config {
            server: "relay.example.com:24443".to_owned(),
            server_key: [7_u8; identity::STATIC_KEY_SIZE],
            token: "a".repeat(MIN_TOKEN_BYTES),
        });
        assert!(result.is_err());
    }
}
