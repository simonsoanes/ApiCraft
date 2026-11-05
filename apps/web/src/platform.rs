//! The browser's [`Platform`]: `fetch` (or the local agent's proxy),
//! localStorage mirrored to the agent's catalog file, downloads, the file
//! picker, browser WebSockets and an OAuth popup.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use apicraft_core::model::Catalog;
use apicraft_core::store;
use apicraft_ui::platform::DroppedFileRef;
use apicraft_ui::{run_job, FolderPurpose, Inbox, Job, Level, Msg, Platform, Status, WsConn, WsEvent};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::dom;
use crate::net::{self, AgentInfo, WebTransport};

const CATALOG_KEY: &str = "fizl.apicraft.catalog.v1";
const SETTINGS_KEY: &str = "fizl.apicraft.web.v1";
/// Keys used before the rename to ApiCraft; read when the new ones are empty.
const LEGACY_CATALOG_KEY: &str = "fizl.apipost.catalog.v1";
const LEGACY_SETTINGS_KEY: &str = "fizl.apipost.web.v1";

struct State {
    agent: RefCell<Option<AgentInfo>>,
    detecting: Cell<bool>,
    use_agent: Cell<bool>,
    mirror: Cell<bool>,
    agent_url: RefCell<String>,
    /// Nothing is stored in this browser yet (the sample catalog is showing),
    /// so the agent's catalog, if there is one, should replace it.
    fresh: Cell<bool>,
}

impl State {
    fn agent_base(&self) -> Option<String> {
        if self.use_agent.get() {
            self.agent.borrow().as_ref().map(|a| a.base.clone())
        } else {
            None
        }
    }

    fn save_settings(&self) {
        let v = serde_json::json!({ "useAgent": self.use_agent.get(), "mirror": self.mirror.get(), "agentUrl": *self.agent_url.borrow() });
        let _ = dom::storage_set(SETTINGS_KEY, &v.to_string());
    }
}

pub struct WebPlatform {
    state: Rc<State>,
}

impl WebPlatform {
    pub fn new() -> Self {
        let v: serde_json::Value =
            dom::storage_get(SETTINGS_KEY).or_else(|| dom::storage_get(LEGACY_SETTINGS_KEY)).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let state = State {
            agent: RefCell::new(None),
            detecting: Cell::new(false),
            use_agent: Cell::new(v.get("useAgent").and_then(|b| b.as_bool()).unwrap_or(true)),
            mirror: Cell::new(v.get("mirror").and_then(|b| b.as_bool()).unwrap_or(true)),
            agent_url: RefCell::new(v.get("agentUrl").and_then(|s| s.as_str()).unwrap_or("").to_string()),
            fresh: Cell::new(false),
        };
        WebPlatform { state: Rc::new(state) }
    }

    fn transport(&self) -> WebTransport {
        WebTransport { agent: self.state.agent_base(), abort: None }
    }
}

/// Looks for the local agent; on a first visit, loads its catalog file.
fn detect(state: Rc<State>, inbox: Inbox, announce: bool) {
    state.detecting.set(true);
    wasm_bindgen_futures::spawn_local(async move {
        let extra = state.agent_url.borrow().clone();
        let found = net::detect_agent(Some(extra)).await;
        if announce {
            match &found {
                Some(a) => inbox.toast(format!("Agent found at {}", a.base), false),
                None => inbox.toast("No agent found", true),
            }
        }
        let base = found.as_ref().map(|a| a.base.clone());
        *state.agent.borrow_mut() = found;
        if let (Some(base), true) = (base, state.fresh.get()) {
            let loaded = net::agent_load_catalog(&base).await.and_then(|bytes| bytes.map(|b| store::decode(&b)).transpose());
            match loaded {
                Ok(Some(imported)) => {
                    inbox.post(Msg::ReplaceCatalog { catalog: Box::new(imported.into_catalog()), note: "Loaded the catalog from the agent's file".into() });
                }
                Ok(None) => {}
                // Whatever is in that file, mirroring would overwrite it unread.
                Err(e) => {
                    state.mirror.set(false);
                    inbox.toast(format!("Could not read the agent's catalog ({e}), so changes here won't be mirrored to it"), true);
                }
            }
            state.fresh.set(false);
        }
        state.detecting.set(false);
        inbox.post(Msg::Refresh);
    });
}

impl Platform for WebPlatform {
    fn start(&self, inbox: Inbox) {
        detect(self.state.clone(), inbox.clone(), false);
        // OAuth popups report back with postMessage from oauth-callback.html.
        let origin = dom::location_origin();
        let cb = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
            if e.origin() != origin {
                return;
            }
            let data = e.data();
            if js_sys::Reflect::get(&data, &"type".into()).ok().and_then(|v| v.as_string()).as_deref() != Some("apicraft-oauth") {
                return;
            }
            let query = js_sys::Reflect::get(&data, &"search".into()).ok().and_then(|v| v.as_string()).unwrap_or_default();
            inbox.post(Msg::AuthRedirect { query });
        });
        let _ = dom::window().add_event_listener_with_callback("message", cb.as_ref().unchecked_ref());
        cb.forget();
    }

    fn status(&self) -> Status {
        let agent = self.state.agent.borrow();
        match (agent.as_ref(), self.state.use_agent.get()) {
            (Some(a), true) => Status { label: "Agent".into(), detail: format!("Requests go through the local agent {} at {}: no CORS limits, native gRPC.", a.version, a.base), level: Level::Good },
            (Some(_), false) => Status { label: "Direct".into(), detail: "An agent is running, but requests go straight from the browser (Settings).".into(), level: Level::Neutral },
            (None, _) if self.state.detecting.get() => Status { label: "Looking for agent\u{2026}".into(), detail: "Checking for the local agent.".into(), level: Level::Neutral },
            (None, _) => Status {
                label: "Browser only".into(),
                detail: "No local agent found. Requests go straight from the browser, so servers must allow CORS and gRPC needs gRPC-Web. Run apicraft-agent and open the app from http://127.0.0.1:8787 to lift these limits.".into(),
                level: Level::Warn,
            },
        }
    }

    fn now_ms(&self) -> u64 {
        dom::now_ms()
    }

    fn spawn(&self, job: Job, inbox: Inbox) {
        let t = self.transport();
        wasm_bindgen_futures::spawn_local(async move { run_job(job, &t, &inbox).await });
    }

    fn load_catalog(&self) -> Option<Catalog> {
        match dom::storage_get(CATALOG_KEY).or_else(|| dom::storage_get(LEGACY_CATALOG_KEY)).and_then(|s| store::from_storage_string(&s).ok()) {
            Some(c) => Some(c),
            None => {
                self.state.fresh.set(true);
                None
            }
        }
    }

    fn save_catalog(&self, catalog: &Catalog) -> Result<(), String> {
        dom::storage_set(CATALOG_KEY, &store::to_storage_string(catalog))?;
        // The agent's catalog may be about to replace this, so it can't go over
        // the agent's file yet.
        if self.state.fresh.get() && self.state.detecting.get() {
            return Ok(());
        }
        self.state.fresh.set(false);
        if let (Some(base), true) = (self.state.agent_base(), self.state.mirror.get()) {
            let gz = store::to_gzip(catalog);
            wasm_bindgen_futures::spawn_local(async move {
                let _ = net::agent_save_catalog(&base, &gz).await;
            });
        }
        Ok(())
    }

    fn storage_description(&self) -> String {
        match (self.state.agent.borrow().as_ref(), self.state.mirror.get() && self.state.use_agent.get()) {
            (Some(a), true) => format!("Kept in this browser's storage and mirrored to {}", a.catalog_file),
            _ => "Kept in this browser's storage.".into(),
        }
    }

    fn save_file(&self, suggested_name: &str, bytes: Vec<u8>, _inbox: Inbox) {
        dom::download(suggested_name, &bytes);
    }

    fn pick_folder(&self, purpose: FolderPurpose, inbox: Inbox) {
        if self.state.agent_base().is_none() || !self.state.mirror.get() {
            return inbox.toast("Collection folders need the local agent (apicraft-agent), with mirroring to the agent turned on in Settings", true);
        }
        let ask = match purpose {
            FolderPurpose::Link(_) => "Path of an empty folder on this computer for the collection's files (the local agent writes them):",
            _ => "Path of a collection folder on this computer (the local agent reads it):",
        };
        if let Ok(Some(path)) = dom::window().prompt_with_message(ask) {
            let path = path.trim().trim_matches('"').to_string();
            if !path.is_empty() {
                inbox.post(Msg::FolderPicked { path, purpose });
            }
        }
    }

    fn load_folder(&self, path: String, purpose: FolderPurpose, inbox: Inbox) {
        let Some(base) = self.state.agent_base() else {
            return inbox.toast("Collection folders need the local agent", true);
        };
        wasm_bindgen_futures::spawn_local(async move {
            let result = net::agent_folder(&base, &path, matches!(purpose, FolderPurpose::Link(_))).await;
            inbox.post(Msg::FolderLoaded { path, purpose, result: result.map(Box::new) });
        });
    }

    fn git(&self, request: apicraft_core::git::GitRequest, inbox: Inbox) {
        let Some(base) = self.state.agent_base() else {
            return inbox.post(Msg::Git { request, result: Err("Git needs the local agent (apicraft-agent)".into()) });
        };
        wasm_bindgen_futures::spawn_local(async move {
            let result = net::agent_json(&base, "git", &request).await;
            inbox.post(Msg::Git { request, result });
        });
    }

    fn read_clipboard(&self, inbox: Inbox) {
        // navigator.clipboard.readText(), which asks the user the first time.
        let read = || -> Result<js_sys::Promise, JsValue> {
            let clipboard = js_sys::Reflect::get(&dom::window().navigator(), &JsValue::from_str("clipboard"))?;
            let f: js_sys::Function = js_sys::Reflect::get(&clipboard, &JsValue::from_str("readText"))?.dyn_into()?;
            f.call0(&clipboard)?.dyn_into()
        };
        match read() {
            Ok(promise) => wasm_bindgen_futures::spawn_local(async move {
                match wasm_bindgen_futures::JsFuture::from(promise).await {
                    Ok(v) => inbox.post(Msg::Clipboard(v.as_string().unwrap_or_default())),
                    Err(_) => inbox.toast("The browser didn't allow reading the clipboard; use Ctrl+V", true),
                }
            }),
            Err(_) => inbox.toast("This browser can't read the clipboard from a menu; use Ctrl+V", true),
        }
    }

    fn open_files(&self, inbox: Inbox) {
        wasm_bindgen_futures::spawn_local(async move {
            let Some(files) = rfd::AsyncFileDialog::new()
                .add_filter("Catalogs, Postman, OpenAPI, WSDL, GraphQL, OData", &["json", "gz", "yaml", "yml", "wsdl", "xml", "edmx"])
                .pick_files()
                .await
            else {
                return;
            };
            for f in files {
                let name = f.file_name();
                let bytes = f.read().await;
                inbox.post(Msg::FileOpened { name, bytes });
            }
        });
    }

    fn read_dropped_file(&self, file: DroppedFileRef, inbox: Inbox) {
        wasm_bindgen_futures::spawn_local(async move {
            let name = file.path().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "dropped file".into());
            match file.bytes_async().await {
                Ok(bytes) => inbox.post(Msg::FileOpened { name, bytes }),
                Err(e) => inbox.toast(format!("Could not read {name}: {e}"), true),
            }
        });
    }

    fn ws_connect(&self, url: &str, _headers: &[(String, String)], protocols: &[String], seq: u32, inbox: Inbox) -> Result<Box<dyn WsConn>, String> {
        let socket = if protocols.is_empty() {
            web_sys::WebSocket::new(url)
        } else {
            let arr = js_sys::Array::new();
            for p in protocols {
                arr.push(&JsValue::from_str(p));
            }
            web_sys::WebSocket::new_with_str_sequence(url, &arr)
        }
        .map_err(|e| dom::js_err(&e))?;
        socket.set_binary_type(web_sys::BinaryType::Arraybuffer);
        let post = move |inbox: &Inbox, event| inbox.post(Msg::Ws { seq, event });

        let i = inbox.clone();
        let on_open = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| post(&i, WsEvent::Open));
        let i = inbox.clone();
        let on_message = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
            let Ok(m) = e.dyn_into::<web_sys::MessageEvent>() else { return };
            let data = m.data();
            if let Some(s) = data.as_string() {
                post(&i, WsEvent::Text(s));
            } else if let Ok(buf) = data.dyn_into::<js_sys::ArrayBuffer>() {
                post(&i, WsEvent::Binary(js_sys::Uint8Array::new(&buf).to_vec()));
            }
        });
        let i = inbox.clone();
        let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            post(&i, WsEvent::Error("Connection error (browsers give no details; check the URL and that the server accepts this origin)".into()))
        });
        let i = inbox;
        let on_close = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
            let why = match e.dyn_ref::<web_sys::CloseEvent>() {
                Some(c) if c.reason().is_empty() => format!("Closed (code {})", c.code()),
                Some(c) => format!("Closed (code {}: {})", c.code(), c.reason()),
                None => "Closed".into(),
            };
            post(&i, WsEvent::Closed(why));
        });
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        Ok(Box::new(BrowserWs { socket, _callbacks: [on_open, on_message, on_error, on_close] }))
    }

    fn ws_sends_headers(&self) -> bool {
        false
    }

    fn default_redirect_uri(&self) -> String {
        format!("{}/oauth-callback.html", dom::location_origin())
    }

    fn authorize(&self, url: &str, _redirect_uri: &str, _inbox: Inbox) -> Result<(), String> {
        match dom::window().open_with_url_and_target_and_features(url, "apicraft-oauth", "width=520,height=720") {
            Ok(Some(_)) => Ok(()),
            _ => Err("The browser blocked the sign-in popup; allow popups for this page".into()),
        }
    }

    fn settings_ui(&self, ui: &mut egui::Ui, inbox: &Inbox) {
        let mut use_agent = self.state.use_agent.get();
        if ui.checkbox(&mut use_agent, "Send requests through the local agent when it's available").changed() {
            self.state.use_agent.set(use_agent);
            self.state.save_settings();
        }
        let mut mirror = self.state.mirror.get();
        if ui.checkbox(&mut mirror, "Mirror the catalog to the agent's file on every change").changed() {
            self.state.mirror.set(mirror);
            self.state.save_settings();
        }
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Agent address (when this page is hosted elsewhere)").strong());
        ui.horizontal(|ui| {
            let mut url = self.state.agent_url.borrow().clone();
            if ui.add(egui::TextEdit::singleline(&mut url).hint_text("http://127.0.0.1:8787").desired_width(260.0)).changed() {
                *self.state.agent_url.borrow_mut() = url;
                self.state.save_settings();
            }
            if ui.button("Detect").clicked() {
                detect(self.state.clone(), inbox.clone(), true);
            }
        });
        ui.label(egui::RichText::new(format!("Start the agent with --allow-origin {} to allow this page.", dom::location_origin())).size(12.0));
    }

    fn native_grpc(&self) -> bool {
        self.state.agent_base().is_some()
    }
}

struct BrowserWs {
    socket: web_sys::WebSocket,
    _callbacks: [Closure<dyn FnMut(web_sys::Event)>; 4],
}

impl WsConn for BrowserWs {
    fn send_text(&self, text: &str) -> Result<(), String> {
        self.socket.send_with_str(text).map_err(|e| dom::js_err(&e))
    }
    fn close(&self) {
        let _ = self.socket.close_with_code_and_reason(1000, "closed by user");
    }
}
