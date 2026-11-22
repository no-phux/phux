# A phux server in a container: the released binary, verified by the installer.
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash phux
USER phux
WORKDIR /home/phux
# Made here so the volumes below belong to this user, not root.
RUN curl -fsSL https://phux.sh/install | sh \
    && mkdir -p .local/state/phux .config/phux
ENV PATH=/home/phux/.local/bin:$PATH LANG=C.UTF-8
# Sessions, credentials and config live here; mount volumes to keep them.
VOLUME ["/home/phux/.local/state/phux", "/home/phux/.config/phux"]
ENTRYPOINT ["phux"]
CMD ["server"]
