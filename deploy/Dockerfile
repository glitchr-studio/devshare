# The two programs of DevShare's public server, built from source: the
# control plane of this repository and iroh's relay, at the version the
# clients use.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p devshare-server \
 && cargo install --locked iroh-relay@1.3.0 --features server --root /out \
 && cp target/release/devshare-server /out/bin/

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --home /nonexistent --shell /usr/sbin/nologin devshare
COPY --from=build /out/bin/devshare-server /out/bin/iroh-relay /usr/local/bin/
USER devshare
