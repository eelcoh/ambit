#!/usr/bin/env bash
# Everything CI runs on the host. Must pass before merge. Human-owned.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
# The app's lint levels come from here, not from its own (agent-owned) source: a forbid on the
# command line can't be relaxed by an #[allow] or by deleting the #![forbid] in app/src/lib.rs.
cargo clippy -p app --lib --locked -- -D warnings \
  -F unsafe_code -F clippy::disallowed_methods -F clippy::disallowed_types \
  -F clippy::print_stdout -F clippy::print_stderr
cargo test --workspace --locked
scripts/check-app.py
