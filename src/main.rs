/// Tunnel Client v2
///
/// Connects to a tunnel server using protocol v2 (multiplexed binary framing).
/// Each incoming REQUEST frame is handled concurrently in a separate tokio task.
/// Supports HTTP/1.1 and HTTP/2 browser requests transparently.

use bytes::{Bytes, BytesMut, BufMut};
use clap::{Arg, Command};
use http_body_util::BodyExt;
use rustls::{ClientConfig, RootCertStore, crypto::CryptoProvider, pki_types::ServerName};
use rustls::pki_types::pem::PemObject;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc};
use tokio_rustls::{TlsConnector, client::TlsStream};
use tracing::{debug, info, warn};

const DEFAULT_RECONNECT_INTERVAL_SECS: u64 = 1;
const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

// ─── Protocol v2 (inlined from rpxy-lib/src/tunnel/protocol.rs) ──────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum FrameType {
  Connect  = 0x01,
  Config   = 0x02,
  Request  = 0x03,
  Response = 0x04,
  Data     = 0x05,
  Reset    = 0x06,
  Ping     = 0x07,
  Pong     = 0x08,
  GoAway   = 0x09,
}

impl FrameType {
  fn from_u8(b: u8) -> Option<Self> {
    match b {
      0x01 => Some(FrameType::Connect),
      0x02 => Some(FrameType::Config),
      0x03 => Some(FrameType::Request),
      0x04 => Some(FrameType::Response),
      0x05 => Some(FrameType::Data),
      0x06 => Some(FrameType::Reset),
      0x07 => Some(FrameType::Ping),
      0x08 => Some(FrameType::Pong),
      0x09 => Some(FrameType::GoAway),
      _    => None,
    }
  }
}

mod flags {
  pub const HAS_BODY: u8        = 0x01;
  pub const END_STREAM: u8      = 0x01;
  pub const RESPONSE_HAS_BODY: u8 = 0x01;
  /// REQUEST: HTTP/1.1 Upgrade handshake (WebSocket) — keep Upgrade/Connection headers.
  pub const UPGRADE: u8         = 0x04;
  /// RESPONSE: backend answered 101; DATA frames now carry raw bytes both ways.
  pub const RESPONSE_UPGRADED: u8 = 0x02;
}

mod caps {
  pub const HTTP2_BACKENDS: u8 = 0x01;
  /// This client can relay WebSocket (HTTP/1.1 Upgrade) streams.
  pub const UPGRADE: u8 = 0x04;
}

type BodySenders = Arc<Mutex<HashMap<u32, mpsc::Sender<Option<Bytes>>>>>;

mod reset_codes {
  pub const BACKEND_UNREACHABLE: u16 = 0x01;
  pub const INTERNAL_ERROR: u16     = 0x03;
}

#[derive(Debug, Clone)]
struct Frame {
  frame_type: FrameType,
  stream_id:  u32,
  flags:      u8,
  payload:    Bytes,
}

impl Frame {
  fn has_flag(&self, f: u8) -> bool { self.flags & f != 0 }
}

async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Frame> {
  let mut hdr = [0u8; 10];
  r.read_exact(&mut hdr).await?;
  let ft = FrameType::from_u8(hdr[0])
    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("unknown frame type 0x{:02x}", hdr[0])))?;
  let stream_id = u32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]);
  let flags     = hdr[5];
  let length    = u32::from_be_bytes([hdr[6], hdr[7], hdr[8], hdr[9]]) as usize;
  if length > MAX_PAYLOAD {
    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "frame payload too large"));
  }
  let mut payload = vec![0u8; length];
  r.read_exact(&mut payload).await?;
  Ok(Frame { frame_type: ft, stream_id, flags, payload: Bytes::from(payload) })
}

async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &Frame) -> std::io::Result<()> {
  let len = frame.payload.len() as u32;
  let mut hdr = [0u8; 10];
  hdr[0] = frame.frame_type as u8;
  hdr[1..5].copy_from_slice(&frame.stream_id.to_be_bytes());
  hdr[5] = frame.flags;
  hdr[6..10].copy_from_slice(&len.to_be_bytes());
  w.write_all(&hdr).await?;
  w.write_all(&frame.payload).await?;
  Ok(())
}

// ─── Payload builders ─────────────────────────────────────────────────────────

fn build_connect_payload(tunnel_id: &str, api_key: &str) -> Bytes {
  let mut buf = BytesMut::new();
  buf.put_u8(0x02); // protocol version 2
  buf.put_u8(caps::HTTP2_BACKENDS | caps::UPGRADE);
  buf.put_u16(tunnel_id.len() as u16);
  buf.extend_from_slice(tunnel_id.as_bytes());
  buf.put_u16(api_key.len() as u16);
  buf.extend_from_slice(api_key.as_bytes());
  buf.freeze()
}

fn build_response_payload(status_code: u16, headers: &[(String, String)]) -> Bytes {
  let mut buf = BytesMut::new();
  buf.put_u16(status_code);
  buf.put_u16(headers.len() as u16);
  for (name, value) in headers {
    let nb = name.as_bytes();
    buf.put_u8(nb.len().min(255) as u8);
    buf.extend_from_slice(&nb[..nb.len().min(255)]);
    let vb = value.as_bytes();
    buf.put_u16(vb.len().min(65535) as u16);
    buf.extend_from_slice(&vb[..vb.len().min(65535)]);
  }
  buf.freeze()
}

// ─── Payload parsers ──────────────────────────────────────────────────────────

struct ConfigPayload {
  domains: Vec<DomainEntry>,
}
struct DomainEntry {
  domain_id: u16,
  _flags: u8,
  _domain: String,
  local_host: String,
}

struct RequestPayload {
  domain_id: u16,
  method: String,
  path: String,
  headers: Vec<(String, String)>,
  /// Set by the caller from the REQUEST frame's UPGRADE flag.
  upgrade: bool,
}

fn parse_config(payload: &[u8]) -> Option<ConfigPayload> {
  if payload.len() < 2 { return None; }
  let count = u16::from_be_bytes([payload[0], payload[1]]) as usize;
  let mut pos = 2;
  let mut domains = Vec::with_capacity(count);
  for _ in 0..count {
    if pos + 3 > payload.len() { return None; }
    let domain_id = u16::from_be_bytes([payload[pos], payload[pos+1]]); pos += 2;
    let flags = payload[pos]; pos += 1;
    let domain = read_u16_str(payload, &mut pos)?;
    let local_host = read_u16_str(payload, &mut pos)?;
    domains.push(DomainEntry { domain_id, _flags: flags, _domain: domain, local_host });
  }
  Some(ConfigPayload { domains })
}

fn parse_request(payload: &[u8]) -> Option<RequestPayload> {
  if payload.len() < 2 { return None; }
  let domain_id = u16::from_be_bytes([payload[0], payload[1]]);
  let mut pos = 2;
  let method  = read_u8_str(payload, &mut pos)?;
  let path    = read_u16_str(payload, &mut pos)?;
  let headers = read_headers(payload, &mut pos)?;
  Some(RequestPayload { domain_id, method, path, headers, upgrade: false })
}

fn read_u8_str(p: &[u8], pos: &mut usize) -> Option<String> {
  if *pos >= p.len() { return None; }
  let len = p[*pos] as usize; *pos += 1;
  if *pos + len > p.len() { return None; }
  let s = std::str::from_utf8(&p[*pos..*pos+len]).ok()?.to_string();
  *pos += len;
  Some(s)
}

fn read_u16_str(p: &[u8], pos: &mut usize) -> Option<String> {
  if *pos + 2 > p.len() { return None; }
  let len = u16::from_be_bytes([p[*pos], p[*pos+1]]) as usize; *pos += 2;
  if *pos + len > p.len() { return None; }
  let s = std::str::from_utf8(&p[*pos..*pos+len]).ok()?.to_string();
  *pos += len;
  Some(s)
}

fn read_headers(p: &[u8], pos: &mut usize) -> Option<Vec<(String, String)>> {
  if *pos + 2 > p.len() { return None; }
  let count = u16::from_be_bytes([p[*pos], p[*pos+1]]) as usize; *pos += 2;
  let mut headers = Vec::with_capacity(count);
  for _ in 0..count {
    let name  = read_u8_str(p, pos)?;
    let value = read_u16_str(p, pos)?;
    headers.push((name, value));
  }
  Some(headers)
}

// ─── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct Args {
  api_url: String,
  tunnel_id: String,
  api_key: String,
  reconnect_interval: u64,
  no_tls: bool,
  tls_server_name: Option<String>,
  tls_ca_cert_path: Option<String>,
  verbose: bool,
}

impl Args {
  fn parse() -> Result<Self, Box<dyn std::error::Error>> {
    let matches = Command::new("tunnel-client")
      // Sourced from Cargo.toml so `--version` can't drift from the crate version
      // (it previously reported a hardcoded "2.0" against a 1.0.x crate).
      .version(env!("CARGO_PKG_VERSION"))
      .about("Tunnel client (protocol v2) for rust-rpxy")
      // Every flag can also come from the environment so containers and NAS
      // packages can configure the client without a shell wrapper.
      .arg(Arg::new("api-url").short('a').long("api-url").env("TUNNEL_API_URL").required(true).help("Proxy-admin API base URL"))
      .arg(Arg::new("tunnel-id").short('t').long("tunnel-id").env("TUNNEL_ID").required(true).help("Tunnel ID"))
      .arg(Arg::new("api-key").short('k').long("api-key").env("TUNNEL_API_KEY").hide_env_values(true).required(true).help("API key (<subscriptionId>_<salt>)"))
      .arg(Arg::new("reconnect-interval").short('r').long("reconnect-interval").env("TUNNEL_RECONNECT_INTERVAL").default_value("1").help("Reconnect interval (seconds)"))
      .arg(Arg::new("no-tls").long("no-tls").env("TUNNEL_NO_TLS").action(clap::ArgAction::SetTrue)
        .value_parser(clap::builder::FalseyValueParser::new())
        .help("Use plain TCP (no TLS) — for ESP32 or local testing. Server must listen on tunnel_port (not tunnel_port_tls)"))
      .arg(Arg::new("tls-server-name").long("tls-server-name").env("TUNNEL_TLS_SERVER_NAME").required(false).help("Override TLS SNI hostname"))
      .arg(Arg::new("tls-ca-cert-path").long("tls-ca-cert-path").env("TUNNEL_TLS_CA_CERT_PATH").required(false).help("Custom CA cert for tunnel TLS"))
      .arg(Arg::new("verbose").short('v').long("verbose").env("TUNNEL_VERBOSE").action(clap::ArgAction::SetTrue)
        .value_parser(clap::builder::FalseyValueParser::new())
        .help("Debug logging"))
      .get_matches();

    let reconnect_interval = matches.get_one::<String>("reconnect-interval").unwrap()
      .parse().unwrap_or(DEFAULT_RECONNECT_INTERVAL_SECS);

    // An empty env var (e.g. `TUNNEL_TLS_SERVER_NAME=` in a compose file) means unset.
    Ok(Args {
      api_url: matches.get_one::<String>("api-url").unwrap().clone(),
      tunnel_id: matches.get_one::<String>("tunnel-id").unwrap().clone(),
      api_key: matches.get_one::<String>("api-key").unwrap().clone(),
      reconnect_interval,
      no_tls: matches.get_flag("no-tls"),
      tls_server_name: matches.get_one::<String>("tls-server-name").filter(|s| !s.is_empty()).cloned(),
      tls_ca_cert_path: matches.get_one::<String>("tls-ca-cert-path").filter(|s| !s.is_empty()).cloned(),
      verbose: matches.get_flag("verbose"),
    })
  }
}

// ─── API: validate tunnel and get proxy address ───────────────────────────────

async fn fetch_tunnel_info(api_url: &str, tunnel_id: &str, api_key: &str) -> Result<String, String> {
  use http_body_util::Full;
  let url = format!("{}/client-tunnel/{}", api_url, tunnel_id);
  let https = hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots().https_or_http().enable_http1().build();
  let client: hyper_util::client::legacy::Client<_, Full<Bytes>> =
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new()).build(https);
  let req = hyper::Request::builder().method("GET").uri(&url)
    .header("Authorization", format!("Bearer {}", api_key)).body(Full::new(Bytes::new()))
    .map_err(|e| format!("Request build error: {}", e))?;
  let res = tokio::time::timeout(Duration::from_secs(15), client.request(req))
    .await
    .map_err(|_| "Tunnel info request timed out after 15s".to_string())?
    .map_err(|e| format!("HTTP error: {}", e))?;
  let status = res.status();
  let body = String::from_utf8_lossy(&res.into_body().collect().await.map_err(|e| e.to_string())?.to_bytes()).to_string();
  if status.is_success() {
    extract_string_field(&body, "proxyAddress").ok_or_else(|| "No proxyAddress in response".to_string())
  } else {
    Err(format!("API error {}: {}", status, body))
  }
}

fn extract_string_field(json: &str, field: &str) -> Option<String> {
  let key = format!("\"{}\"", field);
  let start = json.find(&key)?;
  let after = json[start + key.len()..].trim_start();
  let after = after.strip_prefix(':')?.trim_start();
  if let Some(stripped) = after.strip_prefix('"') {
    Some(stripped[..stripped.find('"')?].to_string())
  } else { None }
}

// ─── TLS client config ────────────────────────────────────────────────────────

async fn build_tls_config(ca_cert_path: Option<&str>) -> Result<ClientConfig, Box<dyn std::error::Error + Send + Sync>> {
  let root_store = if let Some(path) = ca_cert_path {
    let mut reader = BufReader::new(File::open(path)?);
    let certs = rustls::pki_types::CertificateDer::pem_reader_iter(&mut reader).collect::<Result<Vec<_>, _>>()?;
    let mut roots = RootCertStore::empty();
    roots.add_parsable_certificates(certs);
    roots
  } else {
    RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned())
  };
  Ok(ClientConfig::builder().with_root_certificates(root_store).with_no_client_auth())
}

// ─── DNS fallback (system resolver → Cloudflare DoH) ─────────────────────────

fn unix_now() -> u64 {
  SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

async fn resolve_to_ip(hostname: &str) -> Option<String> {
  if let Ok(mut addrs) = tokio::net::lookup_host(format!("{}:0", hostname)).await {
    if let Some(addr) = addrs.next() { return Some(addr.ip().to_string()); }
  }
  let url = format!("https://cloudflare-dns.com/dns-query?name={}&type=A", hostname);
  let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
    .build(hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots().https_only().enable_http1().build());
  let req = hyper::Request::builder().uri(&url).header("Accept", "application/dns-json")
    .body(http_body_util::Full::new(Bytes::new())).ok()?;
  let res = client.request(req).await.ok()?;
  let body = String::from_utf8_lossy(&res.into_body().collect().await.ok()?.to_bytes()).to_string();
  let answer_start = body.find("\"Answer\"")?;
  let after = &body[answer_start..];
  let mut pos = 0;
  while let Some(s) = after[pos..].find('{') {
    let abs = pos + s;
    let end = after[abs..].find('}').map(|e| abs + e + 1)?;
    let entry = &after[abs..end];
    if entry.contains("\"type\":1") || entry.contains("\"type\": 1") {
      if let Some(ip) = extract_string_field(entry, "data") { return Some(ip); }
    }
    pos = abs + 1;
    if pos >= after.len() { break; }
  }
  None
}

// ─── main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let args = Args::parse()?;
  tracing_subscriber::fmt()
    .with_max_level(if args.verbose { tracing::Level::DEBUG } else { tracing::Level::INFO })
    // No colour codes when logging to a file, journald or `docker logs`
    .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
    .init();
  let _ = CryptoProvider::install_default(rustls::crypto::ring::default_provider());

  info!("Tunnel client v2 starting (tunnel_id={}, tls={})", args.tunnel_id, !args.no_tls);

  let default_port = if args.no_tls { "8779" } else { "8778" };
  // Reconnect with exponential backoff: --reconnect-interval, doubling per
  // consecutive failure up to MAX_RECONNECT_BACKOFF. A fixed interval meant a
  // persistently failing client (expired proxy cert, suspended tunnel) retried
  // ~40x/minute indefinitely, each attempt also hitting the API.
  let base_delay = Duration::from_secs(args.reconnect_interval.max(1));
  let mut delay = base_delay;
  let mut last_proxy_addr: Option<String> = None;

  loop {
    // Re-fetch proxy address on every reconnect so address changes take effect without restart
    let proxy_addr = match fetch_tunnel_info(&args.api_url, &args.tunnel_id, &args.api_key).await {
      Ok(addr) => {
        let addr = if addr.contains(':') { addr } else { format!("{}:{}", addr, default_port) };
        if last_proxy_addr.as_deref() != Some(&addr) {
          info!("Proxy address: {}", addr);
          if args.no_tls {
            warn!("--no-tls: plain TCP. Ensure server has tunnel_port configured.");
          }
        }
        last_proxy_addr = Some(addr.clone());
        addr
      }
      Err(e) => {
        warn!("Failed to fetch proxy address: {} — retrying in {}s", e, delay.as_secs());
        tokio::time::sleep(delay).await;
        delay = next_backoff(delay, base_delay);
        continue;
      }
    };

    let started = std::time::Instant::now();
    if let Err(e) = run_session(&args, &proxy_addr).await {
      warn!("Session error: {}", e);
    }
    // A session that stayed up counts as healthy (e.g. the proxy closed it to
    // apply a config change) — reconnect promptly rather than backing off.
    if started.elapsed() >= HEALTHY_SESSION {
      delay = base_delay;
    }
    info!("Reconnecting in {} second(s)...", delay.as_secs());
    tokio::time::sleep(delay).await;
    delay = next_backoff(delay, base_delay);
  }
}

/// Upper bound for the reconnect delay after repeated failures.
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(60);
/// A session that lasted at least this long resets the backoff.
const HEALTHY_SESSION: Duration = Duration::from_secs(60);

fn next_backoff(current: Duration, base: Duration) -> Duration {
  (current * 2).min(MAX_RECONNECT_BACKOFF.max(base))
}

// ─── Single session ───────────────────────────────────────────────────────────

async fn run_session(args: &Args, proxy_address: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
  // Resolve hostname with DoH fallback
  let (host, port) = proxy_address.rfind(':')
    .map(|i| (&proxy_address[..i], &proxy_address[i+1..]))
    .unwrap_or((proxy_address, "8778"));
  let connect_addr = match resolve_to_ip(host).await {
    Some(ip) => { if ip != host { debug!("Resolved {} → {}", host, ip); } format!("{}:{}", ip, port) }
    None => proxy_address.to_string(),
  };

  let tcp = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect(&connect_addr))
    .await
    .map_err(|_| format!("TCP connect to {} timed out after 15s", connect_addr))?
    .map_err(|e| format!("TCP connect error: {}", e))?;

  // Framed protocol with many small writes (frame headers, PING/PONG, DATA
  // chunks) — don't let Nagle's algorithm hold them back waiting to batch.
  // The proxy sets this on its side of the socket too.
  if let Err(e) = tcp.set_nodelay(true) {
    warn!("Failed to set TCP_NODELAY: {}", e);
  }

  if args.no_tls {
    info!("Connected (plain TCP) to {}", proxy_address);
    let (reader, writer) = tokio::io::split(tcp);
    run_session_inner(args, reader, writer).await
  } else {
    let sni = args.tls_server_name.as_deref().unwrap_or(host);
    let server_name = ServerName::try_from(sni.to_owned())
      .map_err(|e| format!("Invalid SNI '{}': {}", sni, e))?;
    let tls_config = build_tls_config(args.tls_ca_cert_path.as_deref()).await?;
    let tls_stream: TlsStream<TcpStream> = TlsConnector::from(Arc::new(tls_config)).connect(server_name, tcp).await?;
    info!("TLS connected to {}", proxy_address);
    let (reader, writer) = tokio::io::split(tls_stream);
    run_session_inner(args, reader, writer).await
  }
}

async fn run_session_inner<R, W>(
  args: &Args,
  mut reader: R,
  writer: W,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
  R: tokio::io::AsyncRead + Unpin + Send,
  W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
  let writer = Arc::new(Mutex::new(writer));

  // ── Send CONNECT frame ────────────────────────────────────────────────────
  {
    let payload = build_connect_payload(&args.tunnel_id, &args.api_key);
    let frame = Frame { frame_type: FrameType::Connect, stream_id: 0, flags: 0, payload };
    let mut w = writer.lock().await;
    write_frame(&mut *w, &frame).await?;
    w.flush().await?;
  }
  info!("Sent CONNECT (tunnel_id={})", args.tunnel_id);

  // ── Read CONFIG frame ─────────────────────────────────────────────────────
  let config_frame = read_frame(&mut reader).await?;
  if config_frame.frame_type != FrameType::Config {
    return Err(format!("Expected CONFIG, got {:?}", config_frame.frame_type).into());
  }
  let config = parse_config(&config_frame.payload)
    .ok_or("Failed to parse CONFIG payload")?;

  // Build domain_id → backend_host map
  let domain_map: Arc<HashMap<u16, String>> = Arc::new(
    config.domains.iter().map(|d| { info!("  domain_id={} → {}", d.domain_id, d.local_host); (d.domain_id, d.local_host.clone()) }).collect()
  );
  info!("CONFIG received: {} domain mappings", domain_map.len());

  // ── Body accumulator: stream_id → body_tx ─────────────────────────────────
  // Streams waiting for body DATA frames before their handler can run.
  let body_senders: Arc<Mutex<HashMap<u32, mpsc::Sender<Option<Bytes>>>>> = Arc::new(Mutex::new(HashMap::new()));

  // ── Keepalive task — PING only once the link has gone quiet ───────────────
  // Receiving *any* frame proves the connection is alive, so a PING is only
  // useful after a genuine idle gap. Pinging unconditionally adds pointless
  // frames in the middle of a large transfer, where they queue behind the data
  // they're supposedly probing. `last_rx` is stamped by the reader loop below.
  let last_rx = Arc::new(AtomicU64::new(unix_now()));
  let kw = writer.clone();
  let ka_last_rx = last_rx.clone();
  let keepalive = tokio::spawn(async move {
    let mut seq: u64 = 0;
    loop {
      // Poll finer than the idle threshold so detection stays responsive.
      tokio::time::sleep(Duration::from_secs(5)).await;
      if unix_now().saturating_sub(ka_last_rx.load(Ordering::Relaxed)) < 30 {
        continue;
      }
      seq += 1;
      let ping = Frame { frame_type: FrameType::Ping, stream_id: 0, flags: 0, payload: Bytes::copy_from_slice(&seq.to_be_bytes()) };
      let mut w = kw.lock().await;
      if write_frame(&mut *w, &ping).await.is_err() { break; }
      let _ = w.flush().await;
    }
  });

  // ── Reader loop ───────────────────────────────────────────────────────────
  const READ_TIMEOUT: Duration = Duration::from_secs(60);
  loop {
    let frame = match tokio::time::timeout(READ_TIMEOUT, read_frame(&mut reader)).await {
      Err(_) => {
        warn!("No frame from server in 60s — reconnecting");
        keepalive.abort();
        return Err("keepalive timeout".into());
      }
      Ok(Ok(f)) => {
        // Any frame — not just PONG — proves the server side is alive.
        last_rx.store(unix_now(), Ordering::Relaxed);
        f
      }
      Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
        info!("Tunnel server disconnected");
        keepalive.abort();
        return Ok(());
      }
      Ok(Err(e)) => {
        keepalive.abort();
        return Err(e.into());
      }
    };

    match frame.frame_type {
      FrameType::Request => {
        let mut req = match parse_request(&frame.payload) {
          Some(r) => r,
          None => { warn!("Bad REQUEST payload on stream {}", frame.stream_id); continue; }
        };
        req.upgrade = frame.has_flag(flags::UPGRADE)
          && req.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("upgrade"));

        let stream_id  = frame.stream_id;
        let has_body   = frame.has_flag(flags::HAS_BODY);
        let writer_c   = writer.clone();
        let domain_map = domain_map.clone();
        let body_senders_c = body_senders.clone();

        if has_body {
          // Create a channel for body DATA frames
          let (body_tx, body_rx) = mpsc::channel::<Option<Bytes>>(64);
          body_senders.lock().await.insert(stream_id, body_tx);
          tokio::spawn(handle_stream(stream_id, req, Some(body_rx), writer_c, domain_map, body_senders_c));
        } else {
          tokio::spawn(handle_stream(stream_id, req, None, writer_c, domain_map, body_senders_c));
        }
      }

      FrameType::Data => {
        let stream_id = frame.stream_id;
        let is_end    = frame.has_flag(flags::END_STREAM);
        let mut senders = body_senders.lock().await;
        if let Some(tx) = senders.get(&stream_id) {
          let _ = tx.send(Some(frame.payload)).await;
          if is_end {
            let _ = tx.send(None).await; // sentinel: end of body
            senders.remove(&stream_id);
          }
        }
      }

      FrameType::Reset => {
        let stream_id = frame.stream_id;
        let code = if frame.payload.len() >= 2 { u16::from_be_bytes([frame.payload[0], frame.payload[1]]) } else { 0 };
        debug!("RESET stream_id={} code={}", stream_id, code);
        body_senders.lock().await.remove(&stream_id);
      }

      FrameType::Ping => {
        let opaque = if frame.payload.len() >= 8 {
          let mut a = [0u8; 8]; a.copy_from_slice(&frame.payload[..8]); a
        } else { [0u8; 8] };
        let pong = Frame { frame_type: FrameType::Pong, stream_id: 0, flags: 0, payload: Bytes::copy_from_slice(&opaque) };
        let mut w = writer.lock().await;
        let _ = write_frame(&mut *w, &pong).await;
        let _ = w.flush().await;
      }

      FrameType::Pong => {
        // PONG received — keepalive round-trip confirmed
      }

      FrameType::GoAway => {
        info!("GOAWAY received, closing session");
        keepalive.abort();
        return Ok(());
      }

      other => {
        debug!("Unexpected frame type from server: {:?}", other);
      }
    }
  }
}

// ─── Per-stream handler ───────────────────────────────────────────────────────

async fn handle_stream(
  stream_id: u32,
  req: RequestPayload,
  mut body_rx: Option<mpsc::Receiver<Option<Bytes>>>,
  writer: Arc<Mutex<impl AsyncWrite + Unpin + Send>>,
  domain_map: Arc<HashMap<u16, String>>,
  body_senders: Arc<Mutex<HashMap<u32, mpsc::Sender<Option<Bytes>>>>>,
) {
  let backend = match domain_map.get(&req.domain_id) {
    Some(h) => h.clone(),
    None => {
      warn!("Unknown domain_id={} for stream {}", req.domain_id, stream_id);
      send_reset(&writer, stream_id, reset_codes::BACKEND_UNREACHABLE).await;
      return;
    }
  };

  // Collect body if expected
  let body: Option<Bytes> = if let Some(rx) = &mut body_rx {
    let mut buf = BytesMut::new();
    loop {
      match rx.recv().await {
        Some(Some(chunk)) => buf.extend_from_slice(&chunk),
        Some(None) | None => break,
      }
    }
    if buf.is_empty() { None } else { Some(buf.freeze()) }
  } else {
    None
  };

  debug!("stream {} → {} {} {}", stream_id, req.method, req.path, backend);

  // Make HTTP/1.1 request to local backend
  let result = forward_to_backend(stream_id, &req, body, &backend, &writer, &body_senders).await;
  if let Err(e) = result {
    warn!("stream {} backend error: {}", stream_id, e);
    body_senders.lock().await.remove(&stream_id);
    send_reset(&writer, stream_id, reset_codes::INTERNAL_ERROR).await;
  }
}

async fn forward_to_backend(
  stream_id: u32,
  req: &RequestPayload,
  body: Option<Bytes>,
  backend: &str,
  writer: &Arc<Mutex<impl AsyncWrite + Unpin + Send>>,
  body_senders: &BodySenders,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
  let mut stream = TcpStream::connect(backend).await
    .map_err(|e| format!("Backend connect failed ({}): {}", backend, e))?;

  // Build HTTP/1.1 request — always send Connection: close so the backend closes
  // after the response, giving us a clean EOF for body detection.
  let mut req_bytes = Vec::new();
  req_bytes.extend_from_slice(format!("{} {} HTTP/1.1\r\n", req.method, req.path).as_bytes());
  for (name, value) in &req.headers {
    let lower = name.to_ascii_lowercase();
    // `expect`: the whole body is sent up front, so there is nothing to wait for (and an
    // interim `100 Continue` from the backend would otherwise be mistaken for the response).
    if lower == "connection" || lower == "keep-alive" || lower == "expect" { continue; } // connection overridden below
    req_bytes.extend_from_slice(format!("{}: {}\r\n", name, value).as_bytes());
  }
  if req.upgrade {
    // WebSocket handshake: the backend must see the upgrade and keep the socket open.
    req_bytes.extend_from_slice(b"connection: Upgrade\r\n");
  } else {
    req_bytes.extend_from_slice(b"connection: close\r\n");
  }
  if let Some(ref b) = body {
    let has_cl = req.headers.iter().any(|(n, _)| n.to_ascii_lowercase() == "content-length");
    if !has_cl {
      req_bytes.extend_from_slice(format!("content-length: {}\r\n", b.len()).as_bytes());
    }
  }
  req_bytes.extend_from_slice(b"\r\n");
  if let Some(ref b) = body { req_bytes.extend_from_slice(b); }

  stream.write_all(&req_bytes).await?;
  stream.flush().await?;

  // Read response headers, skipping interim 1xx responses (100 Continue, 103 Early Hints):
  // only the final response is relayed. A 101 is final when an upgrade was requested.
  let mut resp_buf = Vec::new();
  let mut tmp = vec![0u8; 32 * 1024];
  let header_end = loop {
    let header_end = loop {
      if let Some(pos) = resp_buf.windows(4).position(|w| w == b"\r\n\r\n") {
        break pos + 4;
      }
      let n = stream.read(&mut tmp).await?;
      if n == 0 { break resp_buf.len(); }
      resp_buf.extend_from_slice(&tmp[..n]);
    };
    let interim = status_of(&resp_buf[..header_end])
      .is_some_and(|c| (100..200).contains(&c) && !(c == 101 && req.upgrade));
    if !interim { break header_end; }
    resp_buf.drain(..header_end);
  };

  if header_end == 0 || resp_buf.is_empty() {
    return Err("Backend closed without sending response headers".into());
  }

  // Parse status line + headers
  let header_str = std::str::from_utf8(&resp_buf[..header_end])
    .map_err(|_| "Invalid UTF-8 in response headers")?;
  let mut lines = header_str.lines();
  let status_line = lines.next().ok_or("Missing status line")?;
  let status_code: u16 = status_line.split_whitespace().nth(1)
    .ok_or("Missing status code")?
    .parse().map_err(|_| "Invalid status code")?;

  let mut resp_headers: Vec<(String, String)> = Vec::new();
  let mut content_length: Option<usize> = None;
  let mut is_chunked = false;
  for line in lines {
    if line.is_empty() { break; }
    if let Some((name, value)) = line.split_once(':') {
      let name_lc = name.trim().to_ascii_lowercase();
      let val = value.trim().to_string();
      if name_lc == "content-length" { content_length = val.parse().ok(); }
      if name_lc == "transfer-encoding" && val.to_ascii_lowercase().contains("chunked") { is_chunked = true; }
      if name_lc != "transfer-encoding" { // strip TE — we'll dechunk for client
        resp_headers.push((name.trim().to_string(), val));
      }
    }
  }

  // Backend accepted the WebSocket upgrade: from here on the stream is a raw byte pipe.
  if req.upgrade && status_code == 101 {
    let leftover = resp_buf[header_end..].to_vec();
    let (tx, rx) = mpsc::channel::<Option<Bytes>>(64);
    body_senders.lock().await.insert(stream_id, tx);

    let resp_frame = Frame {
      frame_type: FrameType::Response,
      stream_id,
      flags: flags::RESPONSE_UPGRADED,
      payload: build_response_payload(status_code, &resp_headers),
    };
    let sent = {
      let mut w = writer.lock().await;
      match write_frame(&mut *w, &resp_frame).await {
        Ok(()) => w.flush().await,
        Err(e) => Err(e),
      }
    };
    if let Err(e) = sent {
      body_senders.lock().await.remove(&stream_id);
      return Err(e.into());
    }

    debug!("stream {} upgraded: {} {}", stream_id, req.path, backend);
    relay_upgraded(stream_id, stream, leftover, rx, writer).await;
    body_senders.lock().await.remove(&stream_id);
    debug!("stream {} upgrade closed", stream_id);
    return Ok(());
  }

  // Status codes that carry no body per RFC 7230 §3.3
  let no_body = (100..200).contains(&status_code) || status_code == 204 || status_code == 304
    || req.method.eq_ignore_ascii_case("HEAD");

  // Body already partially read (bytes after header_end)
  let mut body_buf: Vec<u8> = if no_body { Vec::new() } else { resp_buf[header_end..].to_vec() };

  if !no_body {
    if let Some(cl) = content_length {
      while body_buf.len() < cl {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
          // Don't pass a short body off as complete: the browser would cache a corrupt file.
          return Err(format!("Backend closed after {} of {} body bytes", body_buf.len(), cl).into());
        }
        body_buf.extend_from_slice(&tmp[..n]);
      }
      body_buf.truncate(cl);
    } else if is_chunked {
      loop {
        match decode_chunked(&body_buf) {
          Chunked::Complete(decoded) => { body_buf = decoded; break; }
          Chunked::Invalid => return Err("Backend sent invalid chunked encoding".into()),
          Chunked::Incomplete => {}
        }
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
          return Err("Backend closed in the middle of a chunked response".into());
        }
        body_buf.extend_from_slice(&tmp[..n]);
      }
    } else {
      // No Content-Length, not chunked — read until EOF (Connection: close ensures this)
      // Use a short deadline to avoid hanging on responses that never close
      let read_fut = async {
        loop {
          let n = stream.read(&mut tmp).await?;
          if n == 0 { break; }
          body_buf.extend_from_slice(&tmp[..n]);
        }
        Ok::<_, std::io::Error>(())
      };
      let _ = tokio::time::timeout(std::time::Duration::from_secs(10), read_fut).await;
    }

    // Content-Length must describe the body we relay (chunked framing was removed above).
    // Responses without a body (HEAD, 204, 304, 1xx) keep the backend's headers untouched.
    if let Some(pos) = resp_headers.iter().position(|(n, _)| n.to_ascii_lowercase() == "content-length") {
      resp_headers[pos].1 = body_buf.len().to_string();
    } else {
      resp_headers.push(("content-length".to_string(), body_buf.len().to_string()));
    }
  }

  // Send RESPONSE frame
  let resp_flags = if body_buf.is_empty() { 0 } else { flags::RESPONSE_HAS_BODY };
  let resp_payload = build_response_payload(status_code, &resp_headers);
  let resp_frame = Frame { frame_type: FrameType::Response, stream_id, flags: resp_flags, payload: resp_payload };
  {
    let mut w = writer.lock().await;
    write_frame(&mut *w, &resp_frame).await?;
    w.flush().await?;
  }

  // Send DATA frames (chunk at 64 KiB)
  if !body_buf.is_empty() {
    const CHUNK: usize = 65536;
    let total = body_buf.len();
    let mut sent = 0;
    while sent < total {
      let end = (sent + CHUNK).min(total);
      let is_last = end == total;
      let data_flags = if is_last { flags::END_STREAM } else { 0 };
      let chunk = Bytes::copy_from_slice(&body_buf[sent..end]);
      let data_frame = Frame { frame_type: FrameType::Data, stream_id, flags: data_flags, payload: chunk };
      let mut w = writer.lock().await;
      write_frame(&mut *w, &data_frame).await?;
      w.flush().await?;
      sent = end;
    }
  }

  debug!("stream {} done: {} {}", stream_id, status_code, backend);
  Ok(())
}

/// Send one DATA frame; false if the tunnel is gone.
async fn send_data(
  writer: &Arc<Mutex<impl AsyncWrite + Unpin + Send>>,
  stream_id: u32,
  flags: u8,
  payload: Bytes,
) -> bool {
  let frame = Frame { frame_type: FrameType::Data, stream_id, flags, payload };
  let mut w = writer.lock().await;
  write_frame(&mut *w, &frame).await.is_ok() && w.flush().await.is_ok()
}

enum TunnelEnd {
  /// Server sent END_STREAM: the browser half-closed.
  HalfClosed,
  /// Server sent RESET (or the session ended).
  Aborted,
}

/// Pump bytes between the backend socket and the tunnel for an upgraded (WebSocket) stream.
async fn relay_upgraded(
  stream_id: u32,
  stream: TcpStream,
  leftover: Vec<u8>,
  mut body_rx: mpsc::Receiver<Option<Bytes>>,
  writer: &Arc<Mutex<impl AsyncWrite + Unpin + Send>>,
) {
  /// After one side closes, how long the other gets to finish its close handshake.
  const HALF_CLOSE_GRACE: Duration = Duration::from_secs(5);
  let (mut rd, mut wr) = stream.into_split();

  // backend → tunnel
  let up = async {
    // Bytes the backend sent right behind its 101 headers.
    if !leftover.is_empty() && !send_data(writer, stream_id, 0, Bytes::from(leftover)).await {
      return;
    }
    let mut buf = vec![0u8; 16 * 1024];
    loop {
      match rd.read(&mut buf).await {
        Ok(0) | Err(_) => {
          send_data(writer, stream_id, flags::END_STREAM, Bytes::new()).await;
          break;
        }
        Ok(n) => {
          if !send_data(writer, stream_id, 0, Bytes::copy_from_slice(&buf[..n])).await { break; }
        }
      }
    }
  };

  // tunnel → backend
  let down = async {
    loop {
      match body_rx.recv().await {
        Some(Some(chunk)) => {
          if wr.write_all(&chunk).await.is_err() || wr.flush().await.is_err() {
            return TunnelEnd::Aborted;
          }
        }
        Some(None) => {
          let _ = wr.shutdown().await;
          return TunnelEnd::HalfClosed;
        }
        None => return TunnelEnd::Aborted,
      }
    }
  };

  tokio::pin!(up, down);
  tokio::select! {
    end = &mut down => {
      if let TunnelEnd::HalfClosed = end {
        // Browser is gone; let the backend finish and close its side.
        let _ = tokio::time::timeout(HALF_CLOSE_GRACE, &mut up).await;
      }
    }
    _ = &mut up => {
      // Backend closed; give the server a moment to deliver any trailing bytes.
      let _ = tokio::time::timeout(HALF_CLOSE_GRACE, &mut down).await;
    }
  }
}

async fn send_reset(writer: &Arc<Mutex<impl AsyncWrite + Unpin + Send>>, stream_id: u32, error_code: u16) {
  let mut payload = BytesMut::new();
  payload.put_u16(error_code);
  let frame = Frame { frame_type: FrameType::Reset, stream_id, flags: 0, payload: payload.freeze() };
  let mut w = writer.lock().await;
  let _ = write_frame(&mut *w, &frame).await;
  let _ = w.flush().await;
}

// ─── Response parsing helpers ─────────────────────────────────────────────────

/// Status code from the first line of a response head.
fn status_of(head: &[u8]) -> Option<u16> {
  std::str::from_utf8(head).ok()?.lines().next()?.split_whitespace().nth(1)?.parse().ok()
}

// ─── Chunked transfer decoding ────────────────────────────────────────────────

enum Chunked {
  /// Terminating chunk (and trailers) seen: the de-chunked body.
  Complete(Vec<u8>),
  /// More bytes are needed.
  Incomplete,
  /// Not valid chunked framing.
  Invalid,
}

fn find_crlf(data: &[u8]) -> Option<usize> {
  data.windows(2).position(|w| w == b"\r\n")
}

/// Decode a chunked body from its start. Completeness is decided by walking the chunk
/// sizes, never by searching the payload, so chunk data may contain any bytes
/// (including `0\r\n\r\n`). Chunk extensions are ignored; trailers are consumed.
fn decode_chunked(data: &[u8]) -> Chunked {
  let mut pos = 0usize;
  let mut ranges: Vec<(usize, usize)> = Vec::new();
  loop {
    let Some(rel) = find_crlf(&data[pos..]) else { return Chunked::Incomplete };
    let Ok(line) = std::str::from_utf8(&data[pos..pos + rel]) else { return Chunked::Invalid };
    let Ok(size) = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16) else {
      return Chunked::Invalid;
    };
    pos += rel + 2;
    if size == 0 {
      // Trailer section: header lines until an empty line.
      loop {
        let Some(rel) = find_crlf(&data[pos..]) else { return Chunked::Incomplete };
        pos += rel + 2;
        if rel == 0 { break; }
      }
      break;
    }
    let Some(end) = pos.checked_add(size) else { return Chunked::Invalid };
    if end + 2 > data.len() { return Chunked::Incomplete; }
    if &data[end..end + 2] != b"\r\n" { return Chunked::Invalid; }
    ranges.push((pos, end));
    pos = end + 2;
  }
  let mut out = Vec::with_capacity(ranges.iter().map(|(a, b)| b - a).sum());
  for (a, b) in ranges { out.extend_from_slice(&data[a..b]); }
  Chunked::Complete(out)
}

#[cfg(test)]
mod backoff_tests {
  use super::*;

  #[test]
  fn doubles_up_to_the_cap() {
    let base = Duration::from_secs(1);
    let mut d = base;
    let mut seen = vec![];
    for _ in 0..9 {
      seen.push(d.as_secs());
      d = next_backoff(d, base);
    }
    assert_eq!(seen, vec![1, 2, 4, 8, 16, 32, 60, 60, 60]);
  }

  #[test]
  fn base_above_cap_is_respected() {
    let base = Duration::from_secs(120);
    assert_eq!(next_backoff(base, base), base);
  }
}

#[cfg(test)]
mod upgrade_tests {
  use super::*;
  use tokio::net::TcpListener;

  /// Read an HTTP request head from `sock`, returning it lower-cased.
  async fn read_head(sock: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
      let n = sock.read(&mut tmp).await.unwrap();
      assert!(n > 0, "backend peer closed before sending a full request");
      buf.extend_from_slice(&tmp[..n]);
    }
    String::from_utf8(buf).unwrap().to_ascii_lowercase()
  }

  fn request(upgrade: bool) -> RequestPayload {
    let mut headers = vec![("host".to_string(), "x.example".to_string())];
    if upgrade {
      headers.push(("upgrade".to_string(), "websocket".to_string()));
      headers.push(("connection".to_string(), "Upgrade".to_string()));
      headers.push(("sec-websocket-key".to_string(), "dGhlIHNhbXBsZSBub25jZQ==".to_string()));
    }
    RequestPayload { domain_id: 1, method: "GET".into(), path: "/ws".into(), headers, upgrade }
  }

  #[tokio::test]
  async fn websocket_upgrade_becomes_a_raw_pipe() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    // Echo backend that also sends a byte string right behind its 101 headers.
    tokio::spawn(async move {
      let (mut sock, _) = listener.accept().await.unwrap();
      let head = read_head(&mut sock).await;
      assert!(head.contains("upgrade: websocket"), "Upgrade header must reach the backend: {head}");
      assert!(head.contains("connection: upgrade"), "backend must see Connection: Upgrade: {head}");
      assert!(!head.contains("connection: close"), "must not force close on an upgrade: {head}");
      sock.write_all(
        b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: abc\r\n\r\nearly",
      ).await.unwrap();
      let mut buf = [0u8; 4096];
      loop {
        let n = sock.read(&mut buf).await.unwrap();
        if n == 0 { break; }
        sock.write_all(&buf[..n]).await.unwrap();
      }
      // dropping `sock` closes the connection once the client half-closes
    });

    let (client_w, mut server_r) = tokio::io::duplex(1 << 20);
    let writer = Arc::new(Mutex::new(client_w));
    let senders: BodySenders = Arc::new(Mutex::new(HashMap::new()));
    let (w, s) = (writer.clone(), senders.clone());
    let task = tokio::spawn(async move { forward_to_backend(9, &request(true), None, &addr, &w, &s).await });

    let resp = read_frame(&mut server_r).await.unwrap();
    assert_eq!(resp.frame_type, FrameType::Response);
    assert!(resp.has_flag(flags::RESPONSE_UPGRADED));
    assert!(!resp.has_flag(flags::RESPONSE_HAS_BODY));
    assert_eq!(u16::from_be_bytes([resp.payload[0], resp.payload[1]]), 101);
    assert!(String::from_utf8_lossy(&resp.payload).contains("Sec-WebSocket-Accept"));

    let early = read_frame(&mut server_r).await.unwrap();
    assert_eq!((early.frame_type, &early.payload[..]), (FrameType::Data, &b"early"[..]));

    // server → backend → echoed back to the server
    let tx = senders.lock().await.get(&9).cloned().expect("upgraded stream must be registered");
    tx.send(Some(Bytes::from_static(b"ping"))).await.unwrap();
    let echo = read_frame(&mut server_r).await.unwrap();
    assert_eq!((echo.frame_type, &echo.payload[..]), (FrameType::Data, &b"ping"[..]));
    assert!(!echo.has_flag(flags::END_STREAM));

    // browser half-close → backend sees EOF and closes → END_STREAM comes back
    tx.send(None).await.unwrap();
    let end = read_frame(&mut server_r).await.unwrap();
    assert_eq!(end.frame_type, FrameType::Data);
    assert!(end.has_flag(flags::END_STREAM));

    task.await.unwrap().unwrap();
    assert!(senders.lock().await.is_empty(), "stream must be forgotten after the relay ends");
  }

  #[tokio::test]
  async fn declined_upgrade_is_an_ordinary_response() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
      let (mut sock, _) = listener.accept().await.unwrap();
      read_head(&mut sock).await;
      sock.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 3\r\n\r\nbad").await.unwrap();
    });

    let (client_w, mut server_r) = tokio::io::duplex(1 << 20);
    let writer = Arc::new(Mutex::new(client_w));
    let senders: BodySenders = Arc::new(Mutex::new(HashMap::new()));
    forward_to_backend(3, &request(true), None, &addr, &writer, &senders).await.unwrap();

    let resp = read_frame(&mut server_r).await.unwrap();
    assert_eq!(resp.frame_type, FrameType::Response);
    assert!(!resp.has_flag(flags::RESPONSE_UPGRADED));
    assert!(resp.has_flag(flags::RESPONSE_HAS_BODY));
    assert_eq!(u16::from_be_bytes([resp.payload[0], resp.payload[1]]), 400);
    let body = read_frame(&mut server_r).await.unwrap();
    assert_eq!(&body.payload[..], b"bad");
  }

  #[tokio::test]
  async fn plain_requests_still_force_connection_close() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let backend = tokio::spawn(async move {
      let (mut sock, _) = listener.accept().await.unwrap();
      let head = read_head(&mut sock).await;
      sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await.unwrap();
      head
    });

    let (client_w, mut server_r) = tokio::io::duplex(1 << 20);
    let writer = Arc::new(Mutex::new(client_w));
    let senders: BodySenders = Arc::new(Mutex::new(HashMap::new()));
    forward_to_backend(4, &request(false), None, &addr, &writer, &senders).await.unwrap();

    let head = backend.await.unwrap();
    assert!(head.contains("connection: close"));
    let resp = read_frame(&mut server_r).await.unwrap();
    assert!(!resp.has_flag(flags::RESPONSE_UPGRADED));
  }
}

#[cfg(test)]
mod http_semantics_tests {
  use super::*;
  use tokio::net::TcpListener;

  fn complete(data: &[u8]) -> Vec<u8> {
    match decode_chunked(data) { Chunked::Complete(d) => d, Chunked::Incomplete => panic!("incomplete"), Chunked::Invalid => panic!("invalid") }
  }

  #[test]
  fn chunked_data_may_contain_the_terminator_sequence() {
    // "0\r\n\r\n" inside chunk data used to be taken for the end of the body.
    let body = b"a=10\r\n\r\nrest";
    let mut wire = format!("{:x}\r\n", body.len()).into_bytes();
    wire.extend_from_slice(body);
    wire.extend_from_slice(b"\r\n3\r\nend\r\n0\r\n\r\n");
    assert_eq!(complete(&wire), b"a=10\r\n\r\nrestend");
    // …and a prefix that stops after that data is merely incomplete.
    assert!(matches!(decode_chunked(&wire[..wire.len() - 8]), Chunked::Incomplete));
  }

  #[test]
  fn chunked_extensions_and_trailers_are_consumed() {
    assert_eq!(complete(b"5;ext=1\r\nhello\r\n0\r\nX-Trailer: v\r\n\r\n"), b"hello");
    assert!(matches!(decode_chunked(b"5\r\nhello\r\n0\r\nX-Trailer: v\r\n"), Chunked::Incomplete));
    assert!(matches!(decode_chunked(b""), Chunked::Incomplete));
    assert!(matches!(decode_chunked(b"zz\r\nhello\r\n"), Chunked::Invalid));
    assert!(matches!(decode_chunked(b"5\r\nhelloXX0\r\n\r\n"), Chunked::Invalid));
  }

  /// Backend that replies with `reply` to the first request, then closes (or `then` runs).
  async fn backend_with(reply: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
      let (mut sock, _) = listener.accept().await.unwrap();
      let mut buf = [0u8; 4096];
      let _ = sock.read(&mut buf).await; // request head (+ small body)
      sock.write_all(&reply).await.unwrap();
    });
    addr
  }

  fn req(method: &str) -> RequestPayload {
    RequestPayload { domain_id: 1, method: method.into(), path: "/".into(),
      headers: vec![("host".into(), "x.example".into())], upgrade: false }
  }

  /// Run forward_to_backend and return (result, frames written to the tunnel).
  async fn run(method: &str, addr: &str) -> (Result<(), String>, Vec<Frame>) {
    let (client_w, mut server_r) = tokio::io::duplex(1 << 20);
    let writer = Arc::new(Mutex::new(client_w));
    let senders: BodySenders = Arc::new(Mutex::new(HashMap::new()));
    let result = forward_to_backend(1, &req(method), None, addr, &writer, &senders).await.map_err(|e| e.to_string());
    drop(writer); // close our end so the reads below end at EOF
    let mut frames = Vec::new();
    while let Ok(f) = read_frame(&mut server_r).await { frames.push(f); }
    (result, frames)
  }

  fn response_of(frame: &Frame) -> (u16, Vec<(String, String)>) {
    let status = u16::from_be_bytes([frame.payload[0], frame.payload[1]]);
    let mut pos = 2;
    (status, read_headers(&frame.payload, &mut pos).unwrap())
  }

  fn header<'a>(h: &'a [(String, String)], name: &str) -> Option<&'a str> {
    h.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
  }

  #[tokio::test]
  async fn short_body_is_an_error_not_a_complete_response() {
    let addr = backend_with(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\nonly-some".to_vec()).await;
    let (result, frames) = run("GET", &addr).await;
    assert!(result.is_err(), "must fail so the stream is reset, got {result:?}");
    assert!(frames.is_empty(), "no RESPONSE may be sent for a truncated body");
  }

  #[tokio::test]
  async fn cut_off_chunked_stream_is_an_error() {
    let addr = backend_with(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n".to_vec()).await;
    let (result, frames) = run("GET", &addr).await;
    assert!(result.is_err());
    assert!(frames.is_empty());
  }

  #[tokio::test]
  async fn head_keeps_the_backend_content_length() {
    let addr = backend_with(b"HTTP/1.1 200 OK\r\nContent-Length: 1234\r\n\r\n".to_vec()).await;
    let (result, frames) = run("HEAD", &addr).await;
    result.unwrap();
    let (status, headers) = response_of(&frames[0]);
    assert_eq!(status, 200);
    assert_eq!(header(&headers, "content-length"), Some("1234"));
    assert!(!frames[0].has_flag(flags::RESPONSE_HAS_BODY));
  }

  #[tokio::test]
  async fn no_content_and_not_modified_get_no_synthetic_length() {
    let addr = backend_with(b"HTTP/1.1 204 No Content\r\n\r\n".to_vec()).await;
    let (_, frames) = run("GET", &addr).await;
    assert_eq!(header(&response_of(&frames[0]).1, "content-length"), None);

    let addr = backend_with(b"HTTP/1.1 304 Not Modified\r\nETag: \"abc\"\r\n\r\n".to_vec()).await;
    let (_, frames) = run("GET", &addr).await;
    let (status, headers) = response_of(&frames[0]);
    assert_eq!((status, header(&headers, "etag"), header(&headers, "content-length")), (304, Some("\"abc\""), None));
  }

  #[tokio::test]
  async fn interim_responses_are_skipped() {
    let addr = backend_with(
      b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\nLink: </a.css>\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
    ).await;
    let (result, frames) = run("POST", &addr).await;
    result.unwrap();
    let (status, headers) = response_of(&frames[0]);
    assert_eq!(status, 200);
    assert_eq!(header(&headers, "link"), None, "interim headers must not leak into the final response");
    assert_eq!(&frames[1].payload[..], b"ok");
  }

  #[tokio::test]
  async fn expect_header_is_not_forwarded() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let backend = tokio::spawn(async move {
      let (mut sock, _) = listener.accept().await.unwrap();
      let mut buf = [0u8; 4096];
      let n = sock.read(&mut buf).await.unwrap();
      sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await.unwrap();
      String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase()
    });
    let mut r = req("POST");
    r.headers.push(("expect".into(), "100-continue".into()));
    let (client_w, _server_r) = tokio::io::duplex(1 << 20);
    let writer = Arc::new(Mutex::new(client_w));
    let senders: BodySenders = Arc::new(Mutex::new(HashMap::new()));
    forward_to_backend(1, &r, Some(Bytes::from_static(b"data")), &addr, &writer, &senders).await.unwrap();
    assert!(!backend.await.unwrap().contains("expect:"));
  }

  #[tokio::test]
  async fn chunked_body_split_over_many_reads_is_complete() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
      let (mut sock, _) = listener.accept().await.unwrap();
      let mut buf = [0u8; 4096];
      let _ = sock.read(&mut buf).await;
      sock.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
      for part in [&b"total=10\r\n\r\nfirst;"[..], b" second;", b" third"] {
        sock.write_all(format!("{:x}\r\n", part.len()).as_bytes()).await.unwrap();
        sock.write_all(part).await.unwrap();
        sock.write_all(b"\r\n").await.unwrap();
        sock.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
      }
      sock.write_all(b"0\r\n\r\n").await.unwrap();
    });
    let (result, frames) = run("GET", &addr).await;
    result.unwrap();
    let body: Vec<u8> = frames[1..].iter().flat_map(|f| f.payload.to_vec()).collect();
    assert_eq!(body, b"total=10\r\n\r\nfirst; second; third");
    assert_eq!(header(&response_of(&frames[0]).1, "content-length"), Some("32"));
  }
}
