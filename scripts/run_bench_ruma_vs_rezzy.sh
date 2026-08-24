#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(dirname "$SCRIPT_DIR")"
REZZY_DIR="$(dirname "$REPO_DIR")/rezzy"

if [ ! -d "$REZZY_DIR" ]; then
    echo "Error: rezzy repository not found at $REZZY_DIR" >&2
    exit 1
fi

echo "=== Running Matrix State Resolution Shootout (ruma-state-res vs rezzy) ==="
cargo run --manifest-path "$REZZY_DIR/Cargo.toml" --bin bench_ruma_vs_rezzy --features mock-ruma --release
