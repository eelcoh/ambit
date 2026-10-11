#!/usr/bin/env bash
# Runs the capability conformance suite inside a Hermit image under QEMU.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/hermit-env.sh

hermit_build -p ambit-server-hermit --example conformance
image=target/$HERMIT_TARGET/image/examples/conformance

# Hermit writes 1 to isa-debug-exit on success (QEMU status 3) and 0 on failure (status 1).
set +e
timeout 30 bash -c "source scripts/hermit-env.sh; hermit_qemu $image" | grep -E 'conformance|panicked'
status=${PIPESTATUS[0]}
set -e
if [ "$status" -eq 3 ]; then echo "hermit conformance: OK"; else echo "hermit conformance: FAILED (qemu status $status)"; exit 1; fi
