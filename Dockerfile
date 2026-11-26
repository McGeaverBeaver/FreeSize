# syntax=docker/dockerfile:1
# FreeSize Hub, for amd64 and arm64:
#
#   docker buildx build --platform linux/amd64,linux/arm64 -t freesize-hub .
#
# The build stage runs on the build machine and cross-compiles, so building
# for arm64 on a PC needs no emulation. The final image runs the Hub as
# PUID:PGID (99:100 by default, Unraid's nobody:users), with its database
# and key in /data. scripts/release.sh builds only the final image, from
# the Hub it has compiled already (see there).

FROM --platform=$BUILDPLATFORM rust:1-trixie AS build
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
    --mount=type=cache,target=/src/target,id=freesize-hub-target-$TARGETARCH \
    cargo build --release --locked -p freesize-hub --target "$(cat /target)" \
 && mkdir -p "/hub/$TARGETARCH" \
 && cp "target/$(cat /target)/release/freesize-hub" "/hub/$TARGETARCH/" \
 && cp /etc/ssl/certs/ca-certificates.crt /hub/

# What the final image takes: the Hub in a folder per architecture, and
# certificates for reaching the sign-in provider and alert services over
# HTTPS, built without running anything on the target architecture.
FROM scratch AS hub
COPY --from=build /hub/ /

# The final image. scripts/release.sh builds from here on, with its own
# `hub` stage.
#
# First Debian 13 with its fixes, cut down to what the Hub and the start
# script use. The Hub needs only glibc and libgcc (it brings its own TLS and
# SQLite); entrypoint.sh needs sh (dash), coreutils' mkdir, id, stat and
# chown, and util-linux's setpriv with the one library it loads (libcap-ng).
# setpriv stays because it switches user with only SETUID and SETGID, so
# a container started with --cap-drop keeps working (coreutils' chroot
# --userspec would need SYS_CHROOT too). Kept besides: dpkg and the
# libraries it and coreutils load, so scanners still see what's installed,
# and a few small tools (grep, sed, find, awk, tar) for looking around with
# `docker exec`. Everything else goes, apt, gpgv/sqv, perl, bash, PAM,
# login and util-linux's other libraries among them: nothing in the image
# runs them, and no package is installed later. (util-linux's own programs
# besides setpriv can't run without those libraries; nothing uses them.)
# Nothing keeps a setuid or setgid bit: the Hub never needs to gain rights.
# This runs on the image's own architecture, so building for arm64 on a PC
# needs QEMU (see scripts/release.sh).
FROM debian:trixie-slim AS base
RUN set -eu; \
    export DEBIAN_FRONTEND=noninteractive; \
    apt-get update --error-on=any; \
    apt-get -y upgrade; \
    keep="base-files base-passwd coreutils dash debianutils dpkg findutils \
          gcc-14-base grep mawk sed tar tzdata libacl1 libattr1 libbz2-1.0 libc6 \
          libcap2 libgcc-s1 libgmp10 liblzma5 libmd0 libpcre2-8-0 libselinux1 \
          libzstd1 zlib1g util-linux libcap-ng0"; \
    last="debconf perl-base libcrypt1 libc-bin"; \
    remove="$(dpkg-query -W -f='${Package}\n' | grep -vxF "$(printf '%s\n' $keep $last)")"; \
    force="--force-remove-essential --force-remove-protected --force-depends"; \
    dpkg --purge $force $remove; \
    dpkg --purge $force --force-bad-path $last; \
    rm -rf /etc/apt /var/lib/apt /var/cache/* /var/log/* \
        /usr/share/doc/* /usr/share/man/* /usr/share/info/*; \
    find / -xdev -type f -perm /6000 -exec chmod ug-s {} +; \
    for tool in sh mkdir id stat chown; do command -v "$tool" >/dev/null; done; \
    setpriv --reuid=0 --regid=0 --clear-groups -- true

# One layer, so what was removed above isn't in the image underneath.
FROM scratch
COPY --from=base / /
ENV PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
ARG TARGETARCH
COPY --from=hub /ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=hub /$TARGETARCH/freesize-hub /usr/local/bin/freesize-hub
COPY --chmod=755 packaging/docker/entrypoint.sh /usr/local/bin/entrypoint.sh
# The listen address is the Hub's default (0.0.0.0:9090), not set here,
# so a DISKTREE_HUB_LISTEN from a template made before the rename counts.
ENV FREESIZE_HUB_DATA=/data \
    PUID=99 \
    PGID=100
VOLUME /data
EXPOSE 9090
HEALTHCHECK --interval=60s --timeout=10s --start-period=20s CMD ["/usr/local/bin/freesize-hub", "health"]
LABEL org.opencontainers.image.title="FreeSize Hub" \
      org.opencontainers.image.description="One dashboard for the storage of all your devices" \
      org.opencontainers.image.source="https://github.com/McGeaverBeaver/FreeSize"
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
