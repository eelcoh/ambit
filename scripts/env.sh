# Sourced by the scripts. Locally, Rust builds run in the distrobox; in CI (or inside the
# distrobox) they run directly.
in_build_env() { # command...
  if [ -z "${CI:-}" ] && [ -z "${CONTAINER_ID:-}" ] && command -v distrobox >/dev/null; then
    distrobox enter build-machine -- bash -lc "export PATH=\$HOME/.cargo/bin:\$PATH; cd '$PWD' && $*"
  else
    bash -c "$*"
  fi
}
