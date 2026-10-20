# pigeonpost-postbox — multi-stage image.
#
# Debian-slim runtime rather than distroless, matching deploy/Dockerfile's rationale: the postbox
# makes outbound TLS to the registry / OAuth, so it needs glibc + CA certificates. Build context is
# the repo root (see docker-compose.yml `build.context: ../..`); the root .dockerignore whitelists
# only crates/ + the manifests, so the context stays small.
#
# The runtime is bookworm. It used to be bullseye, because the old deploy box ran Docker 18.09,
# whose seccomp profile blocks the clone3 call bookworm's glibc makes. The postbox moved to a host
# with Docker 29, and bullseye's security apt pool now answers 404 (Debian 11 is past LTS), so the
# stock bullseye runtime stopped building at all. The build stage stays on bullseye: a binary
# linked against glibc 2.31 runs unchanged on bookworm's 2.36, never the other way round.
# The runtime user keeps uid 65532, so the data volume's ownership is unchanged.
ARG BUILD_BASE=docker.io/library/rust:1-bullseye
ARG RUNTIME_BASE=docker.io/library/debian:bookworm-slim

FROM ${BUILD_BASE} AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p pigeonpost-postbox

FROM ${RUNTIME_BASE}
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --uid 65532 --user-group --no-create-home --shell /usr/sbin/nologin postbox
COPY --from=build /src/target/release/pigeonpost-postbox /usr/local/bin/pigeonpost-postbox
USER postbox
EXPOSE 8990
ENTRYPOINT ["/usr/local/bin/pigeonpost-postbox"]
