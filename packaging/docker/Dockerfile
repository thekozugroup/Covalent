FROM golang:1.26.7-alpine3.23@sha256:b17af760035fc2f338eed92d448a6c67f2d45438844fc6c60678fa5f99e44b57 AS sync-engine-worker
ARG TARGETARCH
ENV CGO_ENABLED=0 \
    GOTOOLCHAIN=local \
    GOFLAGS=-mod=readonly
ADD --checksum=sha256:dbcc9498602286a843f29a7104833bd1422082999aa51ff92eef493172d47959 \
    https://github.com/syncthing/syncthing/archive/946e2b83a1f6c6ae119427c09e0a5802940b82ff.tar.gz \
    /tmp/syncthing-source.tar.gz
RUN mkdir -m 0700 /tmp/syncthing-source \
    && tar -xzf /tmp/syncthing-source.tar.gz --strip-components=1 -C /tmp/syncthing-source
COPY --chmod=0555 scripts/build-linux-sync-engine.sh /usr/local/bin/build-linux-sync-engine
COPY --chmod=0555 scripts/collect-go-target-license-inventory.py /usr/local/bin/collect-go-target-license-inventory
COPY --chmod=0555 scripts/collect-sync-engine-notices.py /usr/local/bin/collect-sync-engine-notices
COPY --chmod=0444 packaging/sync-engine/engine-guardian.c /tmp/engine-guardian.c
COPY --chmod=0444 LICENSE /tmp/Covalent-LICENSE.txt
COPY --chmod=0444 docs/licenses/sync-engine/OFL-1.1.txt /tmp/OFL-1.1.txt
RUN apk add --no-cache python3=3.12.14-r0 \
    && test "$(python3 --version)" = "Python 3.12.14" \
    && build-linux-sync-engine worker /tmp/syncthing-source /engine-worker "$TARGETARCH" \
    && mkdir -m 0700 /engine-evidence \
    && collect-go-target-license-inventory \
      --source-root /tmp/syncthing-source \
      --module-cache /go/pkg/mod \
      --target "linux-$TARGETARCH" linux "$TARGETARCH" 0 noupgrade \
        /engine-worker/go-target-deps.ndjson \
      --output /engine-evidence/target-license-inventory.json \
    && collect-sync-engine-notices \
      --inventory /engine-evidence/target-license-inventory.json \
      --source-root /tmp/syncthing-source \
      --module-cache /go/pkg/mod \
      --go-root /usr/local/go \
      --guardian-source /tmp/engine-guardian.c \
      --project-license /tmp/Covalent-LICENSE.txt \
      --ofl-license /tmp/OFL-1.1.txt \
      --output /engine-notices \
    && install -m 0444 /engine-evidence/target-license-inventory.json \
      /engine-worker/target-license-inventory.json

FROM rust:1.97.1-alpine3.23@sha256:c4a364ddbf684fe038e6fa6a4f25b30c8dc85247423e0e660676ece0d17be4a2 AS builder
ARG TARGETARCH
WORKDIR /source
ENV CARGO_INCREMENTAL=0
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY packaging/web ./packaging/web
RUN cargo build --locked --release -p covalent-node -p covalent-cli
COPY --chmod=0444 packaging/sync-engine/engine-guardian.c /tmp/engine-guardian.c
COPY --chmod=0555 scripts/build-linux-sync-engine.sh /usr/local/bin/build-linux-sync-engine
COPY --from=sync-engine-worker /engine-worker /engine-worker
COPY --from=sync-engine-worker /engine-notices /engine-notices
RUN build-linux-sync-engine package \
      /engine-worker /tmp/engine-guardian.c /engine-notices /sync-engine "$TARGETARCH"

# Caddy is compiled here rather than lifted out of caddy:2.11.4-alpine, whose
# published binary is linked against go1.26.3. That toolchain accounted for ten
# of the twelve high/critical findings the Grype gate reported, and no Caddy tag
# fixes it: 2.11.4 is the newest x.y.z-alpine upstream publishes, so the only
# lever left is to control the toolchain ourselves.
#
# This is not a fork. packaging/docker/caddy is the same three-line consumer
# module `xcaddy` generates and the official image builds - it imports
# caddy/v2/modules/standard and nothing else. The selected upstream snapshot is
# the canonical compatibility commit b2693fb63a30e6d7be0972c3645e9a2c0a500e93,
# 33 commits after v2.11.4, adapting Caddy's CEL matcher to the post-v0.28 API.
# The consumer module then selects cel-go v0.30.0, which fixes GO-2026-6094.
# The full upstream delta is recorded and reviewed in
# docs/security/container-image-vulnerabilities.md; its pseudo-version is
# checked below so the snapshot cannot float.
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
# keeps the combined runtime within its reviewed image budget.
RUN go build -buildvcs=false -trimpath -ldflags "-s -w" -o /caddy.unprivileged . \
    && chmod 0555 /caddy.unprivileged
# Assert the three properties this stage exists to guarantee. A toolchain
# regression, a Caddy version drift, or either advisory bump being lost to a
# careless `go mod tidy` fails the build here rather than reaching a scanner.
RUN set -eu; \
    go version -m /caddy.unprivileged > /tmp/buildinfo; \
    grep -q 'go1\.26\.[6-9]' /tmp/buildinfo || grep -q 'go1\.2[7-9]\.' /tmp/buildinfo \
      || { echo 'caddy was linked with a toolchain older than go1.26.6' >&2; exit 1; }; \
    grep -q 'github.com/caddyserver/caddy/v2[[:space:]]\+v2\.11\.5-0\.20260711231708-b2693fb63a30' /tmp/buildinfo \
      || { echo 'caddy source snapshot drifted from v2.11.5-0.20260711231708-b2693fb63a30' >&2; exit 1; }; \
    awk '$1 == "dep" && $2 == "google.golang.org/grpc" && $3 == "v1.83.2" { found = 1 } END { exit !found }' /tmp/buildinfo \
      || { echo 'grpc drifted from reviewed v1.83.2 (GHSA-hrxh-6v49-42gf, GHSA-vp52-pcj8-j9qc, GHSA-2v4p-qf9q-27wj)' >&2; exit 1; }; \
    grep -q 'golang.org/x/text[[:space:]]\+v0\.\(39\|[4-9][0-9]\)\.' /tmp/buildinfo \
      || { echo 'x/text regressed below v0.39.0 (GO-2026-5970)' >&2; exit 1; }; \
    grep -q 'go.opentelemetry.io/otel[[:space:]]\+v1\.\(4[4-9]\|[5-9][0-9]\)\.' /tmp/buildinfo \
      || { echo 'OpenTelemetry regressed below v1.44.0 (GO-2026-5158)' >&2; exit 1; }; \
    grep -q 'github.com/google/cel-go[[:space:]]\+v0\.30\.0' /tmp/buildinfo \
      || { echo 'cel-go drifted from reviewed v0.30.0 (GO-2026-6094)' >&2; exit 1; }; \
    grep -q 'github.com/go-chi/chi/v5[[:space:]]\+v5\.3\.0' /tmp/buildinfo \
      || { echo 'go-chi drifted from reviewed v5.3.0 (GO-2026-5774/5775/5777)' >&2; exit 1; }; \
    grep -q 'github.com/klauspost/compress[[:space:]]\+v1\.18\.7' /tmp/buildinfo \
      || { echo 'klauspost/compress drifted from reviewed v1.18.7 (GO-2026-5841)' >&2; exit 1; }; \
    /caddy.unprivileged version
# The Caddyfile that ships in this image must be valid against the binary that
# ships with it. Proving that here means a config/binary mismatch cannot reach
# the runtime stage, where it would surface as a container that starts, fails to
# raise TLS, and takes the local CA down with it.
COPY packaging/docker/Caddyfile /tmp/Caddyfile
RUN /caddy.unprivileged validate --config /tmp/Caddyfile --adapter caddyfile

# Generate target-specific Caddy distribution evidence in a build-only stage.
# Python and the private module cache never enter the runtime image. The main
# consumer's license is copied only after the binary build, so it cannot alter
# executable bytes but is available as exact license evidence for go list's
# main module. TARGETARCH is the same Buildx target used by the shipped binary.
FROM caddy AS caddy-evidence
ARG TARGETARCH
COPY --chmod=0555 scripts/collect-caddy-distribution-evidence.py /usr/local/bin/collect-caddy-distribution-evidence
COPY --chmod=0444 LICENSE /build/LICENSE
# Root owns the evidence directories; owner write permits unprivileged docker cp extraction.
RUN set -eu; \
    case "$TARGETARCH" in amd64|arm64) ;; *) echo 'unsupported Caddy target architecture' >&2; exit 64 ;; esac; \
    apk add --no-cache python3=3.12.14-r0; \
    GOOS=linux GOARCH="$TARGETARCH" CGO_ENABLED=0 \
      go list -mod=readonly -deps -json . > "/tmp/caddy-go-target-deps-linux-${TARGETARCH}.ndjson"; \
    test -s "/tmp/caddy-go-target-deps-linux-${TARGETARCH}.ndjson"; \
    test "$(wc -c < "/tmp/caddy-go-target-deps-linux-${TARGETARCH}.ndjson")" -le 134217728; \
    go mod verify; \
    collect-caddy-distribution-evidence \
      --source-root /build \
      --module-cache /go/pkg/mod \
      --go-root /usr/local/go \
      --binary /caddy.unprivileged \
      --apk-installed /lib/apk/db/installed \
      --ca-bundle /etc/ssl/certs/ca-certificates.crt \
      --target "linux-${TARGETARCH}" linux "$TARGETARCH" "/tmp/caddy-go-target-deps-linux-${TARGETARCH}.ndjson" \
      --output /caddy-evidence; \
    find /caddy-evidence -type d -exec chmod 0755 {} +; \
    find /caddy-evidence -type f -exec chmod 0444 {} +

# Build the reviewed local BusyBox revision from frozen corresponding source.
FROM alpine:3.23@sha256:fd791d74b68913cbb027c6546007b3f0d3bc45125f797758156952bc2d6daf40 AS busybox-package-builder

ARG SOURCE_DATE_EPOCH=1781306698
ENV SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH} \
    SRCDEST=/home/package-builder/distfiles
RUN apk add --no-cache \
      abuild=3.16.0-r0 \
      alpine-sdk=1.1-r0 \
      build-base=0.5-r3 \
      git=2.52.0-r0 \
      linux-headers=6.16.12-r0 \
      openssl-dev=3.5.8-r0 \
      perl=5.42.2-r0 \
      python3=3.12.14-r0 \
      sudo=1.9.17_p2-r0 \
      utmps-dev=0.1.3.1-r0 \
      utmps-static=0.1.3.1-r0 \
      zip=3.0-r13 \
    && adduser -D -G abuild package-builder \
    && install -d -o package-builder -g abuild /home/package-builder/distfiles \
    && printf '%s\n' 'package-builder ALL=(ALL) NOPASSWD: ALL' \
      > /etc/sudoers.d/package-builder \
    && chmod 0440 /etc/sudoers.d/package-builder

COPY packaging/docker/alpine/busybox-cve-2025-60876/ /busybox-overlay/
RUN python3 -B /busybox-overlay/verify_source_backport.py \
      --source-tar /busybox-overlay/busybox-1.37.0.tar.bz2
COPY --chown=package-builder:abuild packaging/docker/alpine/busybox-cve-2025-60876/busybox-1.37.0.tar.bz2 /home/package-builder/distfiles/
COPY --chown=package-builder:abuild packaging/docker/alpine/busybox-cve-2025-60876/aport/ /home/package-builder/aport/
USER package-builder
WORKDIR /home/package-builder/aport
# This key signs only the throwaway build-stage repository. Its public half is
# installed in the disposable builder so abuild can index its repository, then
# used through apk's per-command --keys-dir in package-test. It is never added
# to the tested runtime's global trust store.
RUN abuild-keygen -a -i -n \
    && abuild -r \
    && mkdir -p /home/package-builder/output/keys \
    && apk info -vv | LC_ALL=C sort > /home/package-builder/output/builder-packages.txt \
    && find /home/package-builder/packages -type f -name '*.apk' \
      -exec cp '{}' /home/package-builder/output/ ';' \
    && find /home/package-builder/.abuild -type f -name '*.rsa.pub' \
      -exec cp '{}' /home/package-builder/output/keys/ ';' \
    && test "$(find /home/package-builder/output -type f -name '*.apk' | wc -l)" -eq 11 \
    && for package in \
      busybox busybox-binsh busybox-doc busybox-extras busybox-extras-openrc \
      busybox-ifupdown busybox-mdev-openrc busybox-openrc busybox-static \
      busybox-suid ssl_client; do \
        test -f "/home/package-builder/output/$package-1.37.0-r1000.apk"; \
      done \
    && test "$(find /home/package-builder/output/keys -type f -name '*.rsa.pub' | wc -l)" -eq 1

FROM alpine:3.23@sha256:fd791d74b68913cbb027c6546007b3f0d3bc45125f797758156952bc2d6daf40 AS runtime-base
ARG VCS_REF=unknown
ARG COVALENT_SOURCE_FINGERPRINT=unknown
ARG RELEASE_VERSION=development
LABEL org.opencontainers.image.title="Covalent" \
      org.opencontainers.image.description="Private distributed backup node" \
      org.opencontainers.image.source="https://github.com/thekozugroup/Covalent" \
      org.opencontainers.image.revision="$VCS_REF" \
      io.covalent.source.fingerprint="$COVALENT_SOURCE_FINGERPRINT" \
      org.opencontainers.image.version="$RELEASE_VERSION" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.base.name="docker.io/library/alpine:3.23" \
      org.opencontainers.image.base.digest="sha256:fd791d74b68913cbb027c6546007b3f0d3bc45125f797758156952bc2d6daf40" \
      io.covalent.sync-engine.version="v2.1.3" \
      io.covalent.sync-engine.commit="946e2b83a1f6c6ae119427c09e0a5802940b82ff" \
      io.covalent.sync-engine.source-sha256="dbcc9498602286a843f29a7104833bd1422082999aa51ff92eef493172d47959" \
      io.covalent.runtime.openssl.version="3.5.8-r0"

# Alpine published the fixed OpenSSL security revision after the pinned 3.23.5
# base image was assembled. Install only those two signed repository packages
# at exact versions: a future repository change can fail this build, but cannot
# silently select a different runtime. Both libraries are retained because
# apk-tools and BusyBox ssl_client require them even though Covalent and Caddy
# are statically linked and do not load OpenSSL.
RUN apk add --no-cache --upgrade \
      libcrypto3=3.5.8-r0 \
      libssl3=3.5.8-r0 \
    && apk info -e 'libcrypto3=3.5.8-r0' \
    && apk info -e 'libssl3=3.5.8-r0' \
    && addgroup -S -g 65532 covalent \
    && adduser -S -D -H -u 65532 -G covalent -s /sbin/nologin covalent \
    && install -d -m 0700 -o 65532 -g 65532 /home/covalent /config /data /restore

# Exercise the newly built packages with the same runtime base and OpenSSL.
FROM runtime-base AS busybox-package-test
COPY --from=busybox-package-builder /home/package-builder/output/ /packages/
COPY --chmod=0555 packaging/docker/alpine/busybox-cve-2025-60876/tests/test_wget_request_target.py /test_wget_request_target.py
RUN apk add --no-cache python3=3.12.14-r0 \
    && set -- \
      /packages/busybox-1.37.0-r1000.apk \
      /packages/busybox-binsh-1.37.0-r1000.apk \
      /packages/ssl_client-1.37.0-r1000.apk \
    && test -f "$1" && test -f "$2" && test -f "$3" \
    && apk --keys-dir /packages/keys --no-network \
      --repositories-file /dev/null add --upgrade "$@" \
    && apk info --exists 'busybox=1.37.0-r1000' \
    && apk info --exists 'busybox-binsh=1.37.0-r1000' \
    && apk info --exists 'ssl_client=1.37.0-r1000' \
    && apk info --who-owns /bin/busybox \
      | grep -Fx '/bin/busybox is owned by busybox-1.37.0-r1000' \
    && apk info --who-owns /bin/sh \
      | grep -Fx '/bin/sh is owned by busybox-binsh-1.37.0-r1000' \
    && apk info --who-owns /usr/bin/ssl_client \
      | grep -Fx '/usr/bin/ssl_client is owned by ssl_client-1.37.0-r1000' \
    && /test_wget_request_target.py --busybox /bin/busybox \
    && sha256sum /bin/busybox /bin/sh /usr/bin/ssl_client > /packages/runtime-payload.sha256

FROM runtime-base AS runtime-patched
LABEL io.covalent.runtime.busybox.version="1.37.0-r1000" \
      io.covalent.runtime.busybox.source-sha256="3311dff32e746499f4df0d5df04d7eb396382d7e108bb9250e7b519b837043a4" \
      io.covalent.runtime.busybox.patch-sha256="bdd8391fa2e4020079df71557d880e475f5f517fa949b5e80ef633cdafc71a2a" \
      io.covalent.runtime.busybox.alpine-aports-commit="1e823a60eb85606954b3a5af5f8e5bbd1ea680cf" \
      io.covalent.runtime.busybox.original-apkbuild-sha256="fb2323ac3121c6cd1122c42cabbd19ec28a29a1cf434a4c0759792c63a1c63a8"
# Depending on the tested stage forces all wire checks to pass for this target.
# The command-scoped public key and APK files exist only in this read-only mount.
RUN --mount=type=bind,from=busybox-package-test,source=/packages,target=/tmp/busybox-backport,readonly \
    apk --keys-dir /tmp/busybox-backport/keys --no-network \
      --repositories-file /dev/null add --upgrade \
      /tmp/busybox-backport/busybox-1.37.0-r1000.apk \
      /tmp/busybox-backport/busybox-binsh-1.37.0-r1000.apk \
      /tmp/busybox-backport/ssl_client-1.37.0-r1000.apk \
    && apk info --exists 'busybox=1.37.0-r1000' \
    && apk info --exists 'busybox-binsh=1.37.0-r1000' \
    && apk info --exists 'ssl_client=1.37.0-r1000' \
    && apk info --who-owns /bin/busybox \
      | grep -Fx '/bin/busybox is owned by busybox-1.37.0-r1000' \
    && apk info --who-owns /bin/sh \
      | grep -Fx '/bin/sh is owned by busybox-binsh-1.37.0-r1000' \
    && apk info --who-owns /usr/bin/ssl_client \
      | grep -Fx '/usr/bin/ssl_client is owned by ssl_client-1.37.0-r1000' \
    && sha256sum -c /tmp/busybox-backport/runtime-payload.sha256

# Bind the source and notices to the actual installed runtime database. The
# copied CA comes from the unmodified Caddy builder; Python remains build-only.
FROM caddy-evidence AS alpine-evidence
ARG TARGETARCH
COPY --from=runtime-patched /lib/apk/db/installed /tmp/alpine-runtime-installed
COPY --from=caddy /lib/apk/db/installed /tmp/alpine-builder-installed
COPY --from=caddy /etc/ssl/certs/ca-certificates.crt /tmp/alpine-ca.crt
COPY --chmod=0555 scripts/collect-alpine-runtime-evidence.py /usr/local/bin/collect-alpine-runtime-evidence
COPY packaging/docker/alpine/ /alpine-source/
RUN collect-alpine-runtime-evidence --arch "$TARGETARCH" \
      --lock /alpine-source/runtime-source-lock.json \
      --installed /tmp/alpine-runtime-installed \
      --builder-installed /tmp/alpine-builder-installed \
      --ca-bundle /tmp/alpine-ca.crt \
      --licenses /alpine-source/licenses \
      --local-sources /alpine-source/busybox-cve-2025-60876 \
      --cache /tmp/alpine-source-cache --download --output /alpine-evidence

FROM runtime-patched
COPY --from=builder /source/target/release/covalent-node /usr/local/bin/covalent-node
COPY --from=builder /source/target/release/covalent /usr/local/bin/covalent
COPY --from=caddy /caddy.unprivileged /usr/local/bin/caddy
COPY --from=caddy-evidence /caddy-evidence/ /usr/local/share/covalent/caddy/
COPY --from=alpine-evidence /alpine-evidence/ /usr/local/share/covalent/alpine/
COPY --from=builder /sync-engine/bin/ /usr/local/libexec/covalent-sync-engine/
COPY --from=builder /sync-engine/share/ /usr/local/share/covalent/sync-engine/
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
EXPOSE 8787/udp 8789/tcp 8443/tcp
VOLUME ["/config", "/data"]
USER 65532:65532
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["covalent-node", "healthcheck", "--url", "http://127.0.0.1:8787/healthz"]
ENTRYPOINT ["covalent-entrypoint"]
CMD ["serve"]
