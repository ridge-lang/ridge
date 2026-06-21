#!/usr/bin/env bash
#
# Run the Neovim headless acceptance suite against a ridge-lsp binary.
#
# Usage:
#   RIDGE_LSP=/path/to/ridge-lsp bash tools/lsp-acceptance/run.sh
#
# Exit status is 0 only when every check in neovim/driver.lua passes. Requires
# Neovim 0.10+ (for vim.lsp.start) on PATH.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
: "${RIDGE_LSP:?set RIDGE_LSP to the ridge-lsp binary}"
export RIDGE_LSP
export RIDGE_WS="$here/fixtures/ws"

if ! command -v nvim >/dev/null 2>&1; then
  echo "nvim not found on PATH (need Neovim 0.10+)" >&2
  exit 127
fi

echo "ridge-lsp:  $RIDGE_LSP"
echo "workspace:  $RIDGE_WS"
echo "neovim:     $(nvim --version | head -1)"
echo

# A hard ceiling so a hung handshake fails the job instead of stalling CI.
timeout 180 nvim --headless -u NONE -i NONE -n \
  -c "luafile $here/neovim/driver.lua"
