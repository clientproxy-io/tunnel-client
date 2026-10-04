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

This installs the binary to `C:\Program Files\clientproxy\tunnel-client\` and creates a config file at `C:\ProgramData\clientproxy\tunnel-client\env.conf`.

**Step 3 — Configure**

Edit `C:\ProgramData\clientproxy\tunnel-client\env.conf`:

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

One package for every model: Intel/AMD, ARMv8 and ARMv7 CPUs. The package runs tunnel-client on the NAS itself, so every domain mapping points at a port on the NAS.

#### 1. Create the client tunnel

Do this in the [clientproxy.io dashboard](https://www.clientproxy.io):

1. **Client Tunnels → Create Tunnel**. Enter a description (e.g. `Synology`) and choose a proxy server.
2. On the new tunnel, click **Assign Product** and pick a plan. During the beta, the free and demo plans are available.
3. Copy the **Tunnel ID** from the tunnel's detail view, and the **API key** from **Dashboard → API Key**.

#### 2. Install the package

**From Package Center (recommended — you get updates automatically):**

1. **Package Center → Settings → General → Trust Level** → *Any publisher*.
2. **Package Sources → Add**: name `clientproxy.io`, location `https://api-us.clientproxy.io/api/synology`.
3. Open the **Community** tab, choose **clientproxy.io Tunnel**, and click **Install**.
4. In the wizard, pick your region and paste the **Tunnel ID** and **API key** from step 1.

**Manual install:** download `tunnel-client-<version>-synology-dsm7.spk` from [Releases](https://github.com/clientproxy-io/tunnel-client/releases), then use **Package Center → Manual Install**.

Either way, the package starts right after install and on every boot.

#### 3. Map domains to NAS services

In the dashboard, open **Client Tunnels**, click **Domains** on the tunnel's row, then **Add Domain**. In the *Add Domain Mapping* dialog:

- **Domain**: tick *Auto-generate default domain* for a free `<id>.<proxy>.clientproxy.io` address, or enter your own domain.
- **Local IP:port**: the service's **plain HTTP** port on the NAS.

| What you want to publish | Local IP:port | Notes |
|---|---|---|
| DSM web interface | `localhost:5000` | DSM's HTTP port. **Not 5001**, which is HTTPS only. |
| Website from Web Station | `localhost:80` | Or the HTTP port of your Web Station portal. |
| A DSM app on its own port (e.g. Synology Photos) | `localhost:<HTTP port>` | Control Panel → Login Portal → Applications → edit the app → set an **HTTP** port. |
| Container (Container Manager) | `localhost:<host port>` | e.g. `localhost:8096` (Jellyfin), `localhost:8123` (Home Assistant). |

Always map an HTTP port. tunnel-client talks plain HTTP to the NAS, and the proxy's Let's Encrypt certificate already gives visitors HTTPS. The NAS LAN IP (e.g. `192.168.0.79:5000`) works too, but `localhost` keeps working if the NAS gets a new IP.

Mapping changes reach the running package within 5 minutes. To apply them immediately, Stop and Run the package in Package Center.

#### 4. Check DSM settings

- **Control Panel → Login Portal → DSM → "Automatically redirect HTTP connection to HTTPS"** must be **off**. Otherwise visitors are redirected to port 5001 and get a *too many redirects* error.
- Turn on **2-factor authentication** (Personal → Security) for accounts that log in through the tunnel. The DSM login page is now reachable from the internet.
- **Auto Block** (Control Panel → Security → Protection) sees all tunnel traffic coming from the NAS itself. If it blocks that address after failed logins, the whole tunnel is locked out, so rely on 2-factor authentication instead.

#### Troubleshooting

| Symptom | Fix |
|---|---|
| `400 The plain HTTP request was sent to HTTPS port` | You mapped an HTTPS port (usually 5001). Change it to the HTTP port (5000). |
| *Too many redirects* | Turn off DSM's HTTP → HTTPS redirect (see step 4). |
| Page does not load | Check the package is running and open **View log**. After connecting, each mapping appears as `domain_id=… → localhost:5000`. |
| Old mapping still used | Wait up to 5 minutes, or Stop and Run the package. |

**Change settings:** installing a newer `.spk` over the old one opens an upgrade wizard where you can switch region, Tunnel ID or API key. Fields left blank keep their current values. Between releases, edit `/var/packages/tunnel-client/var/env` over SSH and restart the package.

**Logs:** Package Center → clientproxy.io Tunnel → **View log**, or `/var/packages/tunnel-client/var/tunnel-client.log`.

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

Logs are written to `C:\Program Files\clientproxy\tunnel-client\tunnel-client.log` with automatic rotation at 10 MB (3 files kept).

```powershell
Get-Content "C:\Program Files\clientproxy\tunnel-client\tunnel-client.log" -Wait -Tail 50
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

### Synology package releases

Synology package versions use `<feature version>-<build number>`, for example
`1.1.1-1001001`. **Increase the build number on every release**, even when the
feature version changes. This is required by
[Synology's package versioning guidance](https://help.synology.com/developer-guide/synology_package/INFO_necessary_fields.html).

The package builder derives the build number as
`major * 1000000 + minor * 1000 + patch`, so version increases also increase the
build number. Both the older `1.0.6` and `1.1.0` packages used build `0001`;
reusing it was a possible cause of missing updates in Package Center.

```bash
# bin/ contains tunnel-client-linux-amd64, -arm64 and -armv7.
packaging/synology/build-spk.sh 1.1.1 bin dist
```

For a rebuild of the same feature version, set `SPK_BUILD` explicitly to a
number higher than the previous package's build. Keep it below the next
version's derived build, or override that next build too so it stays higher.
Publish the generated `.spk` and `synology-feed.json` together so the package
version, size and checksum in the Package Center feed match the downloadable
package.

## Protocol

Uses the clientproxy.io tunnel protocol v2 — a binary multiplexed framing protocol over TLS (or plain TCP with `--no-tls`). Frame header: 10 bytes (`type | stream_id | flags | length`, big-endian).

## License

MIT
