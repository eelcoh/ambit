#!/usr/bin/env bash
# Everything CI runs on the host. Must pass before merge. Human-owned.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
# The apps' lint levels come from here, not from their own (agent-owned) source: a forbid on the
# command line can't be relaxed by an #[allow] or by deleting the #![forbid] in src/lib.rs.
# The disallowed lists are apps/clippy.toml, which clippy finds for every crate under apps/.
for manifest in apps/*/Cargo.toml; do
  cargo clippy --manifest-path "$manifest" --lib --locked -- -D warnings \
    -F unsafe_code -F clippy::disallowed_methods -F clippy::disallowed_types \
    -F clippy::print_stdout -F clippy::print_stderr
done
cargo test --workspace --locked
scripts/check-app.py
# Level 2: per-crate API permissions (cackle.toml), dependencies included.
cargo acl -n --fail-on-warnings
