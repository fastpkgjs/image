#!/bin/sh
# Runs inside an Alpine (musl) Node container with the repository mounted as the
# working directory. Used by .github/workflows/{ci,release}.yml.
#
#   scripts/ci-musl.sh build   build the musl .node into $OUT_DIR (default artifacts)
#   scripts/ci-musl.sh test    run the test suite + smoke test against packages/image/*.node
set -eu

mode="${1:?usage: ci-musl.sh build|test}"

case "$(uname -m)" in
  x86_64) target=x86_64-unknown-linux-musl ;;
  aarch64) target=aarch64-unknown-linux-musl ;;
  *) echo "unsupported arch $(uname -m)" >&2; exit 1 ;;
esac

if [ "$mode" = build ]; then
  apk add --no-cache build-base nasm curl git linux-headers perl
  if ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
      sh -s -- -y --profile minimal --default-toolchain none
  fi
  . "$HOME/.cargo/env"
  # rust-toolchain.toml pins the toolchain; this installs it for the musl host.
  rustup show active-toolchain >/dev/null 2>&1 || rustup toolchain install --profile minimal
  npm ci
  # Native build on Alpine: use the container's gcc as the linker. Without this the napi CLI
  # treats the musl target as a cross build and asks for `<arch>-linux-musl-gcc`.
  export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=gcc
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=gcc
  RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,-Bsymbolic" \
    npx napi build --platform --release --target "$target" --no-js \
      --dts native.d.ts --manifest-path crates/fastpkg-napi/Cargo.toml \
      --output-dir "${OUT_DIR:-artifacts}"
  ls -l "${OUT_DIR:-artifacts}"
elif [ "$mode" = test ]; then
  node -p "'node ' + process.version + ' musl ' + process.arch"
  npm ci
  npm test
  node tests/smoke.mjs
else
  echo "unknown mode $mode" >&2
  exit 1
fi
