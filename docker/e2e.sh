#!/usr/bin/env bash
# A session end to end: a host shares two environments, a guest that only has
# the code uses them under their real hostnames, then everything goes away.
#
#   docker/e2e.sh            host and guest on one network: a direct link
#   docker/e2e.sh relayed    on two networks that cannot reach each other
#   KEEP=1 docker/e2e.sh     leaves the containers up for a look around
#   PROFILE=release ...      runs the optimized binaries (make release first)
set -uo pipefail
cd "$(dirname "$0")"

route=${1:-direct}
files=(-f compose.yml)
[[ $route == relayed ]] && files+=(-f relayed.yml)

compose() { docker compose "${files[@]}" "$@"; }
host() { compose exec -T host "$@"; }
guest() { compose exec -T guest "$@"; }

passed=0
failed=0

# expect "what" "text the output must contain" command...
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

# refuse "what" command...   the command must fail
refuse() {
    local what=$1 output
    shift
    if output=$("$@" 2>&1); then
        failed=$((failed + 1)); printf '  ✗ %s\n      it succeeded: %s\n' "$what" "${output:0:400}"
    else
        passed=$((passed + 1)); printf '  ✓ %s\n' "$what"
    fi
}

# until_true seconds command...
until_true() {
    local tries=$(($1 * 5))
    shift
    for _ in $(seq "$tries"); do
        "$@" >/dev/null 2>&1 && return 0
        sleep 0.2
    done
    return 1
}

# join <code or link> [alone]   "alone": without the control plane's address
# in the guest's environment, so that only the invitation can name it.
join() {
    local alone=""
    [[ ${2:-} == alone ]] && alone="env -u DEVSHARE_SERVER"
    # With the tunnel's own log: it counts the connections it holds open.
    compose exec -d guest sh -c "RUST_LOG=off,devshare=warn,devshare_core=debug $alone devshare join '$1' >/tmp/join.log 2>&1"
    until_true 20 guest grep -q '^Connected' /tmp/join.log
}

finish() {
    [[ ${KEEP:-} ]] || compose down -v --remove-orphans >/dev/null 2>&1
}
trap finish EXIT

compose down -v --remove-orphans >/dev/null 2>&1
compose up -d --build --quiet-pull 2>&1 | grep -v -e Container -e Network -e Volume

echo
echo "Before the session ($route)"
until_true 30 host grep -q '^Code:' /shared/share.log || { host cat /shared/share.log; echo "the host did not start sharing"; exit 1; }
code=$(host sh -c "grep -A1 '^Code:' /shared/share.log | tail -1")
expect "the host shows a code" "-" echo "$code"
refuse "the guest does not know shop.test" guest getent hosts shop.test

echo
echo "A guest that is killed"
join "$code" || { guest cat /tmp/join.log; echo "the guest could not join"; exit 1; }
expect "the guest joined" "guest 1 joined" host cat /shared/share.log
guest pkill -KILL -x devshare
# No goodbye from a killed process: the host waits for the link to time out.
until_true 25 host grep -q 'guest 1 left' /shared/share.log
expect "the host sees it leave" "guest 1 left" host cat /shared/share.log
refuse "its interface is gone" guest sh -c "ip -o -4 addr | grep -q 198.18.90.1"

echo
echo "Joining with the QR code"
# A camera on the host's screen: all the guest gets is what the code says.
scanned=$(guest python3 /harness/scan.py /shared/share.log 2>&1)
expect "the code on the host's screen reads as a web address on the control plane" "http://control:8787/$code" echo "$scanned"
expect "a browser opening it gets the invitation's page" "You are invited to a DevShare session" guest curl -fsS -m 10 "$scanned"
# Joined with the scanned address alone: the guest is not told which control
# plane to ask, the link says it.
join "$scanned" alone || { guest cat /tmp/join.log; echo "the guest could not join with the scanned code"; exit 1; }
expect "the guest lists what is shared" "api.shop.test:8080" guest cat /tmp/join.log
until_true 10 guest grep -q '^Route:' /tmp/join.log
expect "the link is $route" "Route: $route" guest cat /tmp/join.log
until_true 10 host grep -q 'guest 2: ' /shared/share.log
expect "the host says so too" "guest 2: $route" host cat /shared/share.log
refuse "the unshared environment is not listed" guest grep -q grafana /tmp/join.log
expect "one block of names, the stale one replaced" "1" guest grep -c '>>> devshare' /etc/hosts
expect "the interface is up" "198.18.90.1" guest ip -o -4 addr

echo
echo "Names"
expect "the tunnel's resolver knows shop.test" "198.18.90." guest dig +short +time=2 +tries=1 @198.18.90.2 shop.test
expect "it knows nothing else" "NXDOMAIN" guest dig +time=2 +tries=1 @198.18.90.2 grafana.test
expect "the guest's own names still resolve" "control" guest sh -c "getent hosts control && echo control"

echo
echo "Same hostnames, same ports"
expect "http://shop.test" "service=shop host=shop.test path=/cart" guest curl -fsS -m 10 http://shop.test/cart
expect "http://admin.test, same port, another service" "service=admin host=admin.test" guest curl -fsS -m 10 http://admin.test/
expect "http://api.shop.test:8080" "service=api host=api.shop.test:8080" guest curl -fsS -m 10 http://api.shop.test:8080/
expect "http://shop.test:5173" "service=vite host=shop.test:5173" guest curl -fsS -m 10 http://shop.test:5173/
expect "https://shop.test, verified end to end" "service=shop-tls host=shop.test" guest curl -fsS -m 10 --cacert /shared/shop.pem https://shop.test/
pin=$(guest sh -c "openssl x509 -in /shared/shop.pem -outform der | sha256sum | cut -c1-16")
expect "the manifest carries that certificate's fingerprint" "shop.test:443  TLS $pin" guest cat /tmp/join.log

echo
echo "How connections end"
endings=$(guest timeout 120 python3 /harness/endings.py http://shop.test 2>&1)
for ending in \
    "a short answer ended by the server" \
    "a 4 MiB answer ended by the server" \
    "the client closes its side, then reads" \
    "the client leaves in the middle of an answer" \
    "and the next connection is fine"; do
    expect "$ending" "$ending: ok" echo "$endings"
done

echo
echo "Nothing else"
refuse "grafana.test does not resolve" guest curl -fsS -m 5 http://grafana.test:3000/
refuse "the database port of a shared hostname" guest curl -fsS -m 5 http://shop.test:5432/
expect "refused at once, not after the program's own timeout" "at once" guest sh -c '
    start=$(date +%s%N)
    curl -s -m 5 http://shop.test:5432/
    [ $((($(date +%s%N) - start) / 1000000)) -lt 1000 ] && echo "at once"'
refuse "an unshared port by address" guest sh -c 'curl -fsS -m 5 "http://$(dig +short @198.18.90.2 shop.test):3000/"'
refuse "an address of the session that maps to no name" guest curl -fsS -m 5 http://198.18.90.200/
refuse "the host's services by its own address" guest sh -c 'curl -fsS -m 5 "http://$(getent hosts host | cut -d" " -f1)/"'

echo
echo "Load"
size=$((32 * 1024 * 1024))
reference=$(host sh -c "curl -fsS 'http://shop.test/blob?size=$size' | sha256sum | cut -d' ' -f1")
expect "32 MiB downloaded intact" "$reference" guest sh -c "curl -fsS -m 120 'http://shop.test/blob?size=$size' | sha256sum"
expect "8 MiB uploaded intact" "match" guest sh -c '
    head -c 8388608 /dev/urandom >/tmp/upload
    sum=$(sha256sum /tmp/upload | cut -d" " -f1)
    curl -fsS -m 120 --data-binary @/tmp/upload http://api.shop.test:8080/ | grep -q "sha256=$sum" && echo match'
expect "40 requests at once" "40" guest sh -c 'seq 40 | xargs -P 40 -I{} curl -fsS -m 30 http://shop.test/{} | grep -c service=shop'
# The payload is in the server's memory by now: this times the path, not the server.
mibs() { local bytes; bytes=$("$@" curl -fsS -m 120 -o /dev/null -w '%{speed_download}' "http://shop.test/blob?size=$size"); echo "$((${bytes%.*} / 1048576))"; }
printf '  · download: %s MiB/s through the tunnel, %s MiB/s on the host itself (%s build)\n' "$(mibs guest)" "$(mibs host)" "${PROFILE:-debug}"
until_true 10 guest sh -c 'grep " closed, " /tmp/join.log | tail -1 | grep -q "closed, 0 open"'
expect "no connection is left open" "closed, 0 open" guest sh -c 'grep " closed, " /tmp/join.log | tail -1'
expect "every connection opened was closed" "same" guest sh -c '
    [ "$(grep -c " opened, " /tmp/join.log)" = "$(grep -c " closed, " /tmp/join.log)" ] && echo same'
peak=$(guest sh -c 'grep VmHWM /proc/$(pgrep -x devshare)/status' | tr -s '\t ' ' ' | cut -d' ' -f2)
printf '  · peak memory of the guest process: %s MiB\n' "$((peak / 1024))"

echo
echo "A guest that is revoked"
host sh -c 'echo guests >/shared/commands'
until_true 5 host grep -q '  guest 2: .*(linux)' /shared/share.log
expect "the host lists its guests" "(linux), $route" host cat /shared/share.log
host sh -c 'echo "revoke 2" >/shared/commands'
until_true 10 guest grep -q '^Session over' /tmp/join.log
expect "the guest is told why" "Session over: the host revoked this device." guest cat /tmp/join.log
expect "the host confirms" "Guest 2 revoked" host cat /shared/share.log
until_true 5 guest sh -c "! pgrep -x devshare"
refuse "the guest process is gone" guest pgrep -x devshare
refuse "the interface is gone" guest sh -c "ip -o -4 addr | grep -q 198.18.90.1"
refuse "the names are gone from the hosts file" guest grep -q devshare /etc/hosts
refuse "shop.test no longer resolves" guest getent hosts shop.test
expect "its code lets nobody in any more" "no session for this invitation" guest sh -c "devshare join '$code' 2>&1 || true"

echo
echo "The end of the session"
host pkill -INT -x devshare
# The host's machine stops with its session; its screen is still on the shared volume.
until_true 10 guest grep -q '^Sharing stopped' /shared/share.log
expect "the host stops sharing" "Sharing stopped." guest cat /shared/share.log
until_true 10 sh -c "! docker compose ${files[*]} ps --status running --services | grep -qx host"
refuse "the host process has exited" sh -c "docker compose ${files[*]} ps --status running --services | grep -qx host"

echo
if ((failed)); then
    echo "$failed failed, $passed passed"
    exit 1
fi
echo "$passed passed"
