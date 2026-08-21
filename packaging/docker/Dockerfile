FROM rust:1.97.1-alpine3.23@sha256:c4a364ddbf684fe038e6fa6a4f25b30c8dc85247423e0e660676ece0d17be4a2 AS builder
WORKDIR /source
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY packaging/web ./packaging/web
RUN cargo build --locked --release -p covalent-node -p covalent-cli

FROM caddy:2.11.4-alpine@sha256:5f5c8640aae01df9654968d946d8f1a56c497f1dd5c5cda4cf95ab7c14d58648 AS caddy
# Create a capability-free inode in this throwaway stage. Only this copy enters
# the runtime image, avoiding cap_net_bind_service and a duplicated Caddy layer.
RUN cp /usr/bin/caddy /caddy.unprivileged \
    && chmod 0555 /caddy.unprivileged

FROM alpine:3.24@sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b
ARG VCS_REF=unknown
LABEL org.opencontainers.image.title="Covalent" \
      org.opencontainers.image.description="Private distributed backup node" \
      org.opencontainers.image.source="https://github.com/thekozugroup/Covalent" \
      org.opencontainers.image.revision="$VCS_REF" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.base.name="docker.io/library/alpine:3.23" \
      org.opencontainers.image.base.digest="sha256:fd791d74b68913cbb027c6546007b3f0d3bc45125f797758156952bc2d6daf40"

RUN addgroup -S -g 65532 covalent \
    && adduser -S -D -H -u 65532 -G covalent -s /sbin/nologin covalent \
    && install -d -m 0700 -o 65532 -g 65532 /config /data /restore
COPY --from=builder /source/target/release/covalent-node /usr/local/bin/covalent-node
COPY --from=builder /source/target/release/covalent /usr/local/bin/covalent
COPY --from=caddy /caddy.unprivileged /usr/local/bin/caddy
COPY --from=caddy /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --chmod=0555 packaging/docker/entrypoint.sh /usr/local/bin/covalent-entrypoint
COPY packaging/docker/Caddyfile /etc/caddy/Caddyfile

ENV COVALENT_LISTEN=127.0.0.1:8787 \
    COVALENT_PEER_LISTEN=0.0.0.0:8787 \
    COVALENT_HTTPS_HOST=localhost \
    COVALENT_DATA_DIR=/data \
    COVALENT_DEVICE_NAME="Covalent node" \
    COVALENT_LAN_DISCOVERY=false \
    XDG_DATA_HOME=/config/caddy/data \
    XDG_CONFIG_HOME=/config/caddy/config \
    UMASK=027 \
    RUST_LOG=info
EXPOSE 8787/udp 8443/tcp
VOLUME ["/config", "/data"]
USER 65532:65532
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["covalent-node", "healthcheck", "--url", "http://127.0.0.1:8787/healthz"]
ENTRYPOINT ["covalent-entrypoint"]
CMD ["serve"]
