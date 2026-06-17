//! diyRAG ingestion worker (binary `diyrag-ingestion-worker`).
//!
//! A stateless Rust worker that drains a NATS JetStream durable consumer and runs
//! the `parse → chunk → embed → persist` pipeline for one document per work unit
//! (MASTER_BUILD_SPEC.md §6). Delivery is at-least-once; combined with the
//! `content_sha256` idempotency check (§6.2) this yields effectively-once
//! semantics. Per §14 the loop is **non-stop**: a failed unit is logged,
//! classified, NAK'd/quarantined, and the worker immediately continues.
#![forbid(unsafe_code)]

mod chunker;
mod embed;
mod parser;
mod persist;

use std::sync::Arc;

use anyhow::Context as _;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Wire envelope for a queued work unit (published by `core-api`, §6.2).
///
/// Identity is the content hash so reprocessing is idempotent. `correlation_id`
/// is propagated from the gateway via the NATS message header and re-attached to
/// the worker span (§13.1).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkUnit {
    pub work_unit_id: uuid::Uuid,
    pub job_id: uuid::Uuid,
    pub tenant_id: uuid::Uuid,
    pub document_ref: String,
    pub content_sha256: String,
    pub blob_key: String,
    #[serde(default)]
    pub correlation_id: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Logging + typed config from the shared crate (§0, §13.1).
    // TODO: diyrag_common::logging::init_json_subscriber()?;
    // TODO: let cfg = diyrag_common::config::load::<WorkerConfig>()?;
    info!("diyrag-ingestion-worker starting");

    // 2. Build shared pipeline state (DB pool, qdrant, blob store, parser router,
    //    embedding backend, tokenizer). All come from `diyrag-common` or are
    //    constructed from typed config — never hardcoded (§0).
    // TODO: let state = WorkerState::from_config(&cfg).await?;
    let state = Arc::new(WorkerState::placeholder());

    // 3. Cooperative shutdown: the supervisor (§16b) sends SIGTERM / Ctrl-C; the
    //    consumer loop drains the in-flight unit, then acks/naks and exits.
    let shutdown = CancellationToken::new();
    spawn_signal_listener(shutdown.clone());

    // 4. Optional liveness/readiness HTTP server. Workers do not serve client
    //    traffic, but expose /healthz + /readyz so the orchestrator can probe
    //    them (§0). Bind only when configured.
    // TODO: serve_health_endpoints(cfg.health_addr, shutdown.clone()).await;

    // 5. Run the JetStream consumer loop until shutdown.
    if let Err(e) = run_consumer_loop(state, shutdown).await {
        error!(error = %e, "consumer loop terminated with error");
        return Err(e);
    }

    info!("diyrag-ingestion-worker stopped cleanly");
    Ok(())
}

/// Shared, cheaply-clonable pipeline dependencies handed to each work unit.
pub struct WorkerState {
    pub router: parser::ParserRouter,
    pub chunker: chunker::Chunker,
    pub embedder: Box<dyn embed::EmbeddingBackend>,
    // TODO: pub db: sqlx::PgPool,
    // TODO: pub qdrant: qdrant_client::Qdrant,
    // TODO: pub blob: std::sync::Arc<dyn object_store::ObjectStore>,
}

impl WorkerState {
    /// Placeholder wiring so the skeleton compiles before `common` lands.
    fn placeholder() -> Self {
        Self {
            router: parser::ParserRouter::with_defaults(),
            chunker: chunker::Chunker::default(),
            embedder: Box::new(embed::NoopEmbeddingBackend),
        }
    }

    // TODO: pub async fn from_config(cfg: &WorkerConfig) -> anyhow::Result<Self>
}

/// Durable JetStream consumer loop (§6.2, §14).
///
/// Pulls a batch, processes each unit, and acks/naks per outcome. A single bad
/// file never halts the loop: failures are classified and routed to retry or
/// quarantine, then the worker moves on (§14 "non-stop workers").
async fn run_consumer_loop(
    state: Arc<WorkerState>,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    // TODO: connect async-nats, get JetStream context, bind/create a *durable*
    //       pull consumer on the ingestion subject with explicit ack policy and
    //       ack_wait = max_proc + 2σ (§14 detector/heartbeat).
    // let client = async_nats::connect(&state.cfg.nats_url).await?;
    // let js = async_nats::jetstream::new(client);
    // let consumer = js.get_consumer_from_stream(...).await?;

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                info!("shutdown requested; draining and exiting consumer loop");
                break;
            }
            // TODO: msg = consumer.next() => { ... process_message(...).await; }
            _ = std::future::pending::<()>() => {}
        }
    }
    Ok(())
}

/// Process one delivered message end-to-end, mapping the outcome to an ack
/// decision. This is the explicit `Result`-propagation boundary required by §14.
#[allow(dead_code)]
async fn process_message(state: &WorkerState, unit: WorkUnit) -> AckDecision {
    let span = tracing::info_span!(
        "work_unit",
        work_unit_id = %unit.work_unit_id,
        tenant_id = %unit.tenant_id,
        content_sha256 = %unit.content_sha256,
        correlation_id = unit.correlation_id.as_deref().unwrap_or("-"),
    );
    let _guard = span.enter();

    match process_unit(state, &unit).await {
        Ok(Outcome::Indexed) => AckDecision::Ack,
        Ok(Outcome::AlreadyIndexed) => {
            info!("idempotent skip: already INDEXED for content_sha256");
            AckDecision::Ack
        }
        Ok(Outcome::Quarantined { reason }) => {
            warn!(reason, "unit quarantined; continuing");
            // Quarantine is a terminal *recorded* state, not a redelivery (§14).
            AckDecision::Term
        }
        Err(e) => {
            // Classify: TRANSIENT → NAK with backoff; PERMANENT → quarantine/Term.
            // TODO: match e.classification() { Transient => Nak{delay}, Permanent => Term }
            error!(error = %e, "work unit failed; continuing to next unit");
            AckDecision::Nak { delay_secs: 0 }
        }
    }
}

/// The pipeline proper: idempotency gate → parse → chunk → embed → persist.
#[allow(dead_code)]
async fn process_unit(state: &WorkerState, unit: &WorkUnit) -> anyhow::Result<Outcome> {
    // 1. Idempotency: skip if an INDEXED document already exists for
    //    (tenant_id, content_sha256) (§6.2).
    // TODO: if persist::is_already_indexed(&state.db, unit).await? {
    //     return Ok(Outcome::AlreadyIndexed);
    // }

    // 2. Fetch original bytes from the content-addressed blob store (§5.3).
    // TODO: let blob = state.blob.get(&blob_path).await?.bytes().await?;
    let blob = bytes::Bytes::new();

    // 3. Parse via the MIME-sniffing router; hard cases delegate to Python (§6.3).
    let parse_opts = parser::ParseOpts::default();
    let blob_ref = parser::BlobRef {
        key: unit.blob_key.clone(),
        bytes: blob,
        declared_name: unit.document_ref.clone(),
    };
    let doc = state
        .router
        .route_and_parse(&blob_ref, &parse_opts)
        .await
        .context("parser router failed")?;

    // 4. Chunk (structure-aware; invariant failures → quarantine) (§6.4).
    let chunks = match state.chunker.chunk(&doc, unit) {
        Ok(c) => c,
        Err(chunker::ChunkError::Invariant(reason)) => {
            return Ok(Outcome::Quarantined { reason });
        }
        Err(e) => return Err(e.into()),
    };

    // 5. Embed in dynamic batches (dense + sparse) (§6.5).
    let _embeddings = state
        .embedder
        .embed_batch(&chunks)
        .await
        .context("embedding backend failed")?;

    // 6. Persist atomically: chunk row (sqlx tx) + qdrant point, idempotent on
    //    vector_id (§6.5). On success the document flips to INDEXED.
    // TODO: persist::persist_chunks(&state.db, &state.qdrant, unit, &chunks, &_embeddings).await?;

    Ok(Outcome::Indexed)
}

/// Terminal outcome of a single unit.
#[derive(Debug)]
pub enum Outcome {
    Indexed,
    AlreadyIndexed,
    Quarantined { reason: String },
}

/// How the JetStream message should be acknowledged.
#[derive(Debug)]
pub enum AckDecision {
    Ack,
    Nak { delay_secs: u64 },
    Term,
}

/// Bridge OS signals to the cancellation token used by the consumer loop.
fn spawn_signal_listener(shutdown: CancellationToken) {
    tokio::spawn(async move {
        if let Err(e) = wait_for_shutdown_signal().await {
            warn!(error = %e, "signal listener error; forcing shutdown");
        }
        shutdown.cancel();
    });
}

/// Wait for SIGINT or (on Unix) SIGTERM (§16b graceful drain).
async fn wait_for_shutdown_signal() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut term = signal::unix::signal(signal::unix::SignalKind::terminate())?;
        tokio::select! {
            _ = signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        signal::ctrl_c().await?;
    }
    Ok(())
}
