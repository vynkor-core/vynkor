# Kernel image: builds the `vyn` binary and runs it in the foreground.
# Foreground mode is required in containers — `vyn start` (no --foreground)
# daemonizes by forking a detached child, which would exit PID 1 immediately.

FROM rust:1-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY . .
RUN cargo build --release --bin vyn

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    # Fixed uid 10001 — must match sdk/python/Dockerfile's uid: the kernel
    # binds its UDS socket 0600 (owner-only), so the plugin container can
    # only connect if it runs as the exact same uid, not just a shared group.
    && useradd --system --create-home --uid 10001 --shell /usr/sbin/nologin veyron \
    && mkdir -p /var/lib/veyron /run/veyron && chown veyron:veyron /var/lib/veyron /run/veyron
COPY --from=builder /build/target/release/vyn /usr/local/bin/vyn
USER veyron
EXPOSE 8080
ENTRYPOINT ["vyn"]
CMD ["start", "--foreground", "--config", "/etc/veyron/config.yaml"]
