#!/usr/bin/env bash
# The showcase, started and shared, in one command: its services come up in
# Docker, then this terminal shows the invitation and its QR code. Ctrl-C
# stops sharing and stops the services.
#
#   make demo                                 or, once bin/ exists:
#   tests/showcase/demo.sh [share options]    e.g. --duration 1h
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
devshare=${DEVSHARE:-$root/bin/devshare}

if [[ ! -x $devshare ]]; then
    echo "devshare is not built yet: run \"make install\" in $root" >&2
    exit 1
fi

stop() {
    echo
    echo "Stopping the showcase's services..."
    (cd "$here" && docker compose down -v --remove-orphans >/dev/null 2>&1)
}
trap stop EXIT
# Ctrl-C is for the session: this script waits for it to end, then cleans up.
trap : INT

echo "Starting the showcase's services (web, api, docs, cache)..."
# Docker's own progress is of no use here, unless it fails.
if ! started=$(cd "$here" && docker compose up -d --wait --quiet-pull 2>&1); then
    echo "$started" >&2
    exit 1
fi
echo "They answer on this machine at http://localhost:8710"
echo

# Long enough to try it from another device, unless asked otherwise.
[[ $# -eq 0 ]] && set -- --duration 15m
"$devshare" share "$here" "$@"
