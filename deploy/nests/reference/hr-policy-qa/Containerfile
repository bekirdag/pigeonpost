# Marketplace reference release (build plan M1): HR policy Q&A.
#
# The image ASSEMBLY only. The build plane compiled the binary first (`nest-build.toml`, in gVisor
# with no network), in a copy of this tree with the vendored dependencies; this file copies the
# result onto one base image pinned by digest. Nothing runs here: RUN, ADD and COPY --from are
# refused, and the base must already be on the host (`--pull=never`).
ARG BASE_IMAGE=docker.io/library/debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3

FROM ${BASE_IMAGE}
COPY target/release/hr-policy-qa /usr/local/bin/hr-policy-qa
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/hr-policy-qa"]
