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
    reconciler::Reconciler,
    token_service::{MintRequest, RedeemRequest, TokenService},
    wire::{ClientMessage, ServerMessage},
};

#[derive(Clone)]
struct AppState {
    reconciler: Arc<Reconciler>,
    token_service: Arc<TokenService>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let state = AppState {
        reconciler: Arc::new(Reconciler::new()),
        token_service: Arc::new(TokenService::new()),
    };

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/ws", get(ws_handler))
        .route("/op", post(op_handler))
        .route("/token/mint", post(token_mint_handler))
        .route("/token/redeem", post(token_redeem_handler))
        .with_state(state)
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
    // Subscribe to broadcast channel so we can push server-initiated messages.
    let mut broadcast_rx = state.reconciler.broadcaster().subscribe();

    loop {
        tokio::select! {
            // Incoming message from the WebSocket client
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                            let response = state.reconciler.reconcile(&client_msg);
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
    Json(state.reconciler.reconcile(&client_msg))
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
