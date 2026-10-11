#!/usr/bin/env bash
# Black-box acceptance tests over HTTP, the same for every backend. Human-owned.
# Usage: scripts/acceptance.sh native|hermit
set -euo pipefail
cd "$(dirname "$0")/.."
backend=${1:?usage: $0 native|hermit}
base=http://localhost:8080
log=$(mktemp)

case $backend in
  native)
    source scripts/env.sh
    in_build_env "cargo build -q --locked -p run-native"
    ./target/debug/run-native >"$log" 2>&1 & ;;
  hermit)
    source scripts/hermit-env.sh
    hermit_build -q -p run-hermit
    hermit_qemu target/$HERMIT_TARGET/image/run-hermit >"$log" 2>&1 & ;;
  *) echo "unknown backend: $backend"; exit 2 ;;
esac
pid=$!
# Cleanup must not change the exit status (set -e also applies inside the trap).
trap 'kill $pid 2>/dev/null || true; pkill -P $pid 2>/dev/null || true; rm -f "$log"' EXIT

for _ in $(seq 100); do curl -s -o /dev/null -m 0.2 $base/ && break; sleep 0.1; done

failures=0
check() { # name, expected, actual
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1: expected '$2', got '$3'"; failures=$((failures + 1)); fi
}
status() { curl -s -o /dev/null -w '%{http_code}' "$@"; }

short=$(curl -s -X POST --data 'https://example.com/a?b=c' $base/links)
check "create returns a short url under base_url" "$base/" "${short%???????}"
check "short link redirects" 302 "$(status "$short")"
check "redirect target" "https://example.com/a?b=c" "$(curl -s -o /dev/null -w '%{redirect_url}' "$short")"
check "unknown id" 404 "$(status $base/zzzzzzz)"
check "unknown route" 404 "$(status $base/nope/nope)"
check "invalid url" 400 "$(status -X POST --data 'javascript:alert(1)' $base/links)"
check "body over 1 MiB" 413 "$(head -c 2000000 /dev/zero | status -X POST --data-binary @- $base/links)"
check "headers over 16 KiB" 431 "$(status -H "X-Big: $(head -c 20000 /dev/zero | tr '\0' a)" $base/)"
check "chunked body" 501 "$(echo https://x.org | status -X POST -H 'Transfer-Encoding: chunked' --data-binary @- $base/links)"

# Not `wait` without arguments: that would also wait for the server.
codes=$(mktemp)
pids=()
for i in $(seq 20); do
  # One write per result (-w with the newline), so concurrent appends don't interleave.
  curl -s -o /dev/null -w '%{http_code}\n' -X POST --data "https://example.com/$i" $base/links >>"$codes" &
  pids+=($!)
done
wait "${pids[@]}"
check "20 concurrent creates" "20" "$(grep -cx 201 "$codes")"
rm -f "$codes"

if [ $failures -gt 0 ]; then
  echo "acceptance ($backend): $failures failed"; echo "--- log"; cat "$log"; exit 1
fi
echo "acceptance ($backend): OK"
