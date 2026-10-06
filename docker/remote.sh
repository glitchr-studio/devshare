#!/usr/bin/env bash
# A guest on another network, a host that opens nothing: the host's control
# plane is on its own machine, out of the guest's reach, and so is the host
# itself. All they share is a relay. The guest joins with the one invitation
# there is, read from the QR code on the host's screen, and uses the shared
# services.
#
#   docker/remote.sh          KEEP=1 leaves the containers up
set -uo pipefail
cd "$(dirname "$0")"

compose() { docker compose -f compose.yml -f remote.yml "$@"; }
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
compose up -d --build --quiet-pull relay host guest 2>&1 | grep -iE 'error'

echo
echo "The host, alone on its network"
until_true 40 host grep -q '^Code:' /shared/share.log || { host cat /shared/share.log; echo "the host did not start sharing"; exit 1; }
code=$(host sh -c "grep -A1 '^Code:' /shared/share.log | tail -1")
expect "no control plane anywhere: the session runs its own" "this session runs its own" host cat /shared/share.log
expect "its invitation is said to work from any network" "work from any network" host cat /shared/share.log

echo
echo "The guest, somewhere else"
address=$(host sh -c "hostname -i | cut -d' ' -f1")
refuse "it cannot reach the host's machine" guest curl -s -m 4 "http://$address:8787/v1"
refuse "nor find it by name" guest getent hosts host
expect "it does reach the relay, like the host" "200" guest curl -s -m 5 -o /dev/null -w '%{http_code}' http://relay:3340/
refuse "the code alone is no use to it: its control plane is out of reach" guest sh -c "devshare join '$code'"

echo
echo "The one invitation, read from the host's screen"
# The QR code, as a camera reads it: the same thing a guest next to the host
# would scan.
invitation=$(guest python3 /harness/scan.py /shared/share.log 2>&1)
expect "the QR code is a web address on the host's machine" "http://$address:8787/" echo "$invitation"
expect "that carries the host's own address too" "#" echo "$invitation"
refuse "the page behind it is out of this guest's reach" guest curl -s -m 4 "${invitation%%#*}"

echo
echo "Joining with it"
compose exec -d guest sh -c "devshare join '$invitation' >/tmp/join.log 2>&1"
until_true 30 guest grep -q '^Connected' /tmp/join.log || { guest cat /tmp/join.log; echo "the guest could not join"; exit 1; }
expect "the guest is in" "guest 1 joined" host cat /shared/share.log
until_true 10 guest grep -q '^Route:' /tmp/join.log
expect "through the relay: there is no other way between them" "Route: relayed" guest cat /tmp/join.log

echo
echo "The shared services, and only them"
expect "http://shop.test" "service=shop host=shop.test" guest curl -fsS -m 15 http://shop.test/
expect "http://api.shop.test:8080" "service=api host=api.shop.test:8080" guest curl -fsS -m 15 http://api.shop.test:8080/
expect "http://admin.test" "service=admin host=admin.test" guest curl -fsS -m 15 http://admin.test/
expect "https://shop.test, verified end to end" "service=shop-tls host=shop.test" guest curl -fsS -m 15 --cacert /shared/shop.pem https://shop.test/
refuse "not the database port of a shared hostname" guest curl -s -m 5 http://shop.test:5432/
refuse "not the environment that was not shared" guest curl -s -m 5 http://grafana.test:3000/
refuse "and still not the host's machine itself" guest curl -s -m 4 "http://$address:8787/v1"

echo
echo "The end of the session"
host pkill -INT -x devshare
until_true 10 guest grep -q '^Session over' /tmp/join.log
expect "the guest is told" "Session over: the host stopped sharing." guest cat /tmp/join.log
until_true 5 guest sh -c "! pgrep -x devshare"
refuse "shop.test no longer resolves" guest getent hosts shop.test
refuse "the invitation is dead" guest sh -c "devshare join '$invitation'"

echo
if ((failed)); then
    echo "$failed failed, $passed passed"
    exit 1
fi
echo "$passed passed"
