# Sourced by the hermit scripts. Builds run in the build environment (see env.sh), QEMU on the host.
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"
HERMIT_TARGET=x86_64-unknown-hermit
HERMIT_LOADER=target/hermit-loader-x86_64-v0.5.6
# Loader v0.5.6 matches hermit 0.13.2; v0.5.7 panics in early paging with this kernel.
[ -f "$HERMIT_LOADER" ] || { mkdir -p target && curl -sSfL -o "$HERMIT_LOADER" \
  https://github.com/hermit-os/loader/releases/download/v0.5.6/hermit-loader-x86_64; }

hermit_build() { # cargo args...
  in_build_env "cargo build --locked --profile image --target $HERMIT_TARGET $*"
}

# -cpu host exposes features the kernel does not handle yet; use Hermit's documented CPU model.
# rdseed is required: without it the kernel's randomness is predictable (HermitRandom refuses).
hermit_qemu() { # image, extra qemu args...
  local image=$1; shift
  # KVM when available; otherwise TCG emulation (slower).
  local accel=()
  [ -w /dev/kvm ] && accel=(-enable-kvm)
  qemu-system-x86_64 "${accel[@]}" \
    -cpu qemu64,apic,fsgsbase,fxsr,rdrand,rdseed,rdtscp,xsave,xsaveopt \
    -smp 1 -m 64M -display none -serial stdio \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -kernel "$HERMIT_LOADER" -initrd "$image" \
    -netdev user,id=net0,hostfwd=tcp::8080-:8080,net=192.168.76.0/24,dhcpstart=192.168.76.9 \
    -device virtio-net-pci,netdev=net0,disable-legacy=on "$@"
}
