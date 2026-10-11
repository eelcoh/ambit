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
  dependency allowlist keeps `hermit` out of `app`, so the app can't add features.
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
- `Random` must be a CSPRNG on real backends. **Open on hermit:** the spike logs
  `Unable to read entropy! Fallback to a naive implementation!` despite `rdrand` in the CPU model.
- hermit has no `Kv` until the storage spike; wiring it there is a compile error until then.

## Workspace shape (sketch)
```
caps/                      capability traits                          (human-owned)
platforms/<p>/api/         entry shape + capability set: the contract  (human-owned)
platforms/<p>/<backend>/   implementations; the only crates doing IO   (human-owned)
platforms/<p>/conformance/ tests every backend must pass               (human-owned)
app/                       all logic, no ambient authority             (agent-owned)
spec/                      properties and acceptance tests on the app  (human-owned)
run-<backend>/             tiny binary crates: wiring only             (human-owned)
```
One binary crate per backend rather than Cargo features: features unify badly across a workspace,
and backends need different target-specific dependencies.

## Enforcement
1. Lints: `app/clippy.toml` `disallowed-methods`/`disallowed-types` (std::fs, std::net,
   std::process, std::env, SystemTime::now, ...), with
   `#![forbid(clippy::disallowed_methods, clippy::disallowed_types, unsafe_code)]` so they can't be
   `#[allow]`ed locally. Dependency allowlist for `app`.
2. cackle (`cargo acl`) for per-crate API permissions, including dependencies, build scripts and
   proc macros (which run at build time with full authority).
3. Runtime isolation: the backend's own mechanism (see table above).

Levels 1 and 2 apply on **every** backend, native included, so dev can't quietly use what prod
doesn't allow.

**Human review surface** = `caps/`, platform APIs, conformance tests, `spec/`, the `run-*` wiring,
and the enforcement configs. Protection:
- Enforce outside the agent: read-only mounts or sandbox write-deny rules for those paths, plus a
  CI check that fails on changes to them without human sign-off.
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
- How the app declares its capability needs: generic bounds, or an `App` trait with associated
  capability types. Missing capability on a backend should be a compile error.
- `gui` toolkit choice.
- Threat model: a careless agent, or a compromised one (prompt injection via dependencies/docs)?
  Decides how hard levels 1-2 and the protection must be.

## Status
- `spikes/hermit-httpd/`: **works.** Blocking `tiny_http` server on Hermit 0.13.2 in QEMU/KVM.
  With minimal kernel features: ~0.26 s to first response, 0.56 MB gzipped image, 48 MB RAM.
  See its `NOTES.md` and `run.sh`.
- **Template workspace, `server` platform, native + sim: works.** `caps/`, `platforms/server/{api,
  native,sim,conformance}`, example `app/` (a link shortener using `Clock`, `Random`, `Kv`),
  `spec/`, `run-native/`. `scripts/check.sh` runs fmt, clippy, tests and the dependency check.
  - Level 1 verified: SystemTime::now, std::fs, File, TcpStream, Command, std::env, thread::spawn
    and println! in `app` all fail clippy; `#[allow(clippy::disallowed_methods)]` is a hard error
    (E0453, conflicts with `forbid`).
  - `app/allowed-deps.txt` covers the full normal tree including proc macros (`tracing-attributes`,
    `syn`, ...), because they run at build time with full authority. Stale entries also fail.
  - Native: `tiny_http` + thread pool, redb for `Kv`, `getrandom`, 1 MiB body limit (413),
    handler panics become 500.
  - Clippy only runs in `cargo clippy`, not `cargo build`: CI must run `scripts/check.sh`.

## Environment
- Rust builds run in the distrobox: `distrobox enter build-machine`.
- rustup is installed **without** PATH changes; call it via `~/.cargo/bin/` (Fedora's cargo stays
  default). Toolchain 1.97.1 has the prebuilt `x86_64-unknown-hermit` std installed.
- QEMU runs on the host; `/dev/kvm` is available.

## Next steps
1. hermit backend for `server` (`Clock`, `Random`, serve; blocking as in the spike) and
   `run-hermit`, with features derived per capability crate. Fix entropy first. Tokio on Hermit
   (Hermit's forks of `tokio`/`socket2` via `[patch.crates-io]`) only if blocking is too limiting.
2. Rest of the template: cackle (level 2), protection of the human-owned paths (sandbox/CI),
   black-box acceptance tests over HTTP against native and hermit, sim failure injection
   (`Kv` errors).
3. `tui` platform (terminal + headless), to prove the Elm entry shape.
4. Later: `cli`; outbound connections + TLS, storage (virtio-fs/block), uhyve/Firecracker for
   hermit; attack-surface report; capabilities to Landlock/seccomp for native; `gui`.
