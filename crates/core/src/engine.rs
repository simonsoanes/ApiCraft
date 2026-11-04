//! Runs one saved request end to end through a [`Transport`]: prepare,
//! obtain/refresh OAuth 2 tokens, sign, send, answer Digest challenges,
//! decode gRPC, then extract variables and run checks.
//!
//! The engine never mutates the catalog. It returns an [`Outcome`], and the
//! caller folds it back in with [`apply_outcome`] (new tokens, captured
//! variables), which is what lets workflows pass values between steps.

use crate::auth::{self, TokenGrant, TokenResult};
use crate::extract::{self, CheckResult, ResponseView};
use crate::form_login::{self, FormLogin};
use crate::grpc::{self, GrpcResult};
use crate::http::{ConnectOptions, HttpRequest, HttpResponse, Transport};
use crate::ids::new_id;
use crate::model::{Auth, Catalog, Check, Collection, Compression, Extraction, GrantType, Item, KeyValue, Protocol, Request, SendOptions, VarScope};
use crate::prepare::{multipart, prepare, Prepared, PreparedBody};
use crate::url::Url;
use crate::vars::Vars;
use crate::{graphql, odata, soap};

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub request_id: String,
    pub request_name: String,
    pub prepared: Option<Prepared>,
    /// What went on the wire, after auth (the last attempt, for Digest).
    pub sent: Option<HttpRequest>,
    pub response: Option<HttpResponse>,
    /// Body for display: gRPC messages as JSON, otherwise the text.
    pub body_text: String,
    pub grpc: Option<GrpcResult>,
    /// A SOAP fault, GraphQL `errors` or an OData error message, for display.
    pub fault: Option<String>,
    pub extracted: Vec<(VarScope, String, String)>,
    pub extract_errors: Vec<String>,
    pub checks: Vec<CheckResult>,
    /// Things worth knowing that don't fail the request (e.g. sent without a token).
    pub warnings: Vec<String>,
    /// A token obtained during this run, for the auth owner (request or collection id).
    pub token: Option<(String, TokenResult)>,
    pub error: Option<String>,
}

impl Outcome {
    fn failed(request_id: &str, name: &str, error: String) -> Outcome {
        Outcome { request_id: request_id.into(), request_name: name.into(), error: Some(error), ..Default::default() }
    }

    pub fn passed(&self) -> bool {
        self.error.is_none() && self.checks.iter().all(|c| c.passed) && self.grpc.as_ref().map(|g| g.ok()).unwrap_or(true) && self.extract_errors.is_empty()
    }
}

/// Stores what an outcome produced: OAuth tokens and captured variables.
pub fn apply_outcome(catalog: &mut Catalog, o: &Outcome) {
    if let Some((owner, t)) = &o.token {
        catalog.store_oauth_token(owner, &t.access_token, &t.refresh_token, t.expires_at);
    }
    for (scope, k, v) in &o.extracted {
        catalog.set_variable(*scope, k, v);
    }
}

/// The request as it would be sent, without network steps. Used for code
/// generation and as the first step of [`execute`].
pub struct Wire {
    pub prepared: Prepared,
    pub http: HttpRequest,
    /// Auth with variables substituted, before it was applied.
    pub auth: Auth,
    pub auth_owner: String,
    pub vars: Vars,
}

pub fn build_wire(catalog: &Catalog, request_id: &str, workflow_vars: &[KeyValue], now_ms: u64, native_grpc: bool) -> Result<Wire, String> {
    let (col, req) = catalog.find_request(request_id).ok_or("request not found")?;
    let vars = Vars::for_request(catalog, Some(col), workflow_vars, now_ms);
    let prepared = prepare(req, &vars);
    let (auth, owner) = catalog.effective_auth(request_id).unwrap_or((Auth::None, req.id.clone()));
    let auth = auth::resolve(&auth, &vars);

    let mut headers = prepared.headers.clone();
    let body = match (&prepared.body, &prepared.grpc) {
        (_, Some(target)) => {
            let pool = grpc::compile(&target.proto)?;
            let m = grpc::method(&pool, &target.service, &target.method)?;
            let json = match &prepared.body {
                PreparedBody::Text(t) => t.as_str(),
                _ => "{}",
            };
            let msg = grpc::encode_json(&m.input(), json)?;
            headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
            if native_grpc {
                headers.push(("content-type".into(), "application/grpc".into()));
                headers.push(("te".into(), "trailers".into()));
            } else {
                headers.push(("content-type".into(), "application/grpc-web+proto".into()));
                headers.push(("x-grpc-web".into(), "1".into()));
                headers.push(("accept".into(), "application/grpc-web+proto".into()));
            }
            grpc::frame(&msg)
        }
        (PreparedBody::None, _) => Vec::new(),
        (PreparedBody::Text(t), _) => t.clone().into_bytes(),
        (PreparedBody::Multipart(fields), _) => {
            let (ct, bytes) = multipart(fields, &format!("----FizlApiCraft{}", new_id()));
            headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
            headers.push(("Content-Type".into(), ct));
            bytes
        }
    };
    let http = HttpRequest {
        method: prepared.method.clone(),
        url: prepared.url.clone(),
        headers,
        body,
        grpc: prepared.grpc.is_some() && native_grpc,
        timeout_ms: 60_000,
        connect: catalog.network.connect_options(&vars),
        no_redirects: false,
    };
    Ok(Wire { prepared, http, auth, auth_owner: owner, vars })
}

/// Fills in a request's automatic headers at send time: compresses the body
/// (adding `Content-Encoding`), asks for compressed responses, and sets
/// `Content-Length` to the size of the body as it goes out.
pub fn apply_send_options(o: &SendOptions, http: &mut HttpRequest) -> Result<(), String> {
    fn set(http: &mut HttpRequest, name: &str, value: String) {
        http.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        http.headers.push((name.into(), value));
    }
    if o.compress != Compression::None && !http.body.is_empty() {
        http.body = compress(&http.body, o.compress)?;
        set(http, "Content-Encoding", o.compress.label().into());
    }
    if o.accept_compressed && !http.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("accept-encoding")) {
        http.headers.push(("Accept-Encoding".into(), "gzip, deflate".into()));
    }
    if o.content_length {
        set(http, "Content-Length", http.body.len().to_string());
    }
    Ok(())
}

pub fn compress(bytes: &[u8], how: Compression) -> Result<Vec<u8>, String> {
    use std::io::Write;
    let level = flate2::Compression::default();
    let r = match how {
        Compression::None => return Ok(bytes.to_vec()),
        Compression::Gzip => {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), level);
            e.write_all(bytes).and_then(|_| e.finish())
        }
        Compression::Deflate => {
            // HTTP's "deflate" is the zlib format.
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), level);
            e.write_all(bytes).and_then(|_| e.finish())
        }
    };
    r.map_err(|e| format!("compressing the body: {e}"))
}

/// Unpacks a gzip or deflate response body that the sender left compressed.
/// Bodies that don't unpack are left as they are.
pub fn inflate_response(resp: &mut HttpResponse) {
    use std::io::Read;
    let Some(enc) = resp.header("content-encoding").map(|e| e.trim().to_ascii_lowercase()) else { return };
    let mut out = Vec::new();
    let ok = match enc.as_str() {
        "gzip" | "x-gzip" if resp.body.starts_with(&[0x1f, 0x8b]) => flate2::read::MultiGzDecoder::new(&resp.body[..]).read_to_end(&mut out).is_ok(),
        "deflate" => {
            flate2::read::ZlibDecoder::new(&resp.body[..]).read_to_end(&mut out).is_ok() || {
                out.clear();
                flate2::read::DeflateDecoder::new(&resp.body[..]).read_to_end(&mut out).is_ok()
            }
        }
        _ => false,
    };
    if ok {
        resp.body = out;
    }
}

/// Makes sure an OAuth 2 auth holds a usable token, fetching one if it can.
/// Token requests go out with `connect` (the proxy and certificates).
pub async fn ensure_oauth_token(o: &mut crate::model::OAuth2, connect: &ConnectOptions, transport: &dyn Transport) -> Result<Option<TokenResult>, String> {
    let now = transport.now_ms();
    if o.token_valid(now) {
        return Ok(None);
    }
    let mut last_err = None;
    if !o.refresh_token.is_empty() && !o.token_url.is_empty() {
        let r = transport.send(with_connect(auth::token_request(o, TokenGrant::Refresh)?, connect)).await?;
        match auth::parse_token_response(&r, transport.now_ms()) {
            Ok(t) => {
                o.access_token = t.access_token.clone();
                return Ok(Some(t));
            }
            Err(e) => last_err = Some(format!("refresh failed: {e}")),
        }
    }
    match o.grant {
        GrantType::ClientCredentials | GrantType::Password => {
            let r = transport.send(with_connect(auth::token_request(o, TokenGrant::Configured)?, connect)).await?;
            let t = auth::parse_token_response(&r, transport.now_ms())?;
            o.access_token = t.access_token.clone();
            Ok(Some(t))
        }
        // An expired token may still work for servers that don't say; try it.
        _ if !o.access_token.is_empty() => Ok(None),
        GrantType::AuthorizationCode => {
            Err(last_err.unwrap_or_else(|| "OAuth 2: no token yet \u{2014} use \u{201c}Get new token\u{201d} in the Auth tab".into()))
        }
        GrantType::Manual => Err(last_err.unwrap_or_else(|| "OAuth 2: paste an access token in the Auth tab".into())),
    }
}

pub async fn execute(
    catalog: &Catalog,
    request_id: &str,
    workflow_vars: &[KeyValue],
    extra_extract: &[Extraction],
    extra_checks: &[Check],
    transport: &dyn Transport,
) -> Outcome {
    let Some((_, req)) = catalog.find_request(request_id) else {
        return Outcome::failed(request_id, "?", "request not found (was it deleted?)".into());
    };
    let name = req.name.clone();
    if req.protocol == Protocol::WebSocket {
        return Outcome::failed(request_id, &name, "WebSocket requests are interactive; open them in the editor to connect".into());
    }
    let now = transport.now_ms();
    let wire = match build_wire(catalog, request_id, workflow_vars, now, transport.native_grpc()) {
        Ok(w) => w,
        Err(e) => return Outcome::failed(request_id, &name, e),
    };
    let mut out = Outcome { request_id: request_id.into(), request_name: name.clone(), prepared: Some(wire.prepared.clone()), ..Default::default() };
    if !wire.prepared.missing_vars.is_empty() {
        out.extract_errors.push(format!("undefined variables: {}", wire.prepared.missing_vars.join(", ")));
    }

    let mut auth = wire.auth.clone();
    if let Auth::OAuth2(o) = &mut auth {
        let interactive = matches!(o.grant, GrantType::AuthorizationCode | GrantType::Manual);
        match ensure_oauth_token(o, &wire.http.connect, transport).await {
            Ok(Some(t)) => out.token = Some((wire.auth_owner.clone(), t)),
            Ok(None) => {}
            // No token that needs a person to sign in: send anyway and let the server decide.
            Err(e) if interactive && o.access_token.is_empty() => {
                out.warnings.push(format!("{e}; sent without a token"));
                auth = Auth::None;
            }
            Err(e) => {
                out.error = Some(e);
                return out;
            }
        }
    }

    let mut http = wire.http.clone();
    // Before signing, so signatures (AWS, OAuth 1) cover the body as sent.
    if wire.prepared.grpc.is_none() && req.protocol != Protocol::Mcp {
        if let Err(e) = apply_send_options(&req.send, &mut http) {
            out.error = Some(e);
            return out;
        }
    }
    if let Err(e) = auth::apply(&auth, &mut http, now) {
        out.error = Some(e);
        return out;
    }
    if !transport.honours_connect_options() {
        let c = &http.connect;
        if !c.client_cert.is_empty() {
            out.error = Some("browsers can't send client certificates; start the local agent (apicraft-agent) or use the desktop app".into());
            return out;
        }
        if c.proxy.as_deref().is_some_and(|p| !p.is_empty()) || c.insecure || !c.ca.is_empty() {
            out.warnings.push("the browser uses its own proxy and certificate settings; start the local agent to use the ones in Settings".into());
        }
    }
    if req.protocol == Protocol::Mcp {
        execute_mcp(req, &wire, http, transport, extra_extract, extra_checks, &mut out).await;
        return out;
    }

    let mut resp = match transport.send(http.clone()).await {
        Ok(r) => r,
        Err(e) => {
            out.sent = Some(http);
            out.error = Some(e);
            return out;
        }
    };

    if let Auth::Digest { username, password } = &auth {
        if resp.status == 401 {
            match resp.header("www-authenticate").filter(|h| h.trim_start().to_ascii_lowercase().starts_with("digest")) {
                Some(challenge) => {
                    let uri = Url::parse(&http.url).map(|u| u.path_and_query()).unwrap_or_else(|_| "/".into());
                    match auth::digest_authorization(challenge, username, password, &http.method, &uri, &http.body, &new_id(), 1) {
                        Ok(value) => {
                            http.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
                            http.headers.push(("Authorization".into(), value));
                            match transport.send(http.clone()).await {
                                Ok(r) => resp = r,
                                Err(e) => {
                                    out.sent = Some(http);
                                    out.error = Some(e);
                                    return out;
                                }
                            }
                        }
                        Err(e) => out.extract_errors.push(e),
                    }
                }
                None => out.extract_errors.push(
                    "401 without a readable Digest challenge (in the browser, the server must expose WWW-Authenticate via CORS; the local agent avoids this)"
                        .into(),
                ),
            }
        }
    }

    // Most senders unpack compressed responses themselves; catch the rest.
    if wire.prepared.grpc.is_none() {
        inflate_response(&mut resp);
    }

    // Decode the body.
    if let Some(target) = &wire.prepared.grpc {
        let decoded = grpc::compile(&target.proto).and_then(|pool| {
            let m = grpc::method(&pool, &target.service, &target.method)?;
            grpc::decode_response(&m.output(), resp.content_type(), &resp.body, &resp.headers, &resp.trailers)
        });
        match decoded {
            Ok(g) => {
                out.body_text = g.body_json();
                out.grpc = Some(g);
            }
            Err(e) => {
                out.body_text = resp.text();
                out.error = Some(e);
            }
        }
    } else {
        out.body_text = resp.text();
        out.fault = match wire.prepared.protocol {
            Protocol::Soap => soap::fault_message(&out.body_text).map(|f| format!("SOAP fault: {f}")),
            Protocol::GraphQl => graphql::error_message(&out.body_text).map(|e| format!("GraphQL error: {e}")),
            Protocol::OData if resp.status >= 400 => odata::error_message(&out.body_text).map(|e| format!("OData error: {e}")),
            _ => None,
        };
    }

    let view = ResponseView { status: resp.status, headers: &resp.headers, body: &out.body_text, elapsed_ms: resp.elapsed_ms };
    let extractions = req.extract.iter().chain(extra_extract.iter()).filter(|e| e.enabled && !e.variable.trim().is_empty());
    for e in extractions {
        match extract::extract(e, &view) {
            Ok(v) => out.extracted.push((e.scope, e.variable.trim().to_string(), v)),
            Err(err) => out.extract_errors.push(format!("{}: {err}", e.variable)),
        }
    }
    for c in req.checks.iter().chain(extra_checks.iter()).filter(|c| c.enabled) {
        let resolved = Check { target: wire.vars.apply(&c.target), expected: wire.vars.apply(&c.expected), ..c.clone() };
        out.checks.push(extract::run_check(&resolved, &view));
    }
    out.sent = Some(http);
    out.response = Some(resp);
    out
}

/// Downloads a definition to import, sent like any other request: the
/// catalog's proxy and certificates apply, `{{variables}}` are filled in, and
/// `auth` can be anything a request can use. `Auth::Inherit` takes the auth of
/// `collection`. With `login`, it signs in through the site's form first. The
/// request only exists in this copy of the catalog, so credentials given for
/// an import aren't saved.
pub async fn fetch_definition(
    mut catalog: Catalog,
    collection: Option<&str>,
    url: &str,
    auth: Auth,
    mut headers: Vec<KeyValue>,
    login: Option<FormLogin>,
    transport: &dyn Transport,
) -> Result<Vec<u8>, String> {
    let signed_in = login.is_some();
    if let Some(login) = login {
        let col = collection.and_then(|c| catalog.collection(c));
        let vars = Vars::for_request(&catalog, col, &[], transport.now_ms());
        let target = vars.apply(url);
        let login = FormLogin { page: vars.apply(&login.page), username: vars.apply(&login.username), password: vars.apply(&login.password) };
        let jar = form_login::sign_in(&login, &target, &catalog.network.connect_options(&vars), transport).await.map_err(|e| {
            if e.contains(url) {
                e
            } else {
                format!("Could not sign in for {url}: {e}")
            }
        })?;
        if let Some(cookies) = jar.header(&Url::parse(&target)?) {
            match headers.iter_mut().find(|h| h.key.eq_ignore_ascii_case("cookie")) {
                Some(h) => h.value = format!("{}; {cookies}", h.value),
                None => headers.push(KeyValue::new("Cookie", cookies)),
            }
        }
    }
    let mut req = Request::new("Import", Protocol::Rest);
    req.url = url.to_string();
    req.auth = auth;
    req.headers = headers;
    let id = req.id.clone();
    match collection.and_then(|c| catalog.collections.iter_mut().find(|x| x.id == c)) {
        Some(c) => c.items.push(Item::Request(req)),
        None => {
            let mut c = Collection::new("Import");
            c.items.push(Item::Request(req));
            catalog.collections.push(c);
        }
    }
    let out = execute(&catalog, &id, &[], &[], &[], transport).await;
    if let Some(e) = out.error {
        return Err(format!("Could not download {url}: {e}"));
    }
    let Some(resp) = out.response else {
        return Err(format!("Could not download {url}: no response"));
    };
    let notes: String = out.warnings.iter().chain(out.extract_errors.iter()).map(|n| format!(" ({n})")).collect();
    if matches!(resp.status, 401 | 403) {
        return Err(format!("{url} returned {} {}: it needs credentials that it accepts{notes}", resp.status, resp.status_text));
    }
    if !(200..300).contains(&resp.status) {
        return Err(format!("{url} returned {} {}{notes}", resp.status, resp.status_text));
    }
    if is_web_page(&resp) && signed_in {
        return Err(format!("{url} still returned a web page after signing in{notes}"));
    }
    if is_web_page(&resp) {
        return Err(format!("{url} returned a web page, not a definition: it's probably a sign-in page, so it needs credentials that it accepts{notes}"));
    }
    Ok(resp.body)
}

/// Sites that want a sign-in usually redirect to a form, which arrives as a
/// success. Servers that label a definition as HTML by mistake are let through.
fn is_web_page(resp: &HttpResponse) -> bool {
    let start = resp.body.trim_ascii_start();
    resp.content_type().to_ascii_lowercase().contains("html") && !(start.starts_with(b"{") || start.starts_with(b"[") || start.starts_with(b"<?xml"))
}

/// What a GraphQL endpoint (by introspection) or an OData service (by its
/// `$metadata`) says it offers.
#[derive(Debug, Clone, PartialEq)]
pub enum ApiSchema {
    GraphQl(graphql::Schema),
    OData(odata::Metadata),
}

/// Loads the schema of the GraphQL endpoint or OData service a request
/// points at, with the request's auth and headers.
pub async fn fetch_schema(catalog: &Catalog, request_id: &str, transport: &dyn Transport) -> Result<ApiSchema, String> {
    let (_, req) = catalog.find_request(request_id).ok_or("request not found")?;
    let now = transport.now_ms();
    let wire = build_wire(catalog, request_id, &[], now, false)?;
    let mut http = wire.http.clone();
    let base = wire.vars.apply(req.url.trim());
    http.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type") && !k.eq_ignore_ascii_case("accept"));
    match req.protocol {
        Protocol::GraphQl => {
            http.method = "POST".into();
            http.url = base;
            http.body = graphql::request_body(graphql::INTROSPECTION_QUERY, "", "IntrospectionQuery")?.into_bytes();
            http.headers.push(("Content-Type".into(), "application/json".into()));
            http.headers.push(("Accept".into(), "application/json".into()));
        }
        Protocol::OData => {
            http.method = "GET".into();
            http.url = odata::metadata_url(&base);
            http.body = Vec::new();
            http.headers.push(("Accept".into(), "application/xml".into()));
        }
        _ => return Err("only GraphQL and OData requests have a schema to load".into()),
    }
    let mut auth = wire.auth.clone();
    if let Auth::OAuth2(o) = &mut auth {
        let _ = ensure_oauth_token(o, &wire.http.connect, transport).await;
    }
    auth::apply(&auth, &mut http, now)?;
    let url = http.url.clone();
    let resp = transport.send(http).await?;
    let text = resp.text();
    if !(200..300).contains(&resp.status) {
        let detail = graphql::error_message(&text).or_else(|| odata::error_message(&text)).unwrap_or_else(|| text.chars().take(200).collect());
        return Err(format!("{url} returned {} {}: {}", resp.status, resp.status_text, detail.trim()));
    }
    match req.protocol {
        Protocol::GraphQl => {
            let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("the introspection response is not JSON: {e}"))?;
            graphql::parse_schema(&v).map(ApiSchema::GraphQl)
        }
        _ => odata::parse_metadata(&text).map(ApiSchema::OData),
    }
}

fn with_connect(req: HttpRequest, connect: &ConnectOptions) -> HttpRequest {
    HttpRequest { connect: connect.clone(), ..req }
}

/// Lists the tools, resources and prompts of the MCP server a request points at.
pub async fn mcp_discover(catalog: &Catalog, request_id: &str, transport: &dyn Transport) -> Result<crate::mcp::McpCatalog, String> {
    let (_, req) = catalog.find_request(request_id).ok_or("request not found")?;
    let now = transport.now_ms();
    let wire = build_wire(catalog, request_id, &[], now, false)?;
    let mut auth = wire.auth.clone();
    if let Auth::OAuth2(o) = &mut auth {
        let _ = ensure_oauth_token(o, &wire.http.connect, transport).await;
    }
    let mut http = wire.http.clone();
    auth::apply(&auth, &mut http, now)?;
    let mut opts = req.mcp.clone();
    opts.command = wire.vars.apply(&opts.command);
    opts.args = opts.args.iter().map(|a| wire.vars.apply(a)).collect();
    for kv in &mut opts.env {
        kv.value = wire.vars.apply(&kv.value);
    }
    let headers: Vec<(String, String)> = http.headers.iter().filter(|(k, _)| !k.eq_ignore_ascii_case("content-type")).cloned().collect();
    crate::mcp::discover(&opts, &http.url, &headers, transport).await
}

/// MCP requests: a short session, with the operation's result as the body.
async fn execute_mcp(
    req: &crate::model::Request,
    wire: &Wire,
    http: HttpRequest,
    transport: &dyn Transport,
    extra_extract: &[Extraction],
    extra_checks: &[Check],
    out: &mut Outcome,
) {
    // Variables apply to the MCP settings too.
    let mut opts = req.mcp.clone();
    opts.command = wire.vars.apply(&opts.command);
    opts.args = opts.args.iter().map(|a| wire.vars.apply(a)).collect();
    opts.target = wire.vars.apply(&opts.target);
    for kv in &mut opts.env {
        kv.value = wire.vars.apply(&kv.value);
    }
    let body = match &wire.prepared.body {
        crate::prepare::PreparedBody::Text(t) => t.clone(),
        _ => String::new(),
    };
    let headers: Vec<(String, String)> = http.headers.iter().filter(|(k, _)| !k.eq_ignore_ascii_case("content-type")).cloned().collect();
    let target = match opts.transport {
        crate::model::McpTransport::Stdio => format!("{} {}", opts.command, opts.args.join(" ")).trim().to_string(),
        _ => http.url.clone(),
    };
    let started = transport.now_ms();
    let result = crate::mcp::run(&opts, &http.url, &headers, &body, transport).await;
    let elapsed = transport.now_ms().saturating_sub(started) as f64;
    let (messages, ex) = match result {
        Ok(r) => r,
        Err(e) => {
            out.sent = Some(HttpRequest { method: "MCP".into(), url: target, headers, ..Default::default() });
            out.error = Some(e);
            return;
        }
    };
    let (value, error) = crate::mcp::outcome_result(&opts, &ex);
    out.body_text = serde_json::to_string_pretty(&value).unwrap_or_default();
    out.error = error;
    let mut info = vec![("transport".to_string(), opts.transport.label().to_string()), ("operation".to_string(), opts.operation.method().to_string())];
    if let Some(si) = ex.responses.first().and_then(|r| r.pointer("/result/serverInfo")) {
        info.push((
            "server".into(),
            format!("{} {}", si.get("name").and_then(|v| v.as_str()).unwrap_or(""), si.get("version").and_then(|v| v.as_str()).unwrap_or(""))
                .trim()
                .to_string(),
        ));
    }
    if let Some(v) = ex.responses.first().and_then(|r| r.pointer("/result/protocolVersion")).and_then(|v| v.as_str()) {
        info.push(("protocol version".into(), v.to_string()));
    }
    info.extend(ex.info.iter().cloned());
    // Server notifications (logs, progress) show as trailers.
    let notes: Vec<(String, String)> = ex
        .notifications
        .iter()
        .map(|n| (n.get("method").and_then(|m| m.as_str()).unwrap_or("message").to_string(), n.get("params").map(|p| p.to_string()).unwrap_or_default()))
        .collect();
    let mut trailers = notes;
    if !ex.stderr.trim().is_empty() {
        trailers.push(("stderr".into(), ex.stderr.trim().to_string()));
    }
    out.response = Some(HttpResponse {
        status: if out.error.is_some() { 500 } else { 200 },
        status_text: if out.error.is_some() { "MCP error".into() } else { "OK".into() },
        headers: info,
        body: out.body_text.clone().into_bytes(),
        trailers,
        elapsed_ms: elapsed,
    });
    out.sent = Some(HttpRequest {
        connect: Default::default(),
        method: "MCP".into(),
        url: target,
        headers,
        body: serde_json::to_vec_pretty(&messages).unwrap_or_default(),
        grpc: false,
        timeout_ms: 0,
        no_redirects: false,
    });
    let view = ResponseView { status: if out.error.is_some() { 500 } else { 200 }, headers: &[], body: &out.body_text, elapsed_ms: elapsed };
    for e in req.extract.iter().chain(extra_extract.iter()).filter(|e| e.enabled && !e.variable.trim().is_empty()) {
        match extract::extract(e, &view) {
            Ok(v) => out.extracted.push((e.scope, e.variable.trim().to_string(), v)),
            Err(err) => out.extract_errors.push(format!("{}: {err}", e.variable)),
        }
    }
    for c in req.checks.iter().chain(extra_checks.iter()).filter(|c| c.enabled) {
        let resolved = Check { target: wire.vars.apply(&c.target), expected: wire.vars.apply(&c.expected), ..c.clone() };
        out.checks.push(extract::run_check(&resolved, &view));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::http::LocalBoxFuture;
    use crate::model::*;
    use std::cell::RefCell;

    /// Replies from a script and records what it was sent.
    pub struct Scripted {
        pub replies: RefCell<Vec<HttpResponse>>,
        pub sent: RefCell<Vec<HttpRequest>>,
    }

    impl Scripted {
        pub fn new(replies: Vec<HttpResponse>) -> Self {
            Scripted { replies: RefCell::new(replies), sent: RefCell::new(Vec::new()) }
        }
    }

    impl Transport for Scripted {
        fn send(&self, req: HttpRequest) -> LocalBoxFuture<'_, Result<HttpResponse, String>> {
            self.sent.borrow_mut().push(req);
            let r = self.replies.borrow_mut().remove(0);
            Box::pin(async move { Ok(r) })
        }
        fn native_grpc(&self) -> bool {
            false
        }
        fn sleep(&self, _ms: u32) -> LocalBoxFuture<'_, ()> {
            Box::pin(async {})
        }
        fn now_ms(&self) -> u64 {
            1_700_000_000_000
        }
    }

    pub fn reply(status: u16, headers: &[(&str, &str)], body: &str) -> HttpResponse {
        HttpResponse {
            status,
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            body: body.as_bytes().to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn definitions_download_with_auth_headers_and_connect_options() {
        let mut cat = Catalog::default();
        cat.network.verify_tls = false;
        let before = cat.clone();
        let url = "https://api.test/odata/$metadata?$format=xml";
        let t = Scripted::new(vec![reply(200, &[("content-type", "application/json")], "{}")]);
        let auth = Auth::Bearer { token: "T".into() };
        let headers = vec![KeyValue::new("Cookie", "session=abc")];
        let body = pollster::block_on(fetch_definition(cat.clone(), None, url, auth, headers, None, &t)).unwrap();
        assert_eq!(body, b"{}");
        let sent = t.sent.borrow();
        assert_eq!(sent[0].url, url);
        assert!(sent[0].connect.insecure);
        assert!(sent[0].headers.iter().any(|(k, v)| k == "Authorization" && v == "Bearer T"));
        assert!(sent[0].headers.iter().any(|(k, v)| k == "Cookie" && v == "session=abc"));
        assert_eq!(cat, before);
    }

    #[test]
    fn definitions_can_use_the_target_collections_auth() {
        let mut cat = Catalog::default();
        let mut col = Collection::new("c");
        col.auth = Auth::Bearer { token: "COL".into() };
        let cid = col.id.clone();
        cat.collections.push(col);
        let t = Scripted::new(vec![reply(200, &[], "{}")]);
        pollster::block_on(fetch_definition(cat, Some(&cid), "https://api.test/d", Auth::Inherit, Vec::new(), None, &t)).unwrap();
        assert!(t.sent.borrow()[0].headers.iter().any(|(k, v)| k == "Authorization" && v == "Bearer COL"));
    }

    #[test]
    fn a_sign_in_page_is_not_a_definition() {
        let fetch = |r: HttpResponse| {
            let t = Scripted::new(vec![r]);
            pollster::block_on(fetch_definition(Catalog::default(), None, "https://api.test/d", Auth::None, Vec::new(), None, &t))
        };
        let html = [("content-type", "text/html; charset=utf-8")];
        assert!(fetch(reply(200, &html, "<!doctype html><form>")).unwrap_err().contains("sign-in page"));
        assert!(fetch(reply(401, &[], "")).unwrap_err().contains("credentials"));
        // Mislabelled, but still a definition.
        assert!(fetch(reply(200, &html, "  {\"openapi\":\"3.0.0\"}")).is_ok());
        assert!(fetch(reply(200, &html, "<?xml version=\"1.0\"?><definitions/>")).is_ok());
    }

    const LOGIN_PAGE: &str = r#"<form method="post" action="/Account/Login"><input type="hidden" name="returnUrl" value="/swagger.json">
        <input type="password" name="password"><input name="__RequestVerificationToken" type="hidden" value="TOKEN"></form>"#;

    fn sign_in_with(replies: Vec<HttpResponse>, password: &str) -> (Result<Vec<u8>, String>, Vec<HttpRequest>) {
        let t = Scripted::new(replies);
        let login = FormLogin { password: password.into(), ..Default::default() };
        let r = pollster::block_on(fetch_definition(Catalog::default(), None, "https://app.test/swagger.json", Auth::None, Vec::new(), Some(login), &t));
        (r, t.sent.into_inner())
    }

    #[test]
    fn signs_in_through_a_form_then_downloads_with_its_cookies() {
        let html = [("content-type", "text/html"), ("set-cookie", "anti=A; path=/; httponly")];
        let (r, sent) = sign_in_with(
            vec![
                reply(302, &[("location", "/Account/Login?returnUrl=%2Fswagger.json")], ""),
                reply(200, &html, LOGIN_PAGE),
                reply(302, &[("location", "/swagger.json"), ("set-cookie", "session=S; path=/; secure; httponly")], ""),
                reply(200, &[("content-type", "application/json")], "{}"),
                reply(200, &[("content-type", "application/json")], r#"{"openapi":"3.0.0"}"#),
            ],
            "p&ss word",
        );
        assert_eq!(r.unwrap(), br#"{"openapi":"3.0.0"}"#);
        assert!(sent[..4].iter().all(|s| s.no_redirects), "redirects are followed by hand, to keep their cookies");
        let header = |s: &HttpRequest, name: &str| s.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());

        let post = &sent[2];
        assert_eq!((post.method.as_str(), post.url.as_str()), ("POST", "https://app.test/Account/Login"));
        assert_eq!(header(post, "Cookie").as_deref(), Some("anti=A"));
        let body = String::from_utf8(post.body.clone()).unwrap();
        for part in ["__RequestVerificationToken=TOKEN", "returnUrl=%2Fswagger.json", "password=p%26ss"] {
            assert!(body.contains(part), "{part} in {body}");
        }

        assert_eq!((sent[3].method.as_str(), sent[3].body.is_empty()), ("GET", true), "the password doesn't follow the redirect");
        assert_eq!(header(&sent[4], "Cookie").as_deref(), Some("anti=A; session=S"));
    }

    #[test]
    fn a_refused_sign_in_says_so_without_the_password() {
        let html = [("content-type", "text/html")];
        let (r, sent) = sign_in_with(vec![reply(200, &html, LOGIN_PAGE), reply(200, &html, LOGIN_PAGE)], "hunter2");
        let e = r.unwrap_err();
        assert!(e.contains("check the username and password") && !e.contains("hunter2"), "{e}");
        assert_eq!(sent.len(), 2);
    }

    #[test]
    fn passwords_are_not_sent_to_a_form_that_would_expose_them() {
        let html = [("content-type", "text/html")];
        let page = |form: &str| reply(200, &html, &format!(r#"{form}<input type="password" name="p"></form>"#));
        for (form, why) in [(r#"<form action="/l">"#, "in the address"), (r#"<form method="post" action="http://app.test/l">"#, "unencrypted")] {
            let (r, sent) = sign_in_with(vec![page(form)], "hunter2");
            assert!(r.unwrap_err().contains(why));
            assert_eq!(sent.len(), 1, "nothing was posted");
        }
    }

    #[test]
    fn signing_in_is_skipped_when_the_definition_is_open() {
        let json = [("content-type", "application/json")];
        let (r, sent) = sign_in_with(vec![reply(200, &json, "{}"), reply(200, &json, "{}")], "hunter2");
        assert_eq!(r.unwrap(), b"{}");
        assert!(sent.iter().all(|s| s.method == "GET" && s.body.is_empty()));
    }

    #[test]
    fn oauth2_client_credentials_then_call() {
        let mut cat = Catalog::default();
        let mut col = Collection::new("c");
        col.auth = Auth::OAuth2(OAuth2 { token_url: "https://auth/token".into(), client_id: "id".into(), client_secret: "sec".into(), ..Default::default() });
        let mut r = Request::new("r", Protocol::Rest);
        r.url = "https://api/me".into();
        r.extract.push(Extraction { variable: "user".into(), source: ExtractSource::Json, expression: "name".into(), scope: VarScope::Runtime, enabled: true });
        let rid = r.id.clone();
        col.items.push(Item::Request(r));
        let cid = col.id.clone();
        cat.collections.push(col);

        let t = Scripted::new(vec![
            reply(200, &[], r#"{"access_token":"T1","expires_in":3600}"#),
            reply(200, &[("content-type", "application/json")], r#"{"name":"ann"}"#),
        ]);
        let o = pollster::block_on(execute(&cat, &rid, &[], &[], &[], &t));
        assert!(o.passed(), "{o:?}");
        let sent = t.sent.borrow();
        assert!(sent[1].headers.iter().any(|(k, v)| k == "Authorization" && v == "Bearer T1"));
        apply_outcome(&mut cat, &o);
        assert_eq!(cat.runtime[0].value, "ann");
        match &cat.collection(&cid).unwrap().auth {
            Auth::OAuth2(o) => assert_eq!(o.access_token, "T1"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn digest_retries_after_challenge() {
        let mut cat = Catalog::default();
        let mut col = Collection::new("c");
        let mut r = Request::new("r", Protocol::Rest);
        r.url = "http://h/dir/index.html".into();
        r.auth = Auth::Digest { username: "Mufasa".into(), password: "Circle Of Life".into() };
        let rid = r.id.clone();
        col.items.push(Item::Request(r));
        cat.collections.push(col);
        let t = Scripted::new(vec![reply(401, &[("WWW-Authenticate", r#"Digest realm="r", nonce="n", qop="auth""#)], ""), reply(200, &[], "ok")]);
        let o = pollster::block_on(execute(&cat, &rid, &[], &[], &[], &t));
        assert_eq!(o.response.unwrap().status, 200);
        assert!(t.sent.borrow()[1].headers.iter().any(|(k, v)| k == "Authorization" && v.starts_with("Digest username=\"Mufasa\"")));
    }
}

#[cfg(test)]
mod send_option_tests {
    use super::*;

    #[test]
    fn compresses_sets_lengths_and_unpacks() {
        let body = br#"{"hello":"world","items":[1,2,3,4,5,6,7,8,9,10]}"#.to_vec();
        for how in [Compression::Gzip, Compression::Deflate] {
            let mut http =
                HttpRequest { method: "POST".into(), body: body.clone(), headers: vec![("content-length".into(), "1".into())], ..Default::default() };
            let o = SendOptions { content_length: true, compress: how, accept_compressed: true };
            apply_send_options(&o, &mut http).unwrap();
            let header = |k: &str| http.headers.iter().filter(|(h, _)| h.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone()).collect::<Vec<_>>();
            assert_eq!(header("content-encoding"), [how.label()]);
            assert_eq!(header("content-length"), [http.body.len().to_string()], "replaces the user's value with the compressed size");
            assert_eq!(header("accept-encoding"), ["gzip, deflate"]);
            let mut resp = HttpResponse { headers: vec![("Content-Encoding".into(), how.label().into())], body: http.body.clone(), ..Default::default() };
            inflate_response(&mut resp);
            assert_eq!(resp.body, body);
        }
        // Already unpacked by the sender: left alone.
        let mut resp = HttpResponse { headers: vec![("Content-Encoding".into(), "gzip".into())], body: body.clone(), ..Default::default() };
        inflate_response(&mut resp);
        assert_eq!(resp.body, body);
    }
}
