# tunnel-client

The open-source client binary for [clientproxy.io](https://clientproxy.io) — expose any local HTTP server to the internet through a secure reverse-proxy tunnel, with no port forwarding required.

Works on Linux, macOS, Windows, NAS devices (Synology package, Docker for QNAP / TrueNAS / Unraid / OpenMediaVault), and (with `--no-tls`) ESP32 / constrained embedded devices.

## Quick start

```bash
tunnel-client \
  --api-url  https://api-us.clientproxy.io/api \
  --tunnel-id <YOUR_TUNNEL_ID> \
  --api-key   <YOUR_API_KEY>
```

Get your tunnel ID and API key from [clientproxy.io](https://clientproxy.io) after signing up.

## Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--api-url` | — | API endpoint (`api-us`, `api-eu`, or `api-asia`) |
| `--tunnel-id` | — | Tunnel ID from the dashboard |
| `--api-key` | — | `<subscriptionId>_<salt>` API key |
| `--no-tls` | off | Plain TCP — for ESP32 or local testing |
| `--tls-server-name` | derived | Override TLS SNI hostname |
| `--tls-ca-cert-path` | — | Custom CA cert for the tunnel TLS connection |
| `--reconnect-interval` | 1 | Seconds between reconnect attempts |
| `--verbose` / `-v` | off | Debug logging |

Every flag can also be set through an environment variable: `TUNNEL_API_URL`, `TUNNEL_ID`, `TUNNEL_API_KEY`, `TUNNEL_RECONNECT_INTERVAL`, `TUNNEL_NO_TLS`, `TUNNEL_TLS_SERVER_NAME`, `TUNNEL_TLS_CA_CERT_PATH`, `TUNNEL_VERBOSE`. Command-line flags take precedence.

## Regions

| Region | API URL |
|--------|---------|
| United States | `https://api-us.clientproxy.io/api` |
| Europe | `https://api-eu.clientproxy.io/api` |
| Asia | `https://api-asia.clientproxy.io/api` |

## Installation

### Linux — Debian / Ubuntu

**Install:**

```bash
VER=$(curl -s https://api.github.com/repos/clientproxy-io/tunnel-client/releases/latest \
  | grep '"tag_name"' | cut -d'"' -f4 | sed 's/v//')
curl -LO "https://github.com/clientproxy-io/tunnel-client/releases/download/v${VER}/tunnel-client_${VER}_amd64.deb"
sudo dpkg -i "tunnel-client_${VER}_amd64.deb"
```

For arm64 replace `amd64` with `arm64` in the filename.

**Configure** `/etc/tunnel-client/env`:

```ini
TUNNEL_API_URL=https://api-us.clientproxy.io/api
TUNNEL_ID=<YOUR_TUNNEL_ID>
TUNNEL_API_KEY=<YOUR_API_KEY>
```

**Start:**

```bash
sudo systemctl enable --now tunnel-client
sudo systemctl status tunnel-client
```

To pass extra flags (e.g. `--no-tls`), override the service unit:

```bash
sudo systemctl edit tunnel-client
```

---

### Linux — RHEL / Rocky / Amazon Linux

**Install:**

```bash
VER=$(curl -s https://api.github.com/repos/clientproxy-io/tunnel-client/releases/latest \
  | grep '"tag_name"' | cut -d'"' -f4 | sed 's/v//')
curl -LO "https://github.com/clientproxy-io/tunnel-client/releases/download/v${VER}/tunnel-client-${VER}-1.amd64.rpm"
sudo rpm -i "tunnel-client-${VER}-1.amd64.rpm"
```

For arm64 replace `amd64` with `arm64` in the filename.

**Configure** `/etc/tunnel-client/env` (same format as above), then:

```bash
sudo systemctl enable --now tunnel-client
```

---

### Windows

**Step 1 — Download**

Download `tunnel-client-windows-<version>.zip` from [Releases](https://github.com/clientproxy-io/tunnel-client/releases) and extract it to a temporary folder.

**Step 2 — Install**

Open **PowerShell as Administrator** and run:

```powershell
powershell -ExecutionPolicy Bypass -File install.ps1
```

This installs the binary to `C:\Program Files\OliBot\tunnel-client\` and creates a config file at `C:\ProgramData\OliBot\tunnel-client\env.conf`.

**Step 3 — Configure**

Edit `C:\ProgramData\OliBot\tunnel-client\env.conf`:

```ini
# API endpoint — choose your region:
TUNNEL_API_URL=https://api-us.clientproxy.io/api
# TUNNEL_API_URL=https://api-eu.clientproxy.io/api
# TUNNEL_API_URL=https://api-asia.clientproxy.io/api

TUNNEL_ID=<YOUR_TUNNEL_ID>
TUNNEL_API_KEY=<YOUR_API_KEY>
```

**Step 4 — Start**

Re-run `install.ps1` to apply credentials and start the service, or:

```powershell
Start-Service tunnel-client
Get-Service   tunnel-client   # should show Running
```

The service starts automatically on boot.

**Update credentials:** edit `env.conf`, re-run `install.ps1` as Administrator.

**Uninstall:**

```powershell
powershell -ExecutionPolicy Bypass -File uninstall.ps1
```

---

### Synology DiskStation (DSM 7)

One package for every model: Intel/AMD, ARMv8 and ARMv7 CPUs.

**From Package Center (recommended — you get updates automatically):**

1. **Package Center → Settings → General → Trust Level** → *Any publisher*.
2. **Package Sources → Add**: name `clientproxy.io`, location `https://api-us.clientproxy.io/api/synology`.
3. Open the **Community** tab, choose **clientproxy.io Tunnel**, and click **Install**.
4. In the wizard, pick your region and paste your **Tunnel ID** and **API key**.

**Manual install:** download `tunnel-client-<version>-synology-dsm7.spk` from [Releases](https://github.com/clientproxy-io/tunnel-client/releases), then use **Package Center → Manual Install**.

Either way, the package starts right after install and on every boot.

In the dashboard, point your domains at services on the NAS, for example `localhost:5000` (DSM) or `localhost:8096` (Jellyfin).

**Change settings:** installing a newer `.spk` over the old one opens an upgrade wizard where you can switch region, Tunnel ID or API key. Fields left blank keep their current values. Between releases, edit `/var/packages/tunnel-client/var/env` over SSH and restart the package.

**Logs:** Package Center → tunnel-client → **View log**, or `/var/packages/tunnel-client/var/tunnel-client.log`.

---

### Docker (QNAP, TrueNAS SCALE, Unraid, OpenMediaVault, CasaOS, Synology Container Manager)

Multi-arch image (`amd64`, `arm64`, `arm/v7`): `ghcr.io/clientproxy-io/tunnel-client`

```bash
docker run -d --name tunnel-client --restart unless-stopped --network host \
  -e TUNNEL_API_URL=https://api-us.clientproxy.io/api \
  -e TUNNEL_ID=<YOUR_TUNNEL_ID> \
  -e TUNNEL_API_KEY=<YOUR_API_KEY> \
  ghcr.io/clientproxy-io/tunnel-client:latest
```

Or use [docker/docker-compose.yml](docker/docker-compose.yml) with any compose UI (Container Manager *Project*, Container Station *Application*, OMV compose plugin, Portainer stack).

`--network host` lets dashboard backends like `localhost:8080` reach services on the host. With bridge networking, use the host's LAN IP or another container's name in the dashboard instead.

**Unraid:** Docker → Add Container. Set Repository to `ghcr.io/clientproxy-io/tunnel-client:latest` and Network Type to **Host**, then add the variables `TUNNEL_API_URL`, `TUNNEL_ID` and `TUNNEL_API_KEY`. A Community Applications template is in [packaging/unraid/tunnel-client.xml](packaging/unraid/tunnel-client.xml).

**Build the image yourself:** `docker build -t tunnel-client .`

---

## Viewing logs

### Linux

```bash
journalctl -u tunnel-client -f        # follow live
journalctl -u tunnel-client -n 100    # last 100 lines
journalctl -u tunnel-client -b        # since last boot
```

### Windows

Logs are written to `C:\Program Files\OliBot\tunnel-client\tunnel-client.log` with automatic rotation at 10 MB (3 files kept).

```powershell
Get-Content "C:\Program Files\OliBot\tunnel-client\tunnel-client.log" -Wait -Tail 50
```

Service events also appear in **Windows Event Viewer** → `Windows Logs → Application` (source: `tunnel-client`).

### Synology / Docker

```bash
tail -f /var/packages/tunnel-client/var/tunnel-client.log   # Synology package
docker logs -f tunnel-client                                 # Docker
```

---

## Build from source

```bash
cargo build --bin tunnel-client --release
```

**Cross-compile for Linux musl (Apple Silicon):**

```bash
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc \
CC_x86_64_unknown_linux_musl=x86_64-linux-musl-gcc \
cargo build --bin tunnel-client --target x86_64-unknown-linux-musl --release
```

**Cross-compile for Windows:**

```bash
brew install mingw-w64
rustup target add x86_64-pc-windows-gnu
./scripts/build-windows.sh
```

## Protocol

Uses the clientproxy.io tunnel protocol v2 — a binary multiplexed framing protocol over TLS (or plain TCP with `--no-tls`). Frame header: 10 bytes (`type | stream_id | flags | length`, big-endian).

## License

MIT
