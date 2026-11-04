//! Fizl ApiCraft agent: serves the web app on localhost, proxies its requests
//! (no CORS limits, native gRPC), mirrors the catalog to a file, and runs
//! requests and workflows from the command line.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apicraft_agent::{default_catalog_path, folders, now_ms, server, NativeTransport};
use apicraft_core::codegen::{self, Target};
use apicraft_core::engine::{self, apply_outcome};
use apicraft_core::form_login::FormLogin;
use apicraft_core::model::{Auth, Catalog, KeyValue};
use apicraft_core::store;
use apicraft_core::workflow;

const USAGE: &str = "\
Fizl ApiCraft agent

USAGE:
  apicraft-agent [serve] [--port 8787] [--web DIR] [--data FILE] [--allow-origin URL]...
  apicraft-agent run [catalog] (--workflow NAME | --request NAME) [--env NAME] [--save]
  apicraft-agent import <file or URL> [-H 'Name: value']... [--password-env VAR [--user NAME] [--sign-in-page URL]]
                        [-o catalog.json.gz]
  apicraft-agent export [catalog] (--request NAME | --workflow NAME) [--format curl|powershell]
  apicraft-agent mcp [--catalog FILE]

  serve    Serve the web app at http://127.0.0.1:<port>/ and act as its local proxy.
           --data is the catalog file the app mirrors to (default: the desktop app's catalog).
  run      Run a workflow or a single request; exits non-zero if anything fails.
           --save writes captured variables and tokens back to the catalog file.
  import   Import a Postman collection/environment, OpenAPI/Swagger (JSON or YAML)
           WSDL, GraphQL introspection JSON or OData $metadata, from a file or URL,
           and merge it into a catalog file. -H adds a header when downloading, such
           as 'Authorization: Bearer ...' or 'Cookie: ...' for a URL behind a sign-in.
           --password-env signs in through the site's web form first, with the
           password from that environment variable.
  export   Print a request as a curl or PowerShell command, or a workflow as a
           Bash or PowerShell script that runs its steps, captures and checks.
  mcp      Run Fizl ApiCraft as an MCP server on stdio, so an agent can list, send,
           create and import requests and run workflows. For Claude Code:
             claude mcp add fizl-apicraft -- apicraft-agent mcp
           While `serve` runs it's also at http://127.0.0.1:<port>/mcp (Streamable
           HTTP) and /sse (legacy SSE).

Catalog files are JSON, plain or gzip-compressed (.json.gz). A collection folder
(a collection.json plus a .request.json file per request) works as a catalog too,
and collections the catalog links to folders are read from them. Without a
catalog argument, commands use the desktop app's catalog file.";

struct Args(Vec<String>);

impl Args {
    fn flag(&mut self, name: &str) -> bool {
        match self.0.iter().position(|a| a == name) {
            Some(i) => {
                self.0.remove(i);
                true
            }
            None => false,
        }
    }
    fn value(&mut self, names: &[&str]) -> Option<String> {
        let i = self.0.iter().position(|a| names.contains(&a.as_str()))?;
        self.0.remove(i);
        (i < self.0.len()).then(|| self.0.remove(i))
    }
    fn values(&mut self, name: &str) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(v) = self.value(&[name]) {
            out.push(v);
        }
        out
    }
    fn positional(&mut self) -> Option<String> {
        let i = self.0.iter().position(|a| !a.starts_with('-'))?;
        Some(self.0.remove(i))
    }
}

fn main() -> ExitCode {
    let mut args = Args(std::env::args().skip(1).collect());
    if args.flag("--help") || args.flag("-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    apicraft_core::ids::seed(now_ms() ^ (std::process::id() as u64) << 32);
    let cmd = match args.0.first().map(String::as_str) {
        Some("serve" | "run" | "import" | "export" | "mcp" | "help") => args.0.remove(0),
        _ => "serve".to_string(),
    };
    let result = match cmd.as_str() {
        "serve" => cmd_serve(args),
        "run" => cmd_run(args),
        "import" => cmd_import(args),
        "export" => cmd_export(args),
        "mcp" => {
            let path = args.value(&["--catalog", "-c"]).map(PathBuf::from).unwrap_or_else(default_catalog_path);
            apicraft_agent::mcp_server::serve_stdio(&path).map(|_| true)
        }
        _ => {
            println!("{USAGE}");
            Ok(true)
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn find_web_dir(explicit: Option<String>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(PathBuf::from(p));
    }
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let mut candidates = Vec::new();
    if let Some(d) = &exe_dir {
        candidates.push(d.join("web"));
        candidates.push(d.join("dist"));
        // target/release/apicraft-agent -> <repo>/dist
        candidates.push(d.join("..").join("..").join("dist"));
    }
    candidates.push(PathBuf::from("dist"));
    candidates.into_iter().find(|d| d.join("index.html").is_file())
}

fn cmd_serve(mut args: Args) -> Result<bool, String> {
    let port: u16 = args.value(&["--port", "-p"]).map(|p| p.parse().map_err(|_| "bad --port")).transpose()?.unwrap_or(8787);
    let web_dir = find_web_dir(args.value(&["--web"]));
    let data_file = args.value(&["--data"]).map(PathBuf::from).unwrap_or_else(default_catalog_path);
    let allow_origins = args.values("--allow-origin").into_iter().map(|o| o.trim_end_matches('/').to_string()).collect();
    match &web_dir {
        Some(d) => println!("Serving web app from {}", d.display()),
        None => println!("Web app not found (build it first, or pass --web DIR); the proxy API still works."),
    }
    println!("Catalog file: {}", data_file.display());
    println!("Fizl ApiCraft is running at http://127.0.0.1:{port}/  (Ctrl+C to stop)");
    server::serve(server::Config { port, web_dir, data_file, allow_origins }).map_err(|e| format!("could not listen on 127.0.0.1:{port}: {e}"))?;
    Ok(true)
}

/// The catalog file given on the command line, or the default one.
fn catalog_arg(args: &mut Args) -> String {
    args.positional().unwrap_or_else(|| default_catalog_path().display().to_string())
}

/// A catalog file, or a collection folder; linked folders are read in.
fn load_catalog(path: &str) -> Result<Catalog, String> {
    let (cat, warnings) = folders::load(Path::new(path)).map_err(|e| format!("reading {e}"))?;
    for w in warnings {
        eprintln!("warning: {w}");
    }
    Ok(cat)
}

fn save_catalog(path: &str, cat: &Catalog) -> Result<(), String> {
    folders::save(Path::new(path), cat).map_err(|e| format!("writing {e}"))
}

fn find_by_name<'a>(items: impl Iterator<Item = (&'a str, &'a str)>, wanted: &str) -> Result<String, String> {
    let all: Vec<(&str, &str)> = items.collect();
    if let Some((id, _)) = all.iter().find(|(id, name)| *id == wanted || name.eq_ignore_ascii_case(wanted)) {
        return Ok(id.to_string());
    }
    let matches: Vec<&(&str, &str)> = all.iter().filter(|(_, n)| n.to_lowercase().contains(&wanted.to_lowercase())).collect();
    match matches.len() {
        1 => Ok(matches[0].0.to_string()),
        0 => Err(format!("nothing named \u{201c}{wanted}\u{201d}")),
        _ => Err(format!("\u{201c}{wanted}\u{201d} is ambiguous: {}", matches.iter().map(|m| m.1).collect::<Vec<_>>().join(", "))),
    }
}

fn find_request(cat: &Catalog, wanted: &str) -> Result<String, String> {
    let reqs = cat.all_requests();
    let labelled: Vec<(String, String)> = reqs.iter().map(|(label, r)| (r.id.clone(), label.clone())).collect();
    if let Some((id, _)) = reqs.iter().map(|(_, r)| (r.id.as_str(), r.name.as_str())).find(|(id, n)| *id == wanted || n.eq_ignore_ascii_case(wanted)) {
        return Ok(id.to_string());
    }
    find_by_name(labelled.iter().map(|(a, b)| (a.as_str(), b.as_str())), wanted)
}

fn print_outcome(prefix: &str, o: &engine::Outcome) {
    let pad = " ".repeat(prefix.chars().count());
    let status = match (&o.response, &o.grpc) {
        (_, Some(g)) => format!("gRPC {} {}", g.status.unwrap_or(0), g.status_name()),
        (Some(r), _) => format!("{} {}", r.status, r.status_text),
        (None, _) => "no response".into(),
    };
    let time = o.response.as_ref().map(|r| format!(" {:.0} ms", r.elapsed_ms)).unwrap_or_default();
    let mark = if o.passed() { "PASS" } else { "FAIL" };
    // What actually went out (for stdio MCP servers, the command line).
    let method_url = o
        .sent
        .as_ref()
        .map(|s| format!("{} {}", s.method, s.url))
        .or_else(|| o.prepared.as_ref().map(|p| format!("{} {}", p.method, p.url)))
        .unwrap_or_default();
    println!("{prefix}{mark}  {}  {status}{time}  {method_url}", o.request_name);
    if let Some(e) = &o.error {
        println!("{pad}      error: {e}");
    }
    for c in &o.checks {
        println!(
            "{pad}      {} {}{}",
            if c.passed { "\u{2713}" } else { "\u{2717}" },
            c.label,
            if c.detail.is_empty() { String::new() } else { format!(" ({})", c.detail) }
        );
    }
    for (_, k, v) in &o.extracted {
        let shown: String = v.chars().take(60).collect();
        println!("{pad}      {{{{{k}}}}} = {shown}{}", if v.chars().count() > 60 { "\u{2026}" } else { "" });
    }
    for w in &o.warnings {
        println!("{pad}      ~ {w}");
    }
    for e in &o.extract_errors {
        println!("{pad}      ! {e}");
    }
    if let Some(f) = &o.fault {
        println!("{pad}      {f}");
    }
}

fn cmd_run(mut args: Args) -> Result<bool, String> {
    let wf = args.value(&["--workflow", "-w"]);
    let rq = args.value(&["--request", "-r"]);
    let env = args.value(&["--env", "-e"]);
    let save = args.flag("--save");
    let path = catalog_arg(&mut args);
    let mut cat = load_catalog(&path)?;
    if let Some(env) = env {
        let id = find_by_name(cat.environments.iter().map(|e| (e.id.as_str(), e.name.as_str())), &env)?;
        cat.active_environment = Some(id);
    }
    let t = NativeTransport;
    // Values from commands (secrets managers): fetched now, never saved.
    let saved_envs = cat.environments.clone();
    let session = std::env::var("BW_SESSION").unwrap_or_default();
    let resolved = pollster::block_on(apicraft_core::sources::resolve(&mut cat, &Default::default(), &session, &t));
    for e in &resolved.errors {
        eprintln!("warning: {e}");
    }
    let ok = match (wf, rq) {
        (Some(w), _) => {
            let id = find_by_name(cat.workflows.iter().map(|w| (w.id.as_str(), w.name.as_str())), &w)?;
            let name = cat.workflow(&id).map(|w| w.name.clone()).unwrap_or_default();
            println!("Workflow {name}");
            let mut outcomes = Vec::new();
            let summary = pollster::block_on(workflow::run(&cat, &id, &t, &mut |r| {
                if let Some(item) = r.item.as_ref().filter(|_| loop_head(&cat, &id, r.index)) {
                    println!("      [{}/{}] {}", item.index + 1, item.count, item.label);
                }
                let prefix = format!("  {:>2}. ", r.index + 1);
                match (&r.outcome, &r.skipped) {
                    (Some(o), _) => {
                        print_outcome(&prefix, o);
                        outcomes.push(o.clone());
                    }
                    (None, Some(why)) => println!("{prefix}SKIP  {} ({why})", r.name),
                    _ => {}
                }
                true
            }))?;
            for o in &outcomes {
                apply_outcome(&mut cat, o);
            }
            println!(
                "\n{} passed, {} failed, {} skipped{} in {:.0} ms",
                summary.passed,
                summary.failed,
                summary.skipped,
                if summary.stopped_early { " (stopped at first failure)" } else { "" },
                summary.elapsed_ms
            );
            summary.failed == 0
        }
        (None, Some(r)) => {
            let id = find_request(&cat, &r)?;
            let o = pollster::block_on(engine::execute(&cat, &id, &[], &[], &[], &t));
            print_outcome("", &o);
            if !o.body_text.is_empty() {
                println!("\n{}", apicraft_core::format::pretty_body(o.response.as_ref().map(|r| r.content_type()).unwrap_or(""), &o.body_text));
            }
            apply_outcome(&mut cat, &o);
            o.passed()
        }
        _ => return Err("run needs --workflow NAME or --request NAME".into()),
    };
    if save {
        // Keep values captured into environments, but not fetched secrets.
        for (env, before) in cat.environments.iter_mut().zip(&saved_envs) {
            for (k, _) in &resolved.fetched {
                match before.variables.iter().find(|kv| &kv.key == k) {
                    Some(orig) => {
                        if let Some(kv) = env.variables.iter_mut().find(|kv| &kv.key == k) {
                            *kv = orig.clone();
                        }
                    }
                    None => env.variables.retain(|kv| &kv.key != k),
                }
            }
        }
        save_catalog(&path, &cat)?;
        println!("Saved variables and tokens to {path}");
    }
    Ok(ok)
}

fn cmd_import(mut args: Args) -> Result<bool, String> {
    let out = args.value(&["-o", "--output"]).unwrap_or_else(|| default_catalog_path().display().to_string());
    let mut headers = Vec::new();
    while let Some(h) = args.value(&["-H", "--header"]) {
        let (k, v) = h.split_once(':').ok_or_else(|| format!("{h} isn't a header: use 'Name: value'"))?;
        headers.push(KeyValue::new(k.trim(), v.trim()));
    }
    let username = args.value(&["--user"]).unwrap_or_default();
    let page = args.value(&["--sign-in-page"]).unwrap_or_default();
    let login = match args.value(&["--password-env"]) {
        Some(var) => Some(FormLogin { page, username, password: std::env::var(&var).map_err(|_| format!("the environment variable {var} isn't set"))? }),
        None => None,
    };
    let input = args.positional().ok_or("import needs a file or URL")?;
    let bytes = if input.starts_with("http://") || input.starts_with("https://") {
        pollster::block_on(engine::fetch_definition(Catalog::default(), None, &input, Auth::None, headers, login, &NativeTransport))?
    } else {
        std::fs::read(&input).map_err(|e| format!("reading {input}: {e}"))?
    };
    let source = (input.starts_with("http://") || input.starts_with("https://")).then_some(input.as_str());
    let imported = store::decode_from(&bytes, source)?;
    let what = imported.describe();
    let mut cat = if Path::new(&out).exists() { load_catalog(&out)? } else { Catalog::default() };
    cat.merge(imported.into_catalog());
    save_catalog(&out, &cat)?;
    println!("Imported {what} into {out}");
    Ok(true)
}

fn cmd_export(mut args: Args) -> Result<bool, String> {
    let rq = args.value(&["--request", "-r"]);
    let wf = args.value(&["--workflow", "-w"]);
    let format = args.value(&["--format", "-f"]).unwrap_or_else(|| "curl".into());
    let path = catalog_arg(&mut args);
    let cat = load_catalog(&path)?;
    let target = match format.to_ascii_lowercase().as_str() {
        "curl" | "sh" | "bash" => Target::Curl,
        "powershell" | "ps" | "ps1" | "pwsh" => Target::PowerShell,
        other => return Err(format!("unknown format {other} (curl or powershell)")),
    };
    let code = match (rq, wf) {
        (Some(rq), None) => codegen::generate(&cat, &find_request(&cat, &rq)?, now_ms(), target)?,
        (None, Some(wf)) => {
            let id = find_by_name(cat.workflows.iter().map(|w| (w.id.as_str(), w.name.as_str())), &wf)?;
            codegen::generate_workflow(&cat, &id, now_ms(), target)?
        }
        _ => return Err("export needs --request NAME or --workflow NAME".into()),
    };
    print!("{code}");
    Ok(true)
}

/// True for the first step of a loop, where each pass is announced.
fn loop_head(cat: &Catalog, workflow_id: &str, index: usize) -> bool {
    cat.workflow(workflow_id).and_then(|w| w.steps.get(index)).is_some_and(|s| s.for_each.is_some())
}
