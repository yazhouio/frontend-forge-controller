use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use snafu::{ResultExt, Snafu};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MANAGED_BY_VALUE: &str = "frontend-forge-builder-controller";
pub const LABEL_MANAGED_BY: &str = "frontend-forge.io/managed-by";
pub const LABEL_FI_NAME: &str = "frontend-forge.io/fi-name";
pub const LABEL_SPEC_HASH: &str = "frontend-forge.io/spec-hash";
pub const LABEL_MANIFEST_HASH: &str = "frontend-forge.io/manifest-hash";
pub const LABEL_BUILD_KIND: &str = "frontend-forge.io/build-kind";
pub const ANNO_BUILD_JOB: &str = "frontend-forge.io/build-job";
pub const ANNO_OBSERVED_GENERATION: &str = "frontend-forge.io/observed-generation";
pub const BUILD_KIND_VALUE: &str = "frontend-forge";
pub const DEFAULT_MANIFEST_FILENAME: &str = "manifest.json";
pub const DEFAULT_MANIFEST_MOUNT_PATH: &str = "/work/manifest/manifest.json";
pub const MAX_SECRET_PAYLOAD_BYTES: usize = 1_000_000;

#[derive(Debug, Snafu)]
pub enum CommonError {
    #[snafu(display("manifest serialization failed: {source}"))]
    Serialize { source: serde_json::Error },
}

pub fn canonicalize_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), canonicalize_json(v)))
                .collect();
            let mut out = Map::new();
            for (k, v) in sorted {
                out.insert(k, v);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_json).collect()),
        _ => value.clone(),
    }
}

pub fn canonical_json_string(value: &Value) -> Result<String, CommonError> {
    serde_json::to_string(&canonicalize_json(value)).context(SerializeSnafu)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub fn manifest_hash_from_content(content: &str) -> String {
    format!("sha256:{}", sha256_hex(content.as_bytes()))
}

pub fn manifest_content_and_hash(source: &Value) -> Result<(String, String), CommonError> {
    let content = canonical_json_string(source)?;
    let hash = manifest_hash_from_content(&content);
    Ok((content, hash))
}

pub fn serializable_content_and_hash<T>(source: &T) -> Result<(String, String), CommonError>
where
    T: Serialize,
{
    let value = serde_json::to_value(source).context(SerializeSnafu)?;
    manifest_content_and_hash(&value)
}

pub fn serializable_hash<T>(source: &T) -> Result<String, CommonError>
where
    T: Serialize,
{
    let (_, hash) = serializable_content_and_hash(source)?;
    Ok(hash)
}

pub fn hash_short(hash: &str) -> String {
    let trimmed = hash.strip_prefix("sha256:").unwrap_or(hash);
    trimmed.chars().take(8).collect()
}

pub fn default_bundle_name(fi_name: &str) -> String {
    bounded_name(&format!("fi-{}", fi_name), 63)
}

pub fn job_name(fi_name: &str, manifest_hash: &str, nonce: &str) -> String {
    bounded_name(
        &format!(
            "fi-{}-build-{}-{}",
            fi_name,
            hash_short(manifest_hash),
            nonce
        ),
        63,
    )
}

pub fn secret_name(fi_name: &str, manifest_hash: &str, nonce: &str) -> String {
    bounded_name(
        &format!("fi-{}-mf-{}-{}", fi_name, hash_short(manifest_hash), nonce),
        63,
    )
}

pub fn bounded_name(raw: &str, max_len: usize) -> String {
    let sanitized = raw
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '-' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => '-',
        })
        .collect::<String>();

    let mut compact = String::with_capacity(sanitized.len());
    let mut last_dash = false;
    for c in sanitized.chars() {
        if c == '-' {
            if !last_dash {
                compact.push(c);
            }
            last_dash = true;
        } else {
            compact.push(c);
            last_dash = false;
        }
    }

    let mut compact = compact.trim_matches('-').to_string();
    if compact.is_empty() {
        compact = "fi".to_string();
    }
    if compact.len() <= max_len {
        return compact;
    }

    let mut truncated = compact[..max_len].trim_end_matches('-').to_string();
    if truncated.is_empty() {
        truncated = compact.chars().take(max_len).collect();
    }
    truncated
}

pub fn time_nonce() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let val = (nanos % (36u128.pow(4))) as u32;
    base36_pad4(val)
}

fn base36_pad4(mut n: u32) -> String {
    let mut buf = ['0'; 4];
    for idx in (0..4).rev() {
        let digit = (n % 36) as u8;
        buf[idx] = match digit {
            0..=9 => (b'0' + digit) as char,
            _ => (b'a' + (digit - 10)) as char,
        };
        n /= 36;
    }
    buf.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_hash_is_stable_for_object_key_order() {
        let a = json!({"b": 1, "a": {"z": 1, "m": [3, 2, 1]}});
        let b = json!({"a": {"m": [3, 2, 1], "z": 1}, "b": 1});

        let (a_content, a_hash) = manifest_content_and_hash(&a).unwrap();
        let (b_content, b_hash) = manifest_content_and_hash(&b).unwrap();

        assert_eq!(a_content, b_content);
        assert_eq!(a_hash, b_hash);
    }

    #[test]
    fn generated_names_are_dns_compatible_and_bounded() {
        let fi_name = "My__Very.Long_FrontendIntegration.Name";
        let hash = "sha256:0123456789abcdef";
        let job = job_name(fi_name, hash, "ab12");
        let secret = secret_name(fi_name, hash, "ab12");
        let bundle = default_bundle_name(fi_name);

        for name in [job, secret, bundle] {
            assert!(name.len() <= 63);
            assert!(!name.starts_with('-'));
            assert!(!name.ends_with('-'));
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            );
        }
    }
}
