#!/bin/sh
# The developer's machine: local services under .test names, then one session
# sharing two of the three declared environments.
set -eu

# A certificate for shop.test from the developer's own CA, which the guest is
# given so the test can check TLS end to end. How a guest device comes to
# trust it for real is work still to do.
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 1 \
    -keyout /tmp/shop.key -out /shared/shop.pem \
    -subj "/CN=shop.test" -addext "subjectAltName=DNS:shop.test" 2>/dev/null

python3 /harness/services.py /shared/shop.pem /tmp/shop.key &
until curl -fs http://shop.test/ >/dev/null; do sleep 0.2; done

# What the developer types while sharing arrives through this pipe.
mkfifo /shared/commands
exec 3<>/shared/commands

exec devshare share --config /harness/devshare.toml --only shop --only admin \
    --duration "${DURATION:-3m}" --guests 2 <&3 >/shared/share.log 2>&1
