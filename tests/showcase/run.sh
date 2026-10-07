#!/usr/bin/env bash
# The showcase end to end, on this machine: its services run in Docker, its
# configuration is read from the compose file, this machine shares it, and a
# guest in a container of its own opens the three ports under one hostname.
#
#   tests/showcase/run.sh          (make showcase builds what it needs first)
#   KEEP=1 tests/showcase/run.sh   leaves everything running for a look
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
bin=${DEVSHARE_BIN:-$root/target-macos/debug}
# The test does not depend on this user's general settings.
export DEVSHARE_SETTINGS=/dev/null
export DEVSHARE_JOIN=
work=$(mktemp -d)
server=http://localhost:8787

passed=0
failed=0
expect() {
    local what=$1 wanted=$2 output
    shift 2
    output=$("$@" 2>&1)
    if [[ $output == *"$wanted"* ]]; then
        passed=$((passed + 1)); printf '  ✓ %s\n' "$what"
    else
        failed=$((failed + 1)); printf '  ✗ %s\n      wanted: %s\n      got:    %s\n' "$what" "$wanted" "${output:0:400}"
    fi
}
refuse() {
    local what=$1 output
    shift
    if output=$("$@" 2>&1); then
        failed=$((failed + 1)); printf '  ✗ %s\n      it succeeded: %s\n' "$what" "${output:0:400}"
    else
        passed=$((passed + 1)); printf '  ✓ %s\n' "$what"
    fi
}
until_true() {
    local tries=$(($1 * 5))
    shift
    for _ in $(seq "$tries"); do
        "$@" >/dev/null 2>&1 && return 0
        sleep 0.2
    done
    return 1
}
guest() { docker exec devshare-showcase-guest "$@"; }

finish() {
    [[ ${KEEP:-} ]] && return
    [[ -f $work/share.pid ]] && kill -INT "$(cat "$work/share.pid")" 2>/dev/null
    sleep 1
    [[ -f $work/server.pid ]] && kill "$(cat "$work/server.pid")" 2>/dev/null
    docker rm -f devshare-showcase-guest >/dev/null 2>&1
    (cd "$here" && docker compose down -v --remove-orphans >/dev/null 2>&1)
    rm -rf "$work"
}
trap finish EXIT

echo "The project"
(cd "$here" && docker compose up -d --wait --quiet-pull 2>&1 | grep -iE 'error' )
until_true 20 curl -fs http://localhost:8710/
expect "the page answers on this machine" "service=web" curl -fsS -m 5 http://localhost:8710/
expect "so does the API, on its own port" '"host":"localhost:8711"' curl -fsS -m 5 http://localhost:8711/hello
expect "and the documentation" "service=docs" curl -fsS -m 5 http://localhost:8712/

echo
echo "Its configuration, from the compose file"
before=$(cat "$here/devshare.toml")
expect "discover lists three ports to share" "8712   docs → 80" "$bin/devshare" discover "$here"
expect "and leaves the cache out" "cache → 6379: a cache is not shared by default" "$bin/devshare" discover "$here"
expect "the file in the folder is what discover writes" "$before" cat "$here/devshare.toml"

echo
echo "The session"
# A control plane already running on this machine is used as it is, and left
# running; otherwise one is started for the time of the test.
if ! curl -fs "$server/healthz" >/dev/null 2>&1; then
    "$bin/devshare-server" >"$work/server.log" 2>&1 &
    echo $! >"$work/server.pid"
    until_true 10 curl -fs "$server/healthz" || { cat "$work/server.log"; echo "no control plane"; exit 1; }
fi
(cd "$here" && exec "$bin/devshare" share --server "$server" --duration 3m --no-qr </dev/null >"$work/share.log" 2>&1) &
echo $! >"$work/share.pid"
until_true 20 grep -q '^Code:' "$work/share.log" || { cat "$work/share.log"; echo "sharing did not start"; exit 1; }
code=$(grep -A1 '^Code:' "$work/share.log" | tail -1)
link=$(grep -A1 '^Invitation:' "$work/share.log" | tail -1)
expect "one environment, three services" "showcase.test:8712" cat "$work/share.log"
refuse "no warning: every service answered when sharing started" grep -q '^!' "$work/share.log"

# A machine of its own for the guest: it reaches this one's control plane
# and nothing of the showcase's network.
docker build -q -t devshare-guest "$root/docker" >/dev/null
docker rm -f devshare-showcase-guest >/dev/null 2>&1
docker run -d --name devshare-showcase-guest --cap-add NET_ADMIN --device /dev/net/tun \
    --add-host host.docker.internal:host-gateway \
    -e DEVSHARE_SERVER=http://host.docker.internal:8787 \
    -v devshare-target:/opt/devshare:ro devshare-guest sleep infinity >/dev/null
refuse "the guest does not know showcase.test" guest getent hosts showcase.test
# With the link: the control plane of this session answers on this machine
# only, so the code alone would be no use to a guest.
docker exec -d devshare-showcase-guest sh -c "/opt/devshare/debug/devshare join '$link' >/tmp/join.log 2>&1"
until_true 30 guest grep -q '^Connected' /tmp/join.log || { guest cat /tmp/join.log; echo "the guest could not join"; exit 1; }
expect "the guest joined" "guest 1 joined" cat "$work/share.log"

echo
echo "One hostname, three ports"
expect "http://showcase.test:8710 is the page" "service=web" guest curl -fsS -m 10 http://showcase.test:8710/
expect "with its script and its style" "200200" guest sh -c 'curl -s -m 10 -o /dev/null -w "%{http_code}" http://showcase.test:8710/showcase.js; curl -s -m 10 -o /dev/null -w "%{http_code}" http://showcase.test:8710/showcase.css'
expect "http://showcase.test:8711 is the API, asked under that name" '"host":"showcase.test:8711"' guest curl -fsS -m 10 http://showcase.test:8711/hello
expect "http://showcase.test:8712 is the documentation" "service=docs" guest curl -fsS -m 10 http://showcase.test:8712/
expect "the three at once, ten times over" "30" guest sh -c '
    for round in $(seq 10); do for port in 8710 8711 8712; do echo $port; done; done |
        xargs -P 30 -I{} curl -s -m 20 -o /dev/null -w "%{http_code}\n" http://showcase.test:{}/ | grep -c 200'

echo
echo "And nothing else"
expect "the cache does listen on this machine" "open" sh -c 'nc -z localhost 8713 && echo open'
refuse "a guest does not reach it" guest curl -s -m 5 http://showcase.test:8713/

echo
echo "The end of the session"
kill -INT "$(cat "$work/share.pid")"
until_true 10 guest grep -q '^Session over' /tmp/join.log
expect "the guest is told" "Session over: the host stopped sharing." guest cat /tmp/join.log
until_true 5 guest sh -c "! pgrep -x devshare"
refuse "showcase.test no longer resolves for the guest" guest getent hosts showcase.test
expect "the project itself still runs" "service=web" curl -fsS -m 5 http://localhost:8710/

echo
if ((failed)); then
    echo "$failed failed, $passed passed"
    exit 1
fi
echo "$passed passed"
