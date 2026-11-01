# The Linux jobs of scripts/ci.sh. The toolchain is the one rust-toolchain.toml
# names, with a cross linker for aarch64, which the release builds too.
FROM rust:1.95-bookworm
RUN rustup toolchain install stable --profile minimal \
      --component clippy,rustfmt --target aarch64-unknown-linux-gnu \
 && rustup default stable \
 && apt-get update \
 && apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
 && rm -rf /var/lib/apt/lists/*
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
# Docker's disk is small, and the build cache lives on it. CI needs no debugger.
ENV CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
