# Marketplace reference release (build plan M1): HR policy Q&A.
#
# Built by the build plane (`pigeonpost-nest-release build`) as a rootless podman build with no
# network: the dependencies were vendored in an earlier, separate fetch step and arrive in the
# build context as `vendor/` with a `.cargo/config.toml` that points cargo at them. Both base
# images are pinned by digest and must already be on the host (`--pull=never`).
ARG RUST_IMAGE=docker.io/library/rust:1.91.1-slim-bookworm@sha256:8514999d4786ef12efe89239e86b3d0a021b94b9d35108c8efe6c79ca7dc1a65
ARG BASE_IMAGE=docker.io/library/debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3

FROM ${RUST_IMAGE} AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked --offline \
 && strip target/release/hr-policy-qa

FROM ${BASE_IMAGE}
COPY --from=build /src/target/release/hr-policy-qa /usr/local/bin/hr-policy-qa
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/hr-policy-qa"]
