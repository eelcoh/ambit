#!/usr/bin/env bash
# Fails if app's normal (non-dev) dependency tree has a crate not in app/allowed-deps.txt.
# Also fails on allowlisted crates that are no longer used, so the list stays the real one.
set -euo pipefail
cd "$(dirname "$0")/.."

actual=$(cargo tree -p app -e normal --prefix none --format '{p}' | awk '{print $1}' | grep -vx app | sort -u)
allowed=$(grep -v '^#' app/allowed-deps.txt | sed '/^$/d' | sort -u)
extra=$(comm -23 <(echo "$actual") <(echo "$allowed"))
stale=$(comm -13 <(echo "$actual") <(echo "$allowed"))

if [ -n "$extra" ]; then
    echo "app depends on crates not in app/allowed-deps.txt:"
    echo "$extra"
fi
if [ -n "$stale" ]; then
    echo "app/allowed-deps.txt lists crates app no longer uses:"
    echo "$stale"
fi
[ -z "$extra" ] && [ -z "$stale" ] || exit 1
echo "app dependencies: OK"
