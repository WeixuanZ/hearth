#!/bin/sh
# Runs what .github/workflows/ci.yml runs, on this Mac.
#   scripts/ci.sh            every job
#   scripts/ci.sh rust app   just these: rust, app, linux, cross
# The Linux jobs run in Docker. Their builds are kept in Docker volumes, so
# only the first run compiles everything.
set -eu
cd "$(dirname "$0")/.."

rust() {
  cargo fmt --check -p hearth
  cargo clippy --locked --all-targets -- -D warnings
  cargo test --locked --workspace
}

# Into a scratch directory: the app in macos/ may be the one running.
app() {
  scratch=$(mktemp -d)
  trap 'rm -rf "$scratch"' EXIT
  for arch in arm64 x86_64; do
    APP="$scratch/Hearth-$arch.app" ARCH=$arch macos/build.sh
  done
}

in_linux() {
  if ! docker info >/dev/null 2>&1; then
    echo "Docker is not running. Start Docker Desktop, or run just the Mac jobs: scripts/ci.sh rust app" >&2
    exit 1
  fi
  docker build -q -t hearth-ci -f scripts/ci.Dockerfile scripts >/dev/null
  # Not incremental, as in CI: the target volume stays a few GB, not tens.
  docker run --rm \
    -e CARGO_INCREMENTAL=0 \
    -v "$PWD":/src -w /src \
    -v hearth-ci-registry:/usr/local/cargo/registry \
    -v hearth-ci-target:/src/target \
    hearth-ci sh -c "$1"
}

linux() {
  in_linux "cargo clippy --locked --all-targets -- -D warnings && cargo test --locked --workspace"
}

cross() {
  in_linux "cargo build --locked --target aarch64-unknown-linux-gnu"
}

jobs=${*:-rust app linux cross}
for job in $jobs; do
  case $job in
    rust | app | linux | cross) ;;
    *) echo "No job $job. Jobs: rust app linux cross" >&2; exit 2 ;;
  esac
done
for job in $jobs; do
  echo "==> $job"
  "$job"
done
echo "==> passed: $jobs"
