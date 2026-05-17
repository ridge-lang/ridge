#!/usr/bin/env bash
# .azure-pipelines/assert-install-timing.sh
#
# Asserts that the Ridge install script completed within the 5-minute budget
# (verifies install completes under target time).
#
# Usage:
#   ./assert-install-timing.sh <elapsed-seconds>
#
# Exit 0 if elapsed < 300; exit 1 otherwise.

set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo 'usage: assert-install-timing.sh <elapsed-seconds>' >&2
    exit 2
fi

elapsed="$1"

# Validate the argument is an integer.
if ! echo "$elapsed" | grep -qE '^[0-9]+$'; then
    echo "error: elapsed must be a non-negative integer, got: $elapsed" >&2
    exit 2
fi

echo "[install-timing] elapsed=${elapsed}s"

if [ "$elapsed" -lt 300 ]; then
    echo "[install-timing] PASS (< 300 s budget)"
    exit 0
else
    echo "[install-timing] FAIL (>= 300 s budget — G2 violated)" >&2
    exit 1
fi
