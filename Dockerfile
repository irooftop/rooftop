# syntax=docker/dockerfile:1.7
# Minimal Alpine image for iRoof.top (static musl binary on alpine).
ARG ALPINE_VERSION=3.24

FROM rust:1-alpine${ALPINE_VERSION} AS build
RUN apk add --no-cache build-base
WORKDIR /src
COPY Cargo.toml Cargo.lock rustfmt.toml ./
COPY src src
COPY web web
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
 && install -Dm755 target/release/rooftop /out/rooftop

FROM alpine:${ALPINE_VERSION}
# ca-certificates: TLS to the LLM / MCP endpoints, tzdata: honour TZ for chat timestamps.
RUN apk add --no-cache ca-certificates tzdata \
 && adduser -D -H -u 10001 rooftop \
 && mkdir -p /data && chown rooftop:rooftop /data
COPY --from=build /out/rooftop /usr/local/bin/rooftop
USER rooftop
ENV ROOFTOP_ADDR=0.0.0.0:8080 \
    ROOFTOP_DATA=/data \
    RUST_LOG=info
VOLUME ["/data"]
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s CMD wget -qO- http://127.0.0.1:8080/healthz >/dev/null || exit 1
ENTRYPOINT ["rooftop"]
CMD ["serve"]
