#!/usr/bin/env bash
set -euo pipefail
INPUT="${AGENTA_TOOL_PARAMS:-}"
if [ -z "$INPUT" ]; then INPUT="$(cat)"; fi
echo "tool received: $INPUT"
