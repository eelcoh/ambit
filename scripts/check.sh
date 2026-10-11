#!/usr/bin/env bash
# Everything CI runs on the host. Must pass before merge. Human-owned.
# The checks themselves are cargo-ambit (tools/cargo-ambit), the same tool other projects use.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo run -q --locked -p cargo-ambit -- check
