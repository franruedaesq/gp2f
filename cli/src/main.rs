use clap::{Parser, Subcommand};
use policy_core::{AstNode, Evaluator, VersionPolicy};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Parser)]
#[command(
    name = "gp2f",
    about = "GP2F – Generic Policy & Prediction Framework CLI",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Evaluate a policy AST against a state document.
    Eval {
        /// Path to the JSON state file.
        #[arg(long)]
        state: PathBuf,

        /// Path to the JSON-encoded AST policy file.
        #[arg(long)]
        policy: PathBuf,

        /// Expected AST version (semver). Rejects if the policy version
        /// does not match.
        #[arg(long)]
        version: Option<String>,
    },

    /// Replay the event history for an instance, reconstructing state at
    /// every accepted op and stepping through AST evaluations.
    ///
    /// Reads a JSON events file produced by the server's event store and
    /// optionally a policy file to re-evaluate at each step.
    ///
    /// Example:
    ///   gp2f replay --events events.json --policy policy.json --op-id op-42
    Replay {
        /// Path to a JSON file containing an array of [`ReplayEvent`]s.
        #[arg(long)]
        events: PathBuf,

        /// Path to the JSON-encoded AST policy file (optional).
        /// When supplied, the policy is evaluated at each step and the trace
        /// is printed alongside the state diff.
        #[arg(long)]
        policy: Option<PathBuf>,

        /// Stop replay at this op_id and show a side-by-side client/server
        /// view.  When omitted, all events are replayed.
        #[arg(long)]
        op_id: Option<String>,
    },

    /// Inject intentionally conflicting operations for testing (Spoofer).
    ///
    /// Generates a pair of [`ClientMessage`] JSON objects that target the
    /// same field with different values from the same base snapshot hash.
    /// Pipe the output into your test harness or WebSocket client to
    /// reproduce concurrent-edit scenarios deterministically.
    ///
    /// Example:
    ///   gp2f spoof --field amount --value-a 100 --value-b 200 --hash abc123
    Spoof {
        /// The JSON field name to conflict on (e.g. `amount`).
        #[arg(long)]
        field: String,

        /// The value that client A proposes (JSON literal, e.g. `100`).
        #[arg(long)]
        value_a: String,

        /// The value that client B proposes (JSON literal, e.g. `200`).
        #[arg(long)]
        value_b: String,

        /// The base snapshot hash both clients believe the server is at.
        /// Defaults to the hash of an empty object when omitted.
        #[arg(long, default_value = "")]
        hash: String,

        /// Tenant identifier to embed in the messages.
        #[arg(long, default_value = "spoof-tenant")]
        tenant_id: String,

        /// Workflow identifier to embed in the messages.
        #[arg(long, default_value = "spoof-wf")]
        workflow_id: String,

        /// Instance identifier to embed in the messages.
        #[arg(long, default_value = "spoof-inst")]
        instance_id: String,
    },
}

// ── replay event format ───────────────────────────────────────────────────────

/// Minimal event record expected in the `--events` JSON file.
///
/// This mirrors the `StoredEvent` type in `gp2f-server` but is re-defined
/// here so the CLI has no dependency on the server crate.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReplayEvent {
    seq: u64,
    #[serde(default)]
    ingested_at: String,
    message: ReplayMessage,
    outcome: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReplayMessage {
    op_id: String,
    #[serde(default)]
    action: String,
    #[serde(default)]
    payload: Value,
    #[serde(default)]
    client_snapshot_hash: String,
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Eval {
            state,
            policy,
            version,
        } => cmd_eval(state, policy, version),

        Commands::Replay {
            events,
            policy,
            op_id,
        } => cmd_replay(events, policy, op_id),

        Commands::Spoof {
            field,
            value_a,
            value_b,
            hash,
            tenant_id,
            workflow_id,
            instance_id,
        } => cmd_spoof(
            field,
            value_a,
            value_b,
            hash,
            tenant_id,
            workflow_id,
            instance_id,
        ),
    }
}

// ── eval command ──────────────────────────────────────────────────────────────

fn cmd_eval(state: PathBuf, policy: PathBuf, version: Option<String>) {
    let state_str = read_file(&state);
    let policy_str = read_file(&policy);

    let state_value: serde_json::Value = match serde_json::from_str(&state_str) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: failed to parse state JSON: {e}");
            process::exit(1);
        }
    };

    let ast_node: AstNode = match serde_json::from_str(&policy_str) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("error: failed to parse policy JSON: {e}");
            process::exit(1);
        }
    };

    // Version check
    if let Some(required_version) = version {
        let node_version = ast_node.version.as_deref().unwrap_or("");
        let vp = VersionPolicy::new([required_version.as_str()]);
        if let Err(e) = vp.check(node_version) {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }

    let evaluator = Evaluator::new();
    match evaluator.evaluate(&state_value, &ast_node) {
        Ok(result) => {
            println!("result:  {}", result.result);
            println!("hash:    {}", result.snapshot_hash);
            println!("trace:");
            for (i, entry) in result.trace.iter().enumerate() {
                println!("  [{i}] {entry}");
            }
            if !result.result {
                process::exit(2);
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}

// ── replay command ────────────────────────────────────────────────────────────

fn cmd_replay(events_path: PathBuf, policy_path: Option<PathBuf>, target_op_id: Option<String>) {
    let events_str = read_file(&events_path);
    let events: Vec<ReplayEvent> = match serde_json::from_str(&events_str) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: failed to parse events JSON: {e}");
            process::exit(1);
        }
    };

    let policy: Option<AstNode> = match policy_path {
        Some(ref p) => {
            let s = read_file(p);
            match serde_json::from_str(&s) {
                Ok(n) => Some(n),
                Err(e) => {
                    eprintln!("error: failed to parse policy JSON: {e}");
                    process::exit(1);
                }
            }
        }
        None => None,
    };

    // Reconstruct authoritative state by applying accepted ops in order.
    let mut server_state: serde_json::Map<String, Value> = serde_json::Map::new();
    let evaluator = Evaluator::new();
    let mut found_target = false;

    println!("── GP2F Deterministic Replay ─────────────────────────────────────");

    for event in &events {
        let is_accepted = event.outcome.to_uppercase() == "ACCEPTED";

        println!(
            "\n[seq={}] op_id={} outcome={}",
            event.seq, event.message.op_id, event.outcome
        );

        // Show client payload (what the client proposed).
        println!(
            "  client payload: {}",
            serde_json::to_string_pretty(&event.message.payload)
                .unwrap_or_else(|_| "null".into())
                .lines()
                .collect::<Vec<_>>()
                .join("\n               ")
        );

        if is_accepted {
            // Apply accepted payload to server state (shallow merge).
            if let Value::Object(patch) = &event.message.payload {
                for (k, v) in patch {
                    server_state.insert(k.clone(), v.clone());
                }
            }
        }

        // Show server state after applying this event.
        println!(
            "  server state:  {}",
            serde_json::to_string_pretty(&Value::Object(server_state.clone()))
                .unwrap_or_else(|_| "{}".into())
                .lines()
                .collect::<Vec<_>>()
                .join("\n               ")
        );

        // Optionally evaluate the policy at this state.
        if let Some(ref ast) = policy {
            let state_val = Value::Object(server_state.clone());
            match evaluator.evaluate(&state_val, ast) {
                Ok(result) => {
                    println!("  policy result: {}", result.result);
                    println!("  policy trace:");
                    for (i, entry) in result.trace.iter().enumerate() {
                        println!("    [{i}] {entry}");
                    }
                }
                Err(e) => {
                    println!("  policy error:  {e}");
                }
            }
        }

        // Stop if we've reached the target op_id.
        if let Some(ref target) = target_op_id {
            if &event.message.op_id == target {
                found_target = true;
                println!("\n── Reached target op_id={target} ────────────────────────────────");
                break;
            }
        }
    }

    if let Some(ref target) = target_op_id {
        if !found_target {
            eprintln!("error: op_id '{target}' not found in events file");
            process::exit(1);
        }
    }

    println!("\n── Replay complete ───────────────────────────────────────────────");
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn read_file(path: &PathBuf) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("error: cannot read '{}': {e}", path.display());
        process::exit(1);
    })
}

// ── spoof command ─────────────────────────────────────────────────────────────

/// A minimal `ClientMessage`-shaped struct for the spoofer output.
///
/// Mirrors the wire format used by the server so the output can be fed
/// directly into a WebSocket test harness without modification.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpoofMessage {
    op_id: String,
    ast_version: String,
    action: String,
    payload: Value,
    client_snapshot_hash: String,
    tenant_id: String,
    workflow_id: String,
    instance_id: String,
}

#[allow(clippy::too_many_arguments)]
fn cmd_spoof(
    field: String,
    value_a: String,
    value_b: String,
    hash: String,
    tenant_id: String,
    workflow_id: String,
    instance_id: String,
) {
    // Parse the JSON values; fall back to JSON strings if the input is not
    // valid JSON (so bare words like `hello` are treated as string literals).
    let parse_val = |raw: &str| -> Value {
        serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
    };

    let val_a = parse_val(&value_a);
    let val_b = parse_val(&value_b);

    // Use epoch-millis as a simple unique prefix for op_ids.
    let epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let msg_a = SpoofMessage {
        op_id: format!("spoof-{epoch_ms}-a"),
        ast_version: "1.0.0".into(),
        action: "update".into(),
        payload: serde_json::json!({ &field: val_a }),
        client_snapshot_hash: hash.clone(),
        tenant_id: tenant_id.clone(),
        workflow_id: workflow_id.clone(),
        instance_id: instance_id.clone(),
    };

    let msg_b = SpoofMessage {
        op_id: format!("spoof-{epoch_ms}-b"),
        ast_version: "1.0.0".into(),
        action: "update".into(),
        payload: serde_json::json!({ &field: val_b }),
        client_snapshot_hash: hash,
        tenant_id,
        workflow_id,
        instance_id,
    };

    let output = serde_json::json!([msg_a, msg_b]);
    match serde_json::to_string_pretty(&output) {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("error: failed to serialize spoof messages: {e}");
            process::exit(1);
        }
    }
}
