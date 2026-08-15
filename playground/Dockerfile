# The playground sandbox image.
#
# Built in two stages so the finished image carries the compiler, a POSIX shell,
# and the entry point - and no network tools. The compiler tarball is pinned by
# version and verified against a SHA-256 passed in as a build argument, so a
# tampered or swapped release fails the build rather than reaching a container.
#
# Build it with playground/build-image.sh, which refuses to guess either
# argument.

ARG DEBIAN_TAG=13-slim

FROM debian:${DEBIAN_TAG} AS fetch

ARG RUX_VERSION
ARG RUX_SHA256
# Space-separated `Package` names to seed into the image's package cache. The
# sandbox runs with --network=none, so anything absent here can never resolve at
# request time, and adding one means rebuilding the image.
ARG RUX_PACKAGES=""

SHELL ["/bin/sh", "-eu", "-c"]

RUN test -n "${RUX_VERSION}" || { echo 'RUX_VERSION is required' >&2; exit 1; }
RUN test -n "${RUX_SHA256}" || { echo 'RUX_SHA256 is required' >&2; exit 1; }

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL -o /tmp/rux.tar.gz \
      "https://github.com/rux-lang/Rux/releases/download/v${RUX_VERSION}/rux-linux.tar.gz" \
 && printf '%s  %s\n' "${RUX_SHA256}" /tmp/rux.tar.gz | sha256sum -c - \
 && mkdir -p /out/bin \
 && tar xzf /tmp/rux.tar.gz -C /out/bin \
 && chmod 0755 /out/bin/rux \
 && /out/bin/rux --version

# Seed the package cache while the network is still reachable. The cache
# directory is always created so the copy below has something to take, even when
# no packages are requested.
RUN mkdir -p /root/.rux /out/cache \
 && if [ -n "${RUX_PACKAGES}" ]; then \
        mkdir -p /tmp/seed/Src && cd /tmp/seed \
     && printf '[Package]\nName = "Seed"\nVersion = "0.0.0"\nType = "Program"\n\n[Dependencies]\n' > Rux.toml \
     && for package in ${RUX_PACKAGES}; do printf '%s = "*"\n' "${package}" >> Rux.toml; done \
     && printf 'func Main() -> int {\n    return 0;\n}\n' > Src/Main.rux \
     && PATH="/out/bin:${PATH}" rux install \
     && cp -a /root/.rux/. /out/cache/; \
    fi

FROM debian:${DEBIAN_TAG}

ARG RUX_VERSION
LABEL org.opencontainers.image.title="rux-playground" \
      org.opencontainers.image.description="Throwaway sandbox for one Rux playground run" \
      org.opencontainers.image.version="${RUX_VERSION}"

# A fixed uid so the daemon can mount its 0700 job directory readable by this
# user; the host's rux-playground service account must be created with the same
# id. See docs/deployment.md.
RUN groupadd --gid 1000 play \
 && useradd --uid 1000 --gid 1000 --home-dir /home/play --create-home --shell /usr/sbin/nologin play

COPY --from=fetch /out/bin/rux /usr/local/bin/rux
COPY --from=fetch --chown=1000:1000 /out/cache/ /home/play/.rux/
COPY run-job /usr/local/bin/run-job

# The daemon passes --user with the uid that owns the job directory, which need
# not be this image's `play`. A process with no matching passwd entry gets no
# home directory, and the compiler looks for its package cache under $HOME, so
# the value is fixed here and the cache is left world-readable.
ENV HOME=/home/play

# The entry point needs a shell and coreutils; nothing needs a package manager.
# It cannot reach a mirror through --network=none anyway, but a sandbox that
# executes submitted code should not carry one at all.
RUN chmod 0755 /usr/local/bin/run-job /usr/local/bin/rux \
 && chmod -R a+rX /home/play \
 && rm -rf /etc/apt /usr/lib/apt /var/lib/apt /var/cache/apt \
           /usr/bin/apt /usr/bin/apt-get /usr/bin/apt-cache /usr/bin/apt-config \
           /usr/bin/apt-key /usr/bin/apt-mark \
 && rux --version

USER play
WORKDIR /work

ENTRYPOINT ["/usr/local/bin/run-job"]
