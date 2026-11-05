//! A small HTTP/1.1 server on std::net: serves the web app and the agent API.
//!
//! The API (`/__apicraft/*`) only answers this machine's own page. It checks
//! the Host header (against DNS rebinding), the Origin header, and a custom
//! `X-ApiCraft-Agent` header that other sites can't send without a CORS
//! preflight, which is refused unless the origin was allowed with
//! `--allow-origin`.
//!
//! It also serves Fizl ApiCraft's MCP server: Streamable HTTP at `/mcp`, and
//! the legacy SSE transport at `/sse` (events) + `/messages` (posts). Those
//! endpoints check Host and Origin but not the custom header, so standard
//! MCP clients can use them.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use apicraft_core::http::{HttpRequest, ProxyError};

const MAX_HEADER: usize = 64 * 1024;
const MAX_BODY: usize = 300 * 1024 * 1024;

pub struct Config {
    pub port: u16,
    pub web_dir: Option<PathBuf>,
    pub data_file: PathBuf,
    /// Extra origins (e.g. a hosted copy of the web app) allowed to use the API.
    pub allow_origins: Vec<String>,
}

impl Config {
    fn own_origins(&self) -> Vec<String> {
        vec![format!("http://127.0.0.1:{}", self.port), format!("http://localhost:{}", self.port)]
    }
}

struct Req {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

struct Resp {
    status: u16,
    content_type: &'static str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Resp {
    fn new(status: u16, content_type: &'static str, body: impl Into<Vec<u8>>) -> Resp {
        Resp { status, content_type, headers: Vec::new(), body: body.into() }
    }
    fn json(status: u16, value: &impl serde::Serialize) -> Resp {
        Resp::new(status, "application/json", serde_json::to_vec(value).unwrap_or_default())
    }
    fn text(status: u16, msg: &str) -> Resp {
        Resp::new(status, "text/plain; charset=utf-8", msg.as_bytes().to_vec())
    }
}

pub fn serve(cfg: Config) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", cfg.port))?;
    let cfg = std::sync::Arc::new(cfg);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let cfg = cfg.clone();
        std::thread::spawn(move || {
            let _ = handle(stream, &cfg);
        });
    }
    Ok(())
}

fn handle(mut stream: TcpStream, cfg: &Config) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let resp = match read_request(&stream) {
        Ok(Some(req)) if req.method == "GET" && req.path == "/sse" && host_ok(&req) && check_origin(&req, cfg).is_ok() => {
            return stream_sse(stream, cfg);
        }
        Ok(Some(req)) => route(&req, cfg),
        Ok(None) => return Ok(()),
        Err(msg) => Resp::text(400, &msg),
    };
    write_response(&mut stream, resp)
}

fn read_request(stream: &TcpStream) -> Result<Option<Req>, String> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or("bad request line")?.to_string();
    let target = parts.next().ok_or("bad request line")?.to_string();
    let mut headers = Vec::new();
    let mut total = line.len();
    loop {
        let mut h = String::new();
        let n = reader.read_line(&mut h).map_err(|e| e.to_string())?;
        total += n;
        if total > MAX_HEADER {
            return Err("headers too large".into());
        }
        let h = h.trim_end();
        if n == 0 || h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let len: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    if len > MAX_BODY {
        return Err("body too large".into());
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    let (path, query) = target.split_once('?').map(|(p, q)| (p.to_string(), q.to_string())).unwrap_or((target, String::new()));
    Ok(Some(Req { method, path, query, headers, body }))
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "",
    }
}

fn write_response(stream: &mut TcpStream, r: Resp) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        r.status,
        reason(r.status),
        r.content_type,
        r.body.len()
    );
    for (k, v) in &r.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&r.body)?;
    stream.flush()
}

fn host_ok(req: &Req) -> bool {
    req.header("host")
        .map(|h| {
            let host = h.rsplit_once(':').map(|(h, _)| h).unwrap_or(h);
            matches!(host, "127.0.0.1" | "localhost" | "[::1]")
        })
        .unwrap_or(false)
}

/// `Some(origin)` for an allowed cross-origin caller, `None` for same-origin.
fn check_origin(req: &Req, cfg: &Config) -> Result<Option<String>, &'static str> {
    match req.header("origin") {
        None => Ok(None),
        Some(o) if cfg.own_origins().iter().any(|x| x == o) => Ok(None),
        Some(o) if cfg.allow_origins.iter().any(|x| x == o) => Ok(Some(o.to_string())),
        Some(_) => Err("origin not allowed (start the agent with --allow-origin to permit it)"),
    }
}

fn cors(resp: &mut Resp, origin: &Option<String>) {
    if let Some(o) = origin {
        resp.headers.push(("Access-Control-Allow-Origin".into(), o.clone()));
        resp.headers.push(("Vary".into(), "Origin".into()));
    }
}

/// The catalog file for the browser app. A file that can't be read is an
/// error, not a missing file: the app would save over a missing one.
fn read_catalog(path: &Path) -> Resp {
    match crate::folders::load(path) {
        Ok((cat, _)) => Resp::new(200, "application/gzip", apicraft_core::store::to_gzip(&cat)),
        Err(_) if !path.exists() => Resp::text(404, "no catalog file yet"),
        Err(e) => Resp::text(500, &format!("could not read {e}")),
    }
}

fn route(req: &Req, cfg: &Config) -> Resp {
    if !host_ok(req) {
        return Resp::text(403, "unexpected Host header");
    }
    if req.path == "/mcp" || req.path == "/messages" || req.path == "/sse" {
        let origin = match check_origin(req, cfg) {
            Ok(o) => o,
            Err(msg) => return Resp::text(403, msg),
        };
        let mut resp = if req.path == "/mcp" { mcp_http(req, cfg) } else { sse_post(req) };
        cors(&mut resp, &origin);
        return resp;
    }
    if let Some(api) = req.path.strip_prefix("/__apicraft/") {
        let origin = match check_origin(req, cfg) {
            Ok(o) => o,
            Err(msg) => return Resp::text(403, msg),
        };
        if req.method == "OPTIONS" {
            let mut r = Resp::new(204, "text/plain", Vec::new());
            cors(&mut r, &origin);
            r.headers.push(("Access-Control-Allow-Methods".into(), "GET, POST, PUT".into()));
            r.headers.push(("Access-Control-Allow-Headers".into(), "content-type, x-apicraft-agent".into()));
            return r;
        }
        if api != "ping" && req.header("x-apicraft-agent").is_none() {
            return Resp::text(403, "missing X-ApiCraft-Agent header");
        }
        let mut resp = api_route(req, api, cfg, origin.is_some());
        cors(&mut resp, &origin);
        return resp;
    }
    if req.method != "GET" && req.method != "HEAD" {
        return Resp::text(405, "method not allowed");
    }
    let accepts_gzip = req.header("accept-encoding").map(|v| v.contains("gzip")).unwrap_or(false);
    static_file(&req.path, cfg, accepts_gzip)
}

fn api_route(req: &Req, api: &str, cfg: &Config, cross_origin: bool) -> Resp {
    match (req.method.as_str(), api) {
        ("POST", "mcp-session") => {
            #[derive(serde::Deserialize)]
            struct Call {
                launch: apicraft_core::mcp::McpLaunch,
                messages: Vec<serde_json::Value>,
            }
            let call: Call = match serde_json::from_slice(&req.body) {
                Ok(c) => c,
                Err(e) => return Resp::json(400, &ProxyError { error: format!("bad MCP session request: {e}") }),
            };
            // Starting processes is only for the agent's own page, never another origin.
            if cross_origin && matches!(call.launch, apicraft_core::mcp::McpLaunch::Stdio { .. }) {
                return Resp::json(403, &ProxyError { error: "stdio MCP servers can only be started from the agent's own page".into() });
            }
            match crate::mcp_client::session(call.launch, call.messages) {
                Ok(ex) => Resp::json(200, &ex),
                Err(e) => Resp::json(502, &ProxyError { error: e }),
            }
        }
        ("GET", "ping") => Resp::json(
            200,
            &serde_json::json!({
                "agent": "fizl-apicraft-agent",
                "version": env!("CARGO_PKG_VERSION"),
                "grpc": true,
                "catalogFile": cfg.data_file.display().to_string(),
            }),
        ),
        ("POST", "proxy") => {
            let call: HttpRequest = match serde_json::from_slice(&req.body) {
                Ok(c) => c,
                Err(e) => return Resp::json(400, &ProxyError { error: format!("bad proxy request: {e}") }),
            };
            match crate::upstream::send(call) {
                Ok(r) => Resp::json(200, &r),
                Err(e) => Resp::json(502, &ProxyError { error: e }),
            }
        }
        ("GET", "catalog") => read_catalog(&cfg.data_file),
        ("PUT", "catalog") => {
            let cat = match apicraft_core::store::decode(&req.body) {
                Ok(c) => c.into_catalog(),
                Err(_) => return Resp::text(400, "not a catalog"),
            };
            match crate::folders::save(&cfg.data_file, &cat) {
                Ok(()) => Resp::json(200, &serde_json::json!({ "saved": cfg.data_file.display().to_string() })),
                Err(e) => Resp::text(500, &format!("could not write {e}")),
            }
        }
        // A collection folder for the browser app: {"path", "link": bool}.
        ("POST", "folder") => {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let path = std::path::PathBuf::from(body["path"].as_str().unwrap_or_default());
            if body["link"].as_bool().unwrap_or(false) {
                return match crate::folders::can_link(&path) {
                    Ok(()) => Resp::json(200, &serde_json::json!({ "collection": apicraft_core::model::Collection::new(""), "workflows": [], "warnings": [] })),
                    Err(e) => Resp::text(400, &e),
                };
            }
            match crate::folders::read_collection(&path) {
                Ok((collection, workflows, warnings)) => {
                    Resp::json(200, &serde_json::json!({ "collection": collection, "workflows": workflows, "warnings": warnings }))
                }
                Err(e) => Resp::text(400, &e),
            }
        }
        // Commands for environment values, and git for collection folders:
        // they run programs, so only for the agent's own page.
        ("POST", "command") | ("POST", "git") if cross_origin => {
            Resp::json(403, &ProxyError { error: "commands can only be run from the agent's own page".into() })
        }
        ("POST", "command") => match serde_json::from_slice::<apicraft_core::sources::CommandSpec>(&req.body) {
            Ok(spec) => match crate::process::run(&spec) {
                Ok(out) => Resp::json(200, &out),
                Err(e) => Resp::json(502, &ProxyError { error: e }),
            },
            Err(e) => Resp::json(400, &ProxyError { error: format!("bad command request: {e}") }),
        },
        ("POST", "git") => match serde_json::from_slice::<apicraft_core::git::GitRequest>(&req.body) {
            Ok(g) => match crate::git::run(&g) {
                Ok(reply) => Resp::json(200, &reply),
                Err(e) => Resp::json(502, &ProxyError { error: e }),
            },
            Err(e) => Resp::json(400, &ProxyError { error: format!("bad git request: {e}") }),
        },
        _ => Resp::text(404, "unknown agent endpoint"),
    }
}

// ---------------------------------------------------------------- MCP server endpoints

fn mcp_http(req: &Req, cfg: &Config) -> Resp {
    match req.method.as_str() {
        "POST" => {}
        "DELETE" => return Resp::new(200, "text/plain", Vec::new()),
        "OPTIONS" => {
            let mut r = Resp::new(204, "text/plain", Vec::new());
            r.headers.push(("Access-Control-Allow-Methods".into(), "POST, DELETE".into()));
            r.headers.push(("Access-Control-Allow-Headers".into(), "content-type, mcp-session-id, mcp-protocol-version, authorization".into()));
            return r;
        }
        _ => {
            let mut r = Resp::text(405, "this server doesn't open a stream on GET; POST JSON-RPC messages");
            r.headers.push(("Allow".into(), "POST, DELETE".into()));
            return r;
        }
    }
    let server = crate::mcp_server::McpServer::new(cfg.data_file.clone());
    let msg: serde_json::Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(e) => {
            return Resp::json(400, &serde_json::json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } }))
        }
    };
    let msgs: Vec<serde_json::Value> = match msg {
        serde_json::Value::Array(a) => a,
        m => vec![m],
    };
    let initialize = msgs.iter().any(|m| m.get("method").and_then(|x| x.as_str()) == Some("initialize"));
    let replies: Vec<serde_json::Value> = msgs.iter().filter_map(|m| server.handle(m)).collect();
    let mut resp = match replies.len() {
        0 => Resp::new(202, "text/plain", Vec::new()),
        1 if msgs.len() == 1 => Resp::json(200, &replies[0]),
        _ => Resp::json(200, &replies),
    };
    if initialize {
        resp.headers.push(("Mcp-Session-Id".into(), apicraft_core::ids::new_id()));
    } else if let Some(s) = req.header("mcp-session-id") {
        resp.headers.push(("Mcp-Session-Id".into(), s.to_string()));
    }
    resp
}

type SseSessions = std::sync::Mutex<std::collections::HashMap<String, (std::sync::mpsc::Sender<String>, PathBuf)>>;

fn sse_sessions() -> &'static SseSessions {
    static S: std::sync::OnceLock<SseSessions> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

/// Legacy SSE transport: hold the stream open, name the POST endpoint, then
/// send each response as a `message` event.
fn stream_sse(mut stream: TcpStream, cfg: &Config) -> std::io::Result<()> {
    let id = apicraft_core::ids::new_id();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    if let Ok(mut s) = sse_sessions().lock() {
        s.insert(id.clone(), (tx, cfg.data_file.clone()));
    }
    let result = (|| -> std::io::Result<()> {
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n")?;
        stream.write_all(format!("event: endpoint\ndata: /messages?sessionId={id}\n\n").as_bytes())?;
        stream.flush()?;
        loop {
            match rx.recv_timeout(Duration::from_secs(15)) {
                Ok(msg) => stream.write_all(format!("event: message\ndata: {msg}\n\n").as_bytes())?,
                // Keep-alive comment; a write error means the client went away.
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => stream.write_all(b": ping\n\n")?,
                Err(_) => return Ok(()),
            }
            stream.flush()?;
        }
    })();
    if let Ok(mut s) = sse_sessions().lock() {
        s.remove(&id);
    }
    result
}

fn sse_post(req: &Req) -> Resp {
    if req.method != "POST" {
        return Resp::text(405, "POST messages here; open the event stream with GET /sse");
    }
    let Some(session) = apicraft_core::url::parse_query(&req.query).into_iter().find(|(k, _)| k == "sessionId").map(|(_, v)| v) else {
        return Resp::text(400, "missing sessionId");
    };
    let Some((tx, data_file)) = sse_sessions().lock().ok().and_then(|s| s.get(&session).cloned()) else {
        return Resp::text(404, "unknown or closed session");
    };
    let msg: serde_json::Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(e) => return Resp::text(400, &format!("bad JSON: {e}")),
    };
    // Answer on the stream, off this thread, so slow tools don't hold the POST.
    std::thread::spawn(move || {
        let server = crate::mcp_server::McpServer::new(data_file);
        if let Some(reply) = server.handle(&msg) {
            let _ = tx.send(reply.to_string());
        }
    });
    Resp::new(202, "text/plain", b"Accepted".to_vec())
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "json" | "webmanifest" => "application/json",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Gzip of a static file, cached by path and modification time (the wasm
/// bundle is large, and compressing it on every load would be slow).
fn gzipped(path: &Path, bytes: &[u8]) -> Vec<u8> {
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::{Mutex, OnceLock};
    type Cache = Mutex<HashMap<PathBuf, (std::time::SystemTime, Vec<u8>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
    let cache = CACHE.get_or_init(Default::default);
    if let Some((m, gz)) = cache.lock().ok().and_then(|c| c.get(path).cloned()) {
        if m == modified {
            return gz;
        }
    }
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    let _ = enc.write_all(bytes);
    let gz = enc.finish().unwrap_or_default();
    if let Ok(mut c) = cache.lock() {
        c.insert(path.to_path_buf(), (modified, gz.clone()));
    }
    gz
}

fn static_file(url_path: &str, cfg: &Config, accepts_gzip: bool) -> Resp {
    let Some(root) = &cfg.web_dir else {
        return Resp::text(404, "web app not found: build it (see README) or pass --web <dir>");
    };
    let decoded = apicraft_core::prepare::percent_decode(&url_path.replace('+', "%2B"));
    let rel = decoded.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let rel_path = Path::new(rel);
    if rel_path.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Resp::text(403, "bad path");
    }
    let mut full = root.join(rel_path);
    if full.is_dir() {
        full = full.join("index.html");
    }
    match std::fs::read(&full) {
        Ok(bytes) => {
            let compressible = matches!(full.extension().and_then(|e| e.to_str()), Some("wasm" | "js" | "css" | "html" | "json" | "svg"));
            let (bytes, gz) = if accepts_gzip && compressible && bytes.len() > 1024 { (gzipped(&full, &bytes), true) } else { (bytes, false) };
            let mut r = Resp::new(200, mime(&full), bytes);
            if gz {
                r.headers.push(("Content-Encoding".into(), "gzip".into()));
                r.headers.push(("Vary".into(), "Accept-Encoding".into()));
            }
            r.headers.push(("Content-Security-Policy".into(), "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src * http: https: ws: wss:; frame-ancestors 'none'".into()));
            r
        }
        Err(_) => Resp::text(404, "not found"),
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn an_unreadable_catalog_is_not_reported_as_missing() {
        let dir = std::env::temp_dir().join(format!("apicraft-server-{}", apicraft_core::ids::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("catalog.json.gz");
        assert_eq!(read_catalog(&file).status, 404);
        std::fs::write(&file, b"not a catalog").unwrap();
        assert_eq!(read_catalog(&file).status, 500);
        std::fs::write(&file, apicraft_core::store::to_gzip(&Default::default())).unwrap();
        assert_eq!(read_catalog(&file).status, 200);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
