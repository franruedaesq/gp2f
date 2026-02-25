use clap::{Parser, Subcommand};
use policy_core::{AstNode, Evaluator, VersionPolicy};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fs, path::PathBuf, process};

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
