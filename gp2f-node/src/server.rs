//! GP2FServer Node.js binding.
//!
//! Wraps a lightweight Axum HTTP server that exposes the GP2F workflow engine
//! to the host Node.js application.  TypeScript callers register
//! [`JsWorkflow`] definitions and then call `start()` to bring the server
//! online.
//!
//! ## Endpoints
//! | Method | Path              | Description                             |
//! |--------|-------------------|-----------------------------------------|
//! | GET    | `/health`         | Health-check (returns `"ok"`).          |
//! | POST   | `/workflow/run`   | Execute the next activity of a workflow.|
//! | POST   | `/workflow/dry-run` | Evaluate policies without side-effects.|

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use napi::bindgen_prelude::*;
use napi_derive::napi;
use serde::Deserialize;
use tokio::sync::oneshot;

use crate::workflow::JsWorkflow;

// ── Server configuration ──────────────────────────────────────────────────────

/// Configuration for [`JsGP2FServer`].
#[napi(object)]
pub struct JsServerConfig {
    /// TCP port the server should listen on.  Defaults to `3000`.
    pub port: Option<u16>,
    /// Hostname / bind address.  Defaults to `"127.0.0.1"`.
    pub host: Option<String>,
}

// ── Shared application state ──────────────────────────────────────────────────

/// Stored workflow data: ordered list of `(name, ActivityEntry)` pairs.
type StoredWorkflow = Vec<(String, crate::workflow::ActivityEntry)>;
type WorkflowRegistry = Arc<RwLock<HashMap<String, StoredWorkflow>>>;

#[derive(Clone)]
struct AppState {
    workflows: WorkflowRegistry,
}

// ── Request / response wire types ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RunRequest {
    workflow_id: String,
    instance_id: String,
    tenant_id: String,
    activity_index: usize,
    state: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct DryRunRequest {
    workflow_id: String,
    state: serde_json::Value,
}

// ── Route handlers ────────────────────────────────────────────────────────────

async fn health_handler() -> &'static str {
    "ok"
}

async fn run_handler(
    State(state): State<AppState>,
    Json(req): Json<RunRequest>,
) -> impl IntoResponse {
    let registry = state.workflows.read().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "workflow registry lock poisoned" })),
        )
    });
    let registry = match registry {
        Ok(r) => r,
        Err(e) => return e,
    };
    let Some(activities) = registry.get(&req.workflow_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "workflow not found" })),
        );
    };

    let Some((name, entry)) = activities.get(req.activity_index) else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "activity index out of bounds" })),
        );
    };

    let result = policy_core::Evaluator::new()
        .evaluate(&req.state, &entry.policy);

    match result {
        Ok(eval) => {
            if eval.result {
                let state_json = serde_json::to_string(&req.state)
                    .unwrap_or_else(|_| "null".to_owned());
                crate::workflow::invoke_on_execute(
                    entry,
                    crate::workflow::JsExecutionContext {
                        instance_id: req.instance_id.clone(),
                        tenant_id: req.tenant_id.clone(),
                        activity_name: name.clone(),
                        state_json,
                    },
                );
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "accepted": eval.result,
                    "workflowId": req.workflow_id,
                    "activityIndex": req.activity_index,
                })),
            )
        }
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

async fn dry_run_handler(
    State(state): State<AppState>,
    Json(req): Json<DryRunRequest>,
) -> impl IntoResponse {
    let registry = state.workflows.read().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "workflow registry lock poisoned" })),
        )
    });
    let registry = match registry {
        Ok(r) => r,
        Err(e) => return e,
    };
    let Some(activities) = registry.get(&req.workflow_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "workflow not found" })),
        );
    };

    let evaluator = policy_core::Evaluator::new();
    let mut all_pass = true;
    for (_, entry) in activities {
        match evaluator.evaluate(&req.state, &entry.policy) {
            Ok(r) if !r.result => {
                all_pass = false;
                break;
            }
            Err(_) => {
                all_pass = false;
                break;
            }
            _ => {}
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "allPass": all_pass,
            "workflowId": req.workflow_id,
        })),
    )
}

// ── Server class ──────────────────────────────────────────────────────────────

/// The GP2F server.
///
/// Hosts an Axum-backed HTTP server that makes the GP2F workflow engine
/// accessible from Node.js and other HTTP clients.
///
/// Example (TypeScript):
/// ```typescript
/// import { GP2FServer, Workflow } from '@gp2f/server';
///
/// const server = new GP2FServer({ port: 3000 });
///
/// const wf = new Workflow('my-workflow');
/// wf.addActivity('step1', { policy: { kind: 'LITERAL_TRUE' } });
///
/// server.register(wf);
/// await server.start();
/// // ... later:
/// await server.stop();
/// ```
#[napi]
pub struct JsGP2FServer {
    port: u16,
    host: String,
    workflows: WorkflowRegistry,
    /// Sender used to request graceful shutdown; wrapped in Mutex so that
    /// `start` / `stop` can be `&self` (napi-rs requires interior mutability
    /// for async methods).
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
}
#[napi]
impl JsGP2FServer {
    /// Create a new server instance.
    ///
    /// The server is not started until [`JsGP2FServer::start`] is called.
    #[napi(constructor)]
    pub fn new(config: Option<JsServerConfig>) -> Self {
        let (port, host) = config
            .map(|c| {
                (
                    c.port.unwrap_or(3000),
                    c.host.unwrap_or_else(|| "127.0.0.1".to_owned()),
                )
            })
            .unwrap_or((3000, "127.0.0.1".to_owned()));

        Self {
            port,
            host,
            workflows: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: Mutex::new(None),
        }
    }

    /// Register a workflow definition with the server.
    ///
    /// Must be called before [`JsGP2FServer::start`].  Workflows can be
    /// registered at any time, even while the server is running.
    #[napi]
    pub fn register(&self, workflow: &JsWorkflow) -> Result<()> {
        // Clone the activity list – ActivityEntry is Clone (ThreadsafeFunction
        // is reference-counted and safe to clone).
        let activities: StoredWorkflow = workflow.activities.clone();
        self.workflows
            .write()
            .map_err(|_| {
                napi::Error::new(
                    napi::Status::GenericFailure,
                    "workflow registry lock poisoned",
                )
            })?
            .insert(workflow.workflow_id.clone(), activities);
        Ok(())
    }

    /// Start the HTTP server asynchronously.
    ///
    /// Resolves once the TCP listener is bound and the server is ready to
    /// accept connections.
    #[napi]
    pub async fn start(&self) -> Result<()> {
        {
            let guard = self.shutdown_tx.lock().map_err(|_| {
                napi::Error::new(
                    napi::Status::GenericFailure,
                    "shutdown handle lock poisoned",
                )
            })?;
            if guard.is_some() {
                return Err(napi::Error::new(
                    napi::Status::GenericFailure,
                    "server is already running",
                ));
            }
        }

        let state = AppState {
            workflows: Arc::clone(&self.workflows),
        };

        let app = Router::new()
            .route("/health", get(health_handler))
            .route("/workflow/run", post(run_handler))
            .route("/workflow/dry-run", post(dry_run_handler))
            .with_state(state);

        let addr = format!("{}:{}", self.host, self.port);
        let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| {
            napi::Error::new(napi::Status::GenericFailure, format!("bind failed: {e}"))
        })?;

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        *self.shutdown_tx.lock().map_err(|_| {
            napi::Error::new(
                napi::Status::GenericFailure,
                "shutdown handle lock poisoned",
            )
        })? = Some(shutdown_tx);

        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .ok();
        });

        Ok(())
    }

    /// Stop the server gracefully.
    ///
    /// Sends a shutdown signal to the Axum server and waits for it to drain
    /// in-flight requests.
    #[napi]
    pub async fn stop(&self) -> Result<()> {
        let tx = self
            .shutdown_tx
            .lock()
            .map_err(|_| {
                napi::Error::new(
                    napi::Status::GenericFailure,
                    "shutdown handle lock poisoned",
                )
            })?
            .take();
        if let Some(sender) = tx {
            let _ = sender.send(());
        }
        Ok(())
    }

    /// Returns `true` while the server is running.
    #[napi(getter)]
    pub fn is_running(&self) -> bool {
        self.shutdown_tx
            .lock()
            .map(|g| g.is_some())
            .unwrap_or(false)
    }

    /// The port the server is (or will be) listening on.
    #[napi(getter)]
    pub fn port(&self) -> u16 {
        self.port
    }
}
