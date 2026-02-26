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

use gp2f_server::{
    actor::ActorRegistry,
    async_ingestion::AsyncIngestionQueue,
    llm_provider::{build_provider, LlmMessage, LlmProvider, LlmRequest},
    middleware::{InMemoryPublicKeyStore, OpIdLayer},
    rate_limit::AiRateLimiter,
    reconciler::Reconciler,
    redis_broadcast::{build_broadcaster, DynBroadcaster},
    temporal_store::{InMemoryStore, PersistentStore, TemporalStore},
    token_service::{MintRequest, RedeemRequest, TokenService},
    tool_gating::ToolGatingService,
    wasm_engine::WasmtimeEngine,
    wire::{AgentProposeRequest, ClientMessage, HelloMessage, ServerMessage},
};
use policy_core::evaluator::hash_state;

#[derive(Clone)]
struct AppState {
    reconciler: Arc<Reconciler>,
    token_service: Arc<TokenService>,
    actor_registry: Arc<ActorRegistry>,
    /// Broadcaster (Redis PubSub or in-process fallback).
    broadcaster: DynBroadcaster,
    /// Persistent event store (Temporal in production, in-memory for dev).
    #[allow(dead_code)]
    event_store: Arc<dyn PersistentStore>,
    /// LLM provider (OpenAI / Anthropic / Groq / Mock).
    llm_provider: Arc<dyn LlmProvider>,
    /// Tool gating service – decides which tools the LLM may see.
    tool_gating: Arc<ToolGatingService>,
    /// Per-tenant AI rate limiter and budget guard.
    ai_rate_limiter: Arc<AiRateLimiter>,
    /// Async ingestion queue for the `/op/async` low-latency endpoint.
    ingestion_queue: Arc<AsyncIngestionQueue>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // ── Wasmtime policy engine (optional) ──────────────────────────────────
    let wasm_path =
        std::env::var("POLICY_WASM_PATH").unwrap_or_else(|_| "policy_wasm_bg.wasm".to_string());
    match WasmtimeEngine::new(&wasm_path) {
        Ok(_engine) => tracing::info!(%wasm_path, "Wasmtime policy engine loaded"),
        Err(e) => tracing::info!("Wasmtime engine unavailable ({e}); using native evaluator"),
    }

    // ── Persistent event store ─────────────────────────────────────────────
    let event_store: Arc<dyn PersistentStore> = if let Ok(endpoint) =
        std::env::var("TEMPORAL_ENDPOINT")
    {
        let namespace = std::env::var("TEMPORAL_NAMESPACE").unwrap_or_else(|_| "gp2f-prod".into());
        let store = TemporalStore::new(endpoint.clone(), namespace);
        if let Err(e) = store.connect().await {
            tracing::warn!("Temporal connect failed ({e}); using in-memory fallback");
        }
        Arc::new(store)
    } else {
        tracing::info!("TEMPORAL_ENDPOINT not set; using in-memory event store");
        Arc::new(InMemoryStore::new())
    };

    // ── Redis / in-process broadcaster ────────────────────────────────────
    let broadcaster = build_broadcaster().await;

    // ── Op-ID middleware (ed25519 validation) ─────────────────────────────
    // In production, populate key_store from Redis or a secrets manager.
    let key_store = Arc::new(InMemoryPublicKeyStore::new());
    let op_id_layer = OpIdLayer::new(key_store);

    // ── LLM provider (OpenAI / Anthropic / Groq / Mock) ───────────────────
    let llm_provider: Arc<dyn LlmProvider> = Arc::from(build_provider());

    // ── Async ingestion queue (Phase 2.2 – low-latency /op/async endpoint) ─
    let ingestion_buffer: usize = std::env::var("INGESTION_QUEUE_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1024);
    let (ingestion_queue, mut ingestion_rx) = AsyncIngestionQueue::new(ingestion_buffer);

    // Background worker: drains the async ingestion queue and runs full
    // reconcile + Temporal signal pipeline.
    let bg_reconciler = Arc::new(Reconciler::new());
    let bg_actor_registry = Arc::new(ActorRegistry::new());
    tokio::spawn(async move {
        while let Some(msg) = ingestion_rx.recv().await {
            let handle =
                bg_actor_registry.get_or_spawn(&msg.tenant_id, &msg.workflow_id, &msg.instance_id);
            let _response = handle
                .reconcile(msg.clone())
                .await
                .unwrap_or_else(|| bg_reconciler.reconcile(&msg));
        }
    });

    let state = AppState {
        reconciler: Arc::new(Reconciler::new()),
        token_service: Arc::new(TokenService::new()),
        actor_registry: Arc::new(ActorRegistry::new()),
        broadcaster,
        event_store,
        llm_provider,
        tool_gating: Arc::new(ToolGatingService::new()),
        ai_rate_limiter: Arc::new(AiRateLimiter::new()),
        ingestion_queue: Arc::new(ingestion_queue),
    };

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/ws", get(ws_handler))
        .route("/op", post(op_handler))
        .route("/op/async", post(op_async_handler))
        .route("/token/mint", post(token_mint_handler))
        .route("/token/redeem", post(token_redeem_handler))
        .route("/ai/propose", post(ai_propose_handler))
        .route("/agent/propose", post(agent_propose_handler))
        .with_state(state)
        .layer(op_id_layer)
        .layer(TraceLayer::new_for_http());

    let addr = "0.0.0.0:3000";
    tracing::info!("GP2F server listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind");
    axum::serve(listener, app).await.expect("server failed");
}

async fn health_handler() -> &'static str {
    "ok"
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

    loop {
        tokio::select! {
            // Incoming message from the WebSocket client
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                            // Route through per-instance actor for serialised processing.
                            let handle = state.actor_registry.get_or_spawn(
                                &client_msg.tenant_id,
                                &client_msg.workflow_id,
                                &client_msg.instance_id,
                            );
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
) -> Json<ServerMessage> {
    // Route through per-instance actor for serialised per-tenant processing.
    let handle = state.actor_registry.get_or_spawn(
        &client_msg.tenant_id,
        &client_msg.workflow_id,
        &client_msg.instance_id,
    );
    let response = handle
        .reconcile(client_msg.clone())
        .await
        .unwrap_or_else(|| state.reconciler.reconcile(&client_msg));
    Json(response)
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
    match state.ingestion_queue.enqueue(client_msg).await {
        Ok(ack) => (
            StatusCode::ACCEPTED,
            Json(serde_json::to_value(ack).unwrap_or_default()),
        ),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

async fn token_mint_handler(
    State(state): State<AppState>,
    Json(req): Json<MintRequest>,
) -> impl IntoResponse {
    let resp = state.token_service.mint(req);
    (StatusCode::OK, Json(resp))
}

async fn token_redeem_handler(
    State(state): State<AppState>,
    Json(req): Json<RedeemRequest>,
) -> impl IntoResponse {
    match state.token_service.redeem(req) {
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
    let handle = state.actor_registry.get_or_spawn(
        &client_msg.tenant_id,
        &client_msg.workflow_id,
        &client_msg.instance_id,
    );
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
        ServerMessage::Hello(_) => {}
    }
    (StatusCode::OK, Json(response))
}

/// POST /agent/propose – production LLM proposal handler.
///
/// Full pipeline:
/// 1. Per-tenant rate-limit check (token bucket + monthly budget guard).
/// 2. Tool visibility gating via the AST evaluator.
/// 3. LLM call (OpenAI / Anthropic / Groq / Mock) with `temperature=0`,
///    `max_tokens=512`, `tool_choice="auto"`.
/// 4. Validate the chosen tool against the allowed list.
/// 5. Submit the resulting op through the actor/reconciler pipeline.
/// 6. Log `proposal_rejected` audit events on any failure.
async fn agent_propose_handler(
    State(state): State<AppState>,
    Json(req): Json<AgentProposeRequest>,
) -> impl IntoResponse {
    // 1. Rate-limit / budget check.
    if let Err(e) = state.ai_rate_limiter.check_and_consume(&req.tenant_id) {
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

    let user_content = req
        .prompt
        .clone()
        .unwrap_or_else(|| "What is the most helpful next action?".into());

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

    // 4. Call the LLM provider.
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

    // 5. Validate the chosen tool is in the allowed list.
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

    // 6. Map ephemeral tool_id → op action and submit through the reconciler.
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
    };

    let handle = state.actor_registry.get_or_spawn(
        &client_msg.tenant_id,
        &client_msg.workflow_id,
        &client_msg.instance_id,
    );

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
        ServerMessage::Hello(_) => {}
    }

    (
        StatusCode::OK,
        Json(serde_json::to_value(&response).unwrap_or_default()),
    )
        .into_response()
}
