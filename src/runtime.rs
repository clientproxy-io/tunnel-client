use serde_json::{Value, json};
use std::path::Path;

const METHODS: &[&str] = &[
    "deb",
    "rpm",
    "docker",
    "synology",
    "windows-service",
    "binary",
    "unknown",
];

fn distribution() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    text.lines()
        .find_map(|line| {
            line.strip_prefix("ID=")
                .map(|s| s.trim_matches('"').to_string())
        })
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 32
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        })
}

pub fn local_runtime(component: &str) -> Value {
    let exe = std::env::current_exe().ok();
    let packaged = format!(
        "/usr/local/bin/tunnel-{}",
        if component == "server" {
            "proxy"
        } else {
            "client"
        }
    );
    let marker = format!(
        "/usr/share/clientproxy/tunnel-{}-install-method",
        if component == "server" {
            "proxy"
        } else {
            "client"
        }
    );
    let method = std::env::var("CLIENTPROXY_INSTALL_METHOD")
        .ok()
        .filter(|s| METHODS.contains(&s.as_str()))
        .or_else(|| {
            (exe.as_deref() == Some(Path::new(&packaged)))
                .then(|| std::fs::read_to_string(marker).ok())
                .flatten()
                .map(|s| s.trim().to_string())
                .filter(|s| METHODS.contains(&s.as_str()))
        })
        .unwrap_or_else(|| {
            if Path::new("/.dockerenv").exists() {
                "docker".into()
            } else {
                "binary".into()
            }
        });
    let mut value = json!({
      "version": env!("CARGO_PKG_VERSION"),
      "os": std::env::consts::OS,
      "arch": std::env::consts::ARCH,
      "target": env!("CLIENTPROXY_BUILD_TARGET"),
      "installMethod": method,
    });
    if let Some(distro) = distribution() {
        value["distribution"] = json!(distro);
    }
    if component == "server" {
        if let Some(path) = exe
            .as_deref()
            .and_then(Path::to_str)
            .filter(|p| ["/usr/sbin/tunnel-proxy", "/usr/local/bin/tunnel-proxy"].contains(p))
        {
            value["binaryPath"] = json!(path);
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_uses_the_compiled_version_and_target_without_credentials() {
        let m = local_runtime("server");
        assert_eq!(m["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(m["os"], std::env::consts::OS);
        assert_eq!(m["arch"], std::env::consts::ARCH);
        assert!(!m["target"].as_str().unwrap().is_empty());
        assert!(METHODS.contains(&m["installMethod"].as_str().unwrap()));
        assert!(m.get("apiKey").is_none());
    }
}

struct ReportGuard(tokio::task::JoinHandle<()>);
impl Drop for ReportGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Send one best-effort report for this connected session; the guard cancels it on disconnect.
pub fn start_reporter(base: String, tunnel_id: String, key: String) -> impl Drop {
    ReportGuard(tokio::spawn(async move {
        use http_body_util::Full;
        use hyper::body::Bytes;
        let url = format!(
            "{}/runtime/client-tunnels/{}",
            base.trim_end_matches('/'),
            tunnel_id
        );
        let payload = serde_json::to_vec(&local_runtime("client")).expect("runtime metadata JSON");
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(
                    hyper_rustls::HttpsConnectorBuilder::new()
                        .with_webpki_roots()
                        .https_or_http()
                        .enable_http1()
                        .build(),
                );
        let request = match hyper::Request::builder()
            .method("POST")
            .uri(&url)
            .header("Authorization", format!("Bearer {}", key))
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::copy_from_slice(&payload)))
        {
            Ok(req) => req,
            Err(_) => return,
        };
        match tokio::time::timeout(std::time::Duration::from_secs(8), client.request(request)).await
        {
            Ok(Ok(response)) if response.status().is_success() => {}
            Ok(Ok(response)) => tracing::warn!(
                "Runtime metadata report returned {}; tunnel continues",
                response.status()
            ),
            _ => tracing::warn!("Runtime metadata report unavailable; tunnel continues"),
        }
    }))
}
