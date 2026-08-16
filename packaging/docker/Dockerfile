FROM rust:1.97.1-bookworm AS builder
WORKDIR /source
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY packaging/web ./packaging/web
RUN cargo build --locked --release -p covalent-node -p covalent-cli

FROM debian:bookworm-slim
LABEL org.opencontainers.image.title="Covalent" \
      org.opencontainers.image.description="Private distributed backup node" \
      org.opencontainers.image.source="https://github.com/thekozugroup/Covalent" \
      org.opencontainers.image.licenses="MIT"

RUN groupadd --system --gid 65532 covalent \
    && useradd --system --uid 65532 --gid 65532 --home-dir /nonexistent --no-create-home --shell /usr/sbin/nologin covalent \
    && install -d -m 0700 -o 65532 -g 65532 /config /data /restore
COPY --from=builder /source/target/release/covalent-node /usr/local/bin/covalent-node
COPY --from=builder /source/target/release/covalent /usr/local/bin/covalent
COPY packaging/docker/entrypoint.sh /usr/local/bin/covalent-entrypoint
RUN chmod 0555 /usr/local/bin/covalent-entrypoint

ENV COVALENT_LISTEN=0.0.0.0:8787 \
    COVALENT_PEER_LISTEN=0.0.0.0:8787 \
    COVALENT_DATA_DIR=/data \
    COVALENT_DEVICE_NAME="Covalent node" \
    COVALENT_LAN_DISCOVERY=false \
    UMASK=027 \
    RUST_LOG=info
EXPOSE 8787/udp 8787/tcp
VOLUME ["/config", "/data"]
USER 65532:65532
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["covalent-node", "healthcheck", "--url", "http://127.0.0.1:8787/healthz"]
ENTRYPOINT ["covalent-entrypoint"]
CMD ["serve"]
