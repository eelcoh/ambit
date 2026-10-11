# Spike: Rust HTTP server as a Hermit unikernel

Status: **works.** A std Rust app (`tiny_http`) boots on Hermit 0.13.2 in QEMU/KVM,
gets an address over DHCP and serves HTTP through a forwarded port. Run `./run.sh`.

## Setup
- rustup installed with `--no-modify-path`; Fedora's cargo stays the default.
- Toolchain pinned to stable `1.97.1` (`rust-toolchain.toml`) plus the prebuilt
  `rust-std` for `x86_64-unknown-hermit` from hermit-os/rust-std-hermit. No nightly or build-std.
- The `hermit` crate (git tag `hermit-0.13.2`) builds the kernel from source during `cargo build`.

## Measurements (i3-1115G4, KVM)
The `hermit` crate's default features enable a lot the app doesn't use. The spike now uses
`default-features = false` with only `acpi`, `pci`, `tcp`, `dhcpv4`, `virtio-net`.

|                          | defaults + listed features | minimal (current) |
|--------------------------|----------------------------|-------------------|
| image                    | 5.4 MB                     | 1.7 MB            |
| stripped                 | 4.9 MB                     | 1.2 MB            |
| gzipped                  | 1.2 MB                     | 0.56 MB           |
| launch to first response | ~0.44 s                    | ~0.26 s (3 runs)  |
| memory                   | boots with 64 MB, not 32   | boots with 48 MB, not 40 (32 MB: exit status 1 after virtio init) |

The defaults dropped: `dns`, `udp`, `virtio-fs`, `smp`, `fsgsbase`, `kernel-stack`, `pci-ids`.
Lower memory was not tested with the default features.

- Clean build: ~1m40s with defaults, ~50 s minimal; mostly the kernel. Incremental: seconds.
- The kernel reports the network up at ~0.17 s.
- Features compose through Cargo: `hermit`'s `build.rs` builds the kernel with
  `--no-default-features` and forwards only the features enabled on the `hermit` crate
  (`CARGO_FEATURE_*`), so the union across the dependency graph is what the kernel gets.

## Gotchas
- Loader version must match the kernel: loader v0.5.7 + kernel 0.13.2 panics in early paging
  (`ParentEntryHugePage` mapping the SMP boot page). v0.5.6 works.
- `-cpu host` triggers the same panic; use Hermit's documented CPU model.
- Hermit's `loader` kernel feature exists only on main, not in 0.13.2.

## Not tried yet
- tokio/axum: Hermit patches `tokio` and `socket2` via forks (`[patch.crates-io]`).
- uhyve (Hermit's own hypervisor), Firecracker.
- Storage (virtio-fs / block), TLS, outbound connections and DNS.
- Dropping `acpi`/`pci` (needs a virtio-mmio target such as Firecracker).
- Why 40 MB doesn't boot (heap size? network buffers?).
