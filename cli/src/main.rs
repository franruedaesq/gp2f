use clap::{Parser, Subcommand};
use policy_core::{AstNode, Evaluator, VersionPolicy};
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
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Eval {
            state,
            policy,
            version,
        } => {
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
    }
}

fn read_file(path: &PathBuf) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("error: cannot read '{}': {e}", path.display());
        process::exit(1);
    })
}
