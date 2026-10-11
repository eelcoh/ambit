# Design notes

## Premise
Humans hardly read agent-written code anymore. So stop optimising code for human reading and
optimise for **guarantees**: the human reads a small, enforced contract; agents write and maintain
the implementation behind it. Trust comes from the compiler and the platform, not from review.

The core idea is **Roc's platform model, in Rust**: the app has no ambient authority, and all
effects come from capabilities that a platform hands in. Unikernels are one way to deploy this
(the minimal-attack-surface target), not the point of the project.

A second goal is **attack surface**. Most deployed software ships huge amounts of code, libraries
and binaries that nobody needs. If the capability set says exactly what an app can do, the deployed
artifact should contain only that, and the OS should enforce only that.

## Decisions so far
- **No new language.** Considered an ML-family language (Haskell/Elm/Roc style) built for agents;
  rejected because of the missing training data and toolchain. **Rust**: agents write it
  well, strong types, good compiler errors.
- **No ambient authority in the app.** A function's capability parameters are its effect signature.
- Clock and randomness are capabilities too, which gives deterministic tests.
- Contracts: doc tests as examples, `proptest` for properties; Verus later, maybe.

## Concepts
Two axes: **platform** (what the app is written against) and **backend** (where it runs).

### Capabilities
A shared library of capability traits, reused across platforms: `Clock`, `Random`, `Log`, `Kv`,
scoped `Fs`, `Outbound`, ... Each platform picks a subset.

### Platform = entry shape + capability set
As in Roc, each platform defines its own API, and apps target one platform. Platforms differ
mainly in how the platform calls the app. **The platform owns `main` and the event loop**:

| platform | entry shape                                                    |
|----------|----------------------------------------------------------------|
| `cli`    | `main(args, caps) -> exit code`                                |
| `server` | `handle(request, caps) -> response`                            |
| `tui`    | `update(model, event) -> (model, commands)`, `view(model) -> View` |
| `gui`    | same Elm shape as `tui`, with a different `View` vocabulary    |

Because the platform owns concurrency, async vs blocking becomes a backend detail and doesn't
leak into the app's contract.

For `tui`/`gui`, the app returns a description of the view and never touches the terminal or
window. `update` and `view` are pure: snapshot tests and deterministic replay of event sequences,
which agents can check without a screen. Effects are commands the platform runs with its
capabilities. No attempt to run the same UI app on both `tui` and `gui`; shared logic goes in a
plain library crate.

`gui`: don't invent a widget vocabulary. Adopt a toolkit whose model already fits (**iced** is
the Elm architecture; Slint and gpui are alternatives) and accept its view types as part of that
platform's contract.

### Backend = where a platform runs
Every backend implements its platform's **full** contract. Constrained environments get fewer
backends or their own platform, never a partial implementation.

| platform | backends                                                       |
|----------|----------------------------------------------------------------|
| `server` | native (dev/test), hermit (acceptance/prod), sim, maybe wasi   |
| `cli`    | native, sim                                                    |
| `tui`    | terminal (crossterm/ratatui), headless                         |
| `gui`    | native (Mac/Linux via the toolkit), headless                   |
| `browser`| wasm-web                                                       |

- **native**: Linux/Mac, for development, debugging, tests.
- **hermit**: **Hermit unikernel**. The same app crate deployed as a VM image with only the drivers
  the capabilities need (cf. MirageOS: app functorised over devices, Unix backend for dev,
  unikernel for prod). Fallback if Hermit falls short: Unikraft/Nanos running the Linux binary
  (deployment only, no link to the capability traits).
- **sim**: in-memory capabilities with deterministic clock, randomness and network.
  Deterministic simulation testing (TigerBeetle/FoundationDB style) almost for free.
- **headless**: renders `View` to a buffer for snapshot tests.

### Capabilities compile to the backend's isolation mechanism
The capability set the human reads is also the input for the backend's own sandboxing, so there
is defence in depth on every backend, not just the unikernel:

| backend          | capabilities become                              |
|------------------|--------------------------------------------------|
| hermit           | kernel features/drivers linked into the image    |
| wasi             | the component's imports                          |
| macOS            | App Sandbox entitlements                         |
| Linux desktop    | Flatpak permissions/portals                      |
| Linux native     | Landlock + seccomp profile                       |

With hermit, the capability list doubles as the bill of materials: no storage capability, no
virtio-fs driver in the image.

### Hermit: minimal by default
**Minimal is the main principle.** Never make the human choose what the capability list already
says. Two kinds of kernel configuration:
- **Capability-driven: derived.** Each hermit capability implementation is its own crate and
  depends on `hermit` with `default-features = false` plus exactly the features it needs (an HTTP
  server capability: `tcp`, network driver; a storage capability: `fs`/`virtio-fs`). Cargo unifies
  features over the dependency graph, and hermit's `build.rs` forwards exactly those to the kernel
  build, so the kernel gets the union for the capabilities actually wired in `run-hermit`. The
  dependency allowlist keeps `hermit` out of the apps, so an app can't add features.
- **Deployment-driven: a user choice, from a fixed set of profiles.** Depends on where the image
  runs, not on what the app does: `qemu` (PCI + ACPI, virtio-net over PCI), `firecracker`
  (virtio-mmio, no PCI/ACPI), `uhyve` (no device emulation). Plus DHCP vs static IP, SMP, memory.
  The user picks a profile, not individual features.
- **The human approves changes, not choices.** The attack-surface report lists enabled kernel
  features and drivers per build; CI shows the diff. A new capability pulling in `fs` shows up as
  "storage added".

Measured in the spike: hermit's default features (dns, udp, virtio-fs, smp, ...) triple the image.
Without them: 5.4 MB → 1.7 MB (0.56 MB gzipped), first response 0.44 s → 0.26 s, 64 MB → 48 MB RAM.

### User-granted capabilities (powerbox)
UI apps get capabilities from the user: a file picker returns a capability for that one file or
directory. Same model as macOS security-scoped bookmarks and the Flatpak document portal. The app
has no `Fs`, only "files the user handed me".

## Server platform API
```rust
// caps/: shared across platforms
pub trait Clock: Send + Sync { fn now(&self) -> SystemTime; fn elapsed(&self) -> Duration; }
pub trait Random: Send + Sync { fn fill(&self, buf: &mut [u8]); }
pub trait Kv: Send + Sync { get, put, delete, scan(prefix) }

// platforms/server/api
pub type Request = http::Request<Vec<u8>>;
pub type Response = http::Response<Vec<u8>>;
pub trait Handler: Send + Sync + 'static { fn handle(&self, req: Request) -> Response; }

// app: this signature is the app's whole capability declaration
pub fn init(clock: impl Clock, random: impl Random, kv: impl Kv, config: Config) -> impl Handler

// run-<backend>: wiring only
server_native::serve(app::init(NativeClock::new(), OsRandom, NativeKv::open(..)?, cfg), listen)
```
- **Capabilities are `init`'s parameters.** One line lists everything the app can do. If the agent
  adds a capability, the human-owned wiring stops compiling: widening authority requires a change
  to a protected file, enforced by the type system.
- **Generics (`impl Trait`), not `dyn`.** Zero cost; a capability a backend lacks is a compile
  error; sim plugs in other types. Internal functions take only what they use
  (`fn expire(clock: &impl Clock, kv: &impl Kv)`).
- **Synchronous handler, the platform owns concurrency** (`Handler: Send + Sync`): a thread pool
  on native, blocking on hermit. Async, if ever needed, is a different platform shape.
- **State lives in capabilities.** `handle(&self)` is read-only; mutable state goes through `Kv`.
- **`http` crate types**, bodies as `Vec<u8>`, no streaming for now.
- **Config is a typed struct** passed in by the wiring; `std::env` is forbidden in the app.
- **Logging: the `tracing` facade**, an explicit exception to "no ambient authority": write-only
  output to an operator-controlled sink. The platform owns the subscriber; tests capture with a
  scoped subscriber.
- `Random` must be a CSPRNG on real backends. On hermit: the kernel CSPRNG is ChaCha20 seeded
  from **RDSEED** (not RDRAND). Without RDSEED it silently falls back to a timestamp-seeded LCG and
  still reports success. So `HermitRandom::new()` fails closed: it checks CPUID at runtime and the
  wiring refuses to start. Not `is_x86_feature_detected!`: the hermit target enables `rdseed` as a
  compile-time feature, so that macro is constant `true`. QEMU needs `rdseed` in `-cpu`.
- hermit has no durable `Kv` until the storage spike. `EphemeralKv` exists, and wiring it is a
  visible choice in `run-hermit` to lose data on restart.
- **The platform bounds resources, not the app.** The server has a fixed worker pool with fixed
  stacks and a bounded accept queue (503 when full), per-read timeouts and a per-connection
  deadline, and hard limits on head (16 KiB, 431) and body (1 MiB, 413). Chunked bodies get 501.
  `tiny_http` spawned a thread per connection without limit, which crashed the 64 MB hermit VM at
  20 concurrent requests; the replacement is a small server on `httparse` (fewer dependencies too).

## TUI platform API
Same principles as the server: capabilities are `init`'s parameters, generics, the platform owns
`main`, the loop and concurrency. Shaped by two real apps to migrate: `sonos-tui` (ratatui, LAN
HTTP to speakers, SSDP discovery, Spotify OAuth) and `crossload` (ratatui + tears, book folders,
mounted e-readers, device over TCP/websocket, sqlite).

```rust
// platforms/tui/api
pub use ratatui_core::{buffer::Buffer, layout::Rect};   // and ratatui-widgets for apps
pub enum Event<M> { Key(KeyEvent), Paste(String), Resize(u16, u16), Msg(M) }
pub enum Cmd<M> { None, Quit, Batch(Vec<Cmd<M>>),
                  After(Duration, M),                               // timer, on the platform clock
                  Task(Box<dyn FnOnce() -> M + Send>),              // blocking work, off the UI thread
                  Serial(Box<dyn FnOnce() -> M + Send>) }           // the same, one at a time, in order
pub trait App: 'static {
    type Msg: Send + 'static;
    fn update(&mut self, event: Event<Self::Msg>) -> Cmd<Self::Msg>;
    fn view(&self, area: Rect, buf: &mut Buffer);
}

// apps/todo: the example
pub fn init(clock: impl Clock, kv: impl Kv) -> impl App
```
- **ratatui types are the view vocabulary**, as `http` is for the server. `view` draws into a
  `Buffer` (ratatui's `Widget` signature). The app depends on `ratatui-core`/`ratatui-widgets`, never
  on `crossterm`: it can't touch the terminal, raw mode or stdout.
- **`update` mutates the model in place** and returns commands. That's the Elm shape in Rust idiom
  (no model cloning).
- **Blocking work is a `Task`**: a closure that captures the capabilities it needs (`Arc`) and
  returns a message. The platform runs it on a bounded pool, so the UI never blocks on the network.
  No `std::thread` in the app.
- **Backends.** `native`: crossterm, raw mode and alternate screen, and the terminal is restored
  on exit and on panic. `sim`: scripted events, tasks run inline, timers advance `SimClock`, and
  the final `Buffer` is asserted as text. The same event script always gives the same screen, so
  UI tests are deterministic and need no terminal.
- **The first event is `Resize`** with the current size, right after `start` and before any
  task result, on every backend: an app never has to guess its size.
- **`Serial` tasks** run one at a time in the order asked, on a thread of their own; plain
  `Task`s keep running alongside. For effects that must not overlap (one device, one session).
- **`start`** returns the first commands (the todo app loads its list as a task), so the first
  frame can say "loading…" instead of blocking.
- **Logging** goes to a file the wiring picks: stderr would draw over the screen.
- **Ordering is part of the contract**, tested in `platforms/tui/sim/tests`: timers fire in
  deadline order (ties in the order they were set), each at its own time on the clock; a timer
  set from a timer counts from when that one fired; tasks run in the order asked; nothing runs
  after `Quit`. On native, tasks run concurrently on 4 workers, so results can arrive in any
  order: an app that cares keys its messages.
- **Weight**: ratatui brings 40 crates into an app's tree (25 at run time, 15 build-time proc
  macros), against 14 for the server example. Acceptable for native-only terminal apps; the
  allowlist makes every one a reviewed choice.
- **Capabilities in order of need.** First increment: `Clock`, `Random`, `Kv`. For the migrations:
  - `Http`, `Discovery` and `WebSocket`: built, see below.
  - `Dir`: built (`caps/dir`, `caps/dir-native` on cap-std, `SimDir`), in three levels so `init`
    shows what an app can do to the files it gets: `Dir` reads; `DirWrite` also adds files and
    directories but never changes or removes what's there (a book copied to a card can't
    overwrite another); `DirModify` also replaces and removes files. New files are written aside,
    flushed, read back and compared, then renamed into place without replacing (atomic, also on
    FAT and APFS). The powerbox shape for a terminal: the wiring picks the tree, from arguments
    or config.
  - `Authorize` (OAuth login in the browser, loopback redirect): built, in `caps/net`.

## Network capabilities
`caps/net` (traits and scopes), `caps/net-native`, and sim versions in `caps/sim`. Separate from
`ambit-caps`, so apps without network access don't carry the `http` crate.

```rust
pub trait Http: Send + Sync { fn send(&self, req: Request) -> Result<Response, HttpError>; }
pub trait Discovery: Send + Sync { fn ssdp(&self, target: &str, wait: Duration) -> Result<Vec<Device>, _>; }

// wiring: the scope is the reviewable part
let http = NativeHttp::new(Scope::new([
    Origin::lan(Scheme::Http, 1400),                  // any speaker, by IP, port 1400
    Origin::parse("https://api.spotify.com")?,
    Origin::parse("https://accounts.spotify.com")?,
]), HttpOptions::default());
let discovery = NativeDiscovery::new(Targets::new(["urn:schemas-upnp-org:device:ZonePlayer:1"]));
```
- **The scope is checked before anything is sent**, by every implementation, sim included: a test
  that reaches outside the scope fails with `Denied` just as production does.
- An origin is scheme + host + port: exact (`https://api.spotify.com`), subdomains
  (`https://*.scdn.co`, not the domain itself), or **the local network**: private and
  link-local IP literals only, never a name (a name can resolve anywhere), never loopback.
- **No redirects** (a 3xx comes back; following it is a new request, checked again), no proxy
  from the environment, no cookies. Response bodies over the limit (16 MiB default) fail
  `TooLarge`; one timeout covers the whole request (10 s default).
- `Discovery` only searches granted SSDP targets and only keeps answers from local-network
  addresses. Multicast TTL 1: the search never leaves the network.
- Native: ureq with rustls (ring) and the bundled Mozilla roots. Verified against the internet:
  real HTTPS works, an expired certificate is refused (`--ignored` test).
- Blocking by design: apps call them from a `Cmd::Task`.
- **`WebSocket`**: `connect(url)` gives a connection with `send`, `recv` (pings answered for
  the app) and `close`. Same scopes as `Http`, with `ws://` origins (`Origin::lan(Scheme::Ws,
  81)` for a reader on the local network); an `http` origin doesn't grant `ws` or the other way
  round. Native: tungstenite over a TCP connection the capability opens, with a connect
  timeout, a timeout per read and write, and a message size limit (64 KiB default). Sim:
  `SimWebSocket::serve` plays the server, one closure per connection. Encrypted `wss://` isn't
  supported yet.
- **`Authorize`**: the OAuth flow for native apps (RFC 8252). The app builds the provider's URL
  (with `state`, PKCE) and checks what comes back; the capability opens the URL only if it's in
  its scope (`https://accounts.spotify.com`), so an app can't use the browser to reach anywhere
  else, and listens on `127.0.0.1:<port>` only while waiting. Native: `xdg-open`/`open` with all
  output discarded (the terminal is the app's), or `.without_browser()`.
- Open for `sonos-tui`: the music-service endpoints (SMAPI) come from Sonos at run time, so
  they can't be listed in advance. Options: a subdomain scope per known service, or a scope
  that grows only with origins the speaker reported (needs a design).
- **Discovery on this machine finds nothing**, not even the router: firewalld's `public` zone
  drops the unicast answers to a multicast search (a Python search on the host shows the same).
  The speaker answers when addressed directly. A firewall rule is the user's call; a fixed
  source port for discovery would make that a single, narrow rule.

## Workspace shape (sketch)
```
caps/api/                  capability traits                           (human-owned)
caps/{std,native,sim}/     implementations, shared by all platforms    (human-owned)
caps/conformance/          tests every implementation must pass        (human-owned)
platforms/<p>/api/         entry shape: the contract                   (human-owned)
platforms/<p>/<backend>/   event loop, IO; re-exports the caps it runs (human-owned)
apps/<name>/               all logic, no ambient authority             (agent-owned)
tools/cargo-ambit/         the checks, for this repo and any other     (human-owned)
spec/                      properties and acceptance tests on the apps (human-owned)
run-<name>/                tiny binary crates: wiring only             (human-owned)
```
Capability implementations live apart from platforms: `NativeKv` is the same store for a server
and a terminal app.
One binary crate per backend rather than Cargo features: features unify badly across a workspace,
and backends need different target-specific dependencies.

## Enforcement
Applies to every crate under `apps/`. All of it is **`cargo ambit check`** (`tools/cargo-ambit`):
this repo runs it through `scripts/check.sh`, other projects install it at the same ambit
commit as their `Cargo.lock` (see ADOPTING.md).
1. **Lints**: `disallowed-methods`/`disallowed-types` (std::fs, std::net, std::process, std::env,
   SystemTime::now, thread::spawn, ...) from `tools/cargo-ambit/app-rules.toml`, **built into the
   tool** and handed to clippy through `CLIPPY_CONF_DIR`: a project has no copy an agent could
   edit, and an app's own `clippy.toml` is ignored (verified). The forbids are passed **on the clippy command line** (`-F`), not only
   via `#![forbid]` in the app's `src/lib.rs`: that file is agent-owned, and deleting the attribute plus an `#[allow]` silenced clippy
   completely. A command-line forbid can't be relaxed from source (E0453).
   **App checks** (via `cargo metadata`): an app has no build script and
   no proc-macro target; every crate reachable over normal and build edges is in its
   `allowed-deps.txt` (stale entries fail too); a directory under `apps/` that isn't a workspace
   member fails, so an app can't escape the checks; every dependency comes from crates.io,
   ambit's repository or the workspace, so an allowlisted name from elsewhere is rejected; `Cargo.lock` must match.
2. **cackle** (`cargo acl`, `cackle.toml`): per-crate API permissions on compiled code, for every
   crate including dependencies. The apps have no entry: no restricted API, no unsafe. It sees
   code that ends up in a binary; unreachable code doesn't, and the lints catch that anyway. Build scripts
   and tests run in Bubblewrap. Catches what level 1 can't see: a dependency's use of
   `std::net`, or code that slips past the lints.
3. **Runtime isolation**: the backend's own mechanism (see table above).

Levels 1 and 2 apply on **every** backend, native included, so dev can't quietly use what prod
doesn't allow.

**Human review surface** = everything except `apps/` (where each `allowed-deps.txt` stays
human-owned): `caps/`, platforms, conformance, `spec/`, `run-*`, `scripts/`,
`cackle.toml`, `Cargo.toml`/`Cargo.lock`, `rust-toolchain.toml`, `.github/`. Listed in
`.github/CODEOWNERS`. Protection:
- **Merge gate**: CODEOWNERS + branch protection ("require review from Code Owners"), plus CI.
  Only works if the agent pushes under **its own identity** (a bot account or GitHub App): the
  owner can't approve their own PR, and an agent using the owner's credentials *is* the owner.
- **Locally**: read-only mounts or sandbox write-deny rules for those paths. Not enabled in this
  repo, because here the platform itself is being built.
- A Claude Code `PreToolUse` hook only as fast feedback; it doesn't cover `sed -i`, `git checkout`,
  scripts, etc.

Capability signatures guarantee **authority**, not **correctness**. Correctness comes from `spec/`,
which the agent can't weaken.

## Engineering journey
Develop and test on the host, ship the minimal form:
- **dev**: native backend, `cargo run`, debugger, fast rebuilds.
- **test**: sim backend, deterministic.
- **acceptance**: hermit image (QEMU/uhyve), black-box suite over HTTP.
- **prod**: hermit image on Firecracker/uhyve.

Guarding against drift between backends ("works on native, breaks on Hermit"):
- Same level 1-2 enforcement on all backends.
- Conformance tests per capability, run against every backend.
- CI builds the app for every target of its platform on every change (Hermit's std has gaps:
  threads, DNS, some syscalls).
- The same black-box acceptance tests against the native binary and the hermit image.
- Observability (`Log`/tracing) is a capability with identical behaviour on all backends, because
  there is no debugger in prod.

For WASM, wasmtime on the host already runs the real artifact quickly; native is mainly for
debugger comfort.

## Attack surface
- Host side counts too: QEMU is a lot of device-emulation code next to a 1.2 MB image, so uhyve or
  Firecracker is part of the claim, not an optional extra.
- Measure it per build and backend: crates in the artifact, size/LOC per crate, drivers enabled,
  host VMM. Makes the argument concrete and catches dependency creep from agents.

## Open questions
- Capability granularity: narrow domain traits (`Payments`) give the best effect signatures but
  grow the human-owned platform code. Alternative: generic but scoped primitives (`Http` limited to
  a host allowlist, `Kv` limited to a namespace) wrapped into domain capabilities in the wiring, so
  the human reviews scopes, not implementations.
- `gui` toolkit choice.
- Threat model: a careless agent, or a compromised one (prompt injection via dependencies/docs)?
  Decides how hard levels 1-2 and the protection must be.

## Status
- `spikes/hermit-httpd/`: **works.** Blocking `tiny_http` server on Hermit 0.13.2 in QEMU/KVM.
  With minimal kernel features: ~0.26 s to first response, 0.56 MB gzipped image, 48 MB RAM.
  See its `NOTES.md` and `run.sh`.
- **Template workspace, `server` platform, native + sim: works.** `caps/`, `platforms/server/{api,
  native,sim}`, example `apps/shortener` (a link shortener using `Clock`, `Random`, `Kv`),
  `spec/`, `run-native/`. `scripts/check.sh` runs fmt, clippy, tests and the dependency check.
  - Level 1 verified: SystemTime::now, std::fs, File, TcpStream, Command, std::env, thread::spawn
    and println! in an app all fail clippy; `#[allow(clippy::disallowed_methods)]` is a hard error
    (E0453, conflicts with `forbid`).
  - `apps/shortener/allowed-deps.txt` covers the full normal tree including proc macros (`tracing-attributes`,
    `syn`, ...), because they run at build time with full authority. Stale entries also fail.
  - Native: redb for `Kv`, `getrandom`. Handler panics become 500.
  - Clippy only runs in `cargo clippy`, not `cargo build`: CI must run `scripts/check.sh`.
- **Enforcement levels 1 and 2 in CI** (`.github/workflows/ci.yml`): `check.sh` (fmt, clippy,
  app lints with command-line forbids, tests, the app checks, cackle), and acceptance on native
  and hermit (KVM on GitHub runners, Hermit std from hermit-os/rust-std-hermit). Verified bypass
  attempts, all rejected: `#![forbid]` removed + `#[allow]`; a `build.rs`; allowlisted name
  from a path; `TcpStream::connect` in an app (cackle: "uses disallowed API `net`").
- **`tui` platform, native + sim: works.** `platforms/tui/{api,native,sim}`, example `apps/todo`
  (`Clock`, `Kv`; storage as tasks, a status timer), `run-todo`, 12 spec tests on screens as
  text, 4 sim ordering tests. Native checked in a pseudo-terminal: items persist across runs, and
  raw mode, alternate screen, bracketed paste and the cursor are restored on quit. The same
  bypass checks hold for `todo` (E0453; cackle "uses disallowed API `net`").
- **`cargo ambit` (check + init): works**, here and in a throwaway project that depends on ambit
  from GitHub (init, allowlist, cackle, check passes). Bypass attempts against it rejected: an
  app's own permissive `clippy.toml`; `#![forbid]` removed + `#[allow]` (E0453).
- **The `sonos-tui` gaps, closed** (see ADOPTING.md): initial `Resize`, `Cmd::Serial`, `Dir` on
  cap-std (conformance on native and sim; escapes through symlinks are `Denied`), `Authorize`
  (loopback flow tested with a fake browser), `MemKv` in `caps/std`.
- **Network capabilities: built, not yet used by an app.** `Http`, `Discovery`, scopes; native
  (ureq + rustls, UDP) and sim. 7 scope tests, 4 native HTTP tests against a local server, 4 sim
  tests, SSDP answer parsing. Cackle permissions for the TLS stack (ring's C build, unsafe in
  rustls' dependencies).
- sim `MemKv` fault injection (`Faults::All`, `Faults::Writes`), with spec tests.
- **hermit backend: works.** `platforms/server/hermit`, `run-hermit`. `platforms/server/std` holds
  what native and hermit share: the bounded HTTP server and `StdClock`.
  - Kernel features derived as designed: `tcp` from the backend crate, `acpi`/`pci`/`dhcpv4`/
    `virtio-net` from the qemu profile in `run-hermit`, nothing else (checked with `cargo tree`).
  - Example app image: 1.3 MB (0.53 MB gzipped, LTO + strip, `image` profile), 0.25 s to first
    response.
  - `scripts/hermit-conformance.sh` runs the conformance suite inside the image under QEMU
    (Hermit's isa-debug-exit: QEMU status 3 = success, 1 = failure). Without RDSEED it fails with
    `NoEntropy`, as intended.
  - `scripts/acceptance.sh native|hermit`: the same black-box HTTP suite passes on both.
  - 200 concurrent requests against the hermit VM: 187 × 201, 13 × 503, VM stays up.

## Environment
- Rust builds run in the distrobox: `distrobox enter build-machine`.
- rustup is installed **without** PATH changes; call it via `~/.cargo/bin/` (Fedora's cargo stays
  default). Toolchain 1.97.1 has the prebuilt `x86_64-unknown-hermit` std installed.
- QEMU runs on the host; `/dev/kvm` is available.

## Next steps
1. Protection, the remaining decisions: branch protection on `main` with code-owner review, an
   agent identity to push under, and whether to enable local write-deny rules.
2. Upstream: commented on hermit-os/kernel#2736 (silent entropy fallback) with the x86_64
   case: the documented QEMU CPU flags lack `rdseed`. No PRs unless maintainers ask.
3. Migrations. `sonos-tui`: done on its `ambit` branch, runs against real speakers, its
   allowlist and permissions approved. `crossload`: the writing `Dir` is built; next a WebSocket
   capability for the reader's upload protocol, then the move itself. Then `cli`.
4. Later: outbound connections + TLS, durable storage on hermit (virtio-fs/block),
   uhyve/Firecracker profiles; attack-surface report; capabilities to Landlock/seccomp for native;
   `gui`. Tokio on Hermit only if the blocking server turns out too limiting.
5. Longer horizon: **iOS and Android** as `gui`-family platforms. Capabilities map onto their
   permission models (iOS entitlements and Info.plist usage keys, Android manifest permissions),
   and the file picker and photo picker are real powerboxes there. Open: a Rust UI toolkit that
   targets both, or a thin native shell around a Rust core.
