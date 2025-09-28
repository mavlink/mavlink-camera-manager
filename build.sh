#!/usr/bin/env bash
set -e

cd $(dirname "$0")
DIRNAME="$PWD"

cd "$DIRNAME"
cargo run --package=bindings "$@"

cd "$DIRNAME"
cargo build "$@"

# To run: cargo run -- --help
