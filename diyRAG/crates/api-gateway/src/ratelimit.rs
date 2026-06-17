#![forbid(unsafe_code)]
//! Gateway rate limiting (spec §12.3, §22 #3/#7).
//!
//! Two complementary token-bucket layers, configurable per endpoint and stricter
//! on the ingestion / sync / answer paths (spec §12.3):
//!
//! 1. **Per-IP** buckets — coarse DoS protection keyed on the source address.
//! 2. **Per-key / per-client-id** buckets — fairness between API principals so a
//!    single key cannot starve others (spec §22 #3).
//!
//! In-process buckets use [`tower_governor`] (which wraps the `governor` crate).
//! For a multi-replica gateway the authoritative buckets are **Redis-backed**
//! (`fred`) so limits hold across instances (spec §12.3 — "backed by Redis token
//! buckets"). The in-proc layer is the fast path / fallback; the Redis layer is
//! the distributed source of truth.

use std::sync::Arc;

use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter};
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::SmartIpKeyExtractor;
use tower_governor::GovernorLayer;

/// A per-endpoint rate-limit policy (spec §12.3 — "configurable per endpoint").
#[derive(Debug, Clone, Copy)]
pub struct RatePolicy {
    /// Sustained requests permitted per second.
    pub per_second: u64,
    /// Additional burst capacity above the sustained rate.
    pub burst: u32,
}

impl RatePolicy {
    /// Default policy for read/query endpoints.
    pub const READ: RatePolicy = RatePolicy {
        per_second: 20,
        burst: 40,
    };
    /// Stricter policy for ingestion endpoints (spec §12.3).
    pub const INGEST: RatePolicy = RatePolicy {
        per_second: 5,
        burst: 10,
    };
    /// Strictest policy for the expensive grounded-answer path (spec §12.3).
    pub const ANSWER: RatePolicy = RatePolicy {
        per_second: 2,
        burst: 4,
    };
}

/// Build a per-IP [`GovernorLayer`] from a [`RatePolicy`].
///
/// Uses [`SmartIpKeyExtractor`] so the source IP is read from `X-Forwarded-For`/
/// `X-Real-IP` when the gateway sits behind Caddy/Traefik (spec §3.2), falling
/// back to the peer address. This is the coarse, always-on DoS guard.
#[must_use]
pub fn per_ip_layer(
    policy: RatePolicy,
) -> GovernorLayer<SmartIpKeyExtractor, governor::middleware::NoOpMiddleware> {
    // `replenish_interval` is expressed as the period between single-token
    // refills; per_second tokens => 1_000_000 / per_second microseconds each.
    let micros = (1_000_000 / policy.per_second.max(1)).max(1);
    let config = GovernorConfigBuilder::default()
        .period(std::time::Duration::from_micros(micros))
        .burst_size(policy.burst)
        .key_extractor(SmartIpKeyExtractor)
        .finish()
        .expect("valid governor config"); // const-derived inputs; cannot fail.
    GovernorLayer {
        config: Arc::new(config),
    }
}

/// Per-principal (API key / client id) token-bucket limiter.
///
/// Distinct from the per-IP layer so a single key behind many IPs (or many keys
/// behind one IP) are each fairly bounded (spec §22 #3). Applied AFTER auth so
/// the principal id is known; consulted inside the auth/route middleware rather
/// than as a Tower layer (the key isn't available pre-auth).
pub struct PerKeyLimiter {
    /// In-process limiter keyed by principal id string.
    inner: Arc<dashmap_shim::Map<String, Arc<DirectLimiter>>>,
    policy: RatePolicy,
}

/// Convenience alias for a single un-keyed in-memory governor limiter.
type DirectLimiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

impl PerKeyLimiter {
    /// Create a per-key limiter with the given policy.
    #[must_use]
    pub fn new(policy: RatePolicy) -> Self {
        Self {
            inner: Arc::new(dashmap_shim::Map::new()),
            policy,
        }
    }

    /// Check (and consume) one token for `principal_id`.
    ///
    /// Returns `Ok(())` when permitted, or `Err(())` when the bucket is empty
    /// (the caller maps this to HTTP 429 via the standard error envelope).
    pub fn check(&self, principal_id: &str) -> Result<(), ()> {
        let limiter = self
            .inner
            .entry(principal_id.to_owned())
            .or_insert_with(|| Arc::new(self.build_limiter()));
        limiter.check().map_err(|_| ())
    }

    fn build_limiter(&self) -> DirectLimiter {
        let per_second = std::num::NonZeroU32::new(self.policy.per_second.max(1) as u32)
            .expect("per_second >= 1");
        let burst = std::num::NonZeroU32::new(self.policy.burst.max(1)).expect("burst >= 1");
        let quota = Quota::per_second(per_second).allow_burst(burst);
        RateLimiter::direct(quota)
    }
}

/// Distributed (Redis-backed) token-bucket limiter (spec §12.3).
///
/// Authoritative across gateway replicas. Implemented with `fred` using an
/// atomic Lua token-bucket script keyed by `ratelimit:{scope}:{principal}`. The
/// in-process [`PerKeyLimiter`] is a fast local pre-filter; this is the source
/// of truth when more than one gateway instance is running.
#[allow(dead_code)] // selected when a Redis URL is configured (multi-replica) — follow-up
pub struct RedisRateLimiter {
    // TODO: hold a `fred::clients::RedisPool` and the configured policies. On
    //       check(), EVALSHA a token-bucket script returning allowed + retry-after.
    policy: RatePolicy,
}

#[allow(dead_code)] // see RedisRateLimiter — wired in the multi-replica follow-up
impl RedisRateLimiter {
    /// Connect to Redis using the configured URL (sourced from config; no
    /// hardcoded host, spec §0).
    pub async fn connect(_redis_url: &str, policy: RatePolicy) -> anyhow::Result<Self> {
        // TODO: build a fred RedisPool from the URL, connect(), and pre-load the
        //       token-bucket Lua script (SCRIPT LOAD) caching its SHA.
        Ok(Self { policy })
    }

    /// Atomically check-and-consume one token for `principal_id`.
    pub async fn check(&self, _scope: &str, _principal_id: &str) -> anyhow::Result<bool> {
        // TODO: EVALSHA the token-bucket script with self.policy params; return
        //       the allowed flag. Fail-open vs fail-closed is a DECISION recorded
        //       in routes.rs (we fail-closed on the answer/ingest paths).
        let _ = &self.policy;
        Ok(true)
    }
}

/// Minimal concurrent-map shim so this scaffold compiles without committing the
/// workspace to a specific concurrent-map crate yet.
///
/// DECISION: a real build replaces this with `dashmap` (pinned in this member
/// crate, like the parser deps in `ingestion-worker`). Kept tiny and behind a
/// `Mutex<HashMap>` so the public `PerKeyLimiter` API is stable across the swap.
mod dashmap_shim {
    use std::collections::HashMap;
    use std::hash::Hash;
    use std::sync::Mutex;

    /// A trivially-concurrent map with an `entry().or_insert_with()` surface.
    pub struct Map<K, V> {
        inner: Mutex<HashMap<K, V>>,
    }

    impl<K: Eq + Hash + Clone, V: Clone> Map<K, V> {
        pub fn new() -> Self {
            Self {
                inner: Mutex::new(HashMap::new()),
            }
        }

        /// Get-or-insert and return a clone of the value (V is an `Arc<…>`).
        pub fn entry(&self, key: K) -> Entry<'_, K, V> {
            Entry { map: self, key }
        }
    }

    /// Lazy entry handle mirroring the subset of the `dashmap` API used here.
    pub struct Entry<'a, K, V> {
        map: &'a Map<K, V>,
        key: K,
    }

    impl<K: Eq + Hash + Clone, V: Clone> Entry<'_, K, V> {
        pub fn or_insert_with(self, f: impl FnOnce() -> V) -> V {
            let mut guard = self.map.inner.lock().expect("ratelimit map poisoned");
            guard.entry(self.key).or_insert_with(f).clone()
        }
    }
}
