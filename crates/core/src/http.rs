//! Wire-level request/response types and the [`Transport`] the engine sends
//! through. The same types are the JSON protocol between the web app and the
//! local agent's `/__apicraft/proxy` endpoint (bodies travel as base64).

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default, with = "b64")]
    pub body: Vec<u8>,
    /// Native gRPC (HTTP/2 with trailers). Only the agent can do this.
    #[serde(default)]
    pub grpc: bool,
    #[serde(default)]
    pub timeout_ms: u32,
    /// Proxy, certificate checks and client certificate. Only native
    /// senders (the desktop app and the agent) can honour these.
    #[serde(default, skip_serializing_if = "ConnectOptions::is_default")]
    pub connect: ConnectOptions,
    /// Return a redirect instead of following it, to read the cookies it
    /// sets. Only native senders (the desktop app and the agent) can do this.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_redirects: bool,
}

/// How to connect: see [`crate::model::NetworkSettings`]. Certificates and
/// keys are PEM text or the path of a PEM file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectOptions {
    /// `None`: the system proxy (environment variables). `Some("")`: no
    /// proxy. Otherwise the proxy URL, credentials included.
    pub proxy: Option<String>,
    pub no_proxy: String,
    /// Accept any server certificate.
    pub insecure: bool,
    /// CA certificates that replace the built-in roots.
    pub ca: String,
    pub client_cert: String,
    pub client_key: String,
}

impl ConnectOptions {
    pub fn is_default(&self) -> bool {
        *self == ConnectOptions::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    #[serde(default)]
    pub status_text: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default, with = "b64")]
    pub body: Vec<u8>,
    #[serde(default)]
    pub trailers: Vec<(String, String)>,
    #[serde(default)]
    pub elapsed_ms: f64,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().chain(self.trailers.iter()).find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    pub fn content_type(&self) -> &str {
        self.header("content-type").unwrap_or("")
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// What the agent returns when it could not reach the upstream at all.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyError {
    pub error: String,
}

pub type LocalBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Sends a request somewhere: `fetch` in the browser, the agent proxy, or a
/// native HTTP client.
pub trait Transport {
    fn send(&self, req: HttpRequest) -> LocalBoxFuture<'_, Result<HttpResponse, String>>;
    /// True when gRPC can go over real HTTP/2 (otherwise gRPC-Web is used).
    fn native_grpc(&self) -> bool;
    /// Waits, for workflow step delays.
    fn sleep(&self, ms: u32) -> LocalBoxFuture<'_, ()>;
    /// Unix time in ms.
    fn now_ms(&self) -> u64;
    /// False for senders that ignore [`ConnectOptions`] (the browser's fetch).
    fn honours_connect_options(&self) -> bool {
        true
    }

    /// Runs an MCP session over stdio or the legacy SSE transport. Only hosts
    /// that can start processes or hold streams open (the desktop app, the
    /// agent) provide this; Streamable HTTP goes through [`Transport::send`].
    fn mcp_session(&self, launch: crate::mcp::McpLaunch, messages: Vec<serde_json::Value>) -> LocalBoxFuture<'_, Result<crate::mcp::McpExchange, String>> {
        let _ = (launch, messages);
        Box::pin(async { Err("stdio and SSE MCP servers need the desktop app or the local agent (Streamable HTTP works everywhere)".to_string()) })
    }

    /// Runs a command on the host, for variables whose values come from a
    /// command (see [`crate::sources`]). Only the desktop app and the local
    /// agent can.
    fn run_command(&self, spec: crate::sources::CommandSpec) -> LocalBoxFuture<'_, Result<crate::sources::CommandOutput, String>> {
        let _ = spec;
        Box::pin(async { Err("values from commands need the desktop app or the local agent".to_string()) })
    }
}

mod b64 {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD.decode(s.as_bytes()).map_err(serde::de::Error::custom)
    }
}
