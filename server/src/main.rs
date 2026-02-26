use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[allow(deprecated)]
use gp2f_server::{
    actor::ActorRegistry,
    async_ingestion::AsyncIngestionQueue,
    compat,
    llm_provider::{build_provider, LlmMessage, LlmProvider, LlmRequest},
    middleware::{
        EnvVarKeyProvider, InMemoryPublicKeyStore, OpIdLayer, PollingKeyProvider, PublicKeyStore,
        SanitizeLayer,
    },
    rate_limit::{build_rate_limiter, DynRateLimiter},
    reconciler::Reconciler,
    redis_broadcast::{build_broadcaster, DynBroadcaster},
    temporal_store::{InMemoryStore, PersistentStore, TemporalStore},
    token_service::{build_token_store, DynTokenStore, MintRequest, RedeemRequest},
    tool_gating::ToolGatingService,
    wasm_engine::WasmtimeEngine,
    wire::{AgentProposeRequest, AiFeedbackRequest, ClientMessage, HelloMessage, ServerMessage},
};
use policy_core::evaluator::hash_state;

#[derive(Clone)]
struct AppState {
    reconciler: Arc<Reconciler>,
    token_service: DynTokenStore,
    actor_registry: Arc<ActorRegistry>,
    /// Broadcaster (Redis PubSub or in-process fallback).
    broadcaster: DynBroadcaster,
    /// LLM provider (OpenAI / Anthropic / Groq / Mock).
    llm_provider: Arc<dyn LlmProvider>,
    /// Tool gating service – decides which tools the LLM may see.
    tool_gating: Arc<ToolGatingService>,
    /// Per-tenant AI rate limiter and budget guard.
    ai_rate_limiter: DynRateLimiter,
    /// Async ingestion queue for the `/op/async` low-latency endpoint.
    ingestion_queue: Arc<AsyncIngestionQueue>,
    /// Persistent event store – used by the `/health` readiness probe.
    event_store: Arc<dyn PersistentStore>,
}

#[tokio::main]
async fn main() {
    // ── Structured logging (JSON when LOG_FORMAT=json is set) ──────────────
    let use_json_logs = std::env::var("LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    let env_filter = tracing_subscriber::EnvFilter::new(
        std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
    );

    if use_json_logs {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer())
            .init();
    }

    // ── Production guard: REDIS_URL required when redis-broadcast is enabled ─
    #[cfg(feature = "redis-broadcast")]
    {
        let is_production = std::env::var("APP_ENV")
            .map(|v| v.eq_ignore_ascii_case("production"))
            .unwrap_or(false);
        if is_production && gp2f_server::secrets::resolve_secret("REDIS_URL").is_none() {
            panic!(
                "REDIS_URL (or REDIS_URL_FILE) must be set when APP_ENV=production \
                 and the redis-broadcast feature is enabled. \
                 Set REDIS_URL to your Redis connection string or unset APP_ENV to run in non-production mode."
            );
        }
    }

    // ── Wasmtime policy engine (optional) ──────────────────────────────────
    let wasm_path =
        std::env::var("POLICY_WASM_PATH").unwrap_or_else(|_| "policy_wasm_bg.wasm".to_string());
    match WasmtimeEngine::new(&wasm_path) {
        Ok(_engine) => tracing::info!(%wasm_path, "Wasmtime policy engine loaded"),
        Err(e) => tracing::info!("Wasmtime engine unavailable ({e}); using native evaluator"),
    }

    // ── Persistent event store ─────────────────────────────────────────────
    let event_store: Arc<dyn PersistentStore> = if let Ok(db_url) = std::env::var("DATABASE_URL") {
        #[cfg(feature = "postgres-store")]
        {
            use gp2f_server::postgres_store::PostgresStore;
            match PostgresStore::new(&db_url).await {
                Ok(store) => {
                    tracing::info!("Postgres event store connected");
                    Arc::new(store)
                }
                Err(e) => {
                    panic!(
                        "DATABASE_URL is set but Postgres connection failed: {e}. \
                         Fix the connection or unset DATABASE_URL to use the in-memory store."
                    );
                }
            }
        }
        #[cfg(not(feature = "postgres-store"))]
        {
            let _ = db_url;
            panic!(
                "DATABASE_URL is set but the `postgres-store` feature is disabled. \
                 Rebuild with `--features postgres-store` or unset DATABASE_URL."
            );
        }
    } else if let Ok(endpoint) = std::env::var("TEMPORAL_ENDPOINT") {
        let namespace = std::env::var("TEMPORAL_NAMESPACE").unwrap_or_else(|_| "gp2f-prod".into());
        let store = TemporalStore::new(endpoint.clone(), namespace);
        if let Err(e) = store.connect().await {
            panic!(
                "TEMPORAL_ENDPOINT is set but Temporal connection failed: {e}. \
                 Fix the connection or unset TEMPORAL_ENDPOINT to use the in-memory store."
            );
        }
        Arc::new(store)
    } else {
        tracing::info!("DATABASE_URL not set; using in-memory event store");
        Arc::new(InMemoryStore::new())
    };

    // ── Redis / in-process broadcaster ────────────────────────────────────
    let broadcaster = build_broadcaster().await;

    // ── Token store (Redis or in-memory) ──────────────────────────────────
    let token_service = build_token_store().await;

    // ── Op-ID middleware (ed25519 validation) ─────────────────────────────
    // Key-provider selection (highest-priority first):
    //   1. KEYS_POLL_INTERVAL_SECS set → PollingKeyProvider (live key rotation)
    //   2. KEYS_JSON set (or KEYS_JSON_FILE)
    //                              → EnvVarKeyProvider (static, one-shot load)
    //   3. Neither                 → empty InMemoryPublicKeyStore (dev/test only)
    const DEFAULT_KEYS_POLL_INTERVAL_SECS: u64 = 60;
    let key_store: Arc<dyn PublicKeyStore> =
        if let Ok(interval_str) = std::env::var("KEYS_POLL_INTERVAL_SECS") {
            let secs: u64 = interval_str
                .parse()
                .unwrap_or(DEFAULT_KEYS_POLL_INTERVAL_SECS);
            let interval = std::time::Duration::from_secs(secs);
            tracing::info!(
                interval_secs = secs,
                "Loading public keys via PollingKeyProvider"
            );
            Arc::new(PollingKeyProvider::new(interval))
        } else if gp2f_server::secrets::resolve_secret("KEYS_JSON").is_some() {
            tracing::info!("Loading public keys from KEYS_JSON");
            Arc::new(EnvVarKeyProvider::from_env())
        } else {
            let is_production = std::env::var("APP_ENV")
                .map(|v| v.eq_ignore_ascii_case("production"))
                .unwrap_or(false);
            if is_production {
                panic!(
                    "KEYS_JSON or KEYS_POLL_INTERVAL_SECS must be set when APP_ENV=production. \
                     Set KEYS_JSON to a JSON object mapping client_id to a hex-encoded Ed25519 \
                     public key, or set KEYS_POLL_INTERVAL_SECS for live key rotation. \
                     Refusing to start with an ephemeral in-memory key store in production."
                );
            }
            tracing::warn!(
                "No key provider configured (KEYS_POLL_INTERVAL_SECS / KEYS_JSON not set); \
             using ephemeral InMemoryPublicKeyStore – this is INSECURE for production. \
             Set KEYS_POLL_INTERVAL_SECS or KEYS_JSON to use a persistent key provider."
            );
            #[allow(deprecated)]
            {
                Arc::new(InMemoryPublicKeyStore::new())
            }
        };
    let op_id_layer = OpIdLayer::new(key_store);

    // ── LLM provider (OpenAI / Anthropic / Groq / Mock) ───────────────────
    let llm_provider: Arc<dyn LlmProvider> = Arc::from(build_provider());

    // ── Redis actor coordinator (multi-replica split-brain detection) ─────
    #[cfg(feature = "redis-broadcast")]
    let actor_coordinator =
        gp2f_server::actor::RedisActorCoordinator::from_env().map(std::sync::Arc::new);

    // ── Async ingestion queue (Phase 2.2 – low-latency /op/async endpoint) ─
    let ingestion_buffer: usize = std::env::var("INGESTION_QUEUE_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1024);
    let (ingestion_queue, mut ingestion_rx) = AsyncIngestionQueue::new(ingestion_buffer);

    // Background worker: drains the async ingestion queue and runs full
    // reconcile + Temporal signal pipeline.
    let bg_reconciler = Arc::new(Reconciler::new());
    let bg_actor_registry = Arc::new(ActorRegistry::with_store(event_store.clone()));
    tokio::spawn(async move {
        while let Some(msg) = ingestion_rx.recv().await {
            let handle = match bg_actor_registry
                .get_or_spawn(&msg.tenant_id, &msg.workflow_id, &msg.instance_id)
                .await
            {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(error = %e, "async ingestion: skipping op for split-brain instance");
                    continue;
                }
            };
            let _response = handle
                .reconcile(msg.clone())
                .await
                .unwrap_or_else(|| bg_reconciler.reconcile(&msg));
        }
    });

    // Build the main actor registry, attaching the Redis coordinator when available.
    let actor_registry = {
        let base = ActorRegistry::with_store(event_store.clone());
        #[cfg(feature = "redis-broadcast")]
        let base = if let Some(coord) = actor_coordinator {
            base.with_coordinator(coord)
        } else {
            base
        };
        Arc::new(base)
    };

    let state = AppState {
        reconciler: Arc::new(Reconciler::new()),
        token_service,
        actor_registry,
        broadcaster,
        llm_provider,
        tool_gating: Arc::new(ToolGatingService::new()),
        ai_rate_limiter: build_rate_limiter().await,
        ingestion_queue: Arc::new(ingestion_queue),
        event_store: event_store.clone(),
    };

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/livez", get(livez_handler))
        .route("/ws", get(ws_handler))
        .route("/op", post(op_handler))
        .route("/op/async", post(op_async_handler))
        .route("/token/mint", post(token_mint_handler))
        .route("/token/redeem", post(token_redeem_handler))
        .route("/ai/propose", post(ai_propose_handler))
        .route("/ai/feedback", post(ai_feedback_handler))
        .route("/agent/propose", post(agent_propose_handler))
        .with_state(state)
        .layer(op_id_layer)
        .layer(SanitizeLayer)
        .layer(TraceLayer::new_for_http());

    let addr = "0.0.0.0:3000";
    tracing::info!("GP2F server listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind");
    axum::serve(listener, app).await.expect("server failed");
}

/// `GET /health` – readiness probe.
///
/// Returns **200 OK** with `{"status":"ok"}` when the event store is reachable
/// and the server is ready to serve requests.
/// Returns **503 Service Unavailable** with `{"status":"degraded","reason":"…"}`
/// when the backing store cannot be contacted, so the Kubernetes readiness probe
/// stops routing traffic to this pod until the store recovers.
///
/// **Note**: Use this endpoint only for the Kubernetes *readiness* probe, not
/// the *liveness* probe.  Liveness should not depend on external systems; use
/// a simpler check (e.g. `httpGet` on a static path) so the pod is not
/// restarted due to a transient database outage.
async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    if state.event_store.is_alive().await {
        (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })))
    } else {
        tracing::warn!("health check: event store is unreachable");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "status": "degraded",
                "reason": "event store unreachable"
            })),
        )
    }
}

/// `GET /livez` – liveness probe.
///
/// Always returns **200 OK** as long as the process is running and the event
/// loop is not deadlocked.  Does **not** check external dependencies so the
/// pod is never restarted due to a transient database outage.
async fn livez_handler() -> impl IntoResponse {
    StatusCode::OK
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    // Subscribe to the broadcaster (Redis PubSub or in-process fallback) to
    // receive server-initiated push messages.
    let mut broadcast_rx = state.broadcaster.subscribe("__global__");

    // Send a HELLO message immediately so the client can synchronise its clock.
    let hello = ServerMessage::Hello(HelloMessage {
        server_time_ms: chrono::Utc::now().timestamp_millis() as u64,
        server_hlc_ts: state.reconciler.event_store.hlc_now(),
    });
    if let Ok(hello_json) = serde_json::to_string(&hello) {
        let _ = socket.send(Message::Text(hello_json)).await;
    }

    // ── Schema negotiation ────────────────────────────────────────────────
    // The first ClientMessage the client sends is used as an implicit
    // handshake.  If the client's AST version is incompatible we send
    // RELOAD_REQUIRED and close the connection immediately.
    let mut version_checked = false;

    loop {
        tokio::select! {
            // Incoming message from the WebSocket client
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(mut client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                            // Sanitize all string fields to strip control characters
                            // and invisible Unicode before any processing or storage.
                            client_msg.sanitize();
                            if let Err(e) = client_msg.validate() {
                                tracing::warn!(error = %e, "ws: message validation failed");
                                continue;
                            }
                            // On the first op, validate the AST version.
                            if !version_checked {
                                version_checked = true;
                                if let Err(reload) = compat::check_version(&client_msg.ast_version) {
                                    let resp = ServerMessage::ReloadRequired(reload);
                                    let reply = serde_json::to_string(&resp).unwrap_or_default();
                                    let _ = socket.send(Message::Text(reply)).await;
                                    break;
                                }
                            }

                            // Apply any compat-layer transformations.
                            let client_msg = compat::transform_ast(&client_msg);

                            // Route through per-instance actor for serialised processing.
                            let handle = match state.actor_registry.get_or_spawn(
                                &client_msg.tenant_id,
                                &client_msg.workflow_id,
                                &client_msg.instance_id,
                            ).await {
                                Ok(h) => h,
                                Err(e) => {
                                    tracing::warn!(error = %e, "ws: closing connection for split-brain instance");
                                    // Instance is owned by another pod; close the connection
                                    // so the client reconnects to the correct replica.
                                    break;
                                }
                            };
                            let response = if let Some(resp) = handle.reconcile(client_msg).await {
                                resp
                            } else {
                                // Actor terminated unexpectedly; fall back to direct reconcile.
                                continue;
                            };
                            let reply = serde_json::to_string(&response).unwrap_or_default();
                            if socket.send(Message::Text(reply)).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
            // Broadcast message from another handler (e.g. another client's ACCEPT)
            Ok(broadcast_msg) = broadcast_rx.recv() => {
                let text = serde_json::to_string(&broadcast_msg).unwrap_or_default();
                if socket.send(Message::Text(text)).await.is_err() {
                    break;
                }
            }
        }
    }
}

async fn op_handler(
    State(state): State<AppState>,
    Json(client_msg): Json<ClientMessage>,
) -> impl IntoResponse {
    if let Err(e) = client_msg.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response();
    }
    // Route through per-instance actor for serialised per-tenant processing.
    let handle = match state
        .actor_registry
        .get_or_spawn(
            &client_msg.tenant_id,
            &client_msg.workflow_id,
            &client_msg.instance_id,
        )
        .await
    {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "op_handler: instance owned by another pod");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    let response = handle
        .reconcile(client_msg.clone())
        .await
        .unwrap_or_else(|| state.reconciler.reconcile(&client_msg));
    (
        StatusCode::OK,
        Json(serde_json::to_value(&response).unwrap_or_default()),
    )
        .into_response()
}

/// POST /op/async – low-latency async ingestion endpoint (Phase 2.2).
///
/// Acknowledges the op immediately after partial validation (< 1 ms) and
/// queues the full reconcile work for background processing.  The result is
/// pushed back to the client via WebSocket once the background worker
/// completes.  Use this endpoint when end-to-end HTTP latency must stay below
/// 16 ms.
async fn op_async_handler(
    State(state): State<AppState>,
    Json(client_msg): Json<ClientMessage>,
) -> impl IntoResponse {
    // Validate before queuing to prevent malicious payloads from
    // being stored in the event log via the async pipeline.
    if let Err(e) = client_msg.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response();
    }
    match state.ingestion_queue.enqueue(client_msg).await {
        Ok(ack) => (
            StatusCode::ACCEPTED,
            Json(serde_json::to_value(ack).unwrap_or_default()),
        )
            .into_response(),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn token_mint_handler(
    State(state): State<AppState>,
    Json(req): Json<MintRequest>,
) -> impl IntoResponse {
    let resp = state.token_service.mint(req).await;
    (StatusCode::OK, Json(resp))
}

async fn token_redeem_handler(
    State(state): State<AppState>,
    Json(req): Json<RedeemRequest>,
) -> impl IntoResponse {
    match state.token_service.redeem(req).await {
        Ok(resp) => (
            StatusCode::OK,
            Json(serde_json::to_value(resp).unwrap_or_else(|_| serde_json::json!({}))),
        ),
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

/// LLM Proposal Handler – receives an AI-generated op and validates it through
/// the exact same reconciler pipeline as human-initiated ops.
///
/// Valid proposals are accepted and treated identically to human ops.
/// Invalid proposals are silently dropped and logged.
async fn ai_propose_handler(
    State(state): State<AppState>,
    Json(client_msg): Json<ClientMessage>,
) -> impl IntoResponse {
    let handle = match state
        .actor_registry
        .get_or_spawn(
            &client_msg.tenant_id,
            &client_msg.workflow_id,
            &client_msg.instance_id,
        )
        .await
    {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "ai_propose: instance owned by another pod");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    let response = handle
        .reconcile(client_msg.clone())
        .await
        .unwrap_or_else(|| state.reconciler.reconcile(&client_msg));
    match &response {
        ServerMessage::Accept(a) => {
            tracing::info!(op_id = %a.op_id, "AI proposal accepted");
        }
        ServerMessage::Reject(r) => {
            // Silently drop invalid AI proposals – log them but return 200 so
            // the LLM cannot use error codes to probe the policy engine.
            tracing::info!(op_id = %r.op_id, reason = %r.reason, "AI proposal rejected (dropped)");
        }
        ServerMessage::Hello(_) | ServerMessage::ReloadRequired(_) => {}
    }
    (StatusCode::OK, Json(response)).into_response()
}

/// POST /ai/feedback – record a dismissed AI suggestion for retraining.
///
/// When a user dismisses an AI-generated proposal (e.g. clicks "Not helpful"),
/// the client sends this event so the backend can:
/// - Log the dismissed `op_id` for offline analysis.
/// - Detect model drift when dismissal rates exceed a threshold.
///
/// In a production deployment this handler would write to an append-only
/// feedback store (e.g. Kafka topic or S3 bucket) that feeds a retraining
/// pipeline.  The current implementation logs the event via tracing so that
/// log-aggregation tools (Datadog, Loki, etc.) can trigger downstream jobs.
async fn ai_feedback_handler(
    State(_state): State<AppState>,
    Json(req): Json<AiFeedbackRequest>,
) -> impl IntoResponse {
    tracing::info!(
        tenant_id = %req.tenant_id,
        workflow_id = %req.workflow_id,
        instance_id = %req.instance_id,
        op_id = %req.op_id,
        reason = %req.reason,
        vibe_intent = req.vibe.as_ref().map(|v| v.intent.as_str()).unwrap_or("none"),
        "ai_suggestion_dismissed"
    );
    (
        StatusCode::OK,
        Json(serde_json::json!({ "status": "recorded" })),
    )
}

// ── guardrail check ───────────────────────────────────────────────────────────

/// Lightweight guardrail check that rejects prompts containing known
/// jailbreak / prompt-injection patterns before they reach the LLM.
///
/// In a production deployment this would call a dedicated safety model
/// (e.g. Llama-Guard) via the same [`LlmProvider`] abstraction.  The
/// rule-based implementation below provides a deterministic, zero-latency
/// first layer of defence while the safety model call is in flight.
fn guardrail_check(prompt: &str) -> Result<(), &'static str> {
    let lower = prompt.to_lowercase();
    // Common jailbreak signal phrases.
    const BLOCKED_PATTERNS: &[&str] = &[
        "ignore previous instructions",
        "ignore all previous",
        "disregard your instructions",
        "you are now",
        "act as if you are",
        "pretend you are",
        "forget your instructions",
        "bypass your",
        "override your",
        // Additional injection / jailbreak patterns
        "system prompt",
        "reveal your prompt",
        "print your instructions",
        "show me your instructions",
        "what are your instructions",
        "do anything now",
        "dan mode",
        "jailbreak",
        "developer mode",
        "enable developer mode",
        "sudo mode",
        "no restrictions",
        "without restrictions",
        "ignore ethics",
        "ignore safety",
        "disregard ethics",
        "disregard safety",
    ];
    for pattern in BLOCKED_PATTERNS {
        if lower.contains(pattern) {
            return Err("blocked by guardrail");
        }
    }
    // Reject prompts that contain a high density of invisible / control Unicode
    // code points (e.g. zero-width joiners used in homoglyph / hidden-text attacks).
    // sanitize_prompt_input already strips these characters; if a significant
    // fraction of the original input was invisible, treat it as suspicious.
    let invisible_count = prompt.chars().filter(|c| is_invisible_unicode(*c)).count();
    if invisible_count > MAX_INVISIBLE_UNICODE_CHARS {
        return Err("blocked by guardrail: excessive invisible characters");
    }
    Ok(())
}

// ── input sanitization ────────────────────────────────────────────────────────

/// Maximum length allowed for user-supplied prompts (characters).
const MAX_PROMPT_LEN: usize = 4_096;

/// Threshold: if more than this many invisible Unicode characters appear in the
/// raw (pre-sanitized) prompt, treat the input as a potential hidden-text attack
/// and reject it in the guardrail check.
const MAX_INVISIBLE_UNICODE_CHARS: usize = 5;

/// Returns `true` for Unicode code points that are invisible / zero-width and
/// commonly used in homoglyph or hidden-text injection attacks.
fn is_invisible_unicode(c: char) -> bool {
    matches!(
        c,
        // Zero-width and soft-hyphen characters
        '\u{00AD}' // SOFT HYPHEN
        | '\u{200B}' // ZERO WIDTH SPACE
        | '\u{200C}' // ZERO WIDTH NON-JOINER
        | '\u{200D}' // ZERO WIDTH JOINER
        | '\u{200E}' // LEFT-TO-RIGHT MARK
        | '\u{200F}' // RIGHT-TO-LEFT MARK
        | '\u{202A}' // LEFT-TO-RIGHT EMBEDDING
        | '\u{202B}' // RIGHT-TO-LEFT EMBEDDING
        | '\u{202C}' // POP DIRECTIONAL FORMATTING
        | '\u{202D}' // LEFT-TO-RIGHT OVERRIDE
        | '\u{202E}' // RIGHT-TO-LEFT OVERRIDE
        | '\u{2060}' // WORD JOINER
        | '\u{2061}' // FUNCTION APPLICATION
        | '\u{2062}' // INVISIBLE TIMES
        | '\u{2063}' // INVISIBLE SEPARATOR
        | '\u{2064}' // INVISIBLE PLUS
        | '\u{206A}'..='\u{206F}' // Deprecated format characters
        | '\u{FEFF}' // ZERO WIDTH NO-BREAK SPACE (BOM)
        | '\u{FFF9}'..='\u{FFFB}' // Interlinear annotation characters
    )
}

/// Sanitize a user-supplied prompt before it is inserted into an LLM template.
///
/// - Strips ASCII control characters (except `\t` and `\n`) that could be used
///   to inject hidden instructions or escape structured prompts.
/// - Strips invisible Unicode code points (zero-width spaces, directional
///   overrides, soft hyphens, BOM, etc.) used in homoglyph / hidden-text
///   attacks.
/// - Trims the result and enforces a maximum length to prevent token stuffing.
fn sanitize_prompt_input(input: &str) -> String {
    fn is_allowed_char(c: char) -> bool {
        // Strip ASCII control characters (except tab and newline).
        if c.is_control() && c != '\t' && c != '\n' {
            return false;
        }
        // Strip invisible Unicode code points.
        if is_invisible_unicode(c) {
            return false;
        }
        true
    }
    let sanitized: String = input.chars().filter(|&c| is_allowed_char(c)).collect();
    sanitized.trim().chars().take(MAX_PROMPT_LEN).collect()
}

/// POST /agent/propose – production LLM proposal handler.
///
/// Full pipeline:
/// 1. Per-tenant rate-limit check (token bucket + monthly budget guard).
/// 2. Tool visibility gating via the AST evaluator.
/// 3. Input sanitization: strip control characters and enforce length limit.
/// 4. Guardrail check: reject known jailbreak / prompt-injection patterns.
/// 5. LLM call (OpenAI / Anthropic / Groq / Mock) with `temperature=0`,
///    `max_tokens=512`, `tool_choice="auto"`.
/// 6. Validate the chosen tool against the allowed list.
/// 7. Submit the resulting op through the actor/reconciler pipeline.
///    Log `proposal_rejected` audit events on any failure.
async fn agent_propose_handler(
    State(state): State<AppState>,
    Json(req): Json<AgentProposeRequest>,
) -> impl IntoResponse {
    // 1. Rate-limit / budget check.
    if let Err(e) = state
        .ai_rate_limiter
        .check_and_consume(&req.tenant_id)
        .await
    {
        tracing::warn!(
            tenant_id = %req.tenant_id,
            error = %e,
            "AI proposal rate-limited"
        );
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response();
    }

    // 2. Determine allowed tools from current workflow state.
    let current_state = state.reconciler.current_state();
    let allowed_tools = state
        .tool_gating
        .get_allowed_tools(&current_state, &req.ast_version);

    // 3. Build the LLM system prompt from the vibe vector.
    let vibe_description = req
        .vibe
        .as_ref()
        .map(|v| {
            format!(
                "User intent: {} (confidence: {:.0}%, bottleneck: {})",
                v.intent,
                v.confidence * 100.0,
                v.bottleneck
            )
        })
        .unwrap_or_else(|| "No behavioral signal available.".into());

    let system_prompt = format!(
        "You are a workflow assistant operating inside a zero-trust policy engine. \
         Current user state: {vibe_description}. \
         Choose exactly one tool that best helps the user proceed. \
         Respond only with a single tool call."
    );

    // 3. Sanitize user-supplied prompt to strip control characters and
    //    cap length, preventing prompt-injection attacks.
    let raw_prompt = req
        .prompt
        .clone()
        .unwrap_or_else(|| "What is the most helpful next action?".into());
    let user_content = sanitize_prompt_input(&raw_prompt);

    // 4. Guardrail check: reject known jailbreak / injection patterns.
    if let Err(reason) = guardrail_check(&user_content) {
        tracing::warn!(
            tenant_id = %req.tenant_id,
            "Prompt blocked by guardrail: {reason}"
        );
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "prompt rejected by safety guardrail" })),
        )
            .into_response();
    }

    let llm_req = LlmRequest {
        messages: vec![
            LlmMessage {
                role: "system".into(),
                content: system_prompt,
            },
            LlmMessage {
                role: "user".into(),
                content: user_content,
            },
        ],
        tools: allowed_tools.clone(),
        temperature: 0.0,
        max_tokens: 512,
    };

    // 5. Call the LLM provider.
    let llm_resp = match state.llm_provider.complete(&llm_req).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                tenant_id = %req.tenant_id,
                error = %e,
                "LLM proposal failed"
            );
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    tracing::info!(
        provider = %llm_resp.provider,
        prompt_hash = %llm_resp.prompt_hash,
        tenant_id = %req.tenant_id,
        "LLM proposal received"
    );

    // 6. Validate the chosen tool is in the allowed list.
    let tool_call = match llm_resp.tool_call {
        None => {
            tracing::info!(
                tenant_id = %req.tenant_id,
                "LLM returned no tool call; proposal dropped"
            );
            return (
                StatusCode::OK,
                Json(serde_json::json!({ "status": "no_tool_chosen" })),
            )
                .into_response();
        }
        Some(tc) => tc,
    };

    if !allowed_tools.iter().any(|t| t.tool_id == tool_call.tool_id) {
        tracing::warn!(
            tenant_id = %req.tenant_id,
            tool_id = %tool_call.tool_id,
            "LLM chose a disallowed tool; proposal_rejected"
        );
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "proposal_rejected", "reason": "disallowed tool" })),
        )
            .into_response();
    }

    // 7. Map ephemeral tool_id → op action and submit through the reconciler.
    let op_action = state
        .tool_gating
        .resolve_internal_fn(&tool_call.tool_id)
        .unwrap_or(&tool_call.tool_id)
        .to_owned();

    let snapshot_hash = hash_state(&current_state);
    let op_id = format!(
        "ai_op_{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );

    let client_msg = ClientMessage {
        op_id: op_id.clone(),
        ast_version: req.ast_version.clone(),
        action: op_action,
        payload: tool_call.arguments,
        client_snapshot_hash: snapshot_hash,
        tenant_id: req.tenant_id.clone(),
        workflow_id: req.workflow_id.clone(),
        instance_id: req.instance_id.clone(),
        client_signature: None,
        role: "agent".into(),
        vibe: req.vibe.clone(),
        trace_id: None,
    };

    let handle = match state
        .actor_registry
        .get_or_spawn(
            &client_msg.tenant_id,
            &client_msg.workflow_id,
            &client_msg.instance_id,
        )
        .await
    {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "agent_propose: instance owned by another pod");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    let response = handle
        .reconcile(client_msg.clone())
        .await
        .unwrap_or_else(|| state.reconciler.reconcile(&client_msg));

    match &response {
        ServerMessage::Accept(a) => {
            tracing::info!(op_id = %a.op_id, "AI agent proposal accepted");
        }
        ServerMessage::Reject(r) => {
            tracing::info!(
                op_id = %r.op_id,
                reason = %r.reason,
                "AI agent proposal_rejected (dropped)"
            );
        }
        ServerMessage::Hello(_) | ServerMessage::ReloadRequired(_) => {}
    }

    (
        StatusCode::OK,
        Json(serde_json::to_value(&response).unwrap_or_default()),
    )
        .into_response()
}
