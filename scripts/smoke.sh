#!/bin/sh
# Offline-only, bounded real-model smoke. Python 3.11+ provides TOML parsing.
set -eu
exec python3 "$(dirname "$0")/smoke.py" "$@"
