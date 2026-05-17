#!/usr/bin/env sh
# tools/install/install.sh — Ridge installer for Linux and macOS (POSIX shell)
#
# Usage:
#   curl -sSf https://ridge-lang.org/install.sh | sh
#   sh install.sh [--dry-run]
#
# Flags:
#   --dry-run   Print every command that would be executed (one per line,
#               prefixed "[dry-run]") then exit 0.  No side-effects.
#               Used by reviewers and the CI dry-run snapshot lane.
#
# Exit codes: 0 = success, 1 = fatal prerequisite missing or cargo install failed.

set -eu

# ── Dry-run mode ────────────────────────────────────────────────────────────
#
# Must be parsed FIRST (step 2 of §3.14) so we exit before any side-effects.
DRY_RUN=0
SNAPSHOT_MODE=0  # --snapshot used by CI to strip platform-detected values
for arg in "$@"; do
    case "$arg" in
        --dry-run)    DRY_RUN=1 ;;
        --snapshot)   DRY_RUN=1; SNAPSHOT_MODE=1 ;;
    esac
done

if [ "$DRY_RUN" -eq 1 ]; then
    # Step 1 — platform detection
    if [ "$SNAPSHOT_MODE" -eq 1 ]; then
        echo "[dry-run] uname -s  # => <OS>"
        echo "[dry-run] uname -m  # => <ARCH>"
    else
        echo "[dry-run] uname -s  # => $(uname -s 2>/dev/null || echo '<unknown>')"
        echo "[dry-run] uname -m  # => $(uname -m 2>/dev/null || echo '<unknown>')"
    fi
    # Step 3 — Rust check
    echo "[dry-run] cargo --version"
    # Step 4 — Erlang check.  Uses `io:put_chars` (no format string) instead of
    # `io:format("~s~n", ...)` so the same eval expression works under
    # PowerShell 5.1, which strips inner `"` chars from native command args.
    echo "[dry-run] erl -noshell -eval 'io:put_chars(erlang:system_info(otp_release)),init:stop().'"
    # Step 5 — git check
    echo "[dry-run] git --version"
    # Step 6 — install binaries.
    # Snapshot mode is environment-independent: it ALWAYS emits the literal
    # canonical default so the snapshot file stays identical across CI runners
    # whether or not RIDGE_REPO/RIDGE_BRANCH are exported.  Non-snapshot
    # `--dry-run` echoes the *resolved* URL so reviewers see what would be used
    # in this environment (D155 attestation).
    if [ "$SNAPSHOT_MODE" -eq 1 ]; then
        DRY_REPO="https://github.com/ridge-lang/ridge"
        DRY_BRANCH="main"
    else
        DRY_REPO="${RIDGE_REPO:-https://github.com/ridge-lang/ridge}"
        DRY_BRANCH="${RIDGE_BRANCH:-main}"
    fi
    echo "[dry-run] cargo install --git $DRY_REPO --branch $DRY_BRANCH ridge-cli"
    echo "[dry-run] cargo install --git $DRY_REPO --branch $DRY_BRANCH ridge-lsp"
    # Step 7 — verify
    echo "[dry-run] ridge --version"
    exit 0
fi

# ── Step 1: Detect platform ───────────────────────────────────────────────────
OS="$(uname -s)"

case "$OS" in
    Linux)  PLATFORM="linux" ;;
    Darwin) PLATFORM="macos" ;;
    *)
        echo "error: unsupported operating system: $OS" >&2
        echo "  Ridge supports Linux and macOS.  For Windows, use install.ps1." >&2
        exit 1
        ;;
esac

# ── Helper: version comparison ────────────────────────────────────────────────
# Returns 0 if $1 >= $2 (both in MAJOR.MINOR[.PATCH] form)
version_ge() {
    a_major="$(echo "$1" | cut -d. -f1)"
    a_minor="$(echo "$1" | cut -d. -f2)"
    b_major="$(echo "$2" | cut -d. -f1)"
    b_minor="$(echo "$2" | cut -d. -f2)"
    if [ "$a_major" -gt "$b_major" ]; then return 0; fi
    if [ "$a_major" -eq "$b_major" ] && [ "$a_minor" -ge "$b_minor" ]; then return 0; fi
    return 1
}

# ── Step 3: Verify Rust ≥ 1.88 ───────────────────────────────────────────────
MIN_RUST="1.88"
if ! cargo_out="$(cargo --version 2>&1)"; then
    echo "error: cargo not found — Rust is not installed." >&2
    echo "" >&2
    echo "  Install Rust via rustup:" >&2
    echo "    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
    echo "" >&2
    exit 1
fi

# Extract MAJOR.MINOR from "cargo 1.85.0 (...)
rust_ver="$(echo "$cargo_out" | sed -n 's/cargo \([0-9][0-9]*\.[0-9][0-9]*\).*/\1/p')"
if [ -z "$rust_ver" ]; then
    echo "error: could not parse cargo version from: $cargo_out" >&2
    exit 1
fi
if ! version_ge "$rust_ver" "$MIN_RUST"; then
    echo "error: Rust $rust_ver is too old; Ridge requires Rust $MIN_RUST or newer." >&2
    echo "" >&2
    echo "  Update via rustup:" >&2
    echo "    rustup update stable" >&2
    echo "" >&2
    exit 1
fi

# ── Step 4: Verify Erlang/OTP ≥ 26 ──────────────────────────────────────────
# Uses `io:put_chars(erlang:system_info(otp_release))` rather than
# `io:format("~s~n", ...)` so the same eval expression works under PowerShell
# 5.1 in install.ps1 (PS 5.1 strips inner `"` chars from native command args).
# Bash preserves the eval verbatim either way; the unified pattern keeps both
# installers symmetric and makes the dry-run snapshot directly comparable.
MIN_OTP=26
if ! otp_out="$(erl -noshell -eval 'io:put_chars(erlang:system_info(otp_release)),init:stop().' 2>&1)"; then
    echo "error: erl not found — Erlang/OTP is not installed." >&2
    echo "" >&2
    case "$PLATFORM" in
        linux)
            echo "  Install Erlang/OTP on Debian/Ubuntu:" >&2
            echo "    sudo apt-get update && sudo apt-get install -y erlang" >&2
            echo "  On Fedora/RHEL:" >&2
            echo "    sudo dnf install -y erlang" >&2
            ;;
        macos)
            echo "  Install Erlang/OTP via Homebrew:" >&2
            echo "    brew install erlang" >&2
            ;;
    esac
    echo "" >&2
    exit 1
fi

# otp_out is the release string, e.g. "27"
otp_ver="$(echo "$otp_out" | tr -d '[:space:]')"
if ! echo "$otp_ver" | grep -qE '^[0-9]+$'; then
    echo "error: could not parse OTP release from erl output: $otp_out" >&2
    exit 1
fi
if [ "$otp_ver" -lt "$MIN_OTP" ]; then
    echo "error: Erlang/OTP $otp_ver is too old; Ridge requires OTP $MIN_OTP or newer." >&2
    echo "" >&2
    case "$PLATFORM" in
        linux)
            echo "  Upgrade Erlang/OTP on Debian/Ubuntu:" >&2
            echo "    sudo apt-get update && sudo apt-get install -y erlang" >&2
            ;;
        macos)
            echo "  Upgrade Erlang/OTP via Homebrew:" >&2
            echo "    brew upgrade erlang" >&2
            ;;
    esac
    echo "" >&2
    exit 1
fi

# ── Step 5: Verify git ≥ 2.20 ────────────────────────────────────────────────
# Uses the same rejection message as ridge-pkg's P008 PkgGitTooOld.
MIN_GIT="2.20"
if ! git_out="$(git --version 2>&1)"; then
    echo "error: git not found — git is not installed." >&2
    echo "" >&2
    case "$PLATFORM" in
        linux)
            echo "  Install git on Debian/Ubuntu:" >&2
            echo "    sudo apt-get update && sudo apt-get install -y git" >&2
            echo "  On Fedora/RHEL:" >&2
            echo "    sudo dnf install -y git" >&2
            ;;
        macos)
            echo "  Install git via Homebrew or Xcode Command Line Tools:" >&2
            echo "    brew install git" >&2
            echo "    # or: xcode-select --install" >&2
            ;;
    esac
    echo "" >&2
    exit 1
fi

# Lenient parse: first MAJOR.MINOR match after "version" keyword (R17 — handles
# "git version 2.39.2 (Apple Git-143)" and similar exotic formats).
git_ver="$(echo "$git_out" | grep -oE '[0-9]+\.[0-9]+' | head -1)"
if [ -z "$git_ver" ]; then
    echo "error: could not parse git version from: $git_out" >&2
    echo "  (P009 PkgGitVersionUnparseable)" >&2
    exit 1
fi
if ! version_ge "$git_ver" "$MIN_GIT"; then
    # P008 PkgGitTooOld — same message as ridge-pkg
    echo "error: git $git_ver is too old; Ridge requires git $MIN_GIT or newer. (P008 PkgGitTooOld)" >&2
    echo "" >&2
    case "$PLATFORM" in
        linux)
            echo "  Upgrade git on Debian/Ubuntu:" >&2
            echo "    sudo apt-get update && sudo apt-get install -y git" >&2
            ;;
        macos)
            echo "  Upgrade git via Homebrew:" >&2
            echo "    brew upgrade git" >&2
            ;;
    esac
    echo "" >&2
    exit 1
fi

# ── Step 6: Install ridge-cli and ridge-lsp ──────────────────────────────────
# Repository / branch are overridable via env vars so CI matrices can pin to
# the transient public mirror (`ridge-lang/ridge`) until `ridge-lang/ridge`
# opens in Phase 9.  Defaults are deterministic and used by `--snapshot` mode.
RIDGE_REPO="${RIDGE_REPO:-https://github.com/ridge-lang/ridge}"
RIDGE_BRANCH="${RIDGE_BRANCH:-main}"

echo "Installing ridge-cli ..."
if ! cargo install --git "$RIDGE_REPO" --branch "$RIDGE_BRANCH" ridge-cli 2>&1; then
    echo "" >&2
    echo "error: cargo install ridge-cli failed (see stderr above)." >&2
    if [ ! -w "$HOME/.cargo/bin" ] 2>/dev/null; then
        echo "" >&2
        echo "  ~/.cargo/bin is not writable by the current user." >&2
        echo "  Fix ownership with:" >&2
        echo "    sudo chown -R \"\$(whoami)\" ~/.cargo/bin" >&2
    fi
    exit 1
fi

echo "Installing ridge-lsp ..."
if ! cargo install --git "$RIDGE_REPO" --branch "$RIDGE_BRANCH" ridge-lsp 2>&1; then
    echo "" >&2
    echo "error: cargo install ridge-lsp failed (see stderr above)." >&2
    if [ ! -w "$HOME/.cargo/bin" ] 2>/dev/null; then
        echo "" >&2
        echo "  ~/.cargo/bin is not writable by the current user." >&2
        echo "  Fix ownership with:" >&2
        echo "    sudo chown -R \"\$(whoami)\" ~/.cargo/bin" >&2
    fi
    exit 1
fi

# ── Step 7: Verify binary works ───────────────────────────────────────────────
echo "Verifying installation ..."
EXPECTED_VERSION="ridge 0.1.0"
if ! ridge_out="$(ridge --version 2>&1)"; then
    echo "error: ridge --version failed after install." >&2
    echo "  Ensure ~/.cargo/bin is on your PATH:" >&2
    echo "    export PATH=\"\$HOME/.cargo/bin:\$PATH\"" >&2
    exit 1
fi

if ! echo "$ridge_out" | grep -qF "$EXPECTED_VERSION"; then
    echo "warning: ridge --version printed '$ridge_out'; expected '$EXPECTED_VERSION'." >&2
    echo "  The binary was installed but may be a different version." >&2
fi

# ── Step 8: Success message ────────────────────────────────────────────────────
echo ""
echo "Ridge installed successfully!"
echo ""
echo "  ridge version: $(ridge --version)"
echo ""
echo "Get started:"
echo "  ridge new my-app && cd my-app && ridge run"
echo ""
echo "Documentation: https://ridge-lang.org/docs"
