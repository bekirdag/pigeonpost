# Pigeonpost — one image, every role.
#
# The binary is the same for a loft, a registry, and a directory; the compose service decides which
# subcommand runs. One image means one thing to build, scan, and pin.

# The official release uses current-stable Debian 13 (Trixie) for both stages. Bullseye's LTS ends
# on 2026-08-31, so it is only an execution floor for the separate static Linux release assets—not
# an acceptable production-image base. Every base and runtime package is an immutable release
# input; changing the Debian family also requires supplying that family's exact package versions
# and re-running compatibility acceptance on every supported architecture.
ARG BUILD_BASE=docker.io/library/rust:1.88.0-trixie@sha256:f2a17efbe58b00470be6e73dbea79705a789ee20ee8de2dcdaf73c0c6091f1db
ARG RUNTIME_BASE=docker.io/library/debian:trixie-slim@sha256:3a39a0592364683e6bab97937b72cad5a8fa6dcbbee90edb3bb48c7f8e94f258
ARG CA_CERTIFICATES_VERSION=20250419
ARG CURL_VERSION=8.14.1-2+deb13u4
ARG TINI_VERSION=0.19.0-3+b7

# ---- build ----------------------------------------------------------------------------------
FROM ${BUILD_BASE} AS build

WORKDIR /src

# Manifests first, so a source-only change does not re-download the dependency tree.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates

RUN cargo build --release --locked -p pigeonpost-cli \
 && strip target/release/pigeonpost

# ---- runtime --------------------------------------------------------------------------------
# (ARG is re-scoped after each FROM; see the block at the top for why this is parameterised.)
# Debian slim rather than distroless or alpine: SQLite is statically bundled, but glibc and CA
# certificates are still needed for outbound TLS (OIDC verification, prober fetches), and a shell
# is worth having when an operator is debugging someone else's donated node.
FROM ${RUNTIME_BASE}

ARG CA_CERTIFICATES_VERSION
ARG CURL_VERSION
ARG TINI_VERSION

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates=${CA_CERTIFICATES_VERSION} \
      curl=${CURL_VERSION} \
      tini=${TINI_VERSION} \
 && rm -rf /var/lib/apt/lists/*

# Unprivileged, with a fixed uid so host bind-mount ownership is predictable.
RUN groupadd --gid 10001 pigeonpost \
 && useradd --uid 10001 --gid 10001 --home-dir /var/lib/pigeonpost --create-home pigeonpost \
 && install -d -m 0700 -o 10001 -g 10001 \
      /var/lib/pigeonpost-traces \
      /var/lib/pigeonpost-traces/loft-network \
      /var/lib/pigeonpost-traces/registry-network \
      /var/lib/pigeonpost-traces/registry-identity

COPY --from=build --chown=0:0 /src/target/release/pigeonpost /usr/local/bin/pigeonpost

USER pigeonpost
WORKDIR /var/lib/pigeonpost

# tini reaps zombies and forwards signals, so graceful shutdown actually reaches the process and
# the retention sweep is not killed mid-batch.
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/pigeonpost"]
CMD ["--help"]
STOPSIGNAL SIGTERM
