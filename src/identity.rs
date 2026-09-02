use anyhow::{anyhow, Context, Result};
use snow::{params::NoiseParams, Builder};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

pub const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_SHA256";
pub const STATIC_KEY_SIZE: usize = 32;

#[derive(Clone)]
pub struct ServerIdentity {
    private_key: Arc<[u8]>,
}

impl ServerIdentity {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .with_context(|| format!("reading server identity key {}", path.display()))?;
        Self::from_private(bytes)
            .with_context(|| format!("loading server identity key {}", path.display()))
    }

    pub(crate) fn from_private(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() != STATIC_KEY_SIZE {
            return Err(anyhow!(
                "Noise server private key must be exactly {STATIC_KEY_SIZE} raw bytes"
            ));
        }
        Ok(Self {
            private_key: Arc::from(bytes.into_boxed_slice()),
        })
    }

    pub(crate) fn private_key(&self) -> &[u8] {
        &self.private_key
    }
}

pub fn generate(private_path: &Path, public_path: &Path) -> Result<()> {
    if private_path.exists() {
        return Err(anyhow!(
            "refusing to overwrite existing private identity key {}",
            private_path.display()
        ));
    }
    if public_path.exists() {
        return Err(anyhow!(
            "refusing to overwrite existing public identity key {}",
            public_path.display()
        ));
    }

    create_parent(private_path)?;
    create_parent(public_path)?;

    let builder = Builder::new(noise_params());
    let keypair = builder
        .generate_keypair()
        .map_err(|error| anyhow!("generating Noise server identity key: {error}"))?;
    let public_key = format!("{}\n", hex::encode(&keypair.public));

    write_new(private_path, &keypair.private)
        .with_context(|| format!("writing private identity key {}", private_path.display()))?;
    write_new(public_path, public_key.as_bytes())
        .with_context(|| format!("writing public identity key {}", public_path.display()))?;
    Ok(())
}

pub fn generate_token() -> Result<String> {
    let builder = Builder::new(noise_params());
    let keypair = builder
        .generate_keypair()
        .map_err(|error| anyhow!("generating token randomness: {error}"))?;
    Ok(hex::encode(keypair.private))
}

pub fn write_token(path: &Path, token: &str) -> Result<()> {
    create_parent(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o640);
    }
    let mut file = options.open(path)?;
    file.write_all(token.trim().as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

pub fn load_public_key(path: &Path) -> Result<[u8; STATIC_KEY_SIZE]> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading pinned server public key {}", path.display()))?;
    decode_public_key(text.trim())
        .with_context(|| format!("loading pinned server public key {}", path.display()))
}

pub fn decode_public_key(value: &str) -> Result<[u8; STATIC_KEY_SIZE]> {
    let bytes = hex::decode(value).context("decoding server public key as hex")?;
    bytes.try_into().map_err(|_| {
        anyhow!("server public key must be exactly {STATIC_KEY_SIZE} bytes encoded as hex")
    })
}

pub(crate) fn noise_params() -> NoiseParams {
    NOISE_PATTERN
        .parse()
        .expect("the built-in Noise pattern must be valid")
}

fn create_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating parent directory {}", parent.display()))?;
    }
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keypair_has_expected_sizes() {
        let keypair = Builder::new(noise_params()).generate_keypair().unwrap();
        assert_eq!(keypair.private.len(), STATIC_KEY_SIZE);
        assert_eq!(keypair.public.len(), STATIC_KEY_SIZE);
        assert!(decode_public_key(&hex::encode(&keypair.public)).is_ok());
    }
}
