#!/bin/sh
# Offline-only, bounded real-model smoke. Python 3.11+ provides TOML parsing.
set -eu
if [ "${1:-}" = "--audio" ]; then
    shift
    exec python3 "$(dirname "$0")/smoke_audio.py" "$@"
fi
exec python3 "$(dirname "$0")/smoke.py" "$@"
