#!/usr/bin/env bash
# Builds the example app as a Hermit image and boots it in QEMU. Then: curl localhost:8080
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/hermit-env.sh

hermit_build -p run-hermit
image=target/$HERMIT_TARGET/image/run-hermit
ls -l "$image"
exec bash -c "source scripts/hermit-env.sh; hermit_qemu $image"
