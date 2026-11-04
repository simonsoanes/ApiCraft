//! Fizl ApiCraft as an MCP server, so a local agent (Claude Code, ...) can
//! list, send and edit requests, run workflows and import API definitions.
//! It works on the catalog file the desktop app uses (loaded per call, saved
//! after changes), and is served over stdio (`apicraft-agent mcp`) and, while
//! `apicraft-agent serve` runs, over Streamable HTTP (`/mcp`) and legacy SSE (`/sse`).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use apicraft_core::codegen::{self, Target};
use apicraft_core::engine::{self, apply_outcome};
use apicraft_core::model::*;
use apicraft_core::store;
use apicraft_core::workflow;
use serde_json::{json, Value};

use crate::{now_ms, NativeTransport};

pub const SERVER_NAME: &str = "fizl-apicraft";
const SUPPORTED_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const MAX_BODY_CHARS: usize = 20_000;

pub struct McpServer {
    catalog_path: PathBuf,
}

fn text_result(text: impl Into<String>, structured: Option<Value>) -> Value {
    let mut r = json!({ "content": [{ "type": "text", "text": text.into() }] });
    if let Some(s) = structured {
        r["structuredContent"] = s;
    }
    r
}

fn tool_error(msg: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": msg.into() }], "isError": true })
}

fn arg<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty())
}

fn schema(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

fn tools() -> Value {
    let request = json!({ "type": "string", "description": "Request id, name, or \"Collection / Folder / Name\" path" });
    let env = json!({ "type": "string", "description": "Environment name or id to make active first (optional)" });
    json!([
        { "name": "list_requests", "description": "List saved requests (id, path, protocol, method, URL). Optionally only one collection.",
          "inputSchema": schema(json!({ "collection": { "type": "string" } }), &[]), "annotations": { "readOnlyHint": true } },
        { "name": "get_request", "description": "The full definition of a saved request (headers, body, auth, captures, checks).",
          "inputSchema": schema(json!({ "request": request }), &["request"]), "annotations": { "readOnlyHint": true } },
        { "name": "send_request", "description": "Send a saved request (REST, GraphQL, OData, SOAP, gRPC or MCP) and return the status, timing, checks, captured variables, headers and body. Captured values and refreshed OAuth tokens are saved.",
          "inputSchema": schema(json!({ "request": request, "environment": env }), &["request"]) },
        { "name": "list_workflows", "description": "List workflows and their steps.",
          "inputSchema": schema(json!({}), &[]), "annotations": { "readOnlyHint": true } },
        { "name": "run_workflow", "description": "Run a workflow (requests in order, passing captured values along) and report each step.",
          "inputSchema": schema(json!({ "workflow": { "type": "string", "description": "Workflow name or id" }, "environment": env }), &["workflow"]) },
        { "name": "create_request", "description": "Add a request to a collection (created if missing). Returns its id.",
          "inputSchema": schema(json!({
              "collection": { "type": "string" },
              "folder": { "type": "string", "description": "Folder inside the collection (created if missing)" },
              "name": { "type": "string" },
              "protocol": { "type": "string", "enum": ["rest", "soap", "grpc", "websocket", "mcp"] },
              "method": { "type": "string", "description": "HTTP method, default GET" },
              "url": { "type": "string" },
              "headers": { "type": "object", "additionalProperties": { "type": "string" } },
              "body": { "type": "string" },
              "content_type": { "type": "string" }
          }), &["collection", "name", "url"]) },
        { "name": "update_request", "description": "Change fields of a saved request. Headers, when given, replace the existing ones.",
          "inputSchema": schema(json!({
              "request": request, "name": { "type": "string" }, "method": { "type": "string" }, "url": { "type": "string" },
              "headers": { "type": "object", "additionalProperties": { "type": "string" } }, "body": { "type": "string" }, "content_type": { "type": "string" }
          }), &["request"]) },
        { "name": "delete_request", "description": "Delete a saved request.",
          "inputSchema": schema(json!({ "request": request }), &["request"]), "annotations": { "destructiveHint": true } },
        { "name": "import_definition", "description": "Import an OpenAPI/Swagger definition (with generated checks and workflows), a WSDL, a GraphQL introspection result, an OData $metadata document, a Postman collection/environment or a curl command, from text or a URL.",
          "inputSchema": schema(json!({ "url": { "type": "string" }, "headers": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Headers for downloading from url, such as Authorization or Cookie when it needs credentials" }, "text": { "type": "string" }, "collection": { "type": "string", "description": "For curl commands: the collection to add to" } }), &[]) },
        { "name": "list_environments", "description": "List environments and their variables, and which is active.",
          "inputSchema": schema(json!({}), &[]), "annotations": { "readOnlyHint": true } },
        { "name": "set_environment", "description": "Make an environment active (or none with an empty name).",
          "inputSchema": schema(json!({ "environment": { "type": "string" } }), &["environment"]) },
        { "name": "set_variable", "description": "Set a variable: runtime (overrides everything) or in the active environment.",
          "inputSchema": schema(json!({ "name": { "type": "string" }, "value": { "type": "string" }, "scope": { "type": "string", "enum": ["runtime", "environment"] } }), &["name", "value"]) },
        { "name": "export_code", "description": "A saved request as a curl or PowerShell command.",
          "inputSchema": schema(json!({ "request": request, "format": { "type": "string", "enum": ["curl", "powershell"] } }), &["request"]), "annotations": { "readOnlyHint": true } }
    ])
}

impl McpServer {
    pub fn new(catalog_path: PathBuf) -> Self {
        McpServer { catalog_path }
    }

    fn load(&self) -> Result<Catalog, String> {
        if !self.catalog_path.exists() {
            return Ok(Catalog::default());
        }
        crate::folders::load(&self.catalog_path).map(|(cat, _)| cat)
    }

    fn save(&self, cat: &Catalog) -> Result<(), String> {
        crate::folders::save(&self.catalog_path, cat).map_err(|e| format!("writing {e}"))
    }

    /// Handles one JSON-RPC message; returns the response for requests.
    pub fn handle(&self, msg: &Value) -> Option<Value> {
        let id = msg.get("id")?.clone();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let result: Result<Value, (i64, String)> = match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
                let version = if SUPPORTED_VERSIONS.contains(&asked) { asked } else { SUPPORTED_VERSIONS[1] };
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false }, "resources": { "listChanged": false } },
                    "serverInfo": { "name": SERVER_NAME, "title": "Fizl ApiCraft", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "Fizl ApiCraft holds saved API requests (REST, GraphQL, OData, SOAP, gRPC, WebSocket, MCP), environments and workflows. Use list_requests to find requests, send_request to run one, run_workflow for ordered tests, and import_definition to load OpenAPI, WSDL, OData $metadata or Postman files."
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(self.call(name, &args).unwrap_or_else(tool_error))
            }
            "resources/list" => Ok(json!({ "resources": [
                { "uri": "apicraft://catalog", "name": "catalog", "title": "The whole catalog (JSON)", "mimeType": "application/json" }
            ] })),
            "resources/read" => match params.get("uri").and_then(|u| u.as_str()) {
                Some("apicraft://catalog") => self
                    .load()
                    .map(|c| json!({ "contents": [{ "uri": "apicraft://catalog", "mimeType": "application/json", "text": store::to_json(&c) }] }))
                    .map_err(|e| (-32603, e)),
                _ => Err((-32002, "resource not found".into())),
            },
            "prompts/list" => Ok(json!({ "prompts": [] })),
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
        })
    }

    fn call(&self, name: &str, args: &Value) -> Result<Value, String> {
        match name {
            "list_requests" => {
                let cat = self.load()?;
                let filter = arg(args, "collection").map(str::to_lowercase);
                let rows: Vec<Value> = cat
                    .all_requests()
                    .into_iter()
                    .filter(|(path, _)| filter.as_ref().map(|f| path.to_lowercase().starts_with(f)).unwrap_or(true))
                    .map(|(path, r)| json!({ "id": r.id, "path": path, "protocol": r.protocol.key(), "method": r.method, "url": r.url }))
                    .collect();
                let text = rows
                    .iter()
                    .map(|r| {
                        format!(
                            "{}  {} {}  {}  [{}]",
                            r["id"].as_str().unwrap_or(""),
                            r["method"].as_str().unwrap_or(""),
                            r["path"].as_str().unwrap_or(""),
                            r["url"].as_str().unwrap_or(""),
                            r["protocol"].as_str().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(text_result(if text.is_empty() { "No requests.".into() } else { text }, Some(json!({ "requests": rows }))))
            }
            "get_request" => {
                let cat = self.load()?;
                let id = find_request(&cat, arg(args, "request").ok_or("request is required")?)?;
                let (_, r) = cat.find_request(&id).ok_or("request not found")?;
                let v = serde_json::to_value(r).map_err(|e| e.to_string())?;
                Ok(text_result(serde_json::to_string_pretty(&v).unwrap_or_default(), Some(v)))
            }
            "send_request" => {
                let mut cat = self.load()?;
                if let Some(env) = arg(args, "environment") {
                    cat.active_environment = Some(find_environment(&cat, env)?);
                }
                let id = find_request(&cat, arg(args, "request").ok_or("request is required")?)?;
                let o = pollster::block_on(engine::execute(&cat, &id, &[], &[], &[], &NativeTransport));
                apply_outcome(&mut cat, &o);
                self.save(&cat)?;
                let summary = outcome_json(&o);
                Ok(text_result(outcome_text(&o), Some(summary)))
            }
            "list_workflows" => {
                let cat = self.load()?;
                let rows: Vec<Value> = cat
                    .workflows
                    .iter()
                    .map(|w| {
                        let steps: Vec<String> = w
                            .steps
                            .iter()
                            .map(|s| cat.find_request(&s.request_id).map(|(_, r)| r.name.clone()).unwrap_or_else(|| "(deleted)".into()))
                            .collect();
                        json!({ "id": w.id, "name": w.name, "steps": steps, "iterations": w.iterations, "stop_on_failure": w.stop_on_failure })
                    })
                    .collect();
                let text = rows
                    .iter()
                    .map(|w| {
                        format!(
                            "{}  {}: {}",
                            w["id"].as_str().unwrap_or(""),
                            w["name"].as_str().unwrap_or(""),
                            w["steps"].as_array().map(|s| s.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" \u{2192} ")).unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(text_result(if text.is_empty() { "No workflows.".into() } else { text }, Some(json!({ "workflows": rows }))))
            }
            "run_workflow" => {
                let mut cat = self.load()?;
                if let Some(env) = arg(args, "environment") {
                    cat.active_environment = Some(find_environment(&cat, env)?);
                }
                let wanted = arg(args, "workflow").ok_or("workflow is required")?;
                let wid = cat
                    .workflows
                    .iter()
                    .find(|w| w.id == wanted || w.name.eq_ignore_ascii_case(wanted))
                    .map(|w| w.id.clone())
                    .ok_or_else(|| format!("no workflow named \u{201c}{wanted}\u{201d}"))?;
                let mut steps = Vec::new();
                let mut outcomes = Vec::new();
                let summary = pollster::block_on(workflow::run(&cat, &wid, &NativeTransport, &mut |r| {
                    let mut step = json!({ "step": r.index + 1, "iteration": r.iteration, "name": r.name });
                    match (&r.outcome, &r.skipped) {
                        (Some(o), _) => {
                            step["result"] = outcome_json(o);
                            outcomes.push(o.clone());
                        }
                        (None, Some(why)) => step["skipped"] = json!(why),
                        _ => {}
                    }
                    steps.push(step);
                    true
                }))?;
                for o in &outcomes {
                    apply_outcome(&mut cat, o);
                }
                self.save(&cat)?;
                let mut text = format!(
                    "{} passed, {} failed, {} skipped{} in {:.0} ms\n",
                    summary.passed,
                    summary.failed,
                    summary.skipped,
                    if summary.stopped_early { " (stopped early)" } else { "" },
                    summary.elapsed_ms
                );
                for (i, o) in outcomes.iter().enumerate() {
                    text.push_str(&format!("\n{}. {}", i + 1, outcome_text(o).lines().next().unwrap_or("")));
                }
                Ok(text_result(text, Some(json!({ "passed": summary.passed, "failed": summary.failed, "skipped": summary.skipped, "steps": steps }))))
            }
            "create_request" => {
                let mut cat = self.load()?;
                let col_name = arg(args, "collection").ok_or("collection is required")?;
                let protocol = arg(args, "protocol").and_then(Protocol::from_key).unwrap_or(Protocol::Rest);
                let mut r = Request::new(arg(args, "name").ok_or("name is required")?, protocol);
                apply_fields(&mut r, args);
                let rid = r.id.clone();
                let cid = match cat.collections.iter().find(|c| c.name.eq_ignore_ascii_case(col_name)) {
                    Some(c) => c.id.clone(),
                    None => {
                        let c = Collection::new(col_name);
                        let id = c.id.clone();
                        cat.collections.push(c);
                        id
                    }
                };
                let container = match arg(args, "folder") {
                    Some(folder) => {
                        let col = cat.collection_mut(&cid).ok_or("collection vanished")?;
                        match col.items.iter().find_map(|i| match i {
                            Item::Folder(f) if f.name.eq_ignore_ascii_case(folder) => Some(f.id.clone()),
                            _ => None,
                        }) {
                            Some(fid) => fid,
                            None => {
                                let f = Folder::new(folder);
                                let fid = f.id.clone();
                                col.items.push(Item::Folder(f));
                                fid
                            }
                        }
                    }
                    None => cid,
                };
                cat.container_items_mut(&container).ok_or("container not found")?.push(Item::Request(r));
                self.save(&cat)?;
                Ok(text_result(format!("Created request {rid}"), Some(json!({ "id": rid }))))
            }
            "update_request" => {
                let mut cat = self.load()?;
                let id = find_request(&cat, arg(args, "request").ok_or("request is required")?)?;
                let r = cat.find_request_mut(&id).ok_or("request not found")?;
                if let Some(n) = arg(args, "name") {
                    r.name = n.to_string();
                }
                apply_fields(r, args);
                self.save(&cat)?;
                Ok(text_result(format!("Updated request {id}"), Some(json!({ "id": id }))))
            }
            "delete_request" => {
                let mut cat = self.load()?;
                let id = find_request(&cat, arg(args, "request").ok_or("request is required")?)?;
                cat.remove_item(&id);
                for w in &mut cat.workflows {
                    w.steps.retain(|s| s.request_id != id);
                }
                self.save(&cat)?;
                Ok(text_result(format!("Deleted request {id}"), None))
            }
            "import_definition" => {
                let mut cat = self.load()?;
                let (bytes, source) = match (arg(args, "url"), args.get("text").and_then(|t| t.as_str())) {
                    (Some(url), _) => {
                        let headers = args
                            .get("headers")
                            .and_then(|h| h.as_object())
                            .map(|h| h.iter().filter_map(|(k, v)| Some(KeyValue::new(k.as_str(), v.as_str()?))).collect())
                            .unwrap_or_default();
                        let bytes = pollster::block_on(engine::fetch_definition(cat.clone(), None, url, Auth::None, headers, None, &NativeTransport))?;
                        (bytes, Some(url.to_string()))
                    }
                    (None, Some(text)) => (text.as_bytes().to_vec(), None),
                    (None, None) => return Err("give a url or text".into()),
                };
                let text = String::from_utf8_lossy(&bytes);
                if text.trim_start().starts_with("curl") {
                    let r = apicraft_core::curl_import::parse_curl(text.trim())?;
                    let col_name = arg(args, "collection").unwrap_or("Imported");
                    let rid = r.id.clone();
                    match cat.collections.iter_mut().find(|c| c.name.eq_ignore_ascii_case(col_name)) {
                        Some(c) => c.items.push(Item::Request(r)),
                        None => {
                            let mut c = Collection::new(col_name);
                            c.items.push(Item::Request(r));
                            cat.collections.push(c);
                        }
                    }
                    self.save(&cat)?;
                    return Ok(text_result(format!("Imported a curl command as request {rid}"), Some(json!({ "id": rid }))));
                }
                let imported = store::decode_from(&bytes, source.as_deref())?;
                let what = imported.describe();
                cat.merge(imported.into_catalog());
                self.save(&cat)?;
                Ok(text_result(format!("Imported {what}"), None))
            }
            "list_environments" => {
                let cat = self.load()?;
                let rows: Vec<Value> = cat
                    .environments
                    .iter()
                    .map(|e| {
                        let vars: serde_json::Map<String, Value> = e.variables.iter().filter(|v| v.enabled).map(|v| (v.key.clone(), json!(v.value))).collect();
                        json!({ "id": e.id, "name": e.name, "active": cat.active_environment.as_deref() == Some(&e.id), "variables": vars })
                    })
                    .collect();
                let runtime: serde_json::Map<String, Value> = cat.runtime.iter().map(|v| (v.key.clone(), json!(v.value))).collect();
                let v = json!({ "environments": rows, "runtime": runtime });
                Ok(text_result(serde_json::to_string_pretty(&v).unwrap_or_default(), Some(v)))
            }
            "set_environment" => {
                let mut cat = self.load()?;
                let wanted = args.get("environment").and_then(|e| e.as_str()).unwrap_or("").trim();
                cat.active_environment = if wanted.is_empty() { None } else { Some(find_environment(&cat, wanted)?) };
                self.save(&cat)?;
                Ok(text_result(if wanted.is_empty() { "No environment is active.".to_string() } else { format!("Active environment: {wanted}") }, None))
            }
            "set_variable" => {
                let mut cat = self.load()?;
                let name = arg(args, "name").ok_or("name is required")?;
                let value = args.get("value").and_then(|v| v.as_str()).unwrap_or("");
                let scope = if arg(args, "scope") == Some("environment") { VarScope::Environment } else { VarScope::Runtime };
                cat.set_variable(scope, name, value);
                self.save(&cat)?;
                Ok(text_result(format!("Set {{{{{name}}}}}"), None))
            }
            "export_code" => {
                let cat = self.load()?;
                let id = find_request(&cat, arg(args, "request").ok_or("request is required")?)?;
                let target = if arg(args, "format").map(|f| f.starts_with("power") || f == "ps").unwrap_or(false) { Target::PowerShell } else { Target::Curl };
                Ok(text_result(codegen::generate(&cat, &id, now_ms(), target)?, None))
            }
            other => Err(format!("unknown tool: {other}")),
        }
    }
}

fn apply_fields(r: &mut Request, args: &Value) {
    if let Some(m) = arg(args, "method") {
        r.method = m.to_uppercase();
    }
    if let Some(u) = arg(args, "url") {
        r.url = u.to_string();
    }
    if let Some(h) = args.get("headers").and_then(|h| h.as_object()) {
        r.headers = h.iter().map(|(k, v)| KeyValue::new(k, v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))).collect();
    }
    if let Some(b) = args.get("body").and_then(|b| b.as_str()) {
        let ct = arg(args, "content_type").unwrap_or("").to_string();
        r.body = Body::Raw { content_type: ct, text: b.to_string() };
    } else if let (Some(ct), Body::Raw { content_type, .. }) = (arg(args, "content_type"), &mut r.body) {
        *content_type = ct.to_string();
    }
}

fn find_request(cat: &Catalog, wanted: &str) -> Result<String, String> {
    let reqs = cat.all_requests();
    if let Some((_, r)) = reqs.iter().find(|(path, r)| r.id == wanted || path.eq_ignore_ascii_case(wanted)) {
        return Ok(r.id.clone());
    }
    let by_name: Vec<&(String, &Request)> = reqs.iter().filter(|(_, r)| r.name.eq_ignore_ascii_case(wanted)).collect();
    match by_name.len() {
        1 => Ok(by_name[0].1.id.clone()),
        0 => Err(format!("no request named \u{201c}{wanted}\u{201d}; use list_requests")),
        _ => Err(format!(
            "\u{201c}{wanted}\u{201d} matches {} requests; use its id or path: {}",
            by_name.len(),
            by_name.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

fn find_environment(cat: &Catalog, wanted: &str) -> Result<String, String> {
    cat.environments
        .iter()
        .find(|e| e.id == wanted || e.name.eq_ignore_ascii_case(wanted))
        .map(|e| e.id.clone())
        .ok_or_else(|| format!("no environment named \u{201c}{wanted}\u{201d}"))
}

fn outcome_json(o: &engine::Outcome) -> Value {
    let body: String = o.body_text.chars().take(MAX_BODY_CHARS).collect();
    json!({
        "request": o.request_name,
        "passed": o.passed(),
        "method": o.prepared.as_ref().map(|p| p.method.clone()),
        "url": o.prepared.as_ref().map(|p| p.url.clone()),
        "status": o.response.as_ref().map(|r| r.status),
        "status_text": o.response.as_ref().map(|r| r.status_text.clone()),
        "elapsed_ms": o.response.as_ref().map(|r| r.elapsed_ms.round()),
        "headers": o.response.as_ref().map(|r| r.headers.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>()),
        "grpc_status": o.grpc.as_ref().and_then(|g| g.status),
        "error": o.error,
        "warnings": o.warnings,
        "checks": o.checks.iter().map(|c| json!({ "check": c.label, "passed": c.passed, "detail": c.detail })).collect::<Vec<_>>(),
        "captured": o.extracted.iter().map(|(_, k, v)| json!({ "variable": k, "value": v })).collect::<Vec<_>>(),
        "body": body,
        "body_truncated": o.body_text.chars().count() > MAX_BODY_CHARS,
    })
}

fn outcome_text(o: &engine::Outcome) -> String {
    let mut t = String::new();
    let status = match (&o.response, &o.error) {
        (Some(r), _) => format!("{} {} in {:.0} ms", r.status, r.status_text, r.elapsed_ms),
        (None, Some(e)) => format!("failed: {e}"),
        _ => "no response".into(),
    };
    t.push_str(&format!("{} {}: {status}", if o.passed() { "PASS" } else { "FAIL" }, o.request_name));
    if let (Some(e), Some(_)) = (&o.error, &o.response) {
        t.push_str(&format!("\nerror: {e}"));
    }
    for c in &o.checks {
        t.push_str(&format!("\n{} {} {}", if c.passed { "\u{2713}" } else { "\u{2717}" }, c.label, c.detail));
    }
    for (_, k, v) in &o.extracted {
        t.push_str(&format!("\n{{{{{k}}}}} = {v}"));
    }
    for w in &o.warnings {
        t.push_str(&format!("\nwarning: {w}"));
    }
    if !o.body_text.is_empty() {
        let body: String = o.body_text.chars().take(MAX_BODY_CHARS).collect();
        t.push_str(&format!("\n\n{body}"));
        if o.body_text.chars().count() > MAX_BODY_CHARS {
            t.push_str("\n\u{2026}(truncated)");
        }
    }
    t
}

/// Serves MCP over stdin/stdout (one JSON message per line). Logs go to stderr.
pub fn serve_stdio(catalog_path: &Path) -> Result<(), String> {
    let server = McpServer::new(catalog_path.to_path_buf());
    eprintln!("Fizl ApiCraft MCP server on stdio; catalog: {}", catalog_path.display());
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let out: Vec<Value> = batch.iter().filter_map(|m| server.handle(m)).collect();
                (!out.is_empty()).then_some(Value::Array(out))
            }
            Ok(msg) => server.handle(&msg),
            Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } })),
        };
        if let Some(r) = reply {
            writeln!(stdout, "{r}").and_then(|_| stdout.flush()).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_with_sample() -> (McpServer, PathBuf) {
        let dir = std::env::temp_dir().join(format!("apicraft-mcp-test-{}", apicraft_core::ids::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("catalog.json.gz");
        let mut cat = Catalog::default();
        let mut col = Collection::new("Demo");
        let mut r = Request::new("Ping", Protocol::Rest);
        r.url = "http://127.0.0.1:9/never".into();
        col.items.push(Item::Request(r));
        cat.collections.push(col);
        std::fs::write(&path, store::to_gzip(&cat)).unwrap();
        (McpServer::new(path.clone()), path)
    }

    #[test]
    fn handshake_and_tools() {
        let (s, _) = server_with_sample();
        let init = s.handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26" } })).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(s.handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none());
        let tools = s.handle(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).unwrap();
        assert!(tools["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "send_request"));
        let list = s.handle(&json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "list_requests", "arguments": {} } })).unwrap();
        assert!(list["result"]["content"][0]["text"].as_str().unwrap().contains("Demo / Ping"));
    }

    #[test]
    fn creates_and_imports() {
        let (s, path) = server_with_sample();
        let r = s.call("create_request", &json!({ "collection": "Agent", "folder": "Made by MCP", "name": "Echo", "method": "post", "url": "https://httpbin.org/post", "body": "{\"a\":1}" })).unwrap();
        let id = r["structuredContent"]["id"].as_str().unwrap().to_string();
        let cat = store::decode(&std::fs::read(&path).unwrap()).unwrap().into_catalog();
        let (_, req) = cat.find_request(&id).unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.body.raw_text(), "{\"a\":1}");
        s.call("import_definition", &json!({ "text": "curl -X DELETE https://api.test/items/1" })).unwrap();
        let listed = s.call("list_requests", &json!({ "collection": "Imported" })).unwrap();
        assert!(listed["content"][0]["text"].as_str().unwrap().contains("DELETE"));
        assert!(s.call("get_request", &json!({ "request": "nope" })).is_err());
    }
}
