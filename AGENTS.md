# tunnel-client developer context

This is the standalone open-source ClientProxy client, public repository `clientproxy-io/tunnel-client`. Source: `src/main.rs`; protocol version 2; use ring for TLS.

## Official release path

The sole client release publisher is `.github/workflows/release.yml` on a `v*` source tag. It tests the client, builds Linux amd64/arm64/armv7 and Windows, packages Debian/RPM and Synology DSM 7, publishes multi-architecture GHCR images and a GitHub Release with checksums and `synology-feed.json`.

Follow [docs/releasing.md](docs/releasing.md). Update `Cargo.toml`, the client entry in `Cargo.lock`, and `docs/releases/<version>.md` before tagging. Synology build numbers must increase; the package builder derives them from the version. The public release waits for platform packages and the container image to succeed.

Local cargo builds are for development/testing. Do not add a local package/release upload script or Google client publisher. There is no client Google credential requirement and no Cloudsmith publisher.

## Separate server

The private server is `../rust-rpxy`. It builds **locally on Apple Silicon**, using native musl compilers and `scripts/build-and-publish.sh`, and uploads amd64/arm64 Debian/RPM packages to Google `tunnel-proxy-apt` / `tunnel-proxy-yum` in `proxyconfig-71f70`, `us-central1`. Its GitHub workflow runs checks only. Never move that server publishing path into this client workflow.

## Checks

```sh
cargo test --locked
python3 packaging/synology/test-build-spk.py
```

The server repository's `scripts/e2e_tls_modes.py` tests both clients and server together against local HTTP/TLS backends. Passthrough requires server 0.11.0/client 1.2.0, an upgraded coordinator, a TLS-capable product without HTTP request quotas, HTTP/3 disabled, and an existing local TLS listener/certificate. Site certificate generation/renewal is not part of this client.
