# Client release procedure

Updated 2026-10-24. The public `clientproxy-io/tunnel-client` GitHub tag workflow is the sole official client build/publish path. Local builds are for development and combined server/client testing.

1. Update `Cargo.toml`, the client version in `Cargo.lock`, and `docs/releases/<version>.md`. Validate the matching server/coordinator requirements. Use a new version for changed release assets.
2. Run:

   ```sh
   cargo test --locked
   python3 packaging/synology/test-build-spk.py
   ```

   For transport changes, also run the local server/client end-to-end checks in `../rust-rpxy`.
3. Commit and push source to `main` in `clientproxy-io/tunnel-client`.
4. Push the matching source tag once:

   ```sh
   git tag -a v<version> -m 'Release <version>'
   git push origin v<version>
   ```

5. Monitor `.github/workflows/release.yml`. It verifies tag/version/notes, tests, builds Linux amd64/arm64/armv7 and Windows, packages amd64/arm64 Debian/RPM, Windows installer ZIP and the Synology DSM 7 package/feed, and publishes `ghcr.io/clientproxy-io/tunnel-client:<version>` plus `:latest`. The GitHub Release follows successful packages and container publication.
6. Verify release checksums, the Docker manifest architectures, and the Synology manifest/package size and MD5. Synology build numbers must strictly increase; the builder derives them as `major * 1000000 + minor * 1000 + patch`. Never overwrite an already published version with different artifacts.

The coordinator Download page follows the latest client GitHub release. Its `/api/synology` feed reads the latest `synology-feed.json`, with up to 10 minutes of caching. Neither requires a Firebase deploy for a client release.

There is no local official client publishing script, Google client publisher or Cloudsmith path. Existing Google client packages are historical mirrors; they are not the current client release channel. No GCP or Cloudsmith secret is required by this workflow. Manual workflow dispatch is for a matching source tag and uses the same pipeline; do not publish from a mismatched branch.

The private server (`../rust-rpxy`) uses a different sequence: local Apple Silicon builds via `scripts/build-and-publish.sh`, Google Artifact Registry `tunnel-proxy-apt` / `tunnel-proxy-yum`, then deployment of the same tested local binary. Server GitHub Actions run checks only.
