# What a developer machine has at hand, on both sides of a session, and what
# stands in for a phone's camera: zbar reads the QR code of the host's screen.
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl dnsutils iproute2 openssl procps python3 python3-pil zbar-tools \
 && rm -rf /var/lib/apt/lists/*
