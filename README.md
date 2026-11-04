# Fizl ApiCraft

Copyright (C) Fizl Ltd & Simon Soanes. Licensed under the [Apache License 2.0](LICENSE).

A Postman/Insomnia-style API client written in Rust. One user interface, built with [egui](https://github.com/emilk/egui), runs as a **native desktop app** on Windows, macOS and Linux, and as **WebAssembly in the browser**. The structure follows the ArtCraft apps (e.g. VectorCraft): an engine with no UI, one UI crate, and thin per-platform apps.

- **Protocols:** REST, GraphQL (POST or GET, variables, operation picker, schema browser from introspection), OData v2/v3/v4 (resource paths, a `$select`/`$expand`/`$filter`/`$orderby`/`$top`/`$skip`/`$count`/`$search` builder that offers fields from `$metadata`, and the version headers), SOAP 1.1/1.2, gRPC (unary and server-streaming, from a pasted `.proto`, with no code generation), WebSocket, and **MCP** servers over stdio, legacy SSE or Streamable HTTP.
- **Catalogs:** collections, folders and requests, plus environments, workflows and captured variables. Stored as **gzip-compressed JSON**: a file on the desktop, browser storage on the web. Both export and import as `.json.gz` or plain `.json` text.
- **Collections as folders of text files:** open a folder, or save a collection as one, and it's kept as `collection.json` plus one pretty-printed `.request.json` file per request (subfolders for folders) and a `.workflow.json` per workflow, ready for version control. Files added or edited by hand are picked up; the desktop app reloads when they change on disk. The CLI accepts such a folder in place of a catalog file.
- **Save as code:** any request as a **curl** or **PowerShell** command (`grpcurl` for gRPC, a `ClientWebSocket` script for WebSockets).
- **Import** (file picker, drag-and-drop, paste, or URL, with auth, headers or a web sign-in form for URLs that need credentials): Postman collections (v2.0/v2.1) and environments, curl commands, catalog files, and:
  - **OpenAPI 3.x / Swagger 2.0** (JSON or YAML). Each operation becomes a request, grouped by tag, with example parameters and bodies built from the schemas, the server URL and path parameters as variables, and security schemes mapped onto auth. Test artifacts are generated: checks per operation (documented status, required fields), a smoke-test workflow over the safe GETs, and CRUD workflows where "create" captures the new id for get/update/delete.
  - **WSDL 1.1:** one SOAP request per operation, with SOAPAction, SOAP version and endpoint from the binding, an example envelope built from the XML Schema, and checks.
  - **OData `$metadata`** (v2-v4, from the URL or a saved file): a folder per entity set with list, get-by-key, create, update (PATCH, or MERGE for v2) and delete requests with example keys and bodies, plus singletons and function/action imports.
  - **GraphQL introspection results** (JSON): one request per query and mutation, with a selection set and example variables. From a GraphQL request, **Load schema** does the same against the live endpoint.

  Collections export back to Postman v2.1.
- **Workflows:** run saved requests in a set order, with per-step delays, extra captures and checks, iterations, and stop-on-failure. A step can **loop over a list** captured earlier (a JSON array, a path such as `$.value` or `$.items[*].id`, or lines of text), repeating it and the next few steps once per item with `{{item}}`, `{{item.field}}` and `{{$index}}`. Exported Bash and PowerShell scripts loop the same way.
- **Variables from responses:** capture rules (JSON path, XML path, header, status, regex) store values as `{{variables}}` for later requests. Postman's dynamic variables also work (`{{$guid}}`, `{{$timestamp}}`, `{{$isoTimestamp}}`, `{{$randomInt}}`).
- **Checks:** status (`200`, `2xx`, `200-204`), body contains, JSON value, header, response time.
- **Auth:** Basic, Digest (MD5/SHA-256, `-sess`, `qop`), Bearer, API key, OAuth 2.0 (client credentials, password, authorization code with PKCE, refresh, automatic renewal), OAuth 1.0a, AWS Signature v4, JWT (HS256/384/512), client certificates (mutual TLS, PEM text or files). Requests can inherit their collection's auth.
- **Network settings:** system, no, or manual proxy (HTTP, HTTPS or SOCKS5, with credentials and a bypass list), turning certificate checks off, and trusting your own CA certificates instead of the built-in roots. They're kept with the catalog, so the desktop app, the agent and the CLI all use them, and generated curl/PowerShell includes them.
- **MCP server:** agents such as Claude Code can drive Fizl ApiCraft (list, send, create and import requests, run workflows); see below.

## Layout

| Path | What it is |
| --- | --- |
| `crates/core` | The engine. Model, storage, Postman/curl/OpenAPI/WSDL/OData/GraphQL import, curl/PowerShell generation, auth, gRPC (protox + prost-reflect), SOAP, the MCP client, captures/checks, request engine, workflow runner. No I/O and no UI; builds natively and for wasm. |
| `crates/ui-egui` | The whole UI (egui), themed with the Nullify.WebMail palette and using Phosphor icons. Talks to its host through the `Platform` trait. |
| `crates/agent` | Native networking (ureq for HTTP/1.1; h2 + rustls for gRPC), native MCP sessions (stdio, SSE), the MCP server, and the `apicraft-agent` CLI: runs collections, and serves/proxies the browser build. |
| `apps/desktop` | `fizl-apicraft`: the native app (eframe + wgpu). It sends requests in-process: no CORS, native gRPC, WebSocket handshake headers, OS file dialogs, and a loopback redirect for OAuth sign-in. |
| `apps/web` | The browser app (eframe web runner, WebGL2). Uses `fetch` or the local agent's proxy. |
| `xtask` | Build, serve, package and check tasks (`cargo xtask help`). |
| `web/` | The HTML shell for the browser build, and the OAuth callback page. |

## Getting started

You need stable Rust. For the browser build, `cargo xtask web` also installs the `wasm32-unknown-unknown` target and `wasm-bindgen-cli` 0.2.129 if they're missing.

```bash
cargo run -p apicraft-desktop
```

That runs the desktop app. For the browser build:

```bash
cargo xtask serve
```

It builds the web app into `dist/web` and serves it with the agent at <http://127.0.0.1:8787/>.

On Linux the desktop app needs the usual windowing libraries; on Debian/Ubuntu: `libxkbcommon-dev libwayland-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libgtk-3-dev`.

### RustRover

Open the folder. The shared run configurations in `.run/` appear in the run menu, each ready to **Run** or **Debug**:

- **Desktop app** and **Desktop app (release)**: the native app. Debug builds keep a console window for logs and panics.
- **Web / Build web app**: builds the wasm bundle into `dist/web`.
- **Web / Web app (agent on :8787)**: builds the web app first (a before-launch step), then runs the agent under the debugger and serves it.
- **CLI / run "Token flow" workflow**: the command-line runner against your catalog.
- **CLI / MCP server on stdio**: the MCP server, under the debugger if you like.
- **Checks / All tests** and **Checks / CI checks**.
- **Release / Package release for this OS** and **Release / Regenerate app icons**.

Breakpoints work in all the native code (engine, UI, agent). The wasm build is debugged in the browser.

## Where your data lives

- **Desktop:** `catalog.json.gz`, saved automatically:
  - Windows: `%APPDATA%\Fizl ApiCraft\`
  - macOS: `~/Library/Application Support/Fizl ApiCraft/`
  - Linux: `~/.local/share/fizl-apicraft/`
- **Browser:** local storage. With the agent running, every change is also mirrored to that same desktop file, and a browser with nothing stored yet loads it from there.
- **CLI and MCP server:** the agent's commands use the same file when you don't name one. The desktop app notices changes made to it from outside and reloads; if you have unsaved edits it keeps yours and tells you.

## MCP

### Testing MCP servers

Choose the **MCP** protocol on a request, then:

- **Pick a transport:**
  - **Streamable HTTP:** the URL, usually ending in `/mcp`. Works in the browser too.
  - **SSE (legacy):** the event-stream URL, usually `/sse`.
  - **stdio:** the command that starts the server, e.g. `npx -y @modelcontextprotocol/server-everything`, plus environment variables.
- **Pick an operation:** initialize, list or call tools, list or read resources, list or get prompts, ping, or any raw JSON-RPC call.
- **Discover:** lists the server's tools, resources and prompts. Clicking a tool fills in example arguments from its input schema.
- **Results:** each Send runs a fresh session (initialize, `notifications/initialized`, then the call) and shows the result as JSON. Server info and session details appear as headers, and server notifications and stderr as trailers. Captures (e.g. `$.content[0].text`), checks and workflows work as for any request.
- **Save as code:** Streamable HTTP gives a curl/PowerShell script that does the handshake and keeps the session id; stdio and SSE give the official MCP Inspector CLI command.
- **Where it runs:** stdio and SSE need the desktop app or the local agent; the browser routes them through the agent.

**Test mode:** the starter catalog's **MCP** folder exercises Fizl ApiCraft's own MCP server over all three transports, plus the reference "everything" server (needs Node.js).

### Letting an agent drive Fizl ApiCraft

`apicraft-agent mcp` is an MCP server on stdio. Its tools:

- `list_requests`, `get_request`
- `send_request`, `run_workflow`
- `create_request`, `update_request`, `delete_request`
- `import_definition` (OpenAPI, WSDL, OData $metadata, GraphQL introspection, Postman or curl, from text or a URL)
- `list_environments`, `set_environment`, `set_variable`
- `export_code`

The tools work on the same catalog file as the desktop app, which picks up their changes within a couple of seconds. For Claude Code:

```bash
claude mcp add fizl-apicraft -- apicraft-agent mcp
```

While `apicraft-agent serve` runs, the same server is also at `http://127.0.0.1:8787/mcp` (Streamable HTTP) and `/sse` (legacy SSE). These endpoints check the Host and Origin headers, so web pages can't reach them.

## Command line

```bash
apicraft-agent run --workflow "Token flow" --env httpbin
apicraft-agent run --request "Get with query"
apicraft-agent import my.postman_collection.json
apicraft-agent import https://petstore3.swagger.io/api/v3/openapi.json
apicraft-agent import https://api.example.com/swagger.json -H "Authorization: Bearer $TOKEN"
apicraft-agent import https://app.example.com/swagger/v1/swagger.json --user me --password-env APP_PASSWORD
apicraft-agent import "http://www.dneonline.com/calculator.asmx?WSDL"
apicraft-agent mcp
apicraft-agent export --request "Post JSON" --format powershell
apicraft-agent serve --web dist/web
```

`run` exits with code 1 if a step fails, so it can gate CI. `--save` writes captured variables and refreshed OAuth tokens back to the catalog.

## Building releases

```bash
cargo xtask dist
```

It builds release versions of everything and packages them for the OS it runs on:

- **Windows:** a zip with `fizl-apicraft.exe` (icon embedded), `apicraft-agent.exe` and `web/`, and an `.msi` installer of the same files. The installer adds a Start menu shortcut and, optionally, puts `apicraft-agent` on the system `PATH`; installing a newer version replaces the old one. It's built with the [WiX Toolset](https://wixtoolset.org/) v5, pinned as a local .NET tool in `dotnet-tools.json`, so it needs the .NET SDK (without it you just get the zip). The installer source is `apps/desktop/installer/fizl-apicraft.wxs`.
- **macOS:** `Fizl ApiCraft.app`, zipped.
- **Linux:** a tarball with a `.desktop` file and an icon.

`.github/workflows/build.yml` runs the checks and builds all three in CI; Gitea Actions reads that folder too. Run `cargo xtask ci` for the same checks locally: formatting, clippy, tests and a wasm build.

### Hosting the web app with Docker

```bash
docker build -t fizl-apicraft-web .
docker run --rm -p 8080:8080 fizl-apicraft-web
```

The image is just the browser build (release wasm, run through `wasm-opt`) on `nginx-unprivileged`: it runs as a non-root user on port 8080 and serves pre-compressed files. CI builds it for `linux/amd64` and `linux/arm64` and pushes it to `ghcr.io/<owner>/fizl-apicraft-web` from `master` and `v*` tags.

The agent isn't in the image, because it's a local proxy that can start processes and isn't meant to be reachable over a network. Requests from the hosted app go straight from the browser, with the CORS limits described below. To lift them, a user runs the agent on their own machine and lets the hosted page use it:

```bash
apicraft-agent serve --allow-origin https://apicraft.example.com
```

## How values flow between requests

Variables resolve in this order, last wins: collection, workflow, active environment, **runtime**. Runtime variables are the values captured from responses. They persist with the catalog until you clear them from the `{x}` chip in the top bar. A capture rule can write into the active environment instead.

In the starter catalog, **Log in** captures `$.json.token` as `token`. **Use the token** then sends `Authorization: Bearer {{token}}` and checks that the server echoed it. The **Token flow** workflow runs the two in order.

Postman test scripts aren't executed. Common patterns are converted on import:

- `pm.environment.set("x", pm.response.json().a.b)`
- `jsonData.x`-style variables
- `pm.response.headers.get(...)`
- `pm.response.to.have.status(200)`
- `responseTime` limits
- `text().include(...)`

Anything else is kept in the request's Docs tab.

## Notes and limits

- **Browser:** requests from the browser build are subject to CORS unless the agent is running. Browsers can't send custom WebSocket headers. gRPC from a browser uses gRPC-Web. stdio and SSE MCP servers need the agent; starting stdio processes is refused to any page other than the agent's own.
- **Imports:** WSDL 2.0 and external schema imports (`xsd:import`) aren't followed. OpenAPI `$ref`s to other files aren't followed.
- **MCP:** each request is its own short session; server-initiated requests (sampling, roots, elicitation) are declined.
- **GraphQL:** subscriptions aren't supported (they need a WebSocket); schemas come from introspection, not SDL files.
- **OData:** `$batch` requests aren't built for you; send them as raw multipart bodies.
- **gRPC:** client-streaming and bidirectional calls aren't supported. Proto imports other than `google/protobuf/*` must be inlined.
- **Multipart bodies:** text fields only.
- **Other auth:** NTLM and Kerberos aren't supported. Client certificate keys must be unencrypted PEM (no PKCS#12 or passphrases).
- **Network settings:** the browser build ignores them unless requests go through the agent. The proxy applies to HTTP requests, not to native gRPC, WebSockets or MCP sessions (certificate settings do apply to native gRPC).
- **Loops:** loops don't nest, and a loop covers a run of consecutive steps.
- **Generated scripts:** signatures for OAuth 1.0a, AWS SigV4 and JWT are computed when the script is generated, so they expire. For AWS, curl uses its own `--aws-sigv4` instead.
- **Fonts and icons:** Phosphor icons (MIT) are bundled in `crates/ui-egui/assets`. The desktop app uses Segoe UI on Windows.

## Licence

Copyright (C) Fizl Ltd & Simon Soanes

Licensed under the Apache License, Version 2.0 (the "License"); you may not use this software except in compliance with the License. You may obtain a copy of the License in [LICENSE](LICENSE) or at <http://www.apache.org/licenses/LICENSE-2.0>.

Unless required by applicable law or agreed to in writing, software distributed under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the License for the specific language governing permissions and limitations under the License.

Third-party notices are in [NOTICE](NOTICE). The bundled Phosphor icon font is MIT-licensed (`crates/ui-egui/assets/Phosphor-LICENSE-MIT.txt`).
