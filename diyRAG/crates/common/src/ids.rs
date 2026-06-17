#![forbid(unsafe_code)]
//! Identifier & hashing helpers (spec §5.1, §5.3).
//!
//! - All primary keys are **UUIDv7** (time-sortable, generated in Rust; spec §5.1).
//! - Content addressing uses **sha256** (spec §5.3, §6.7).

use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Generate a fresh time-sortable UUIDv7, used for every PK (spec §5.1).
#[must_use]
pub fn new_id() -> Uuid {
    Uuid::now_v7()
}

/// Compute the lowercase hex sha256 digest of the given bytes (spec §5.3).
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_sortable_v7() {
        let a = new_id();
        let b = new_id();
        // v7 ids minted later should not sort before earlier ones.
        assert!(a <= b, "uuidv7 ids should be monotonic-ish: {a} > {b}");
        assert_eq!(a.get_version_num(), 7);
    }

    #[test]
    fn sha256_is_stable_and_64_hex_chars() {
        let d = sha256_hex(b"diyRAG");
        assert_eq!(d.len(), 64);
        assert_eq!(d, sha256_hex(b"diyRAG"));
    }
}
