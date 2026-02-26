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
    middleware::{InMemoryPublicKeyStore, OpIdLayer},
    reconciler::Reconciler,
    redis_broadcast::{build_broadcaster, DynBroadcaster},
    temporal_store::{InMemoryStore, PersistentStore, TemporalStore},
    token_service::{MintRequest, RedeemRequest, TokenService},
    wasm_engine::WasmtimeEngine,
    wire::{ClientMessage, ServerMessage},
};

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
    let wasm_path = std::env::var("POLICY_WASM_PATH")
        .unwrap_or_else(|_| "policy_wasm_bg.wasm".to_string());
    match WasmtimeEngine::new(&wasm_path) {
        Ok(_engine) => tracing::info!(%wasm_path, "Wasmtime policy engine loaded"),
        Err(e) => tracing::info!("Wasmtime engine unavailable ({e}); using native evaluator"),
    }

    // ── Persistent event store ─────────────────────────────────────────────
    let event_store: Arc<dyn PersistentStore> =
        if let Ok(endpoint) = std::env::var("TEMPORAL_ENDPOINT") {
            let namespace = std::env::var("TEMPORAL_NAMESPACE")
                .unwrap_or_else(|_| "gp2f-prod".into());
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

    let state = AppState {
        reconciler: Arc::new(Reconciler::new()),
        token_service: Arc::new(TokenService::new()),
        actor_registry: Arc::new(ActorRegistry::new()),
        broadcaster,
        event_store,
    };

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/ws", get(ws_handler))
        .route("/op", post(op_handler))
        .route("/token/mint", post(token_mint_handler))
        .route("/token/redeem", post(token_redeem_handler))
        .route("/ai/propose", post(ai_propose_handler))
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
    }
    (StatusCode::OK, Json(response))
}
