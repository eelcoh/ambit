#!/usr/bin/env bash
# Build the app for Hermit (inside the distrobox) and boot it in QEMU (on the host).
# Then: curl http://localhost:9975/
set -euo pipefail
cd "$(dirname "$0")"

LOADER=hermit-loader-x86_64.loader
# Loader v0.5.6 matches hermit 0.13.2; v0.5.7 panics in early paging with this kernel.
[ -f "$LOADER" ] || curl -sSfL -o "$LOADER" \
  https://github.com/hermit-os/loader/releases/download/v0.5.6/hermit-loader-x86_64

distrobox enter build-machine -- bash -lc \
  "export PATH=\$HOME/.cargo/bin:\$PATH; cd '$PWD' && cargo build --release --target x86_64-unknown-hermit"

# -cpu host exposes features the kernel does not handle yet; use Hermit's documented CPU model.
exec qemu-system-x86_64 -enable-kvm \
  -cpu qemu64,apic,fsgsbase,fxsr,rdrand,rdtscp,xsave,xsaveopt \
  -smp 1 -m 64M -display none -serial stdio \
  -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
  -kernel "$LOADER" -initrd target/x86_64-unknown-hermit/release/hermit-httpd \
  -netdev user,id=net0,hostfwd=tcp::9975-:9975,net=192.168.76.0/24,dhcpstart=192.168.76.9 \
  -device virtio-net-pci,netdev=net0,disable-legacy=on
