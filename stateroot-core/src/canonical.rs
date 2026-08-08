//! Canonical serialization for StateRoot hashing (binding contract).
//!
//! Byte-identical mirror of `app/core/stateroot/canonical.py` — sorted keys,
//! compact separators, raw UTF-8, no floats in hashed payloads (contract error).
//! Cross-language stability is pinned by `tests/fixtures/canonical_root_manifest.json`
//! hashed identically from pytest and from this crate.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// A value violates the canonical hash contract (float, or a non-JSON type).
#[derive(Debug, thiserror::Error)]
#[error("canonical hash contract violation at {path}: {reason}")]
pub struct HashContractError {
    /// JSON path of the offending value.
    pub path: String,
    /// Why it violates the contract.
    pub reason: String,
}

fn check(value: &Value, path: &str) -> Result<(), HashContractError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(n) => {
            if n.is_f64() {
                Err(HashContractError {
                    path: path.to_string(),
                    reason: "floats are not allowed in hashed payloads".to_string(),
                })
            } else {
                Ok(())
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                check(item, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for (key, item) in map {
                check(item, &format!("{path}.{key}"))?;
            }
            Ok(())
        }
    }
}

/// Serialize to the canonical form. `serde_json::Value::Object` is a BTreeMap
/// (default feature set), so keys serialize sorted; `to_string` is compact and
/// emits raw UTF-8 (no \uXXXX escaping), matching the Python contract.
pub fn canonical_json(value: &Value) -> Result<String, HashContractError> {
    check(value, "$")?;
    // serde_json cannot fail to serialize a Value.
    Ok(value.to_string())
}

/// `sha256:` hex digest of the canonical serialization.
pub fn content_hash(value: &Value) -> Result<String, HashContractError> {
    let canonical = canonical_json(value)?;
    let digest = Sha256::digest(canonical.as_bytes());
    let mut hex_digest = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex_digest.push_str(&format!("{byte:02x}"));
    }
    Ok(format!("sha256:{hex_digest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_keys_and_is_compact() {
        let value: Value = serde_json::json!({"b": 1, "a": {"d": true, "c": "x"}});
        assert_eq!(
            canonical_json(&value).unwrap(),
            r#"{"a":{"c":"x","d":true},"b":1}"#
        );
    }

    #[test]
    fn rejects_floats() {
        let value: Value = serde_json::json!({"x": 1.5});
        assert!(content_hash(&value).is_err());
    }

    #[test]
    fn emits_raw_utf8() {
        let value: Value = serde_json::json!({"s": "héllo"});
        assert_eq!(canonical_json(&value).unwrap(), r#"{"s":"héllo"}"#);
    }

    #[test]
    fn fixture_matches_shared_expected_hash() {
        let raw = include_str!("../../tests/fixtures/canonical_root_manifest.json");
        let value: Value = serde_json::from_str(raw).expect("fixture parses");
        let expected: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/canonical_expected_hashes.json"
        ))
        .expect("expected hashes parse");
        let expected_hash = expected["canonical_root_manifest"]
            .as_str()
            .expect("hash string");
        assert_eq!(content_hash(&value).unwrap(), expected_hash);
    }
}
