use serde_json::Value;
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Minimal HTTP/1.1 client over TCP — the gateway is plain HTTP inside
/// WireGuard (TLS is an M6 option), so no TLS stack is needed.
pub struct HttpClient {
    host: String,
    port: u16,
    token: String,
}

impl HttpClient {
    /// `base` like `http://10.77.0.1:8080`.
    pub fn new(base: &str, token: &str) -> Result<HttpClient> {
        let rest = base
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        let (host, port) = match rest.split_once(':') {
            Some((h, p)) => (h.to_string(), p.parse()?),
            None => (rest.to_string(), 80),
        };
        Ok(HttpClient {
            host,
            port,
            token: token.into(),
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value)> {
        let mut stream = tokio::net::TcpStream::connect((self.host.as_str(), self.port)).await?;
        let body_text = body.map(|b| b.to_string()).unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
            host = self.host,
            port = self.port,
            token = self.token,
            len = body_text.len(),
            body = body_text,
        );
        stream.write_all(request.as_bytes()).await?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await?;
        parse_response(&raw)
    }

    pub async fn get(&self, path: &str) -> Result<(u16, Value)> {
        self.request("GET", path, None).await
    }

    pub async fn post(&self, path: &str, body: &Value) -> Result<(u16, Value)> {
        self.request("POST", path, Some(body)).await
    }

    pub async fn delete(&self, path: &str) -> Result<(u16, Value)> {
        self.request("DELETE", path, None).await
    }
}

pub(crate) fn parse_response(raw: &[u8]) -> Result<(u16, Value)> {
    let header_end = find(raw, b"\r\n\r\n").ok_or("malformed HTTP response")?;
    let head = std::str::from_utf8(&raw[..header_end])?;
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or("malformed HTTP status")?;
    let body = &raw[header_end + 4..];
    if body.is_empty() {
        return Ok((status, Value::Null));
    }
    let text = std::str::from_utf8(body)?;
    let value = serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()));
    Ok((status, value))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Error type carrying an HTTP status for actionable messages.
#[derive(Debug)]
pub struct HttpError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}: {}", self.status, self.message)
    }
}

impl std::error::Error for HttpError {}

impl From<HttpError> for io::Error {
    fn from(e: HttpError) -> io::Error {
        io::Error::other(e.to_string())
    }
}

/// Extract an actionable message from a gateway error body.
pub fn error_message(status: u16, body: &Value) -> HttpError {
    let message = body
        .get("message")
        .or_else(|| body.get("error"))
        .and_then(|m| m.as_str())
        .unwrap_or("request failed")
        .to_string();
    HttpError { status, message }
}
