#!/usr/bin/env bash
# .azure-pipelines/setup-toolchain-macos.sh
#
# Installs Rust (stable, minimal) + Erlang/OTP (Homebrew) on a fresh macOS
# agent so subsequent pipeline steps can run `cargo`, `erl`, `git`.  Used by
# Stage 3 `BuildTestMatrix` of `azure-pipelines.yml`.
#
# Idempotent: re-running on a partially provisioned agent is safe.

set -euo pipefail

echo '[setup-toolchain-macos] brew update'
brew update

echo '[setup-toolchain-macos] install erlang via Homebrew'
# brew install is idempotent: if erlang is already installed it just prints
# "already installed" and exits 0.
brew install erlang || brew upgrade erlang || true

# ── Rust via rustup (stable, minimal profile) ────────────────────────────────
if ! command -v cargo >/dev/null 2>&1; then
    echo '[setup-toolchain-macos] install Rust via rustup'
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable --profile minimal
fi
# shellcheck disable=SC1090
. "$HOME/.cargo/env"

# Ensure subsequent pipeline steps see ~/.cargo/bin on PATH.
echo "##vso[task.prependpath]$HOME/.cargo/bin"

echo '[setup-toolchain-macos] tool versions:'
cargo --version
erl -version 2>&1 || true
git --version
