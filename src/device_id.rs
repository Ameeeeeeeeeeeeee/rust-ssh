use anyhow::{anyhow, Result};
use snow::Builder;

const GENERATED_PREFIX: &str = "rssh-";
const GENERATED_RANDOM_BYTES: usize = 16;
pub const MAX_LENGTH: usize = 128;

/// Generate a stable-looking, non-secret device identifier from fresh OS randomness.
/// The random material is never persisted as a key; only the identifier is stored.
pub fn generate() -> Result<String> {
    let keypair = Builder::new(crate::identity::noise_params())
        .generate_keypair()
        .map_err(|error| anyhow!("generating device identity: {error}"))?;
    let random = keypair
        .private
        .get(..GENERATED_RANDOM_BYTES)
        .ok_or_else(|| anyhow!("generated device identity has an invalid random source"))?;
    Ok(format!("{GENERATED_PREFIX}{}", hex::encode(random)))
}

pub fn is_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub fn is_generated(value: &str) -> bool {
    let Some(random) = value.strip_prefix(GENERATED_PREFIX) else {
        return false;
    };
    random.len() == GENERATED_RANDOM_BYTES * 2
        && random.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_valid_and_distinct() {
        let first = generate().unwrap();
        let second = generate().unwrap();
        assert_ne!(first, second);
        assert!(is_valid(&first));
        assert!(is_generated(&first));
        assert!(is_valid(&second));
        assert!(is_generated(&second));
    }

    #[test]
    fn validation_rejects_unsafe_ids() {
        assert!(is_valid("WIN-CLIENT-01"));
        assert!(!is_valid("device/name"));
        assert!(!is_valid("device name"));
    }
}
