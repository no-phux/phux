# syntax=docker/dockerfile:1.7@sha256:a57df69d0ea827fb7266491f2813635de6f17269be881f696fbfdf2d83dda33e
FROM --platform=linux/amd64 rust:1.98.1-bookworm@sha256:9a73a5088750b4c95158ab26629c854c3d6fc4b173cb7bc8079ad252d8ed7bfa AS phux-build

# PHUX_VERSION must match workspace.package.version. The image RUN below
# asserts `phux --version`, and `just toolchain-check` rejects a stale pin.
# release-please extra-files rewrites the annotated version literals.
# x-release-please-start-version
ARG PHUX_VERSION=0.39.0
# x-release-please-end
ARG PHUX_REVISION=43cc30950230dc40d043df9f48a142a466a56074
ARG PHUX_SOURCE_SHA256=d178127b31d07ecdbc90cd60d379c264dd42c45afee008545945399e1d07c0d1
ARG PHUX_LOCK_SHA256=7d4c51dedd53fbc52e256c52482534aa6d156685ab155b03959587fa2db8675b
ARG PHUX_PATCHED_LOCK_SHA256=5a195806b8fd266b1c677ab70e696373ee21c5873459535c16f216b38f3050e2
ARG ZIG_VERSION=0.16.0
ARG ZIG_SOURCE_SHA256=70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00
ARG LIBGHOSTTY_REVISION=467acba3a2f6b726b6922ea7c7df68d562c98657
ARG LIBGHOSTTY_SOURCE_SHA256=fdf27a5362df99e7f5eb1d344229cd0af110a91b6c9ac711609134d8a8f7f5b9
ARG GHOSTTY_REVISION=03c6f818eaeadf76e66dda99ab5529c9b47fa15c
ARG GHOSTTY_SOURCE_SHA256=3d5ca7cc063c993d16ddb00c5635e99e48767861ff9a1ba33f0b5ff2764ad7a5

ADD --checksum=sha256:d178127b31d07ecdbc90cd60d379c264dd42c45afee008545945399e1d07c0d1 \
    https://github.com/no-phux/phux/archive/43cc30950230dc40d043df9f48a142a466a56074.tar.gz /tmp/phux.tar.gz
ADD --checksum=sha256:70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00 \
    https://ziglang.org/download/0.16.0/zig-x86_64-linux-0.16.0.tar.xz /tmp/zig.tar.xz
ADD --checksum=sha256:fdf27a5362df99e7f5eb1d344229cd0af110a91b6c9ac711609134d8a8f7f5b9 \
    https://github.com/phall1/libghostty-rs/archive/467acba3a2f6b726b6922ea7c7df68d562c98657.tar.gz /tmp/libghostty.tar.gz
ADD --checksum=sha256:3d5ca7cc063c993d16ddb00c5635e99e48767861ff9a1ba33f0b5ff2764ad7a5 \
    https://github.com/phall1/ghostty/archive/03c6f818eaeadf76e66dda99ab5529c9b47fa15c.tar.gz /tmp/ghostty.tar.gz
# x-release-please-start-version
RUN test "$PHUX_VERSION" = "0.39.0"
# x-release-please-end
RUN test "$PHUX_REVISION" = "43cc30950230dc40d043df9f48a142a466a56074" \
    && test "$PHUX_SOURCE_SHA256" = "d178127b31d07ecdbc90cd60d379c264dd42c45afee008545945399e1d07c0d1" \
    && test "$ZIG_VERSION" = "0.16.0" \
    && test "$ZIG_SOURCE_SHA256" = "70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00" \
    && test "$LIBGHOSTTY_REVISION" = "467acba3a2f6b726b6922ea7c7df68d562c98657" \
    && test "$LIBGHOSTTY_SOURCE_SHA256" = "fdf27a5362df99e7f5eb1d344229cd0af110a91b6c9ac711609134d8a8f7f5b9" \
    && test "$GHOSTTY_REVISION" = "03c6f818eaeadf76e66dda99ab5529c9b47fa15c" \
    && test "$GHOSTTY_SOURCE_SHA256" = "3d5ca7cc063c993d16ddb00c5635e99e48767861ff9a1ba33f0b5ff2764ad7a5" \
    && mkdir /src /opt/zig /libghostty /ghostty \
    && tar --extract --gzip --file /tmp/phux.tar.gz --directory /src --strip-components=1 \
    && tar --extract --xz --file /tmp/zig.tar.xz --directory /opt/zig --strip-components=1 \
    && tar --extract --gzip --file /tmp/libghostty.tar.gz --directory /libghostty --strip-components=1 \
    && tar --extract --gzip --file /tmp/ghostty.tar.gz --directory /ghostty --strip-components=1 \
    && echo "$PHUX_LOCK_SHA256  /src/Cargo.lock" | sha256sum --check --strict
WORKDIR /src
ENV PATH=/opt/zig:$PATH
ENV GHOSTTY_SOURCE_DIR=/ghostty
ENV LIBGHOSTTY_VT_SYS_CPU=baseline
RUN cargo \
      --config 'patch."https://github.com/phall1/libghostty-rs.git".libghostty-vt.path="/libghostty/crates/libghostty-vt"' \
      metadata --format-version 1 >/dev/null \
    && echo "$PHUX_PATCHED_LOCK_SHA256  Cargo.lock" | sha256sum --check --strict
RUN RUSTFLAGS="-C target-cpu=x86-64" cargo \
      --config 'patch."https://github.com/phall1/libghostty-rs.git".libghostty-vt.path="/libghostty/crates/libghostty-vt"' \
      build --locked --release --bin phux

FROM --platform=linux/amd64 oven/bun:1.4.0@sha256:5ff609364c049b54eb0ff560ec96319729a972078ef2c755d758f0c6ef89c2d6 AS phui-build

ARG PHUI_REVISION=81ddf0a00b73e522394181278ec78cce133b57d0
ARG PHUI_SOURCE_SHA256=09600d6fa6a8069df3564cd94a4a5d6b75471df2017721f7cd221c83e8fe4682
ARG PHUI_LOCK_SHA256=182e16724ec52507cff5e4233eac8065141e5593386f95c60f7d50e69af768ed

ADD --checksum=sha256:09600d6fa6a8069df3564cd94a4a5d6b75471df2017721f7cd221c83e8fe4682 \
    https://github.com/phall1/phui/archive/81ddf0a00b73e522394181278ec78cce133b57d0.tar.gz /tmp/phui.tar.gz
RUN test "$PHUI_REVISION" = "81ddf0a00b73e522394181278ec78cce133b57d0" \
    && test "$PHUI_SOURCE_SHA256" = "09600d6fa6a8069df3564cd94a4a5d6b75471df2017721f7cd221c83e8fe4682" \
    && mkdir /src \
    && tar --extract --gzip --file /tmp/phui.tar.gz --directory /src --strip-components=1 \
    && echo "$PHUI_LOCK_SHA256  /src/bun.lock" | sha256sum --check --strict
WORKDIR /src
RUN bun install --frozen-lockfile \
    && bun build --compile --bytecode --format=esm --target=bun-linux-x64-baseline \
      --outfile=/tmp/phui src/standalone.ts

FROM --platform=linux/amd64 debian:bookworm-slim@sha256:1def178129dfb5f24db43afbf2fcac04530012e3264ba4ff81c71184e17a9ee4

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
      bash=5.2.15-2+b13 \
      build-essential=12.9 \
      ca-certificates=20250419~deb12u1 \
      coreutils=9.1-1 \
      curl=7.88.1-10+deb12u15 \
      fd-find=8.6.0-3 \
      file=1:5.44-3 \
      git=1:2.39.5-0+deb12u3 \
      jq=1.6-2.1+deb12u2 \
      less=590-2.1~deb12u2 \
      ncurses-base=6.4-4 \
      nodejs=18.20.4+dfsg-1~deb12u2 \
      patch=2.7.6-7 \
      procps=2:4.0.2-3 \
      python3=3.11.2-1+b1 \
      python3-pip=23.0.1+dfsg-1 \
      ripgrep=13.0.0-4+b2 \
      socat=1.7.4.4-2 \
      tar=1.34+dfsg-1.2+deb12u1 \
      unzip=6.0-28 \
      xz-utils=5.4.1-1+deb12u1 \
    && rm -rf /var/lib/apt/lists/* /var/cache/apt/* /usr/lib/apt /var/lib/dpkg \
    && rm -f /usr/bin/apt* /usr/bin/dpkg* /usr/sbin/dpkg* /usr/bin/update-alternatives /usr/bin/deb-systemd-* \
    && rm -f /bin/dash \
    && ln -sf bash /bin/sh \
    && ln -sf /usr/bin/fdfind /usr/local/bin/fd \
    && groupadd --gid 10001 guest \
    && useradd --uid 10001 --gid 10001 --home-dir /home/guest --shell /bin/bash guest \
    && mkdir -p /home/guest /opt/internal /usr/local/libexec \
    && chown 10001:10001 /home/guest

COPY --from=phux-build --chmod=0755 /src/target/release/phux /usr/local/bin/phux
COPY --from=phui-build --chmod=0755 /tmp/phui /usr/local/libexec/phui.real
COPY --chmod=0755 container/entrypoint.sh /usr/local/libexec/entrypoint
COPY --chmod=0755 container/health.sh /usr/local/libexec/health
COPY --chmod=0755 container/phux-shell /usr/local/bin/phux-shell
COPY --chmod=0755 container/phui /usr/local/bin/phui
COPY --chmod=0644 container/bashrc /etc/phux/bashrc
COPY --chmod=0644 container/profile /etc/profile

# x-release-please-start-version
ARG PHUX_VERSION=0.39.0
# x-release-please-end
RUN test "$(/usr/local/bin/phux --version)" = "phux $PHUX_VERSION" \
    && cp /usr/bin/socat /opt/internal/socat \
    && chown 10001:10001 /opt/internal/socat \
    && chmod 0500 /opt/internal/socat \
    && chmod 0755 /etc/phux \
    && rm -f /usr/bin/socat /usr/bin/nc /usr/bin/netcat /usr/bin/wget /usr/bin/setpriv \
    && rm -rf /usr/share/doc /usr/share/man /usr/share/info

EXPOSE 8080 8082
USER 10001:10001
ENTRYPOINT ["/usr/local/libexec/entrypoint"]
