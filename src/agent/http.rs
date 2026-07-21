use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context as _, Result, anyhow, bail};

use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

use super::{
    AgentBackend, AgentCommand,
    queue::{DEFAULT_QUEUE_CAPACITY, DispatchQueue, QueueError},
};

/// Largest request header block we'll buffer before giving up. Defends against
/// a slow-loris-style peer that streams headers forever.
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Largest request body we'll accept. A single agent command JSON is well under
/// this; it's a guardrail against a peer that lies in its `Content-Length`.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Configuration for the dev HTTP agent API (`agent-serve` subcommand).
///
/// This is a development / Tailscale-only harness: the production transport is
/// the WSS control-plane connection in [`super::websocket`]. The HTTP API
/// exists so the dashboard can drive the same Kameo-backed agent from a
/// browser test UI on a private network.
#[derive(Debug, Clone)]
pub struct HttpAgentConfig {
    pub listen: SocketAddr,
    pub bearer_token: Option<String>,
}

/// Bind the listener and serve connections forever. Each connection is handled
/// on its own tokio task; the shared [`AgentBackend`] actor serializes the
/// actual dispatch work.
pub async fn serve(config: HttpAgentConfig) -> Result<()> {
    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("failed to bind agent HTTP listener on {}", config.listen))?;
    let queue = DispatchQueue::spawn(AgentBackend::spawn_default(), DEFAULT_QUEUE_CAPACITY);
    let config = Arc::new(config);

    loop {
        let (stream, source) = listener.accept().await?;
        let queue = queue.clone();
        let config = Arc::clone(&config);

        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, source, queue, config).await {
                println!(
                    "source={} method=- path=- status=500 duration_ms=0 error={:?}",
                    source.ip(),
                    error.to_string()
                );
            }
        });
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    source: SocketAddr,
    queue: DispatchQueue,
    config: Arc<HttpAgentConfig>,
) -> Result<()> {
    let started = Instant::now();
    let request = read_request(&mut stream).await?;
    let method = request.method.clone();
    let path = request.path.clone();
    let response = route_request(request, queue, &config).await;
    log_request(source.ip(), &method, &path, response.status, started);
    stream.write_all(response.to_bytes().as_slice()).await?;
    stream.shutdown().await?;
    Ok(())
}

fn log_request(source: IpAddr, method: &str, path: &str, status: u16, started: Instant) {
    println!(
        "source={source} method={method} path={path} status={status} duration_ms={}",
        started.elapsed().as_millis()
    );
}

/// Route a parsed request to the agent API. Three endpoints are exposed:
/// - `GET /health` — unauthenticated liveness check.
/// - `GET /capabilities` — dispatches the `agent.capabilities` command and
///   returns the result; lets the dashboard discover modules without a token.
/// - `POST /dispatch` — accepts a full [`AgentCommand`] envelope as JSON and
///   returns the [`AgentResponse`].
///
/// `OPTIONS` is answered with a 204 for CORS preflight, and every response
/// carries permissive `Access-Control-Allow-*` headers (see [`HttpResponse::to_bytes`])
/// so a browser test UI on a different origin can call the API.
async fn route_request(
    request: HttpRequest,
    queue: DispatchQueue,
    config: &HttpAgentConfig,
) -> HttpResponse {
    if request.method == "OPTIONS" {
        return HttpResponse::empty(204);
    }

    if let Err(error) = authorize(&request, config) {
        return json_response(401, json!({ "error": error.to_string() }));
    }

    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/health") => json_response(200, json!({ "ok": true })),
        ("GET", "/capabilities") => {
            // Synthesize a capabilities command so the dashboard can hit a
            // plain GET without constructing an envelope. The command id is
            // fixed because the response id is irrelevant for a GET.
            let command = AgentCommand {
                id: "ui-capabilities".into(),
                module: "agent".into(),
                action: "capabilities".into(),
                payload: json!({}),
                signature: None,
            };
            dispatch_json(&queue, command).await
        }
        ("POST", "/dispatch") => match serde_json::from_slice::<AgentCommand>(&request.body) {
            Ok(command) => dispatch_json(&queue, command).await,
            Err(error) => json_response(
                400,
                json!({ "ok": false, "error": format!("invalid command JSON: {error}") }),
            ),
        },
        _ => json_response(404, json!({ "ok": false, "error": "not found" })),
    }
}

async fn dispatch_json(queue: &DispatchQueue, command: AgentCommand) -> HttpResponse {
    match queue.dispatch(command).await {
        Ok(response) => json_response(200, response),
        Err(QueueError::Full) => json_response(
            429,
            json!({ "ok": false, "error": "Tetra command queue is full; retry after backoff" }),
        ),
        Err(QueueError::Closed) => json_response(
            503,
            json!({ "ok": false, "error": "Tetra command queue is unavailable" }),
        ),
    }
}

/// Validate the bearer token if one is configured. When no token is set the
/// API is open — appropriate for localhost dev but *not* for a routable
/// address. The README warns about this; the dashboard's register-host flow
/// encourages setting a token for anything beyond 127.0.0.1.
fn authorize(request: &HttpRequest, config: &HttpAgentConfig) -> Result<()> {
    let Some(expected) = config.bearer_token.as_deref() else {
        return Ok(());
    };

    let Some(actual) = request.headers.get("authorization") else {
        bail!("missing bearer token");
    };

    if actual != &format!("Bearer {expected}") {
        bail!("invalid bearer token");
    }

    Ok(())
}

/// Read one HTTP/1.1 request from `stream` into an [`HttpRequest`].
///
/// This is a deliberately small hand-rolled parser — the dev HTTP API doesn't
/// need chunked transfer encoding, keep-alive, or HTTP/2, and pulling in a
/// full HTTP framework would dwarf the rest of the agent. Headers are read
/// incrementally until the `\r\n\r\n` terminator, then the body is read to the
/// declared `Content-Length`. Both are capped by [`MAX_HEADER_BYTES`] and
/// [`MAX_BODY_BYTES`] respectively to bound memory use against a hostile peer.
async fn read_request(stream: &mut TcpStream) -> Result<HttpRequest> {
    let mut buffer = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 1024];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            bail!("connection closed before request headers completed");
        }

        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_HEADER_BYTES {
            bail!("request headers exceed {MAX_HEADER_BYTES} bytes");
        }

        if let Some(index) = find_header_end(&buffer) {
            break index;
        }
    };

    let header_text = std::str::from_utf8(&buffer[..header_end])
        .context("request headers are not valid UTF-8")?;
    let (request_line, headers) = parse_headers(header_text)?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow!("missing HTTP method"))?
        .to_string();
    let target = parts
        .next()
        .ok_or_else(|| anyhow!("missing HTTP request target"))?;
    let path = target.split('?').next().unwrap_or(target).to_string();

    let content_length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("invalid Content-Length header")?
        .unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        bail!("request body exceeds {MAX_BODY_BYTES} bytes");
    }

    let body_start = header_end + 4;
    let mut body = buffer[body_start..].to_vec();
    while body.len() < content_length {
        let mut chunk = vec![0_u8; content_length - body.len()];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            bail!("connection closed before request body completed");
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);

    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Parse a header block (without the terminating blank line) into a request
/// line and a lowercased-name → value map. Header names are lowercased so the
/// `authorization`/`content-length` lookups above are case-insensitive per
/// RFC 7230.
fn parse_headers(header_text: &str) -> Result<(String, BTreeMap<String, String>)> {
    let mut lines = header_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow!("missing HTTP request line"))?
        .to_string();
    let mut headers = BTreeMap::new();

    for line in lines {
        if line.is_empty() {
            continue;
        }

        let Some((name, value)) = line.split_once(':') else {
            bail!("invalid HTTP header line `{line}`");
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }

    Ok((request_line, headers))
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl HttpResponse {
    fn empty(status: u16) -> Self {
        Self {
            status,
            content_type: "text/plain",
            body: Vec::new(),
        }
    }

    /// Serialize the response as a complete HTTP/1.1 message with `Connection:
    /// close`. The permissive CORS headers are added to every response so the
    /// browser test UI can call the dev API from a different origin.
    fn to_bytes(&self) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            204 => "No Content",
            400 => "Bad Request",
            401 => "Unauthorized",
            404 => "Not Found",
            500 => "Internal Server Error",
            _ => "OK",
        };
        let headers = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: Authorization, Content-Type\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nConnection: close\r\n\r\n",
            self.status,
            reason,
            self.content_type,
            self.body.len()
        );

        let mut bytes = headers.into_bytes();
        bytes.extend_from_slice(&self.body);
        bytes
    }
}

fn json_response(status: u16, value: impl serde::Serialize) -> HttpResponse {
    let body = serde_json::to_vec_pretty(&value).unwrap_or_else(|error| {
        format!(r#"{{"error":"failed to serialize response: {error}"}}"#).into_bytes()
    });
    HttpResponse {
        status,
        content_type: "application/json",
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_headers_case_insensitively() {
        let (request_line, headers) = parse_headers(
            "POST /dispatch HTTP/1.1\r\nContent-Length: 2\r\nAuthorization: Bearer test",
        )
        .unwrap();

        assert_eq!(request_line, "POST /dispatch HTTP/1.1");
        assert_eq!(headers["content-length"], "2");
        assert_eq!(headers["authorization"], "Bearer test");
    }

    #[test]
    fn finds_header_terminator() {
        assert_eq!(find_header_end(b"GET / HTTP/1.1\r\n\r\nbody"), Some(14));
    }
}
