# tunnel-client container image — builds from source.
#
#   docker build -t tunnel-client .
#   docker run -d --network host \
#     -e TUNNEL_API_URL=https://api-us.clientproxy.io/api \
#     -e TUNNEL_ID=<YOUR_TUNNEL_ID> \
#     -e TUNNEL_API_KEY=<YOUR_API_KEY> \
#     tunnel-client
#
# CI publishes multi-arch images from the prebuilt release binaries instead
# (docker/Dockerfile.release), which avoids compiling under QEMU.

FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin tunnel-client \
 && install -m 0755 target/release/tunnel-client /tunnel-client

# Static musl binary with bundled root CAs — nothing else is needed at runtime.
FROM scratch
ENV CLIENTPROXY_INSTALL_METHOD=docker
COPY --from=build /tunnel-client /tunnel-client
USER 65534:65534
ENTRYPOINT ["/tunnel-client"]
