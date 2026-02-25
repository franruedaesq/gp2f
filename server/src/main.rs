use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use gp2f_server::{
    reconciler::Reconciler,
    wire::{ClientMessage, ServerMessage},
};

#[derive(Clone)]
struct AppState {
    reconciler: Arc<Reconciler>,
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
    };

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/ws", get(ws_handler))
        .route("/op", post(op_handler))
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
    while let Some(msg) = socket.recv().await {
        if let Ok(Message::Text(text)) = msg {
            if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                let response = state.reconciler.reconcile(&client_msg);
                let reply = serde_json::to_string(&response).unwrap_or_default();
                let _ = socket.send(Message::Text(reply)).await;
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
