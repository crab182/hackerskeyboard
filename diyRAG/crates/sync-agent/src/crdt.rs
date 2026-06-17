#![forbid(unsafe_code)]
//! Version-vector CRDT for the registry sync (MASTER_BUILD_SPEC.md §9, §22 #4).
//!
//! Registry records are keyed by `content_sha256`; writes are idempotent on the
//! hash, so most "conflicts" are no-ops. When two peers hold genuinely concurrent
//! edits of the *same* record, we resolve **deterministically**:
//!
//!   1. **Version-vector dominance** — if one record's vector causally dominates
//!      the other (≥ on every entry, and the maps are not equal), the dominant
//!      record wins. This is the common, correct case.
//!   2. **Deterministic tiebreak** for truly concurrent vectors (neither
//!      dominates): take the record whose origin node has the **highest
//!      `nodes.priority`**; break a priority tie by the **lexicographically
//!      smallest `node_id`**.
//!
//! There is **NO wall-clock LWW** anywhere — clocks skew across a LAN and an
//! attacker can backdate, which would silently drop good writes (§22 #4). The
//! tiebreak is a pure function of data already replicated (vectors, priorities,
//! ids), so every peer reaches the *same* decision without coordination.

use std::cmp::Ordering;

use diyrag_common::schemas::VersionVector;

/// A registry record participating in conflict resolution. This is the minimal
/// projection the resolver needs; the full payload travels alongside it but is
/// opaque here (it is data, never instructions — §12.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryRecord {
    /// Identity of the record (content hash); equal keys are the same logical row.
    pub record_key: String,
    /// `{node: counter}` version vector (spec §5.1 `VersionVector`).
    pub version_vector: VersionVector,
    /// Node that last authored this record (`node_id`).
    pub origin_node: String,
}

/// How two records' version vectors relate causally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Causality {
    /// The vectors are identical — the same write; resolution is a no-op.
    Equal,
    /// `left` causally dominates `right` (≥ everywhere, strictly greater somewhere).
    LeftDominates,
    /// `right` causally dominates `left`.
    RightDominates,
    /// Neither dominates — a genuine concurrent edit needing a tiebreak.
    Concurrent,
}

/// Resolution outcome the caller persists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Keep the local record; the remote is older-or-equal.
    KeepLocal,
    /// Adopt the remote record; it dominates or won the deterministic tiebreak.
    TakeRemote,
}

/// Look-up of a node's sync priority (`nodes.priority`, spec §5.1). Higher wins.
/// Implemented over the in-memory peer table (`discovery::PeerTable`) so the
/// resolver stays pure and testable without a DB round-trip.
pub trait NodePriority {
    /// Priority for a node id; unknown nodes get the lowest priority so an
    /// unenrolled origin can never win a tiebreak (defense-in-depth with the
    /// cert-pinning gate in `discovery.rs`).
    fn priority_of(&self, node_id: &str) -> i32;
}

/// Compare two version vectors for causal dominance (the entry-wise partial
/// order over `{node: counter}` maps). Missing entries are treated as `0`.
#[must_use]
pub fn compare_vectors(left: &VersionVector, right: &VersionVector) -> Causality {
    let mut left_greater = false;
    let mut right_greater = false;

    // Union of node keys from both vectors; absent => counter 0.
    for node in left.keys().chain(right.keys()) {
        let l = left.get(node).copied().unwrap_or(0);
        let r = right.get(node).copied().unwrap_or(0);
        match l.cmp(&r) {
            Ordering::Greater => left_greater = true,
            Ordering::Less => right_greater = true,
            Ordering::Equal => {}
        }
    }

    match (left_greater, right_greater) {
        (false, false) => Causality::Equal,
        (true, false) => Causality::LeftDominates,
        (false, true) => Causality::RightDominates,
        (true, true) => Causality::Concurrent,
    }
}

/// Resolve a conflict between the `local` record and an incoming `remote` record
/// for the same `record_key`. Deterministic and free of wall-clock time (§9).
///
/// Precondition (debug-asserted): the two records share a `record_key`.
#[must_use]
pub fn resolve<P: NodePriority>(
    local: &RegistryRecord,
    remote: &RegistryRecord,
    priorities: &P,
) -> Resolution {
    debug_assert_eq!(
        local.record_key, remote.record_key,
        "resolve() called on records with different keys"
    );

    match compare_vectors(&local.version_vector, &remote.version_vector) {
        // Same write — keep what we have (idempotent, §9).
        Causality::Equal => Resolution::KeepLocal,
        Causality::LeftDominates => Resolution::KeepLocal,
        Causality::RightDominates => Resolution::TakeRemote,
        // Genuine concurrent edit: deterministic tiebreak (§9).
        Causality::Concurrent => {
            let lp = priorities.priority_of(&local.origin_node);
            let rp = priorities.priority_of(&remote.origin_node);
            match rp.cmp(&lp) {
                // Highest priority wins.
                Ordering::Greater => Resolution::TakeRemote,
                Ordering::Less => Resolution::KeepLocal,
                // Priority tie → lexicographically smallest node_id wins.
                Ordering::Equal => {
                    if remote.origin_node < local.origin_node {
                        Resolution::TakeRemote
                    } else {
                        // Includes the equal-id case: keep local (stable, no flap).
                        Resolution::KeepLocal
                    }
                }
            }
        }
    }
}

/// Merge two version vectors element-wise (the join in the lattice): for every
/// node, take the max counter. Used after adopting a remote record so the merged
/// record's vector dominates both inputs (prevents resurrecting old conflicts).
#[must_use]
pub fn merge_vectors(a: &VersionVector, b: &VersionVector) -> VersionVector {
    let mut out = a.clone();
    for (node, &counter) in b {
        let entry = out.entry(node.clone()).or_insert(0);
        *entry = (*entry).max(counter);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Static priority table for tests: a > b > c.
    struct TestPriorities;
    impl NodePriority for TestPriorities {
        fn priority_of(&self, node_id: &str) -> i32 {
            match node_id {
                "nodeA" => 30,
                "nodeB" => 20,
                "nodeC" => 10,
                _ => i32::MIN, // unknown nodes never win (defense-in-depth)
            }
        }
    }

    fn vv(pairs: &[(&str, i64)]) -> VersionVector {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), *v))
            .collect::<BTreeMap<_, _>>()
    }

    fn rec(key: &str, origin: &str, vv_pairs: &[(&str, i64)]) -> RegistryRecord {
        RegistryRecord {
            record_key: key.to_owned(),
            version_vector: vv(vv_pairs),
            origin_node: origin.to_owned(),
        }
    }

    #[test]
    fn equal_vectors_keep_local() {
        let l = rec("h1", "nodeA", &[("nodeA", 5), ("nodeB", 2)]);
        let r = rec("h1", "nodeB", &[("nodeA", 5), ("nodeB", 2)]);
        assert_eq!(resolve(&l, &r, &TestPriorities), Resolution::KeepLocal);
        assert_eq!(
            compare_vectors(&l.version_vector, &r.version_vector),
            Causality::Equal
        );
    }

    #[test]
    fn remote_dominates_is_taken() {
        let l = rec("h1", "nodeA", &[("nodeA", 5), ("nodeB", 2)]);
        let r = rec("h1", "nodeB", &[("nodeA", 5), ("nodeB", 3)]); // strictly ahead
        assert_eq!(
            compare_vectors(&l.version_vector, &r.version_vector),
            Causality::RightDominates
        );
        assert_eq!(resolve(&l, &r, &TestPriorities), Resolution::TakeRemote);
    }

    #[test]
    fn local_dominates_is_kept() {
        let l = rec("h1", "nodeA", &[("nodeA", 6), ("nodeB", 2)]);
        let r = rec("h1", "nodeB", &[("nodeA", 5), ("nodeB", 2)]);
        assert_eq!(
            compare_vectors(&l.version_vector, &r.version_vector),
            Causality::LeftDominates
        );
        assert_eq!(resolve(&l, &r, &TestPriorities), Resolution::KeepLocal);
    }

    #[test]
    fn concurrent_resolves_by_highest_priority() {
        // Concurrent: local ahead on nodeA, remote ahead on nodeB.
        let l = rec("h1", "nodeB", &[("nodeA", 6), ("nodeB", 1)]); // local authored by nodeB (prio 20)
        let r = rec("h1", "nodeA", &[("nodeA", 5), ("nodeB", 2)]); // remote authored by nodeA (prio 30)
        assert_eq!(
            compare_vectors(&l.version_vector, &r.version_vector),
            Causality::Concurrent
        );
        // nodeA (30) > nodeB (20) → take remote.
        assert_eq!(resolve(&l, &r, &TestPriorities), Resolution::TakeRemote);
    }

    #[test]
    fn concurrent_priority_tie_breaks_by_smallest_node_id() {
        // Same priority bucket: invent two equal-priority nodes via the unknown
        // path would lose; instead give both the same known priority by origin.
        struct FlatPriorities;
        impl NodePriority for FlatPriorities {
            fn priority_of(&self, _node_id: &str) -> i32 {
                42
            }
        }
        // Concurrent vectors, equal priority → lexicographically smallest id wins.
        let l = rec("h1", "nodeZ", &[("nodeA", 6), ("nodeB", 1)]);
        let r = rec("h1", "nodeA", &[("nodeA", 5), ("nodeB", 2)]);
        assert_eq!(
            compare_vectors(&l.version_vector, &r.version_vector),
            Causality::Concurrent
        );
        // "nodeA" < "nodeZ" → take remote.
        assert_eq!(resolve(&l, &r, &FlatPriorities), Resolution::TakeRemote);

        // Swap roles: remote id now larger → keep local.
        let l2 = rec("h1", "nodeA", &[("nodeA", 6), ("nodeB", 1)]);
        let r2 = rec("h1", "nodeZ", &[("nodeA", 5), ("nodeB", 2)]);
        assert_eq!(resolve(&l2, &r2, &FlatPriorities), Resolution::KeepLocal);
    }

    #[test]
    fn resolution_is_symmetric_and_deterministic() {
        // Whatever the order of comparison, both peers must converge to the SAME
        // winning record (acceptance #5). We check that resolving (l,r) and (r,l)
        // selects the same physical record.
        let a = rec("h1", "nodeB", &[("nodeA", 6), ("nodeB", 1)]);
        let b = rec("h1", "nodeA", &[("nodeA", 5), ("nodeB", 2)]);

        let on_node1 = match resolve(&a, &b, &TestPriorities) {
            Resolution::KeepLocal => &a,
            Resolution::TakeRemote => &b,
        };
        // Peer sees them swapped (its local is `b`, remote is `a`).
        let on_node2 = match resolve(&b, &a, &TestPriorities) {
            Resolution::KeepLocal => &b,
            Resolution::TakeRemote => &a,
        };
        assert_eq!(on_node1, on_node2, "peers diverged — sync would not converge");
    }

    #[test]
    fn merge_takes_elementwise_max() {
        let merged = merge_vectors(&vv(&[("nodeA", 6), ("nodeB", 1)]), &vv(&[("nodeA", 5), ("nodeB", 2)]));
        assert_eq!(merged.get("nodeA"), Some(&6));
        assert_eq!(merged.get("nodeB"), Some(&2));
        // Merged vector dominates both inputs.
        assert_eq!(
            compare_vectors(&merged, &vv(&[("nodeA", 5), ("nodeB", 2)])),
            Causality::LeftDominates
        );
    }
}
