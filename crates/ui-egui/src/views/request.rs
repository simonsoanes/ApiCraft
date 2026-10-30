//! The request editor: URL bar, tabs, and the response pane beside it.

use apicraft_core::codegen::Target;
use apicraft_core::engine::ApiSchema;
use apicraft_core::model::*;
use apicraft_core::{graphql, grpc, odata};
use egui::{Align, Frame, Layout, Margin, RichText, TextEdit, Ui};

use super::widgets::{self, accent_button, alert, help, kv_table, AlertKind, AuthCtx};
use super::{response, topbar};
use crate::actions::{self, Action};
use crate::app::{ApiCraftApp, Modal, ProtoCache, ReqTab, RespTab};
use crate::highlight::Lang;
use crate::icons;
use crate::theme::{method_color, pal};

const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

pub fn show(app: &mut ApiCraftApp, ui: &mut Ui, id: &str) {
    // Breadcrumb from the collection and folders above the request.
    let crumbs: Vec<(String, Option<String>)> = actions::ancestors_of(&app.catalog, id)
        .iter()
        .map(|a| match app.catalog.collection(a) {
            Some(c) => (c.name.clone(), Some(c.id.clone())),
            None => (actions::folder_name(&app.catalog, a).unwrap_or_default(), None),
        })
        .collect();
    topbar(app, ui, |app, ui| {
        let p = pal(ui);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (i, (name, col)) in crumbs.iter().enumerate() {
                if i > 0 {
                    ui.label(RichText::new("/").size(11.5).color(p.faint));
                }
                match col {
                    Some(cid) => {
                        if ui
                            .add(egui::Label::new(RichText::new(name).size(11.5).color(p.muted)).sense(egui::Sense::click()))
                            .on_hover_text("Collection variables & auth")
                            .clicked()
                        {
                            app.actions.push(Action::CollectionSettings(cid.clone()));
                        }
                    }
                    None => {
                        ui.label(RichText::new(name).size(11.5).color(p.muted));
                    }
                }
            }
        });
        if let Some(r) = app.catalog.find_request_mut(id) {
            ui.add(
                TextEdit::singleline(&mut r.name)
                    .font(egui::FontId::proportional(17.0))
                    .frame(egui::Frame::NONE)
                    .desired_width(ui.available_width().min(520.0))
                    .margin(Margin::ZERO),
            );
        }
    });
    urlbar(app, ui, id);

    let p = pal(ui);
    let wide = ui.available_width() > 1000.0;
    let pane = Frame::new().fill(p.surface).inner_margin(Margin { left: 14, right: 14, top: 10, bottom: 10 });
    if wide {
        let w = ui.available_width();
        egui::Panel::right("response-side")
            .resizable(true)
            .default_size((w * 0.47).clamp(340.0, (w - 360.0).max(340.0)))
            .size_range(340.0..=(w - 360.0).max(340.0))
            .frame(pane)
            .show(ui, |ui| widgets::fixed_size(ui, |ui| response::show(app, ui, id)));
    } else {
        let h = ui.available_height();
        egui::Panel::bottom("response-below")
            .resizable(true)
            .default_size((h * 0.5).clamp(160.0, (h - 160.0).max(160.0)))
            .size_range(160.0..=(h - 160.0).max(160.0))
            .frame(pane)
            .show(ui, |ui| widgets::fixed_size(ui, |ui| response::show(app, ui, id)));
    }
    egui::CentralPanel::default().frame(pane).show(ui, |ui| editor(app, ui, id));
}

fn urlbar(app: &mut ApiCraftApp, ui: &mut Ui, id: &str) {
    let p = pal(ui);
    let sending = app.sending.contains(id);
    let ws_live = app.ws.as_ref().map(|w| w.request_id == id && !w.closed).unwrap_or(false);
    let mut new_tab = None;
    Frame::new().inner_margin(Margin { left: 14, right: 14, top: 10, bottom: 10 }).show(ui, |ui| {
        // Wrapped: protocol, method, the URL field, Send and Code together are
        // wider than the view in a narrow window, and a plain horizontal lets
        // them run off the right edge instead of starting a second row.
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let Some(r) = app.catalog.find_request_mut(id) else { return };
            // protocol
            let mut proto = r.protocol;
            egui::ComboBox::from_id_salt("protocol").selected_text(RichText::new(proto.label()).strong()).width(104.0).show_ui(ui, |ui| {
                for pr in Protocol::ALL {
                    ui.selectable_value(&mut proto, pr, pr.label());
                }
            });
            if proto != r.protocol {
                actions::switch_protocol(r, proto);
                new_tab = Some(match proto {
                    Protocol::Soap | Protocol::Grpc => ReqTab::Body,
                    Protocol::WebSocket => ReqTab::Messages,
                    Protocol::Mcp => ReqTab::Mcp,
                    Protocol::GraphQl => ReqTab::GraphQl,
                    Protocol::OData => ReqTab::OData,
                    Protocol::Rest => ReqTab::Params,
                });
            }
            // method
            let methods: &[&str] = match r.protocol {
                Protocol::Rest => &METHODS,
                // v2 updates entities with MERGE.
                Protocol::OData if r.odata.version == ODataVersion::V2 => &["GET", "POST", "PUT", "MERGE", "PATCH", "DELETE"],
                Protocol::OData => &["GET", "POST", "PUT", "PATCH", "DELETE"],
                Protocol::GraphQl => &["POST", "GET"],
                _ => &[],
            };
            if !methods.is_empty() {
                let color = method_color(p, Protocol::Rest, &r.method);
                let combo = egui::ComboBox::from_id_salt("method")
                    .selected_text(RichText::new(r.method.to_uppercase()).monospace().strong().color(color))
                    .width(92.0)
                    .show_ui(ui, |ui| {
                        for m in methods {
                            let c = method_color(p, Protocol::Rest, m);
                            ui.selectable_value(&mut r.method, m.to_string(), RichText::new(*m).monospace().strong().color(c));
                        }
                    })
                    .response;
                if r.protocol == Protocol::GraphQl {
                    combo.on_hover_text("POST sends a JSON body; GET puts the query in the URL (queries only)");
                }
            }
            let hint = match r.protocol {
                Protocol::Rest => "https://api.example.com/v1/items   ({{variables}} work here)",
                Protocol::Soap => "https://example.com/Service.asmx",
                Protocol::Grpc => "https://grpc.example.com:443   (server address; the method path is added)",
                Protocol::WebSocket => "wss://echo.example.com/socket",
                Protocol::Mcp if r.mcp.transport == McpTransport::Sse => "http://127.0.0.1:8787/sse   (the server's event stream)",
                Protocol::Mcp => "http://127.0.0.1:8787/mcp   (the server's MCP endpoint)",
                Protocol::GraphQl => "https://api.example.com/graphql",
                Protocol::OData => "https://services.odata.org/V4/TripPinServiceRW   (service root)",
            };
            let url_w = (ui.available_width() - 210.0).max(160.0);
            if r.protocol == Protocol::Mcp && r.mcp.transport == McpTransport::Stdio {
                // stdio servers are started by a command instead of reached at a URL.
                ui.add(
                    TextEdit::singleline(&mut r.mcp.command)
                        .hint_text("npx -y @modelcontextprotocol/server-everything   (command that starts the server)")
                        .font(egui::TextStyle::Monospace)
                        .desired_width(url_w)
                        .margin(Margin::symmetric(8, 7)),
                );
            } else {
                ui.add(TextEdit::singleline(&mut r.url).hint_text(hint).font(egui::TextStyle::Monospace).desired_width(url_w).margin(Margin::symmetric(8, 7)));
            }
            let is_ws = r.protocol == Protocol::WebSocket;
            // send / connect
            ui.scope(|ui| {
                ui.spacing_mut().button_padding = egui::vec2(16.0, 7.0);
                if is_ws {
                    if ws_live {
                        if widgets::danger_button(ui, format!("{} Disconnect", icons::X)).clicked() {
                            app.actions.push(Action::WsDisconnect);
                        }
                    } else if accent_button(ui, format!("{} Connect", icons::PLUGS_CONNECTED)).on_hover_text("Ctrl+Enter").clicked() {
                        app.actions.push(Action::WsConnect(id.to_string()));
                    }
                } else if sending {
                    if ui.button(format!("{} Cancel", icons::STOP)).clicked() {
                        app.actions.push(Action::Cancel(id.to_string()));
                    }
                } else if accent_button(ui, format!("{} Send", icons::SEND)).on_hover_text("Ctrl+Enter").clicked() {
                    app.actions.push(Action::Send(id.to_string()));
                }
            });
            ui.menu_button("</> Code", |ui| {
                ui.set_min_width(200.0);
                let rid = id.to_string();
                if ui.button(format!("{}  Copy as curl", icons::COPY)).clicked() {
                    app.actions.push(Action::CopyCode(rid.clone(), Target::Curl));
                    ui.close();
                }
                if ui.button(format!("{}  Copy as PowerShell", icons::COPY)).clicked() {
                    app.actions.push(Action::CopyCode(rid.clone(), Target::PowerShell));
                    ui.close();
                }
                ui.separator();
                if ui.button(format!("{}  Show code…", icons::CODE)).clicked() {
                    app.modal = Some(Modal::Code { request_id: rid.clone(), curl: true });
                    ui.close();
                }
                if ui.button(format!("{}  Save as .sh", icons::FLOPPY_DISK)).clicked() {
                    app.actions.push(Action::SaveCode(rid.clone(), Target::Curl));
                    ui.close();
                }
                if ui.button(format!("{}  Save as .ps1", icons::FLOPPY_DISK)).clicked() {
                    app.actions.push(Action::SaveCode(rid, Target::PowerShell));
                    ui.close();
                }
            });
        });
    });
    if let Some(t) = new_tab {
        app.req_tab = t;
        app.resp_tab = RespTab::Body;
    }
    ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), egui::Stroke::new(1.0, p.border));
}

/// The "magic" headers worked out when the request is sent.
fn auto_headers(ui: &mut Ui, o: &mut SendOptions, headers: &[KeyValue]) {
    let p = pal(ui);
    let mine = |name: &str| headers.iter().any(|h| h.enabled && h.key.trim().eq_ignore_ascii_case(name));
    Frame::new().fill(p.surface2).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(Margin::symmetric(10, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Automatic headers").strong().size(12.5));
        help(ui, "Worked out at the moment the request is sent.");
        ui.add_space(2.0);
        let mono = |t: &str| RichText::new(t).monospace();
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut o.content_length, mono("Content-Length"));
            ui.label(RichText::new("the size of the body as sent (after compression)").size(12.0).color(p.muted));
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut o.accept_compressed, mono("Accept-Encoding: gzip, deflate"));
            ui.label(RichText::new("asks for a compressed response, and unpacks it").size(12.0).color(p.muted));
        });
        ui.horizontal_wrapped(|ui| {
            let mut on = o.compress != Compression::None;
            if ui.checkbox(&mut on, mono("Content-Encoding")).changed() {
                o.compress = if on { Compression::Gzip } else { Compression::None };
            }
            ui.add_enabled_ui(on, |ui| {
                egui::ComboBox::from_id_salt("compress").selected_text(if on { o.compress.label() } else { "gzip" }).width(90.0).show_ui(ui, |ui| {
                    for c in [Compression::Gzip, Compression::Deflate] {
                        ui.selectable_value(&mut o.compress, c, c.label());
                    }
                });
            });
            ui.label(RichText::new("compresses the body before sending").size(12.0).color(p.muted));
        });
        let clash: Vec<&str> =
            [("Content-Length", o.content_length), ("Accept-Encoding", o.accept_compressed), ("Content-Encoding", o.compress != Compression::None)]
                .into_iter()
                .filter(|(h, on)| *on && mine(h))
                .map(|(h, _)| h)
                .collect();
        if !clash.is_empty() {
            let note = if clash.contains(&"Accept-Encoding") && clash.len() == 1 {
                "Your own Accept-Encoding header below is sent instead of the automatic one.".to_string()
            } else {
                format!("The automatic value replaces your own {} header below.", clash.join(" and "))
            };
            ui.label(RichText::new(note).size(12.0).color(p.warn_text));
        }
    });
    ui.add_space(8.0);
}

fn auto_count(o: &SendOptions) -> usize {
    [o.content_length, o.accept_compressed, o.compress != Compression::None].iter().filter(|b| **b).count()
}

fn count(kvs: &[KeyValue]) -> usize {
    kvs.iter().filter(|k| k.enabled && !k.key.is_empty()).count()
}

fn editor(app: &mut ApiCraftApp, ui: &mut Ui, id: &str) {
    // Read-only context gathered before borrowing the request mutably.
    let (col_id, inherited_label) = match app.catalog.find_request(id) {
        Some((c, _)) => (c.id.clone(), Auth::KINDS.iter().find(|(k, _)| *k == c.auth.key()).map(|(_, l)| l.to_string()).unwrap_or_default()),
        None => return,
    };
    let native = app.platform.native_grpc();
    let ws_headers = app.platform.ws_sends_headers();
    let default_redirect = app.platform.default_redirect_uri();
    let now = app.platform.now_ms();
    let last_extracted = app.outcomes.get(id).map(|o| o.extracted.clone());
    let ws_open = app.ws.as_ref().map(|w| w.request_id == id && w.open).unwrap_or(false);

    let ApiCraftApp { catalog, highlighter, actions, req_tab, proto_cache, mcp_catalogs, schemas, discovering, .. } = app;
    let Some(r) = catalog.find_request_mut(id) else { return };

    let mut items: Vec<(ReqTab, String, usize)> = Vec::new();
    match r.protocol {
        Protocol::Rest => {
            items.push((ReqTab::Params, "Params".into(), count(&r.params)));
            items.push((ReqTab::Headers, "Headers".into(), count(&r.headers) + auto_count(&r.send)));
            items.push((ReqTab::Body, "Body".into(), 0));
        }
        Protocol::Soap => {
            items.push((ReqTab::Body, "Envelope".into(), 0));
            items.push((ReqTab::Soap, "SOAP".into(), 0));
            items.push((ReqTab::Headers, "Headers".into(), count(&r.headers)));
            items.push((ReqTab::Params, "Params".into(), count(&r.params)));
        }
        Protocol::Grpc => {
            items.push((ReqTab::Body, "Message".into(), 0));
            items.push((ReqTab::Proto, "Service".into(), 0));
            items.push((ReqTab::Headers, "Metadata".into(), count(&r.headers)));
        }
        Protocol::WebSocket => {
            items.push((ReqTab::Messages, "Messages".into(), r.websocket.messages.len()));
            items.push((ReqTab::Params, "Params".into(), count(&r.params)));
            items.push((ReqTab::Headers, "Headers".into(), count(&r.headers)));
        }
        Protocol::Mcp => {
            items.push((ReqTab::Mcp, "MCP".into(), 0));
            items.push((ReqTab::Body, "Arguments".into(), 0));
            if r.mcp.transport != McpTransport::Stdio {
                items.push((ReqTab::Headers, "Headers".into(), count(&r.headers)));
            }
        }
        Protocol::GraphQl => {
            items.push((ReqTab::GraphQl, "Query".into(), 0));
            items.push((ReqTab::Schema, "Schema".into(), 0));
            items.push((ReqTab::Headers, "Headers".into(), count(&r.headers)));
        }
        Protocol::OData => {
            items.push((ReqTab::OData, "Query".into(), 0));
            if !matches!(r.method.as_str(), "GET" | "HEAD" | "DELETE") {
                items.push((ReqTab::Body, "Body".into(), 0));
            }
            items.push((ReqTab::Headers, "Headers".into(), count(&r.headers)));
            items.push((ReqTab::Params, "Params".into(), count(&r.params)));
        }
    }
    items.push((ReqTab::Auth, "Auth".into(), 0));
    if r.protocol != Protocol::WebSocket {
        items.push((ReqTab::Capture, "Capture".into(), r.extract.len()));
        items.push((ReqTab::Checks, "Checks".into(), r.checks.len()));
    }
    items.push((ReqTab::Docs, "Docs".into(), 0));
    if !items.iter().any(|(t, _, _)| t == req_tab) {
        *req_tab = items[0].0;
    }
    widgets::tabs(ui, req_tab, &items);

    egui::ScrollArea::vertical().id_salt(("editor", id)).auto_shrink([false, false]).show(ui, |ui| {
        let p = pal(ui);
        match *req_tab {
            ReqTab::Params => {
                kv_table(ui, "params", &mut r.params, "parameter", "value");
                if r.protocol == Protocol::WebSocket {
                    help(ui, "Query parameters are added to the WebSocket URL.");
                }
                if r.protocol == Protocol::OData {
                    help(ui, "Custom query options (such as sap-client), added after the system options from the Query tab.");
                }
            }
            ReqTab::Headers => {
                if matches!(r.protocol, Protocol::Rest | Protocol::Soap | Protocol::GraphQl | Protocol::OData) {
                    auto_headers(ui, &mut r.send, &r.headers);
                }
                kv_table(ui, "headers", &mut r.headers, "Header", "value");
                help(
                    ui,
                    match r.protocol {
                        Protocol::WebSocket if !ws_headers => {
                            "Browsers can't set custom headers on a WebSocket; they're used by the desktop app and in generated curl/PowerShell."
                        }
                        Protocol::WebSocket => "Sent with the WebSocket handshake.",
                        Protocol::Grpc => "Sent as gRPC metadata. Content-Type and TE are set for you.",
                        Protocol::Soap => "Content-Type and SOAPAction are set from the SOAP tab.",
                        Protocol::Rest => "Content-Type is set from the body when you don't set it here.",
                        Protocol::Mcp => "Sent with every HTTP request to the server. Content-Type, Accept and the MCP session headers are set for you.",
                        Protocol::GraphQl => "Content-Type and Accept are set for you. Also used when loading the schema.",
                        Protocol::OData => {
                            "OData-Version (DataServiceVersion for v2/v3) and Accept: application/json are set for you unless you set them here."
                        }
                    },
                );
            }
            ReqTab::Body => body_tab(ui, r, highlighter),
            ReqTab::Mcp => mcp_tab(ui, r, mcp_catalogs.get(id), discovering.contains(id), actions, id),
            ReqTab::GraphQl => graphql_tab(ui, r, highlighter),
            ReqTab::Schema => {
                if let Some(t) = schema_tab(ui, r, schemas.get(id), discovering.contains(id), actions, id) {
                    *req_tab = t;
                }
            }
            ReqTab::OData => odata_tab(ui, r, schemas.get(id), discovering.contains(id), actions, id),
            ReqTab::Soap => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("SOAP version").strong());
                    let mut v = r.soap.version;
                    widgets::segmented(ui, &mut v, &[(SoapVersion::V11, "1.1"), (SoapVersion::V12, "1.2")]);
                    if v != r.soap.version {
                        let text = r.body.raw_text().replace(apicraft_core::soap::namespace(r.soap.version), apicraft_core::soap::namespace(v));
                        r.body = Body::Raw { content_type: String::new(), text };
                        r.soap.version = v;
                    }
                });
                help(ui, "1.1 sends text/xml + SOAPAction; 1.2 sends application/soap+xml; action=\u{2026}");
                ui.add_space(8.0);
                ui.label(RichText::new("SOAPAction").strong());
                ui.add(TextEdit::singleline(&mut r.soap.action).hint_text("http://tempuri.org/Add").desired_width(f32::INFINITY));
                help(ui, "Find it in the WSDL's soapAction attribute; some services accept it empty.");
            }
            ReqTab::Proto => proto_tab(ui, r, proto_cache, highlighter, native),
            ReqTab::Messages => {
                help(ui, "Saved messages can be sent with one click once connected. {{variables}} are substituted when sending.");
                ui.add_space(4.0);
                let mut remove = None;
                for (i, m) in r.websocket.messages.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        let w = ui.available_width() - 110.0;
                        ui.add_sized([w, 44.0], TextEdit::multiline(m).id_salt(("wsmsg", i)).code_editor().desired_rows(2));
                        ui.vertical(|ui| {
                            if ui.add_enabled(ws_open, egui::Button::new(format!("{} Send", icons::SEND))).clicked() {
                                actions.push(Action::WsSend(m.clone()));
                            }
                            if widgets::icon_button(ui, icons::TRASH, "Remove").clicked() {
                                remove = Some(i);
                            }
                        });
                    });
                }
                if let Some(i) = remove {
                    r.websocket.messages.remove(i);
                }
                if ui.button(format!("{} Add message", icons::PLUS)).clicked() {
                    r.websocket.messages.push(String::new());
                }
                ui.add_space(10.0);
                ui.label(RichText::new("Subprotocols").strong());
                let mut sp = r.websocket.subprotocols.join(", ");
                if ui.add(TextEdit::singleline(&mut sp).hint_text("graphql-ws, mqtt (comma separated)").desired_width(f32::INFINITY)).changed() {
                    r.websocket.subprotocols = sp.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                }
            }
            ReqTab::Auth => {
                let cx = AuthCtx {
                    owner: id,
                    allow_inherit: true,
                    inherited: Some((col_id.as_str(), inherited_label.clone())),
                    native,
                    default_redirect: default_redirect.clone(),
                    now,
                };
                let mut open_col = None;
                widgets::auth_editor(ui, &mut r.auth, &cx, actions, &mut open_col);
                if let Some(c) = open_col {
                    actions.push(Action::CollectionSettings(c));
                }
            }
            ReqTab::Capture => {
                let default_source = if r.protocol == Protocol::Soap { ExtractSource::Xml } else { ExtractSource::Json };
                widgets::capture_table(ui, "capture", &mut r.extract, last_extracted.as_deref(), default_source);
            }
            ReqTab::Checks => widgets::checks_table(ui, "checks", &mut r.checks),
            ReqTab::Docs => {
                ui.add(TextEdit::multiline(&mut r.description).hint_text("Notes about this request").desired_rows(14).desired_width(f32::INFINITY));
            }
        }
        let _ = p;
    });
}

fn mcp_tab(ui: &mut Ui, r: &mut Request, discovered: Option<&Result<apicraft_core::mcp::McpCatalog, String>>, busy: bool, actions: &mut Vec<Action>, id: &str) {
    let p = pal(ui);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Transport").strong());
        let items: Vec<(McpTransport, &str)> = McpTransport::ALL.iter().map(|(t, _, l)| (*t, *l)).collect();
        widgets::segmented(ui, &mut r.mcp.transport, &items);
    });
    help(
        ui,
        match r.mcp.transport {
            McpTransport::Http => "Streamable HTTP: JSON-RPC POSTs to the URL above (usually ending in /mcp). Works in the browser too.",
            McpTransport::Sse => "The older HTTP+SSE transport: the URL above is the event stream (usually /sse). Desktop app or local agent.",
            McpTransport::Stdio => "The command above starts the server as a local process, fresh for each request. Desktop app or local agent.",
        },
    );
    if r.mcp.transport == McpTransport::Stdio {
        ui.add_space(6.0);
        ui.label(RichText::new("Environment variables for the process").strong());
        kv_table(ui, "mcp-env", &mut r.mcp.env, "NAME", "value");
    }
    ui.add_space(10.0);

    // Operation and target
    let catalog = discovered.and_then(|d| d.as_ref().ok());
    ui.horizontal(|ui| {
        ui.label(RichText::new("Operation").strong());
        egui::ComboBox::from_id_salt("mcp-op").selected_text(r.mcp.operation.label()).width(200.0).show_ui(ui, |ui| {
            for (op, _, label) in McpOperation::ALL {
                ui.selectable_value(&mut r.mcp.operation, op, label);
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if busy {
                ui.spinner();
            } else if ui.button(format!("{} Discover", icons::MAGNIFYING_GLASS)).on_hover_text("List the server's tools, resources and prompts").clicked() {
                actions.push(Action::McpDiscover(id.to_string()));
            }
        });
    });
    if let Some(label) = r.mcp.operation.target_label() {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).strong());
            let names: Vec<String> = match (r.mcp.operation, catalog) {
                (McpOperation::CallTool, Some(c)) => c.tools.iter().map(|t| t.0.clone()).collect(),
                (McpOperation::ReadResource, Some(c)) => c.resources.iter().map(|t| t.0.clone()).collect(),
                (McpOperation::GetPrompt, Some(c)) => c.prompts.iter().map(|t| t.0.clone()).collect(),
                _ => Vec::new(),
            };
            ui.add(
                TextEdit::singleline(&mut r.mcp.target)
                    .hint_text(if names.is_empty() { "name (Discover fills a list)" } else { "name" })
                    .desired_width(240.0)
                    .font(egui::TextStyle::Monospace),
            );
            if !names.is_empty() {
                egui::ComboBox::from_id_salt("mcp-target").selected_text("Choose\u{2026}").width(110.0).show_ui(ui, |ui| {
                    for n in names {
                        let on = r.mcp.target == n;
                        if ui.selectable_label(on, &n).clicked() {
                            r.mcp.target = n;
                        }
                    }
                });
            }
            if r.mcp.operation == McpOperation::CallTool {
                if let Some((_, _, schema)) = catalog.and_then(|c| c.tools.iter().find(|t| t.0 == r.mcp.target)) {
                    if ui.button("Example arguments").on_hover_text("Fill the Arguments tab from the tool's input schema").clicked() {
                        let ex = apicraft_core::mcp::schema_example(schema);
                        r.body = Body::Raw { content_type: "application/json".into(), text: serde_json::to_string_pretty(&ex).unwrap_or_default() };
                    }
                }
            }
        });
    }

    // What discovery found
    match discovered {
        Some(Err(e)) => {
            ui.add_space(8.0);
            alert(ui, AlertKind::Error, e);
        }
        Some(Ok(c)) => {
            ui.add_space(10.0);
            ui.label(RichText::new(if c.server.is_empty() { "Server".to_string() } else { c.server.clone() }).strong());
            let mut pick: Option<(McpOperation, String, Option<serde_json::Value>)> = None;
            egui::CollapsingHeader::new(format!("Tools ({})", c.tools.len())).id_salt("mcp-tools").default_open(true).show(ui, |ui| {
                for (name, desc, schema) in &c.tools {
                    ui.horizontal_wrapped(|ui| {
                        if ui.link(RichText::new(name).monospace()).on_hover_text("Call this tool").clicked() {
                            pick = Some((McpOperation::CallTool, name.clone(), Some(schema.clone())));
                        }
                        ui.label(RichText::new(desc).size(12.0).color(p.muted));
                    });
                }
            });
            if !c.resources.is_empty() {
                egui::CollapsingHeader::new(format!("Resources ({})", c.resources.len())).id_salt("mcp-res").show(ui, |ui| {
                    for (uri, name) in &c.resources {
                        ui.horizontal_wrapped(|ui| {
                            if ui.link(RichText::new(uri).monospace()).on_hover_text("Read this resource").clicked() {
                                pick = Some((McpOperation::ReadResource, uri.clone(), None));
                            }
                            ui.label(RichText::new(name).size(12.0).color(p.muted));
                        });
                    }
                });
            }
            if !c.prompts.is_empty() {
                egui::CollapsingHeader::new(format!("Prompts ({})", c.prompts.len())).id_salt("mcp-prompts").show(ui, |ui| {
                    for (name, desc, args) in &c.prompts {
                        ui.horizontal_wrapped(|ui| {
                            if ui.link(RichText::new(name).monospace()).on_hover_text("Get this prompt").clicked() {
                                let obj: serde_json::Map<String, serde_json::Value> = args.iter().map(|a| (a.clone(), serde_json::json!(""))).collect();
                                pick = Some((McpOperation::GetPrompt, name.clone(), Some(serde_json::Value::Object(obj))));
                            }
                            ui.label(RichText::new(desc).size(12.0).color(p.muted));
                        });
                    }
                });
            }
            if let Some((op, target, schema_or_args)) = pick {
                r.mcp.operation = op;
                r.mcp.target = target;
                if let Some(v) = schema_or_args {
                    let ex = if op == McpOperation::CallTool { apicraft_core::mcp::schema_example(&v) } else { v };
                    r.body = Body::Raw { content_type: "application/json".into(), text: serde_json::to_string_pretty(&ex).unwrap_or_default() };
                }
            }
        }
        None => {}
    }
}

fn body_tab(ui: &mut Ui, r: &mut Request, hl: &crate::highlight::Highlighter) {
    match r.protocol {
        Protocol::Mcp => {
            help(
                ui,
                match r.mcp.operation {
                    McpOperation::CallTool => "The tool's arguments, as a JSON object.",
                    McpOperation::GetPrompt => "The prompt's arguments, as a JSON object of strings.",
                    McpOperation::Raw => "The whole call: {\"method\": \"...\", \"params\": {...}}",
                    _ => "Not used by this operation.",
                },
            );
            raw_editor(ui, r, Lang::Json, hl);
            return;
        }
        Protocol::Soap => {
            ui.horizontal(|ui| {
                help(
                    ui,
                    if r.soap.version == SoapVersion::V12 {
                        "SOAP 1.2 envelope. A bare payload is wrapped when sent."
                    } else {
                        "SOAP 1.1 envelope. A bare payload is wrapped when sent."
                    },
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Beautify").clicked() {
                        actions::beautify(r);
                    }
                    if ui.button("Wrap in envelope").clicked() {
                        let wrapped = apicraft_core::soap::ensure_envelope(r.body.raw_text(), r.soap.version);
                        r.body = Body::Raw { content_type: String::new(), text: wrapped };
                    }
                });
            });
            raw_editor(ui, r, Lang::Xml, hl);
            return;
        }
        Protocol::Grpc => {
            ui.horizontal(|ui| {
                help(ui, "Request message as JSON (proto3 JSON mapping).");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Beautify").clicked() {
                        actions::beautify(r);
                    }
                    if ui.button("Example from proto").clicked() {
                        match grpc::compile(&r.grpc.proto)
                            .and_then(|pool| grpc::method(&pool, &r.grpc.service, &r.grpc.method).map(|m| grpc::example_json(&m.input())))
                        {
                            Ok(json) => r.body = Body::Raw { content_type: "application/json".into(), text: json },
                            Err(e) => {
                                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("grpc-example-error"), e));
                            }
                        }
                    }
                });
            });
            if let Some(e) = ui.ctx().data(|d| d.get_temp::<String>(egui::Id::unique("grpc-example-error"))) {
                alert(ui, AlertKind::Error, &e);
                if ui.small_button("Dismiss").clicked() {
                    ui.ctx().data_mut(|d| d.remove::<String>(egui::Id::unique("grpc-example-error")));
                }
            }
            raw_editor(ui, r, Lang::Json, hl);
            return;
        }
        _ => {}
    }
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        None,
        Raw,
        Url,
        Form,
    }
    let mut mode = match r.body {
        Body::None => Mode::None,
        Body::Raw { .. } => Mode::Raw,
        Body::UrlEncoded { .. } => Mode::Url,
        Body::FormData { .. } => Mode::Form,
    };
    ui.horizontal(|ui| {
        if widgets::segmented(ui, &mut mode, &[(Mode::None, "None"), (Mode::Raw, "Raw"), (Mode::Url, "Form URL-encoded"), (Mode::Form, "Multipart form")]) {
            let fields = match &r.body {
                Body::UrlEncoded { fields } | Body::FormData { fields } => fields.clone(),
                _ => Vec::new(),
            };
            let text = r.body.raw_text().to_string();
            r.body = match mode {
                Mode::None => Body::None,
                Mode::Raw => Body::Raw { content_type: String::new(), text },
                Mode::Url => Body::UrlEncoded { fields },
                Mode::Form => Body::FormData { fields },
            };
        }
        if let Body::Raw { content_type, .. } = &mut r.body {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let types = [
                    ("", "Auto"),
                    ("application/json", "JSON"),
                    ("application/xml", "XML"),
                    ("text/plain", "Text"),
                    ("text/html", "HTML"),
                    ("application/graphql", "GraphQL"),
                ];
                let label = types.iter().find(|(t, _)| t == content_type).map(|(_, l)| l.to_string()).unwrap_or_else(|| content_type.clone());
                egui::ComboBox::from_id_salt("ctype").selected_text(label).width(90.0).show_ui(ui, |ui| {
                    for (t, l) in types {
                        ui.selectable_value(content_type, t.to_string(), l);
                    }
                });
            });
        }
    });
    ui.add_space(6.0);
    match &r.body {
        Body::None => {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| help(ui, "This request has no body."));
        }
        Body::Raw { .. } => {
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("Beautify").clicked() {
                        actions::beautify(r);
                    }
                });
            });
            let lang = match &r.body {
                Body::Raw { content_type, text } => Lang::detect(content_type, text),
                _ => Lang::Plain,
            };
            raw_editor(ui, r, lang, hl);
        }
        Body::UrlEncoded { .. } | Body::FormData { .. } => {
            if let Body::UrlEncoded { fields } | Body::FormData { fields } = &mut r.body {
                kv_table(ui, "form", fields, "field", "value");
            }
        }
    }
}

fn raw_editor(ui: &mut Ui, r: &mut Request, lang: Lang, hl: &crate::highlight::Highlighter) {
    if !matches!(r.body, Body::Raw { .. }) {
        r.body = Body::Raw { content_type: String::new(), text: String::new() };
    }
    if let Body::Raw { text, .. } = &mut r.body {
        let rows = ((ui.available_height() - 20.0) / 18.0).max(10.0) as usize;
        widgets::code_editor(ui, ("body", &r.id), text, lang, rows, hl);
    }
}

fn services(cache: &ProtoCache, proto: &str) -> Result<Vec<grpc::ServiceInfo>, String> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    proto.hash(&mut h);
    let key = h.finish();
    if let Some((k, v)) = &*cache.borrow() {
        if *k == key {
            return v.clone();
        }
    }
    let v = grpc::compile(proto).map(|pool| grpc::services(&pool));
    *cache.borrow_mut() = Some((key, v.clone()));
    v
}

fn proto_tab(ui: &mut Ui, r: &mut Request, cache: &ProtoCache, hl: &crate::highlight::Highlighter, native: bool) {
    let p = pal(ui);
    let parsed = if r.grpc.proto.trim().is_empty() { None } else { Some(services(cache, &r.grpc.proto)) };
    let svcs = match &parsed {
        Some(Ok(s)) => s.clone(),
        _ => Vec::new(),
    };
    ui.columns(2, |cols| {
        cols[0].label(RichText::new("Service").strong());
        let current = r.grpc.service.clone();
        egui::ComboBox::from_id_salt("grpc-svc")
            .selected_text(if current.is_empty() { "Choose a service".into() } else { current.clone() })
            .width(cols[0].available_width())
            .show_ui(&mut cols[0], |ui| {
                for s in &svcs {
                    if ui.selectable_label(s.name == current, &s.name).clicked() && s.name != current {
                        r.grpc.service = s.name.clone();
                        r.grpc.method.clear();
                    }
                }
            });
        cols[1].label(RichText::new("Method").strong());
        let methods: Vec<grpc::MethodInfo> = svcs.iter().find(|s| s.name == r.grpc.service).map(|s| s.methods.clone()).unwrap_or_default();
        egui::ComboBox::from_id_salt("grpc-method")
            .selected_text(if r.grpc.method.is_empty() { "Choose a method".into() } else { r.grpc.method.clone() })
            .width(cols[1].available_width())
            .show_ui(&mut cols[1], |ui| {
                for m in &methods {
                    let kind = match (m.client_streaming, m.server_streaming) {
                        (false, false) => "unary",
                        (false, true) => "server streaming",
                        (true, false) => "client streaming, not supported",
                        (true, true) => "bidi streaming, not supported",
                    };
                    let short = |s: &str| s.rsplit('.').next().unwrap_or(s).to_string();
                    ui.selectable_value(&mut r.grpc.method, m.name.clone(), format!("{}  ({} \u{2192} {}, {kind})", m.name, short(&m.input), short(&m.output)));
                }
            });
    });
    ui.add_space(8.0);
    match &parsed {
        Some(Err(e)) => alert(ui, AlertKind::Error, e),
        Some(Ok(s)) if s.is_empty() => alert(ui, AlertKind::Warn, "The proto has no services."),
        None => alert(ui, AlertKind::Warn, "Paste the service's .proto below. Imports of google/protobuf/* work; other imports need to be inlined."),
        _ => {}
    }
    ui.add_space(6.0);
    ui.label(RichText::new("Proto definition").strong());
    widgets::code_editor(ui, ("proto", &r.id), &mut r.grpc.proto, Lang::Plain, 14, hl);
    ui.add_space(4.0);
    ui.label(
        RichText::new(if native {
            "Calls use native gRPC over HTTP/2."
        } else {
            "In the browser, calls use gRPC-Web, which the server (or a proxy such as Envoy) must support. With the local agent, they use native gRPC."
        })
        .size(12.0)
        .color(p.muted),
    );
}

// ---------------------------------------------------------------- GraphQL

fn graphql_tab(ui: &mut Ui, r: &mut Request, hl: &crate::highlight::Highlighter) {
    let p = pal(ui);
    let named: Vec<(String, String)> = graphql::operations(&r.graphql.query).into_iter().filter(|(_, n)| !n.is_empty()).collect();
    ui.horizontal(|ui| {
        help(ui, "The query document. {{variables}} work anywhere; GraphQL $variables come from the box below.");
        if named.len() > 1 {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let current = if r.graphql.operation_name.is_empty() { "Choose\u{2026}".to_string() } else { r.graphql.operation_name.clone() };
                egui::ComboBox::from_id_salt("gql-op").selected_text(current).width(160.0).show_ui(ui, |ui| {
                    for (kind, name) in &named {
                        ui.selectable_value(&mut r.graphql.operation_name, name.clone(), format!("{name}  ({kind})"));
                    }
                });
                ui.label(RichText::new("Operation").strong());
            });
        }
    });
    if named.len() > 1 && !named.iter().any(|(_, n)| *n == r.graphql.operation_name) {
        alert(ui, AlertKind::Warn, "The document has several operations; choose the one to run.");
    }
    let rows = ((ui.available_height() - 200.0) / 18.0).max(8.0) as usize;
    widgets::code_editor(ui, ("gql", &r.id), &mut r.graphql.query, Lang::GraphQl, rows, hl);
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Variables").strong());
        ui.label(RichText::new("JSON object").size(11.5).color(p.faint));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("Beautify").clicked() {
                if let Some(t) = apicraft_core::format::pretty_json(&r.graphql.variables) {
                    r.graphql.variables = t;
                }
            }
        });
    });
    // {{placeholders}} may stand for numbers or objects, so check with them stood in for.
    if let Err(e) = graphql::request_body("", &without_placeholders(&r.graphql.variables), "") {
        alert(ui, AlertKind::Warn, &e);
    }
    widgets::code_editor(ui, ("gql-vars", &r.id), &mut r.graphql.variables, Lang::Json, 6, hl);
}

/// `{{name}}` placeholders replaced by `0`, for checking JSON before sending.
fn without_placeholders(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start..].find("}}") else { break };
        out.push_str(&rest[..start]);
        // Inside a string the placeholder stays text; outside it becomes a number.
        let quoted = out.matches('"').count() % 2 == 1;
        out.push_str(if quoted { "x" } else { "0" });
        rest = &rest[start + len + 2..];
    }
    out.push_str(rest);
    out
}

fn type_kind_label(kind: &str) -> &'static str {
    match kind {
        "OBJECT" => "type",
        "INPUT_OBJECT" => "input",
        "ENUM" => "enum",
        "INTERFACE" => "interface",
        "UNION" => "union",
        _ => "scalar",
    }
}

/// The GraphQL schema browser. Returns the tab to switch to when a field
/// was picked (its query replaces the current one).
fn schema_tab(ui: &mut Ui, r: &mut Request, loaded: Option<&Result<ApiSchema, String>>, busy: bool, actions: &mut Vec<Action>, id: &str) -> Option<ReqTab> {
    let p = pal(ui);
    let schema = match loaded {
        Some(Ok(ApiSchema::GraphQl(s))) => Some(s),
        _ => None,
    };
    ui.horizontal(|ui| {
        if busy {
            ui.spinner();
            ui.label(RichText::new("Loading the schema\u{2026}").color(p.muted));
        } else if ui
            .button(format!("{} {}", icons::MAGNIFYING_GLASS, if schema.is_some() { "Reload schema" } else { "Load schema" }))
            .on_hover_text("Run the introspection query against the URL above, with this request's auth and headers")
            .clicked()
        {
            actions.push(Action::FetchSchema(id.to_string()));
        }
        if schema.is_some()
            && ui.button(format!("{} Create requests", icons::PLUS)).on_hover_text("Add a folder with one request per query and mutation").clicked()
        {
            actions.push(Action::RequestsFromSchema(id.to_string()));
        }
    });
    match loaded {
        Some(Err(e)) => alert(ui, AlertKind::Error, e),
        None if !busy => help(ui, "Load the schema to browse its queries, mutations and types. Click a field to write a query for it."),
        _ => {}
    }
    let s = schema?;
    let fid = egui::Id::unique(("gql-schema-filter", id));
    let mut filter = ui.ctx().data(|d| d.get_temp::<String>(fid)).unwrap_or_default();
    ui.add_space(6.0);
    if ui.add(TextEdit::singleline(&mut filter).hint_text(format!("{} Filter fields and types", icons::MAGNIFYING_GLASS)).desired_width(280.0)).changed() {
        ui.ctx().data_mut(|d| d.insert_temp(fid, filter.clone()));
    }
    let f = filter.to_lowercase();
    let shown = |name: &str| f.is_empty() || name.to_lowercase().contains(&f);
    let mut switch = None;
    let roots = s.roots();
    for (kind, root) in &roots {
        let fields: Vec<&graphql::Field> = root.fields.iter().filter(|x| shown(&x.name)).collect();
        egui::CollapsingHeader::new(format!("{} ({})", kind.plural(), fields.len()))
            .id_salt(("gql-root", kind.keyword()))
            .default_open(*kind == graphql::OpKind::Query)
            .show(ui, |ui| {
                for fd in fields {
                    ui.horizontal_wrapped(|ui| {
                        let mut link = ui.link(RichText::new(&fd.name).monospace());
                        if !fd.description.is_empty() {
                            link = link.on_hover_text(&fd.description);
                        }
                        if link.clicked() {
                            let (query, variables) = graphql::operation(s, *kind, fd);
                            r.graphql = GraphQlOptions { query, variables, operation_name: String::new() };
                            switch = Some(ReqTab::GraphQl);
                        }
                        let args: Vec<String> = fd.args.iter().map(|a| format!("{}: {}", a.name, a.ty.display())).collect();
                        let sig = if args.is_empty() {
                            format!("\u{2192} {}", fd.ty.display())
                        } else {
                            format!("({}) \u{2192} {}", args.join(", "), fd.ty.display())
                        };
                        ui.label(RichText::new(sig).monospace().size(11.5).color(p.muted));
                    });
                }
            });
    }
    let types: Vec<&graphql::TypeDef> = s.user_types().filter(|t| shown(&t.name) && !roots.iter().any(|(_, rt)| rt.name == t.name)).collect();
    egui::CollapsingHeader::new(format!("Types ({})", types.len())).id_salt("gql-types").show(ui, |ui| {
        for t in types {
            egui::CollapsingHeader::new(RichText::new(format!("{}  {}", type_kind_label(&t.kind), t.name)).monospace()).id_salt(("gql-type", &t.name)).show(
                ui,
                |ui| {
                    if !t.description.is_empty() {
                        help(ui, &t.description);
                    }
                    for fd in &t.fields {
                        ui.label(RichText::new(format!("{}: {}", fd.name, fd.ty.display())).monospace().size(12.0));
                    }
                    for iv in &t.input_fields {
                        ui.label(RichText::new(format!("{}: {}", iv.name, iv.ty.display())).monospace().size(12.0));
                    }
                    if !t.enum_values.is_empty() {
                        ui.label(RichText::new(t.enum_values.join(" | ")).monospace().size(12.0));
                    }
                },
            );
        }
    });
    switch
}

// ---------------------------------------------------------------- OData

fn csv_items(s: &str) -> Vec<String> {
    s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

/// Whether a comma list holds `item` (`Name desc` counts for `Name`).
fn csv_has(s: &str, item: &str) -> bool {
    csv_items(s).iter().any(|x| x.split_whitespace().next() == Some(item))
}

fn csv_toggle(s: &mut String, item: &str, on: bool) {
    let mut items = csv_items(s);
    if on {
        if !csv_has(s, item) {
            items.push(item.to_string());
        }
    } else {
        items.retain(|x| x.split_whitespace().next() != Some(item));
    }
    *s = items.join(",");
}

/// One system query option: a text field, and a picker when the metadata
/// lists the choices.
fn option_row(ui: &mut Ui, label: &str, value: &mut String, hint: &str, choices: &[String], width: f32) {
    ui.label(RichText::new(label).monospace().strong());
    ui.horizontal(|ui| {
        let w = if choices.is_empty() { width } else { width - 36.0 };
        ui.add(TextEdit::singleline(value).hint_text(hint).font(egui::TextStyle::Monospace).desired_width(w));
        if !choices.is_empty() {
            let config = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
            let (button, _) = egui::containers::menu::MenuButton::new(icons::LIST).config(config).ui(ui, |ui| {
                ui.set_min_width(200.0);
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for c in choices {
                        let mut on = csv_has(value, c);
                        if ui.checkbox(&mut on, c).changed() {
                            csv_toggle(value, c, on);
                        }
                    }
                });
            });
            button.on_hover_text("Choose from the metadata");
        }
    });
    ui.end_row();
}

fn odata_tab(ui: &mut Ui, r: &mut Request, loaded: Option<&Result<ApiSchema, String>>, busy: bool, actions: &mut Vec<Action>, id: &str) {
    let p = pal(ui);
    let md = match loaded {
        Some(Ok(ApiSchema::OData(m))) => Some(m),
        _ => None,
    };
    // Wrapped, so the buttons drop below the version switch when the pane is narrow.
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Version").strong());
        widgets::segmented(ui, &mut r.odata.version, &ODataVersion::ALL);
        ui.add_space(16.0);
        if busy {
            ui.spinner();
        } else if ui
            .button(format!("{} {}", icons::MAGNIFYING_GLASS, if md.is_some() { "Reload $metadata" } else { "Load $metadata" }))
            .on_hover_text("Read the entity sets and properties from the service root's $metadata")
            .clicked()
        {
            actions.push(Action::FetchSchema(id.to_string()));
        }
        if md.is_some()
            && ui
                .button(format!("{} Create requests", icons::PLUS))
                .on_hover_text("Add a folder of list, get, create, update and delete requests per entity set")
                .clicked()
        {
            actions.push(Action::RequestsFromSchema(id.to_string()));
        }
    });
    if let Some(Err(e)) = loaded {
        alert(ui, AlertKind::Error, e);
    }
    if let Some(m) = md.filter(|m| m.version != r.odata.version) {
        let label = ODataVersion::ALL.iter().find(|(v, _)| *v == m.version).map(|(_, l)| *l).unwrap_or_default();
        ui.horizontal(|ui| {
            alert(ui, AlertKind::Warn, &format!("The service's $metadata is OData {label}."));
            if ui.small_button(format!("Use {label}")).clicked() {
                r.odata.version = m.version;
            }
        });
    }

    ui.add_space(8.0);
    ui.label(RichText::new("Resource").strong());
    ui.horizontal(|ui| {
        let w = ui.available_width() - if md.is_some() { 132.0 } else { 0.0 };
        ui.add(
            TextEdit::singleline(&mut r.odata.resource)
                .hint_text("People('russellwhyte')/Friends   (entity set, key, navigation, function\u{2026})")
                .font(egui::TextStyle::Monospace)
                .desired_width(w),
        );
        if let Some(m) = md {
            egui::ComboBox::from_id_salt("odata-set").selected_text("Choose\u{2026}").width(120.0).height(360.0).show_ui(ui, |ui| {
                let mut pick = |ui: &mut Ui, label: &str, resource: String| {
                    if ui.selectable_label(r.odata.resource == resource, label).clicked() {
                        r.odata.resource = resource;
                    }
                };
                for s in &m.entity_sets {
                    pick(ui, &s.name, s.name.clone());
                }
                if !m.singletons.is_empty() {
                    ui.separator();
                    for s in &m.singletons {
                        pick(ui, &s.name, s.name.clone());
                    }
                }
                if !m.operations.is_empty() {
                    ui.separator();
                    for o in &m.operations {
                        let resource = if m.version == ODataVersion::V4 && !o.is_action { format!("{}()", o.name) } else { o.name.clone() };
                        pick(ui, &format!("{}{}", o.name, if o.is_action { " (action)" } else { "()" }), resource);
                    }
                }
            });
        }
    });
    let head = r.odata.resource.split(['(', '/', '?']).next().unwrap_or_default().trim();
    let set = md.and_then(|m| m.entity_set(head));
    if let Some(s) = set {
        let keys: Vec<String> = s.keys.iter().map(|k| format!("{} ({})", k.name, k.ty)).collect();
        help(
            ui,
            &format!(
                "{} \u{b7} key {} \u{b7} {} properties \u{b7} {} navigation properties",
                s.type_name.rsplit('.').next().unwrap_or_default(),
                if keys.is_empty() { "unknown".to_string() } else { keys.join(", ") },
                s.properties.len(),
                s.navigation.len()
            ),
        );
    }

    ui.add_space(8.0);
    let props: Vec<String> = set.map(|s| s.properties.iter().map(|p| p.name.clone()).collect()).unwrap_or_default();
    let navs: Vec<String> = set.map(|s| s.navigation.clone()).unwrap_or_default();
    let width = (ui.available_width() - 90.0).max(220.0);
    egui::Grid::new("odata-options").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        option_row(ui, "$select", &mut r.odata.select, "Name,Price", &props, width);
        option_row(ui, "$expand", &mut r.odata.expand, "Category,Supplier", &navs, width);
        option_row(ui, "$filter", &mut r.odata.filter, "Price gt 20 and contains(Name,'a')", &[], width);
        option_row(ui, "$orderby", &mut r.odata.orderby, "Name desc,Price", &props, width);
        if r.odata.version == ODataVersion::V4 {
            option_row(ui, "$search", &mut r.odata.search, "blue OR green", &[], width);
        }
        ui.label(RichText::new("Paging").strong());
        ui.horizontal(|ui| {
            ui.label(RichText::new("$top").monospace());
            ui.add(TextEdit::singleline(&mut r.odata.top).hint_text("10").font(egui::TextStyle::Monospace).desired_width(56.0));
            ui.add_space(8.0);
            ui.label(RichText::new("$skip").monospace());
            ui.add(TextEdit::singleline(&mut r.odata.skip).hint_text("0").font(egui::TextStyle::Monospace).desired_width(56.0));
            ui.add_space(8.0);
            ui.checkbox(&mut r.odata.count, "Count").on_hover_text(if r.odata.version == ODataVersion::V4 { "$count=true" } else { "$inlinecount=allpages" });
        });
        ui.end_row();
    });

    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("URL").strong());
        let body_method = !matches!(r.method.as_str(), "GET" | "HEAD" | "DELETE");
        if let (Some(s), true) = (set, body_method) {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("Example body").on_hover_text("Fill the Body tab with the entity's properties").clicked() {
                    r.body = Body::Raw { content_type: "application/json".into(), text: odata::example_entity(s) };
                }
            });
        }
    });
    let qs = odata::encode_query(&odata::query_options(&r.odata));
    let mut url = odata::resource_url(&r.url, &r.odata.resource);
    if !qs.is_empty() {
        url = format!("{url}{}{qs}", if url.contains('?') { "&" } else { "?" });
    }
    ui.add(egui::Label::new(RichText::new(url).monospace().size(12.0).color(p.muted)).wrap());
    help(ui, "{{variables}} work in every field. Custom query options go in the Params tab.");
}
