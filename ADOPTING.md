# Using ambit in a project

For a project (e.g. an existing terminal app) that wants ambit's platform model and its checks.
Read DESIGN.md for the why; this is the how.

## Layout
```
apps/<name>/                 the app: all logic, no ambient authority     (agent-owned)
apps/<name>/allowed-deps.txt every crate the app may pull in               (human-owned)
run-<name>/                  wiring: which capabilities, with which scope  (human-owned)
spec/                        tests on the app, against the sim backends   (human-owned)
cackle.toml, .github/        checks and review rules                       (human-owned)
```
The app is a library crate with an `init(...) -> impl App` (tui) or `-> impl Handler` (server)
whose parameters are its capabilities. The wiring is a small binary that calls `init` with real
implementations and hands the result to the platform's `run`/`serve`.

## Depend on ambit
By git, one commit for all ambit crates, over https (that's what the checks trust):
```toml
[workspace.dependencies]
ambit-caps       = { git = "https://github.com/eelcoh/ambit", rev = "<commit>" }
ambit-tui        = { git = "https://github.com/eelcoh/ambit", rev = "<commit>" }
ambit-tui-native = { git = "https://github.com/eelcoh/ambit", rev = "<commit>" }   # wiring only
ambit-tui-sim    = { git = "https://github.com/eelcoh/ambit", rev = "<commit>" }   # spec only
```
Copy ambit's `rust-toolchain.toml`: the checks rely on clippy behaving the same.

## Set up the checks
```sh
cargo install --locked --git https://github.com/eelcoh/ambit --rev <commit> cargo-ambit
cargo install --locked cargo-acl --version 0.9.0      # needs bubblewrap (bwrap) on Linux
cargo ambit init --owner @your-github-handle          # CODEOWNERS, CI workflow, cackle.toml
cargo ambit check                                     # lists what each app pulls in
```
Then, once, as the human: review what `check` reports and put it in each
`apps/<name>/allowed-deps.txt`; run `cargo acl` to review and accept what the platform and the
dependencies may do. After that, `cargo ambit check` must pass before every merge; CI runs it
with the cargo-ambit from the same ambit commit as `Cargo.lock`.

## What `cargo ambit check` enforces
- fmt, clippy (`-D warnings`), tests, all with `--locked`.
- For every crate under `apps/`: no filesystem, network, processes, environment, threads,
  clock or stdout from std; no `unsafe`. The rules are built into cargo-ambit, and the lint
  levels come from its command line, so the app's own source can't relax them.
- For every app: no build script, no proc-macro crate; every dependency in its
  `allowed-deps.txt` (and nothing stale there); every dependency from crates.io, ambit's own
  repository or the workspace; every directory under `apps/` is a workspace member.
- cackle: per-crate API permissions on the compiled code, dependencies included.

## Protecting the human-owned files
CODEOWNERS only counts with branch protection on `main` ("Require review from Code Owners"),
and only if the agent pushes under its own GitHub identity: an agent using your credentials is
you, and you can't review yourself.
