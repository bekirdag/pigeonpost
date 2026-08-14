# Pigeonpost — one image, every role.
#
# The binary is the same for a loft, a registry, and a directory; the compose service decides which
# subcommand runs. One image means one thing to build, scan, and pin.

# Statically linked against musl, on a distroless base with no OS packages at all.
#
# This replaced a Debian-slim runtime on 2026-08-14. The old comment here argued for Debian on
# three grounds: glibc, CA certificates, and a shell for debugging a donated node. The first is
# moot once the binary is static, the second is satisfied by distroless/static (it ships
# ca-certificates), and the third was traded away deliberately — Debian trixie carries critical
# CVEs in curl, perl-base and libc6 that Debian has marked **won't fix**, so no base bump could
# ever clear them and the release scan failed on every build. A static binary on an empty base has
# nothing to have a CVE in: `grype` reports zero findings against distroless/static.
#
# What that costs: **there is no shell in this image.** `docker exec … sh` will not work. Debug a
# running container with `docker logs`, `docker cp`, or by attaching a separate toolbox container
# sharing its namespaces.
#
# musl rather than glibc-static because glibc's static linking of NSS is a known trap; the release
# pipeline already builds these exact musl targets for the published Linux binaries, so this uses
# a toolchain that is proven rather than a new one.
ARG BUILD_BASE=docker.io/library/rust:1.88.0-alpine3.22@sha256:9dfaae478ecd298b6b5a039e1f2cc4fc040fc818a2de9aa78fa714dea036574d
ARG RUNTIME_BASE=gcr.io/distroless/static-debian13:nonroot@sha256:f7f8f729987ad0fdf6b05eeeae94b26e6a0f613bdf46feea7fc40f7bd72953e6
ARG MUSL_VERSION=1.2.5-r12
# tini is still load-bearing: the CLI's serve paths handle ctrl_c (SIGINT) but not SIGTERM, and a
# process that is PID 1 gets no default signal handlers — so without an init, `docker stop` would
# be ignored until the SIGKILL timeout and the retention sweep would die mid-batch. The static
# build is used because there is no libc in the runtime to link against.
ARG TINI_VERSION=v0.19.0
ARG TINI_SHA256_AMD64=c5b0666b4cb676901f90dfcb37106783c5fe2077b04590973b885950611b30ee
ARG TINI_SHA256_ARM64=eae1d3aa50c48fb23b8cbdf4e369d0910dfc538566bfd09df89a774aa84a48b9

# Where the release binary comes from: `compile` builds it here (the default, so `docker build .`
# still works for local use and for deploy/acceptance/install-defaults.sh), `prebuilt` takes the
# one the release matrix already built on a native runner.
#
# The release passes `prebuilt` because building arm64 in this Dockerfile means QEMU: ~70 minutes
# of emulated compilation for a binary the pipeline had already produced natively minutes earlier.
# Reusing it is not only faster, it is stricter — the image then provably contains the same bytes
# as the published linux asset, rather than a second build that merely ought to match.
ARG BINARY_STAGE=compile

# ---- build ----------------------------------------------------------------------------------
FROM ${BUILD_BASE} AS compile

ARG MUSL_VERSION
# The digest-pinned Rust/Alpine image carries a compiler but no C development headers, and both
# `ring` and the bundled SQLite compile C during the build.
RUN apk add --no-cache musl=${MUSL_VERSION} musl-dev=${MUSL_VERSION}

WORKDIR /src

# Manifests first, so a source-only change does not re-download the dependency tree.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates

# Alpine's Rust is already musl-native, so the host target is the static one.
#
# The staticness check reads the ELF rather than parsing `ldd`. musl's ldd wording varies by
# architecture — x86_64 produces a static-PIE whose output looks nothing like aarch64's — and
# matching on that failed the amd64 build while the binary was perfectly static. A dynamic
# dependency, which is the thing actually worth catching, always shows up as a NEEDED entry, and
# that is true for static, static-PIE and dynamic alike.
RUN cargo build --release --locked -p pigeonpost-cli \
 && strip target/release/pigeonpost \
 && if readelf -d target/release/pigeonpost 2>/dev/null | grep -q NEEDED; then \
      echo "the release binary links a shared library; a dynamic dependency crept in"; \
      readelf -d target/release/pigeonpost | grep NEEDED; \
      exit 1; \
    fi \
 && install -D target/release/pigeonpost /out/pigeonpost

# ---- prebuilt -------------------------------------------------------------------------------
# Only built when BINARY_STAGE selects it, so the missing `dist/` costs a source build nothing:
# BuildKit builds only the stages the target actually depends on.
FROM --platform=$BUILDPLATFORM ${BUILD_BASE} AS prebuilt

ARG TARGETARCH
COPY dist/linux/${TARGETARCH}/pigeonpost /out/pigeonpost
RUN chmod 0755 /out/pigeonpost \
 && if readelf -d /out/pigeonpost 2>/dev/null | grep -q NEEDED; then \
      echo "the prebuilt release binary links a shared library"; exit 1; \
    fi

# ---- binary ---------------------------------------------------------------------------------
# Indirection so the runtime copies from one place regardless of which route produced it.
FROM ${BINARY_STAGE} AS binary

# ---- init ------------------------------------------------------------------------------------
# Fetched rather than packaged, because the runtime has no package manager. Verified against a
# pinned digest, so a compromised or swapped release fails the build instead of shipping.
FROM --platform=$BUILDPLATFORM ${BUILD_BASE} AS init

ARG TINI_VERSION
ARG TINI_SHA256_AMD64
ARG TINI_SHA256_ARM64
ARG TARGETARCH

RUN set -eux; \
    case "${TARGETARCH}" in \
      amd64) suffix=static-amd64; expected="${TINI_SHA256_AMD64}" ;; \
      arm64) suffix=static-arm64; expected="${TINI_SHA256_ARM64}" ;; \
      *) echo "unsupported architecture ${TARGETARCH}"; exit 1 ;; \
    esac; \
    wget -q -O /tini "https://github.com/krallin/tini/releases/download/${TINI_VERSION}/tini-${suffix}"; \
    echo "${expected}  /tini" | sha256sum -c -; \
    chmod 0755 /tini

# ---- rootfs ----------------------------------------------------------------------------------
# distroless ships a `nonroot` user at uid 65532, but this image has always run as 10001 and host
# bind mounts on deployed nodes are owned accordingly. Changing it would silently break every
# existing volume, so the account is carried over rather than adopted.
FROM --platform=$BUILDPLATFORM ${BUILD_BASE} AS rootfs

RUN set -eux; \
    addgroup -g 10001 pigeonpost; \
    adduser -D -u 10001 -G pigeonpost -h /var/lib/pigeonpost pigeonpost; \
    install -d -m 0700 -o 10001 -g 10001 \
      /rootfs/var/lib/pigeonpost \
      /rootfs/var/lib/pigeonpost-traces \
      /rootfs/var/lib/pigeonpost-traces/loft-network \
      /rootfs/var/lib/pigeonpost-traces/registry-network \
      /rootfs/var/lib/pigeonpost-traces/registry-identity; \
    printf 'root:x:0:0:root:/root:/sbin/nologin\npigeonpost:x:10001:10001::/var/lib/pigeonpost:/sbin/nologin\n' > /passwd; \
    printf 'root:x:0:\npigeonpost:x:10001:\n' > /group

# ---- runtime ---------------------------------------------------------------------------------
FROM ${RUNTIME_BASE}

# distroless has no shell to append to a file, so complete passwd/group files are composed in the
# previous stage and replace the base's. Both list root and pigeonpost only; distroless's own
# `nonroot` (65532) goes with them, which is intended — nothing here runs as it.
COPY --from=rootfs /passwd /etc/passwd
COPY --from=rootfs /group /etc/group
COPY --from=rootfs --chown=10001:10001 /rootfs/var /var
COPY --from=init /tini /usr/bin/tini
COPY --from=binary --chown=0:0 /out/pigeonpost /usr/local/bin/pigeonpost

# `USER` takes a numeric id because there is no shell to resolve a name against, and a numeric id
# is what the kernel and the host's bind mounts agree on anyway.
USER 10001:10001
WORKDIR /var/lib/pigeonpost

# tini reaps zombies and forwards signals, so graceful shutdown actually reaches the process and
# the retention sweep is not killed mid-batch.
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/pigeonpost"]
CMD ["--help"]
STOPSIGNAL SIGTERM
