#!/usr/bin/env bash
set -euo pipefail

SCRATCH_DIR=".tmp/roaring_bench"
mkdir -p "$SCRATCH_DIR/src"

cat <<EOF > "$SCRATCH_DIR/Cargo.toml"
[package]
name = "roaring_bench"
version = "0.1.0"
edition = "2021"

[dependencies]
roaring = "0.11.4"

[workspace]
EOF

cp scripts/bench_auth_difference.rs "$SCRATCH_DIR/src/main.rs"

cargo run --manifest-path "$SCRATCH_DIR/Cargo.toml" --release
