#!/usr/bin/env bash
# The public server's routing, end to end: a host on one network and a guest
# on another, with nothing between them but Caddy, the relay and the control
# plane behind it. The guest joins with the short code alone.
#
#   deploy/test.sh          KEEP=1 leaves the containers up
set -uo pipefail
cd "$(dirname "$0")/test"

compose() { docker compose "$@"; }
host() { compose exec -T host "$@"; }
guest() { compose exec -T guest "$@"; }

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
finish() { [[ ${KEEP:-} ]] || compose down -v --remove-orphans >/dev/null 2>&1; }
trap finish EXIT

compose down -v --remove-orphans >/dev/null 2>&1
compose up -d --build --quiet-pull 2>&1 | grep -iE 'error'

echo
echo "The server, as the internet sees it"
expect "the control plane answers at /v2" "protocol 2" guest curl -fsS -m 5 http://relay.join.test/v2
expect "the relay answers at its own paths" "200" guest curl -s -m 5 -o /dev/null -w '%{http_code}' http://relay.join.test/ping
refuse "the relay itself is out of reach" guest curl -s -m 3 http://relay:3340/ping
refuse "and so is the control plane" guest curl -s -m 3 http://control:8787/v2

echo
echo "A host on one network"
until_true 40 host grep -q '^Code:' /shared/share.log || { host cat /shared/share.log; echo "the host did not start sharing"; exit 1; }
code=$(host sh -c "grep -A1 '^Code:' /shared/share.log | tail -1")
refuse "it started no control plane of its own" host grep -q "runs its own" /shared/share.log
expect "its code" "-" echo "$code"

echo
echo "A guest on another, with the code alone"
refuse "the guest cannot reach the host" guest getent hosts host
compose exec -d guest sh -c "devshare join '$code' >/tmp/join.log 2>&1"
until_true 40 guest grep -q '^Connected' /tmp/join.log || { guest cat /tmp/join.log; echo "the guest could not join"; exit 1; }
expect "the code found the host through the control plane" "guest 1 joined" host cat /shared/share.log
until_true 10 guest grep -q '^Route:' /tmp/join.log
expect "the traffic goes through the relay" "Route: relayed" guest cat /tmp/join.log
expect "http://shop.test" "service=shop host=shop.test" guest curl -fsS -m 15 http://shop.test/
expect "http://api.shop.test:8080" "service=api" guest curl -fsS -m 15 http://api.shop.test:8080/
expect "a 4 MiB answer, through the proxy and the relay" "4194304" guest sh -c 'curl -fsS -m 60 "http://shop.test/blob?size=4194304" | wc -c'

echo
echo "The end"
host pkill -INT -x devshare
until_true 10 guest grep -q '^Session over' /tmp/join.log
expect "the guest is told" "Session over: the host stopped sharing." guest cat /tmp/join.log
expect "the code is withdrawn from the control plane" "no session for this invitation" guest sh -c "devshare join '$code' 2>&1 || true"

echo
if ((failed)); then
    echo "$failed failed, $passed passed"
    exit 1
fi
echo "$passed passed"
