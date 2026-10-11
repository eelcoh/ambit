# Using ambit in a project

For a project (e.g. an existing terminal app) that wants ambit's platform model and its checks.
Read DESIGN.md for the why; this is the how. The first real migration was `sonos-tui` (its
`ambit` branch); the recipe and the gaps below come from it.

## Layout
```
apps/<name>/                 the app: all logic, no ambient authority     (agent-owned)
apps/<name>/allowed-deps.txt every crate the app may pull in               (human-owned)
run-<name>/                  wiring: which capabilities, with which scope  (human-owned)
spec/                        tests on the app, against the sim backends   (human-owned)
caps/<cap>/                  capabilities ambit doesn't have yet           (human-owned)
cackle.toml, .github/        checks and review rules                       (human-owned)
```
The app is a library crate with an `init(...) -> impl App` (tui) or `-> impl Handler` (server)
whose parameters are its capabilities. The wiring is a small binary that calls `init` with real
implementations and hands the result to the platform's `run`/`serve`.

## Depend on ambit
By git, one commit for all ambit crates, over https (that's what the checks trust):
```toml
[workspace.dependencies]
ambit-caps            = { git = "https://github.com/eelcoh/ambit", rev = "<commit>" }
ambit-caps-net        = { git = "https://github.com/eelcoh/ambit", rev = "<commit>" }   # if it uses the network
ambit-tui             = { git = "https://github.com/eelcoh/ambit", rev = "<commit>" }
ambit-tui-native      = { git = "https://github.com/eelcoh/ambit", rev = "<commit>" }   # wiring only
ambit-caps-net-native = { git = "https://github.com/eelcoh/ambit", rev = "<commit>" }   # wiring only
ambit-tui-sim         = { git = "https://github.com/eelcoh/ambit", rev = "<commit>" }   # spec only
ambit-caps-sim        = { git = "https://github.com/eelcoh/ambit", rev = "<commit>" }   # spec: SimHttp, SimDiscovery, SimWebSocket, SimDir

# Match ambit's versions and features, or the tree gets two ratatuis.
ratatui-widgets = { version = "0.3.2", default-features = false }
```
Copy ambit's `rust-toolchain.toml`: the checks rely on clippy behaving the same.

## Set up the checks
```sh
cargo install --locked --git https://github.com/eelcoh/ambit --rev <commit> cargo-ambit
cargo install --locked cargo-acl --version 0.9.0      # needs bubblewrap (bwrap) on Linux
cargo ambit init --owner @your-github-handle          # CODEOWNERS, CI workflow, cackle.toml
cargo ambit check                                     # lists what each app pulls in
```
(To try it without touching `~/.cargo/bin`: `cargo install --root <dir> ...` and put
`<dir>/bin` first on `PATH`.)

Then, once, as the human: review what `check` reports and put it in each
`apps/<name>/allowed-deps.txt`; run `cargo acl` to review and accept what the platform and the
dependencies may do. After that, `cargo ambit check` must pass before every merge; CI runs it
with the cargo-ambit from the same ambit commit as `Cargo.lock`.

`cargo ambit check` runs in this order and stops at the first failure: fmt, clippy on the
workspace, clippy on each app with the app rules, tests, the app checks (allowlist, sources),
cackle. So whether the app holds any ambient authority (level 1) is settled before the
allowlist and cackle come up.

## What `cargo ambit check` enforces
- fmt, clippy (`-D warnings`), tests, all with `--locked`.
- For every crate under `apps/`: no filesystem, network, processes, environment, threads,
  clock or stdout from std; no `unsafe`. The rules are built into cargo-ambit, and the lint
  levels come from its command line, so the app's own source can't relax them.
- For every app: no build script, no proc-macro crate; every dependency in its
  `allowed-deps.txt` (and nothing stale there); every dependency from crates.io, ambit's own
  repository or the workspace; every directory under `apps/` is a workspace member.
- cackle: per-crate API permissions on the compiled code, dependencies included.

## Migrating an existing app
In this order; each step leaves something that builds.

1. **Branch, then make it a workspace.** Move the sources into `apps/<name>/src/` (`git mv`, so
   history follows), turn `main.rs` into `lib.rs`, and start `run-<name>/` with the old
   argument parsing. Keep the old `Cargo.lock`: it pins the versions you were running.
2. **Inventory the ambient authority.** `grep -n` for `std::fs`, `fs::`, `std::net`, `UdpSocket`,
   `TcpListener`, `reqwest` and other clients, `std::env`, `std::process`, `thread::`,
   `Instant::now`, `SystemTime::now`, `rand::`, `println!`. Each hit becomes a capability, moves
   to the wiring, or goes away. For sonos-tui:

   | was                                             | became                                    |
   |-------------------------------------------------|-------------------------------------------|
   | reqwest to speakers, Spotify, SMAPI             | three `Http`s, one per purpose, each with its own scope and timeout |
   | UDP multicast search                            | `Discovery` with one target               |
   | JSON files (catalogue, token), atomic rename    | `Kv` keys; the store is atomic per `put`  |
   | reading tags under a mounted share              | `Dir` (not in ambit yet, see below)       |
   | `SystemTime::now` for token expiry              | `Clock`                                   |
   | `rand::random` for PKCE                         | `Random`                                  |
   | effect worker thread, timer in the main loop    | `Cmd::Task`, `Cmd::After`                 |
   | `XDG_DATA_HOME`, `HOME`, arguments, env var     | wiring                                    |
   | OAuth: print a URL, loopback listener           | wiring; the rest stays in the app as functions taking capabilities |

3. **Let `init`'s parameters say it.** One parameter per kind of authority, named for its
   purpose (`speakers`, `spotify`), not its type. Splitting `Http` by purpose costs nothing and
   makes the scopes reviewable one at a time. Settings that aren't authority go in a `Config`.
   Clippy's `too_many_arguments` fires at eight; allow it on `init`: the long signature *is*
   the declaration.
4. **Effects become tasks.** If the app already has an Elm-style `update -> Vec<Effect>`, keep
   it and write an interpreter from `Effect` to `Cmd::task(...)`. What changes:
   - Native runs tasks on 4 workers, concurrently. If the old code ran effects one at a time
     on a single worker, and its state assumes that, put that state behind one
     `Arc<Mutex<_>>` the tasks lock. Erased to `Arc<Mutex<dyn Run>>`, it also keeps the app
     struct free of capability generics.
   - A worker that sent several messages for one effect (progress) becomes a chain: each task
     does one step and returns a message carrying the rest; `update` starts the next.
   - A periodic tick is a `Cmd::After` from `start`, set again each time it fires.
5. **State files become `Kv` keys.** Keep the stored format (JSON bytes under one key) so the
   wiring can import the old files once, unchanged, when the key is missing. Load lazily inside
   a task, and leave it unloaded after a failed parse so nothing overwrites data the app can't
   read. If the store holds a secret, the wiring sets the file to `0600`.
6. **The view draws into a buffer.** `frame.render_widget(w, area)` becomes
   `w.render(area, buf)` (`ambit_tui::widgets::Widget`); `frame.area()` is `view`'s `area`;
   `layout`, `style` and `text` come from `ambit_tui`, widgets from `ratatui_widgets::{block,
   list, paragraph, gauge, ...}`. Unit tests that used `TestBackend` render into
   `Buffer::empty(Rect::new(0, 0, w, h))`.
7. **Demo and fake modes.** Keep the app's fake backend, and have the wiring give it
   capabilities with empty scopes (`Scope::default()`, `Targets::default()`, a memory `Kv`):
   "demo never touches the network" becomes something the wiring shows, not a promise.
8. **Tests.** App unit tests that need real files take the native capability as a dev-dependency
   plus `tempfile`. Acceptance goes in `spec/`: one `SimHttp::handle` can play a whole device
   (match on the URI and the `SOAPACTION` header), with the same scopes as the wiring. Worth
   having from the start: "demo sends nothing", "an address outside the scope is refused and
   nothing is sent", and timer cadence with `advance`.
9. **Allowlist and cackle.** Run `cargo ambit check`, group what it reports by reason in
   `allowed-deps.txt`, and review it. For `cackle.toml`, start from ambit's own (the platform
   crates' permissions are already reviewed there), then run `cargo acl` for the rest. Cackle
   reports a build script's API use under the plain crate name; the path in the report
   (`.../build.rs`) says it belongs in `build.allow_apis`, not `allow_apis`.

   Two things to expect when copying ambit's `cackle.toml` (found moving sonos-tui to 81cad7b):
   - **Run-time grants are missing.** No binary in ambit wires `NativeDir`, `NativeAuthorize` or
     `NativeHttp`, so ambit's file grants their APIs only to tests (`test.allow_apis`,
     `from.test.allow_apis`). A project whose wiring uses them needs `fs` for
     `ambit-caps-dir-native`, `cap-std` and `cap-primitives`; `process` for
     `ambit-caps-net-native` (the browser for `Authorize`); `net` and `env` for `ureq` (`env`
     is its proxy-from-environment code, which `NativeHttp` switches off).
   - **Entries for crates you don't build fail the check.** With `--fail-on-warnings`, cackle
     rejects config for packages outside the tree (`cargo-ambit`, `run-todo`,
     `ambit-server-std`, ...) and grants nothing uses. Delete exactly what it names.
   Don't silence a name clash with `[api.*] no_auto_detect`: it switches detection off for that
   crate everywhere. Ambit's `[api.fs]` / `[api.process]` already include `rustix::fs` and
   `rustix::process`.

## When ambit lacks a capability
Write it in the project as a human-owned crate under `caps/`, in ambit's shape, so it can move
upstream unchanged: a trait crate the app depends on (no implementation, so the app's tree has
no `std::fs`), and a separate `-native` crate the wiring depends on. sonos-tui's `ambit-dir`
(`Dir`, `RelPath`) and `ambit-dir-native` are the example. The trait crate goes in the app's
`allowed-deps.txt`; the native one gets its cackle entry (`fs`).

## Gaps found by the sonos-tui migration
Resolved in ambit, as follows:
- **Initial size**: the first event is always `Event::Resize` with the current size, right after
  `start` and before any task result, on native and sim. Drop size workarounds (a `Cell`
  written from `view`).
- **`Dir`**: `ambit-caps-dir` (`Dir` reads: `metadata`, `open`, `list`; `RelPath` can't climb
  out. `DirWrite` adds: `create_dir`, `create_new`, which never replaces anything, and
  `available_space`. `DirModify` changes: `replace`, `remove_file`). Ask for the lowest level
  the app needs. `ambit-caps-dir-native` (cap-std: paths are resolved inside the tree, symlinks
  leading out are `Denied`, no check-then-open gap; new files are verified and renamed into
  place atomically), `ambit_caps_sim::SimDir` for tests (`space` to make a volume fill up,
  `read` to see what the app wrote).
- **OAuth**: `Authorize` in `ambit-caps-net`: opens a scoped URL in the browser and returns the
  loopback redirect's query (`NativeAuthorize::new(scope, port)`, `.without_browser()` for
  remote sessions; `SimAuthorize` echoes `state`). The app builds the URL and checks `state`;
  no listener in the wiring.
- **Serial effects**: `Cmd::Serial(task)` / `Cmd::serial(..)`: run one at a time, in the order
  asked, still off the UI thread. Replaces a `Mutex` around the backend.
- **A memory `Kv` outside sim**: `ambit_caps_std::MemKv` (re-exported by the native backends).
- Still open: **endpoints known only at run time** (SMAPI): list the ones you use; the rest
  fail as `Denied`. **Weight**: the allowlist makes it visible, not small.

## Protecting the human-owned files
CODEOWNERS only counts with branch protection on `main` ("Require review from Code Owners"),
and only if the agent pushes under its own GitHub identity: an agent using your credentials is
you, and you can't review yourself.
