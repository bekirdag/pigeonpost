# pigeonpost-postbox — multi-stage image.
#
# Debian-slim runtime rather than distroless, matching deploy/Dockerfile's rationale: the postbox
# makes outbound TLS to the registry / OAuth, so it needs glibc + CA certificates. Build context is
# the repo root (see docker-compose.yml `build.context: ../..`); the root .dockerignore whitelists
# only crates/ + the manifests, so the context stays small.
ARG BUILD_BASE=docker.io/library/rust:1-bookworm
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
