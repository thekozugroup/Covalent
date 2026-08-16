FROM rust:1.97.1-bookworm@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97 AS builder
WORKDIR /source
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY packaging/web ./packaging/web
RUN cargo build --locked --release -p covalent-node -p covalent-cli

FROM caddy:2.10.2-alpine@sha256:4c6e91c6ed0e2fa03efd5b44747b625fec79bc9cd06ac5235a779726618e530d AS caddy

FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241
ARG VCS_REF=unknown
LABEL org.opencontainers.image.title="Covalent" \
      org.opencontainers.image.description="Private distributed backup node" \
      org.opencontainers.image.source="https://github.com/thekozugroup/Covalent" \
      org.opencontainers.image.revision="$VCS_REF" \
      org.opencontainers.image.licenses="MIT"

RUN groupadd --system --gid 65532 covalent \
    && useradd --system --uid 65532 --gid 65532 --home-dir /nonexistent --no-create-home --shell /usr/sbin/nologin covalent \
    && install -d -m 0700 -o 65532 -g 65532 /config /data /restore
COPY --from=builder /source/target/release/covalent-node /usr/local/bin/covalent-node
COPY --from=builder /source/target/release/covalent /usr/local/bin/covalent
COPY --from=caddy /usr/bin/caddy /usr/local/bin/caddy
COPY packaging/docker/entrypoint.sh /usr/local/bin/covalent-entrypoint
COPY packaging/docker/Caddyfile /etc/caddy/Caddyfile
# The upstream Caddy binary carries cap_net_bind_service. Linux refuses to exec
# a file-capability binary when the runtime intentionally drops the complete
# capability bounding set. Covalent only binds Caddy to unprivileged port 8443,
# so copy to a fresh inode without xattrs and retain a true cap-drop=ALL runtime.
RUN cp --no-preserve=all /usr/local/bin/caddy /usr/local/bin/caddy.unprivileged \
    && mv /usr/local/bin/caddy.unprivileged /usr/local/bin/caddy \
    && chmod 0555 /usr/local/bin/caddy /usr/local/bin/covalent-entrypoint

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
