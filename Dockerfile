# syntax=docker/dockerfile:1
# DiskTree Hub, for amd64 and arm64:
#
#   docker buildx build --platform linux/amd64,linux/arm64 -t disktree-hub .
#
# The build stage runs on the build machine and cross-compiles, so building
# for arm64 on a PC needs no emulation. The final image runs the Hub as
# PUID:PGID (99:100 by default, Unraid's nobody:users), with its database
# and key in /data. scripts/release.sh builds only the final image, from
# the Hub it has compiled already (see there).

FROM --platform=$BUILDPLATFORM rust:1-bookworm AS build
ARG TARGETARCH
RUN case "$TARGETARCH" in \
      amd64) echo x86_64-unknown-linux-gnu > /target ;; \
      arm64) echo aarch64-unknown-linux-gnu > /target ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac \
 && if [ "$(cat /target)" != "$(rustc -vV | sed -n 's/^host: //p')" ]; then \
      apt-get update \
      && apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
      && rm -rf /var/lib/apt/lists/*; \
    fi \
 && rustup target add "$(cat /target)"
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target,id=disktree-hub-target-$TARGETARCH \
    cargo build --release --locked -p disktree-hub --target "$(cat /target)" \
 && mkdir -p "/hub/$TARGETARCH" \
 && cp "target/$(cat /target)/release/disktree-hub" "/hub/$TARGETARCH/" \
 && cp /etc/ssl/certs/ca-certificates.crt /hub/

# What the final image takes: the Hub in a folder per architecture, and
# certificates for reaching the sign-in provider and alert services over
# HTTPS, so the final image runs nothing on the target architecture.
FROM scratch AS hub
COPY --from=build /hub/ /

# The final image. scripts/release.sh builds from here on, with its own
# `hub` stage.
FROM debian:bookworm-slim
ARG TARGETARCH
COPY --from=hub /ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=hub /$TARGETARCH/disktree-hub /usr/local/bin/disktree-hub
COPY --chmod=755 packaging/docker/entrypoint.sh /usr/local/bin/entrypoint.sh
ENV DISKTREE_HUB_DATA=/data \
    DISKTREE_HUB_LISTEN=0.0.0.0:9090 \
    PUID=99 \
    PGID=100
VOLUME /data
EXPOSE 9090
HEALTHCHECK --interval=60s --timeout=10s --start-period=20s CMD ["/usr/local/bin/disktree-hub", "health"]
LABEL org.opencontainers.image.title="DiskTree Hub" \
      org.opencontainers.image.description="One dashboard for the storage of all your devices" \
      org.opencontainers.image.source="https://github.com/McGeaverBeaver/TreeSizeCloneAndroid"
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
