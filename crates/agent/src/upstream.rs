//! Sends requests to the real servers: HTTP/1.1 through ureq, and native
//! gRPC over HTTP/2 (h2) so trailers can be read.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use apicraft_core::http::{HttpRequest, HttpResponse};
use apicraft_core::url::Url;

const MAX_BODY: u64 = 256 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Tries IPv4 addresses before IPv6. Connections are attempted one address
/// at a time, so an unreachable IPv6 route would otherwise stall every
/// request until the OS connect timeout.
#[derive(Debug, Default)]
struct Ipv4First(ureq::unversioned::resolver::DefaultResolver);

impl ureq::unversioned::resolver::Resolver for Ipv4First {
    fn resolve(
        &self,
        uri: &http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let mut addrs = self.0.resolve(uri, config, timeout)?;
        addrs.sort_by_key(|a| a.is_ipv6());
        Ok(addrs)
    }
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .allow_non_standard_methods(true)
            .max_redirects(10)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(Duration::from_secs(120)))
            .user_agent("FizlApiCraft/0.1")
            .build();
        ureq::Agent::with_parts(config, ureq::unversioned::transport::DefaultConnector::default(), Ipv4First::default())
    })
}

/// Connects to the first reachable address, IPv4 first.
async fn connect_tcp(host: &str, port: u16) -> Result<tokio::net::TcpStream, String> {
    let mut addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port)).await.map_err(|e| format!("resolve {host}: {e}"))?.collect();
    addrs.sort_by_key(|a| a.is_ipv6());
    let mut last = format!("no addresses for {host}");
    for addr in addrs {
        match tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr)).await {
            Ok(Ok(s)) => return Ok(s),
            Ok(Err(e)) => last = format!("connect {addr}: {e}"),
            Err(_) => last = format!("connect {addr}: timed out"),
        }
    }
    Err(last)
}

pub fn send(req: HttpRequest) -> Result<HttpResponse, String> {
    if req.grpc {
        return send_grpc(req);
    }
    let started = Instant::now();
    let mut builder = http::Request::builder().method(req.method.as_str()).uri(req.url.as_str());
    for (k, v) in &req.headers {
        if k.eq_ignore_ascii_case("host") || k.eq_ignore_ascii_case("content-length") {
            continue;
        }
        builder = builder.header(k.as_str(), v.as_str());
    }
    let follow = !req.no_redirects;
    // No body at all (rather than `Content-Length: 0`) when there's nothing to send.
    let resp = if req.body.is_empty() {
        let r = builder.body(ureq::SendBody::none()).map_err(|e| format!("invalid request: {e}"))?;
        agent().run(redirects(crate::connect::configure(agent(), r, &req.connect)?, follow))
    } else {
        let r = builder.body(req.body).map_err(|e| format!("invalid request: {e}"))?;
        agent().run(redirects(crate::connect::configure(agent(), r, &req.connect)?, follow))
    }
    .map_err(|e| format!("{e}"))?;
    let status = resp.status();
    let headers = header_pairs(resp.headers());
    let body = resp.into_body().into_with_config().limit(MAX_BODY).read_to_vec().map_err(|e| format!("reading response: {e}"))?;
    Ok(HttpResponse {
        status: status.as_u16(),
        status_text: status.canonical_reason().unwrap_or("").to_string(),
        headers,
        body,
        trailers: Vec::new(),
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}

fn redirects<S: ureq::AsSendBody>(req: http::Request<S>, follow: bool) -> http::Request<S> {
    if follow {
        req
    } else {
        agent().configure_request(req).max_redirects(0).build()
    }
}

fn header_pairs(h: &http::HeaderMap) -> Vec<(String, String)> {
    h.iter().map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned())).collect()
}

fn send_grpc(req: HttpRequest) -> Result<HttpResponse, String> {
    // A current-thread runtime per call keeps this simple; gRPC calls are infrequent.
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let timeout = Duration::from_millis(if req.timeout_ms == 0 { 60_000 } else { req.timeout_ms as u64 });
        tokio::time::timeout(timeout, grpc_call(req)).await.map_err(|_| "gRPC call timed out".to_string())?
    })
}

async fn grpc_call(req: HttpRequest) -> Result<HttpResponse, String> {
    let started = Instant::now();
    let u = Url::parse(&req.url)?;
    let tcp = connect_tcp(u.host.trim_matches(['[', ']']), u.port_or_default()).await?;
    let _ = tcp.set_nodelay(true);

    let scheme = if u.is_tls() { "https" } else { "http" };
    let uri = format!("{scheme}://{}{}", u.host_header(), u.path_and_query());
    let mut builder = http::Request::builder().method("POST").uri(uri.as_str());
    for (k, v) in &req.headers {
        let lk = k.to_ascii_lowercase();
        if matches!(lk.as_str(), "host" | "connection" | "content-length" | "transfer-encoding" | "keep-alive" | "upgrade") {
            continue;
        }
        builder = builder.header(lk.as_str(), v.as_str());
    }
    let request = builder.body(()).map_err(|e| format!("invalid request: {e}"))?;
    let body = bytes::Bytes::from(req.body);

    let (status, headers, data, trailers) = if u.is_tls() {
        let mut config = crate::connect::rustls_config(&req.connect)?;
        config.alpn_protocols = vec![b"h2".to_vec()];
        let name = rustls::pki_types::ServerName::try_from(u.host.trim_matches(['[', ']']).to_string()).map_err(|e| format!("bad TLS name: {e}"))?;
        let tls = tokio_rustls::TlsConnector::from(Arc::new(config)).connect(name, tcp).await.map_err(|e| format!("TLS: {e}"))?;
        exchange(tls, request, body).await?
    } else {
        exchange(tcp, request, body).await?
    };
    Ok(HttpResponse {
        status,
        status_text: http::StatusCode::from_u16(status).ok().and_then(|s| s.canonical_reason()).unwrap_or("").to_string(),
        headers,
        body: data,
        trailers,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}

type Exchange = (u16, Vec<(String, String)>, Vec<u8>, Vec<(String, String)>);

async fn exchange<T>(io: T, request: http::Request<()>, body: bytes::Bytes) -> Result<Exchange, String>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (client, connection) = h2::client::handshake(io).await.map_err(|e| format!("HTTP/2 handshake: {e}"))?;
    let conn_task = tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut client = client.ready().await.map_err(|e| format!("HTTP/2: {e}"))?;
    let (response, mut stream) = client.send_request(request, false).map_err(|e| format!("HTTP/2 send: {e}"))?;
    stream.send_data(body, true).map_err(|e| format!("HTTP/2 send body: {e}"))?;
    let response = response.await.map_err(|e| format!("HTTP/2 response: {e}"))?;
    let (parts, mut recv) = response.into_parts();
    let mut data = Vec::new();
    while let Some(chunk) = recv.data().await {
        let chunk = chunk.map_err(|e| format!("HTTP/2 body: {e}"))?;
        let _ = recv.flow_control().release_capacity(chunk.len());
        data.extend_from_slice(&chunk);
        if data.len() as u64 > MAX_BODY {
            return Err("response too large".into());
        }
    }
    let trailers = recv.trailers().await.map_err(|e| format!("HTTP/2 trailers: {e}"))?.map(|t| header_pairs(&t)).unwrap_or_default();
    conn_task.abort();
    Ok((parts.status.as_u16(), header_pairs(&parts.headers), data, trailers))
}
