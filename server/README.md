# GP2F Server

Axum-based reconciliation server for the GP2F platform.

## Overview
This repository manages the core reconciliation, broadcast routing, CRDT structures, and the Rust-based executable serving the primary API logic for the platform. It integrates closely with the `gp2f-core`, `poly-core`, and other services inside the workspace.

## Features
- **Redis Broadcast**: Pub/Sub broadcasting integration using Tokio & Redis
- **Temporal Production**: Real-time integration and worker configuration via `temporalio-client`
- **Wasmtime Engine**: Execution of isolated policies via webassembly `wasmtime`

## Run
```bash
cargo run --release -p gp2f-server
```

## License
MIT
