#!/usr/bin/env bash
# .azure-pipelines/setup-toolchain-linux.sh
#
# Installs Rust (stable, minimal) + Erlang/OTP + git on a fresh Ubuntu agent so
# subsequent pipeline steps can run `cargo`, `erl`, `git`.  Used by Stage 3
# `BuildTestMatrix` of `azure-pipelines.yml`.
#
# Idempotent: re-running on a partially provisioned agent is safe.

set -euo pipefail

echo '[setup-toolchain-linux] apt-get update'
sudo apt-get update -y

# Need curl + ca-certificates before we can fetch the Erlang Solutions .deb.
sudo apt-get install -y --no-install-recommends ca-certificates curl build-essential git

# ── Erlang/OTP from Erlang Solutions S3 mirror.  Ubuntu Noble universe ships
#    only OTP 25.x, which is below Ridge's MinOTP=26 (E101).  Erlang Solutions
#    publishes `esl-erlang` at the current major (OTP 27+) but the primary
#    apt repo `packages.erlang-solutions.com` rides on a CloudFront edge that
#    has had multi-day 502 outages (observed during ADO runs #206 / #207 /
#    #208).  Their backing S3 bucket `binaries2.erlang-solutions.com` is the
#    same content served by Amazon S3 directly (eu-west-2) and stays up
#    independently of the CloudFront edge, so we download the .deb from
#    there and install it locally with `apt-get install ./file.deb` (which
#    resolves transitive deps from the Ubuntu archive).  Bumping the version
#    pin is a one-line edit.
ESL_ERLANG_DEB_VERSION='27.3.4-1~ubuntu~noble'
ESL_ERLANG_DEB_URL="https://binaries2.erlang-solutions.com/ubuntu/pool/contrib/e/esl-erlang/esl-erlang_${ESL_ERLANG_DEB_VERSION}_amd64.deb"
ESL_ERLANG_DEB_PATH="/tmp/esl-erlang_${ESL_ERLANG_DEB_VERSION}_amd64.deb"

echo "[setup-toolchain-linux] download esl-erlang from $ESL_ERLANG_DEB_URL"
# --retry rides out transient S3 5xx without aborting the build; --fail
# turns any non-2xx into a non-zero exit so we don't accept an error page
# as the .deb payload.
curl --proto '=https' --tlsv1.2 --fail --silent --show-error \
    --retry 5 --retry-delay 5 --retry-all-errors \
    -o "$ESL_ERLANG_DEB_PATH" "$ESL_ERLANG_DEB_URL"

echo '[setup-toolchain-linux] install esl-erlang (transitive deps via apt archive)'
sudo apt-get install -y "$ESL_ERLANG_DEB_PATH"
rm -f "$ESL_ERLANG_DEB_PATH"

# ── /etc/hosts fixup for BEAM startup on hosted Linux container ──────────────
# Azure DevOps Linux agents are containers and may ship with /etc/hosts that
# does NOT map the current hostname to 127.0.0.1.  Erlang VM's
# `inet_gethost_native` resolver tries to resolve the local hostname at boot
# (for epmd registration and crypto driver init); on a container with no
# entry, the resolver falls back to DNS, which can hang for the full
# resolver timeout (5-30 s) on EVERY `erl` invocation.  With 151+ stdlib
# tests each spawning a fresh BEAM, this cascades to a 45+ min job timeout
# (observed in ADO #209).  Defensively prepend the hostname mapping; if it
# was already present, the extra entry is harmless.
host_name="$(hostname)"
if ! grep -qE "^127\.0\.0\.1[[:space:]].*\<${host_name}\>" /etc/hosts; then
    echo "[setup-toolchain-linux] adding '127.0.0.1 ${host_name}' to /etc/hosts"
    echo "127.0.0.1 ${host_name}" | sudo tee -a /etc/hosts >/dev/null
fi

# ── Rust via rustup (stable, minimal profile) ────────────────────────────────
if ! command -v cargo >/dev/null 2>&1; then
    echo '[setup-toolchain-linux] install Rust via rustup'
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable --profile minimal
fi
# shellcheck disable=SC1090
. "$HOME/.cargo/env"

# Ensure subsequent pipeline steps see ~/.cargo/bin on PATH.
echo "##vso[task.prependpath]$HOME/.cargo/bin"

echo '[setup-toolchain-linux] tool versions:'
cargo --version
erl -version 2>&1 || true
git --version
