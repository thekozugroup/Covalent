FROM rust:1.97.1-alpine3.23@sha256:c4a364ddbf684fe038e6fa6a4f25b30c8dc85247423e0e660676ece0d17be4a2 AS builder
WORKDIR /source
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY packaging/web ./packaging/web
RUN cargo build --locked --release -p covalent-node -p covalent-cli

# Caddy is compiled here rather than lifted out of caddy:2.11.4-alpine, whose
# published binary is linked against go1.26.3. That toolchain accounted for ten
# of the twelve high/critical findings the Grype gate reported, and no Caddy tag
# fixes it: 2.11.4 is the newest x.y.z-alpine upstream publishes, so the only
# lever left is to control the toolchain ourselves.
#
# This is not a fork. packaging/docker/caddy is the same three-line consumer
# module `xcaddy` generates and the official image builds - it imports
# caddy/v2/modules/standard and nothing else. Verified against the upstream
# binary: identical 132-module set, and the unchanged packaging/docker/Caddyfile
# adapts to a byte-identical JSON config. What changes is the compiler and two
# module selections, not Caddy.
#
# Pinning, in three independent places, because a supply-chain stage that
# floats is not worth having:
#   - the toolchain image by digest, below;
#   - the Caddy source version and the entire module graph by
#     packaging/docker/caddy/go.mod + go.sum, built with -mod=readonly so any
#     drift from the committed checksums is a build failure, not a silent swap;
#   - GOTOOLCHAIN=local, so the `go 1.26.7` directive in that go.mod is enforced
#     by the image's own compiler instead of being papered over by an automatic
#     toolchain download from the network.
FROM golang:1.26.7-alpine3.23@sha256:b17af760035fc2f338eed92d448a6c67f2d45438844fc6c60678fa5f99e44b57 AS caddy
ENV CGO_ENABLED=0 \
    GOTOOLCHAIN=local \
    GOFLAGS=-mod=readonly
WORKDIR /build
COPY packaging/docker/caddy/go.mod packaging/docker/caddy/go.sum ./
RUN go mod download
COPY packaging/docker/caddy/main.go ./
# -trimpath keeps absolute build paths out of the binary so the two release
# runners produce the same bytes; -s -w drops the symbol and DWARF tables, which
# is what keeps this inside the 96 MiB image budget.
RUN go build -trimpath -ldflags "-s -w" -o /caddy.unprivileged . \
    && chmod 0555 /caddy.unprivileged
# Assert the three properties this stage exists to guarantee. A toolchain
# regression, a Caddy version drift, or either advisory bump being lost to a
# careless `go mod tidy` fails the build here rather than reaching a scanner.
RUN set -eu; \
    go version -m /caddy.unprivileged > /tmp/buildinfo; \
    grep -q 'go1\.26\.[6-9]' /tmp/buildinfo || grep -q 'go1\.2[7-9]\.' /tmp/buildinfo \
      || { echo 'caddy was linked with a toolchain older than go1.26.6' >&2; exit 1; }; \
    grep -q 'github.com/caddyserver/caddy/v2[[:space:]]\+v2\.11\.4' /tmp/buildinfo \
      || { echo 'caddy source version drifted from v2.11.4' >&2; exit 1; }; \
    grep -q 'google.golang.org/grpc[[:space:]]\+v1\.8[2-9]\.' /tmp/buildinfo \
      || { echo 'grpc regressed below v1.82.1 (GHSA-hrxh-6v49-42gf)' >&2; exit 1; }; \
    grep -q 'golang.org/x/text[[:space:]]\+v0\.\(39\|[4-9][0-9]\)\.' /tmp/buildinfo \
      || { echo 'x/text regressed below v0.39.0 (GO-2026-5970)' >&2; exit 1; }; \
    /caddy.unprivileged version
# The Caddyfile that ships in this image must be valid against the binary that
# ships with it. Proving that here means a config/binary mismatch cannot reach
# the runtime stage, where it would surface as a container that starts, fails to
# raise TLS, and takes the local CA down with it.
COPY packaging/docker/Caddyfile /tmp/Caddyfile
RUN /caddy.unprivileged validate --config /tmp/Caddyfile --adapter caddyfile

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
