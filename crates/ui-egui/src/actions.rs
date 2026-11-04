//! Things the UI does. Views queue an [`Action`] while drawing (when they
//! can't mutate the catalog mid-iteration); [`apply`] runs them after the frame.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use apicraft_core::codegen::{self, Target};
use apicraft_core::model::*;
use apicraft_core::{auth, curl_import, engine, format, postman, soap, store};

use crate::app::{ApiCraftApp, Confirm, Modal, View, WorkflowRun, WsDir, WsEntry, WsSession};
use crate::platform::{CodeExchange, CommandPurpose, FolderContents, FolderPurpose, Job};

#[derive(Debug, Clone)]
pub enum Action {
    Open(String),
    OpenWorkflow(String),
    Toggle(String),
    /// New REST request in a collection/folder (or the current collection).
    NewRequest(Option<String>),
    NewFolder(String),
    NewCollection,
    NewWorkflow,
    WorkflowFrom(String),
    Rename(String),
    RenameSave,
    CollectionSettings(String),
    Duplicate(String),
    /// A whole collection, or a folder and everything in it.
    DuplicateCollection(String),
    DuplicateFolder(String),
    Undo,
    Redo,
    AskDelete(String),
    Confirm,
    ExportCollectionGz(String),
    ExportCollectionPostman(String),
    ExportCatalogGz,
    ExportCatalogJson,
    ExportEnvironment(String),
    CopyCode(String, Target),
    SaveCode(String, Target),
    /// A workflow as a Bash or PowerShell script.
    CopyWorkflowScript(String, Target),
    SaveWorkflowScript(String, Target),
    Send(String),
    Cancel(String),
    WsConnect(String),
    WsDisconnect,
    WsSend(String),
    WsSendCompose,
    RunWorkflow(String),
    StopWorkflow,
    DuplicateWorkflow(String),
    OAuthGet(String),
    McpDiscover(String),
    /// Open a folder of request files as a collection.
    OpenFolder,
    /// Keep a collection in a folder of files from now on.
    LinkFolder(String),
    ReloadFolder(String),
    /// Keep the collection in the catalog again; the files stay where they are.
    UnlinkFolder(String),
    /// Load the GraphQL schema or OData $metadata of a request's server.
    FetchSchema(String),
    /// Add a folder of requests, one per operation, from the loaded schema.
    RequestsFromSchema(String),
    AddExamples,
    OpenFiles,
    ImportText,
    ImportUrl,
    /// Opens the import dialog, aimed at a collection (or a new one).
    ImportInto(Option<String>),
    /// Drag and drop in the sidebar.
    MoveItem(String, MoveTo),
    SaveResponse,
    /// Fetch every command-backed value of the active environment again.
    FetchSources,
    BitwardenUnlock,
    BitwardenStatus,
    BitwardenSync,
    /// Forget the Bitwarden session key.
    BitwardenLock,
    Git(apicraft_core::git::GitRequest),
    /// The collection dialog on its folder and Git tab.
    CollectionFiles(String),
}

pub fn ancestors_of(cat: &Catalog, id: &str) -> Vec<String> {
    fn walk(items: &[Item], id: &str, path: &mut Vec<String>) -> bool {
        for i in items {
            if i.id() == id {
                return true;
            }
            if let Item::Folder(f) = i {
                path.push(f.id.clone());
                if walk(&f.items, id, path) {
                    return true;
                }
                path.pop();
            }
        }
        false
    }
    for c in &cat.collections {
        let mut path = vec![c.id.clone()];
        if walk(&c.items, id, &mut path) {
            return path;
        }
    }
    Vec::new()
}

pub fn folder_name(cat: &Catalog, id: &str) -> Option<String> {
    fn walk(items: &[Item], id: &str) -> Option<String> {
        items.iter().find_map(|i| match i {
            Item::Folder(f) if f.id == id => Some(f.name.clone()),
            Item::Folder(f) => walk(&f.items, id),
            _ => None,
        })
    }
    cat.collections.iter().find_map(|c| walk(&c.items, id))
}

fn file_stem(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let s = s.trim_matches('-').to_lowercase();
    if s.is_empty() {
        "export".into()
    } else {
        s
    }
}

pub fn insert_request(app: &mut ApiCraftApp, container: Option<String>, req: Request) {
    let id = req.id.clone();
    let container = container.or_else(|| match &app.view {
        View::Request(rid) => app.catalog.collection_of(rid).map(|c| c.id.clone()),
        _ => None,
    });
    let container = match container.or_else(|| app.catalog.collections.first().map(|c| c.id.clone())) {
        Some(c) => c,
        None => {
            let c = Collection::new("My collection");
            let cid = c.id.clone();
            app.catalog.collections.push(c);
            cid
        }
    };
    if let Some(items) = app.catalog.container_items_mut(&container) {
        items.push(Item::Request(req));
    }
    app.expanded.insert(container);
    app.open_request(&id);
}

pub fn apply(app: &mut ApiCraftApp, action: Action, ctx: &egui::Context) {
    match action {
        Action::Open(id) => {
            app.modal = None;
            app.open_request(&id);
        }
        Action::OpenWorkflow(id) => app.view = View::Workflow(id),
        Action::Toggle(id) => {
            if !app.expanded.remove(&id) {
                app.expanded.insert(id);
            }
        }
        Action::NewRequest(container) => insert_request(app, container, Request::new("New request", Protocol::Rest)),
        Action::NewFolder(container) => {
            let f = Folder::new("New folder");
            let fid = f.id.clone();
            if let Some(items) = app.catalog.container_items_mut(&container) {
                items.push(Item::Folder(f));
            }
            app.expanded.insert(container);
            app.expanded.insert(fid.clone());
            app.modal = Some(Modal::Rename { id: fid, value: "New folder".into() });
        }
        Action::NewCollection => {
            let c = Collection::new("New collection");
            let cid = c.id.clone();
            app.catalog.collections.push(c);
            app.expanded.insert(cid.clone());
            app.modal = Some(Modal::Rename { id: cid, value: "New collection".into() });
        }
        Action::NewWorkflow => {
            let w = Workflow::new("New workflow");
            let wid = w.id.clone();
            app.catalog.workflows.push(w);
            app.view = View::Workflow(wid);
        }
        Action::WorkflowFrom(rid) => {
            let name = app.catalog.find_request(&rid).map(|(_, r)| format!("{} flow", r.name)).unwrap_or_else(|| "New workflow".into());
            let mut w = Workflow::new(name);
            w.steps.push(WorkflowStep::new(rid));
            let wid = w.id.clone();
            app.catalog.workflows.push(w);
            app.view = View::Workflow(wid);
        }
        Action::Rename(id) => {
            let name = app
                .catalog
                .collection(&id)
                .map(|c| c.name.clone())
                .or_else(|| app.catalog.workflow(&id).map(|w| w.name.clone()))
                .or_else(|| app.catalog.find_request(&id).map(|(_, r)| r.name.clone()))
                .or_else(|| folder_name(&app.catalog, &id))
                .unwrap_or_default();
            app.modal = Some(Modal::Rename { id, value: name });
        }
        Action::CollectionSettings(id) => app.modal = Some(Modal::Collection { id, tab: crate::app::CollectionTab::Variables }),
        Action::RenameSave => {
            if let Some(Modal::Rename { id, value }) = app.modal.clone() {
                let name = value.trim();
                if name.is_empty() {
                    app.toast("Name can't be empty", true);
                    return;
                }
                if let Some(w) = app.catalog.workflow_mut(&id) {
                    w.name = name.into();
                } else {
                    app.catalog.rename_item(&id, name);
                }
                app.modal = None;
            }
        }
        Action::Duplicate(id) => {
            if let Some(new_id) = app.catalog.duplicate_request(&id) {
                app.open_request(&new_id);
            }
        }
        Action::DuplicateCollection(id) => {
            if let Some(new_id) = app.catalog.duplicate_collection(&id) {
                app.expanded.insert(new_id.clone());
                let name = app.catalog.collection(&new_id).map(|c| c.name.clone()).unwrap_or_default();
                app.modal = Some(Modal::Rename { id: new_id, value: name });
            }
        }
        Action::DuplicateFolder(id) => {
            if let Some(new_id) = app.catalog.duplicate_folder(&id) {
                app.expanded.insert(new_id.clone());
                let name = folder_name(&app.catalog, &new_id).unwrap_or_default();
                app.modal = Some(Modal::Rename { id: new_id, value: name });
            }
        }
        Action::Undo => app.undo(),
        Action::Redo => app.redo(),
        Action::FetchSources => {
            if app.catalog.active_environment.is_none() {
                return app.toast("Choose an environment first", true);
            }
            app.fetching_sources = true;
            app.spawn(Job::FetchSources { catalog: Box::new(app.catalog.clone()), sources: app.source_ctx() });
        }
        Action::BitwardenUnlock => {
            // bw asks for the master password in a window of its own.
            app.bitwarden.busy = true;
            let spec = apicraft_core::sources::bitwarden_unlock(app.bitwarden.logged_in());
            app.spawn(Job::Command { spec, purpose: CommandPurpose::BitwardenUnlock });
        }
        Action::BitwardenStatus => {
            app.bitwarden.busy = true;
            app.spawn(Job::Command { spec: apicraft_core::sources::bitwarden_status(&app.bitwarden.session), purpose: CommandPurpose::BitwardenStatus });
        }
        Action::BitwardenSync => {
            app.bitwarden.busy = true;
            app.spawn(Job::Command { spec: apicraft_core::sources::bitwarden_sync(&app.bitwarden.session), purpose: CommandPurpose::BitwardenSync });
        }
        Action::BitwardenLock => {
            app.bitwarden.session.clear();
            app.bitwarden.status = None;
            app.source_values.clear();
            app.toast("Forgot the Bitwarden session and fetched values", false);
        }
        Action::Git(request) => app.run_git(request),
        Action::CollectionFiles(id) => {
            if let Some(dir) = app.catalog.collection(&id).and_then(|c| c.directory.clone()) {
                app.run_git(apicraft_core::git::GitRequest::Status { dir });
            }
            app.modal = Some(Modal::Collection { id, tab: crate::app::CollectionTab::Files });
        }
        Action::AskDelete(id) => {
            let what = if app.catalog.collection(&id).is_some() {
                "this collection and everything in it"
            } else if app.catalog.workflow(&id).is_some() {
                "this workflow"
            } else if app.catalog.find_request(&id).is_some() {
                "this request"
            } else {
                "this folder and everything in it"
            };
            app.modal = Some(Modal::Confirm { message: format!("Delete {what}? Export it first if you want a copy."), action: Confirm::Delete(id) });
        }
        Action::Confirm => {
            let Some(Modal::Confirm { action, .. }) = app.modal.take() else { return };
            match action {
                Confirm::Delete(id) => {
                    if let Some(pos) = app.catalog.collections.iter().position(|c| c.id == id) {
                        app.catalog.collections.remove(pos);
                    } else if let Some(pos) = app.catalog.workflows.iter().position(|w| w.id == id) {
                        app.catalog.workflows.remove(pos);
                    } else {
                        app.catalog.remove_item(&id);
                    }
                    let gone = match &app.view {
                        View::Request(r) => app.catalog.find_request(r).is_none(),
                        View::Workflow(w) => app.catalog.workflow(w).is_none(),
                        View::Welcome => false,
                    };
                    if gone {
                        app.view = View::Welcome;
                        if let Some((_, r)) = app.catalog.all_requests().first() {
                            let rid = r.id.clone();
                            app.open_request(&rid);
                        }
                    }
                    app.toast("Deleted. Press Ctrl+Z (or Undo at the top) to bring it back.", false);
                }
                Confirm::GitDiscard { dir, paths } => app.run_git(apicraft_core::git::GitRequest::Discard { dir, paths }),
                Confirm::DeleteEnv(id) => {
                    app.catalog.environments.retain(|e| e.id != id);
                    if app.catalog.active_environment.as_deref() == Some(&id) {
                        app.catalog.active_environment = app.catalog.environments.first().map(|e| e.id.clone());
                    }
                    app.modal = Some(Modal::Environments { selected: None });
                }
            }
        }
        Action::ExportCollectionGz(id) => {
            if let Some(c) = app.catalog.collection(&id) {
                let cat = Catalog { collections: vec![c.clone()], ..Catalog::default() };
                app.platform.save_file(&format!("{}.apicraft.json.gz", file_stem(&c.name)), store::to_gzip(&cat), app.inbox.clone());
            }
        }
        Action::ExportCollectionPostman(id) => {
            if let Some(c) = app.catalog.collection(&id) {
                let v = postman::export_collection(c);
                app.platform.save_file(
                    &format!("{}.postman_collection.json", file_stem(&c.name)),
                    serde_json::to_string_pretty(&v).unwrap_or_default().into_bytes(),
                    app.inbox.clone(),
                );
            }
        }
        Action::ExportCatalogGz => app.platform.save_file("fizl-apicraft-catalog.json.gz", store::to_gzip(&app.catalog), app.inbox.clone()),
        Action::ExportCatalogJson => app.platform.save_file("fizl-apicraft-catalog.json", store::to_json(&app.catalog).into_bytes(), app.inbox.clone()),
        Action::ExportEnvironment(id) => {
            if let Some(e) = app.catalog.environments.iter().find(|e| e.id == id) {
                let v = postman::export_environment(e);
                app.platform.save_file(
                    &format!("{}.postman_environment.json", file_stem(&e.name)),
                    serde_json::to_string_pretty(&v).unwrap_or_default().into_bytes(),
                    app.inbox.clone(),
                );
            }
        }
        Action::CopyCode(id, target) => match codegen::generate(&app.job_catalog(), &id, app.platform.now_ms(), target) {
            Ok(code) => {
                ctx.copy_text(code);
                app.toast(if target == Target::Curl { "Copied as curl" } else { "Copied as PowerShell" }, false);
            }
            Err(e) => app.toast(e, true),
        },
        Action::SaveCode(id, target) => {
            let name = app.catalog.find_request(&id).map(|(_, r)| r.name.clone()).unwrap_or_default();
            match codegen::generate(&app.job_catalog(), &id, app.platform.now_ms(), target) {
                Ok(code) => app.platform.save_file(&format!("{}.{}", file_stem(&name), target.file_extension()), code.into_bytes(), app.inbox.clone()),
                Err(e) => app.toast(e, true),
            }
        }
        Action::CopyWorkflowScript(id, target) => match codegen::generate_workflow(&app.catalog, &id, app.platform.now_ms(), target) {
            Ok(code) => {
                ctx.copy_text(code);
                app.toast(if target == Target::Curl { "Copied as a Bash script" } else { "Copied as a PowerShell script" }, false);
            }
            Err(e) => app.toast(e, true),
        },
        Action::SaveWorkflowScript(id, target) => {
            let name = app.catalog.workflow(&id).map(|w| w.name.clone()).unwrap_or_default();
            match codegen::generate_workflow(&app.catalog, &id, app.platform.now_ms(), target) {
                Ok(code) => app.platform.save_file(&format!("{}.{}", file_stem(&name), target.file_extension()), code.into_bytes(), app.inbox.clone()),
                Err(e) => app.toast(e, true),
            }
        }
        Action::Send(id) => {
            if app.sending.contains(&id) {
                return;
            }
            if app.catalog.find_request(&id).map(|(_, r)| r.protocol == Protocol::WebSocket).unwrap_or(false) {
                return apply(app, Action::WsConnect(id), ctx);
            }
            app.sending.insert(id.clone());
            app.resp_tab = crate::app::RespTab::Body;
            app.spawn(Job::Execute { catalog: app.job_catalog(), sources: app.source_ctx(), request_id: id });
        }
        Action::Cancel(id) => {
            app.sending.remove(&id);
            app.toast("Request cancelled", false);
        }
        Action::WsConnect(id) => ws_connect(app, &id),
        Action::WsDisconnect => {
            if let Some(w) = &app.ws {
                if !w.closed {
                    w.conn.close();
                }
            }
        }
        Action::WsSend(text) => ws_send(app, &text),
        Action::WsSendCompose => {
            let text = std::mem::take(&mut app.ws_compose);
            if !text.is_empty() {
                ws_send(app, &text);
            }
        }
        Action::RunWorkflow(id) => {
            if app.run.as_ref().map(|r| r.running).unwrap_or(false) {
                app.toast("A workflow is already running", true);
                return;
            }
            if app.catalog.workflow(&id).map(|w| w.steps.is_empty()).unwrap_or(true) {
                app.toast("Add some steps first", true);
                return;
            }
            let stop = Arc::new(AtomicBool::new(false));
            app.run = Some(WorkflowRun { workflow_id: id.clone(), running: true, stop: stop.clone(), reports: Vec::new(), summary: None, error: None });
            app.view = View::Workflow(id.clone());
            app.spawn(Job::Workflow { catalog: app.job_catalog(), sources: app.source_ctx(), workflow_id: id, stop });
        }
        Action::StopWorkflow => app.stop_workflow(),
        Action::DuplicateWorkflow(id) => {
            if let Some(mut copy) = app.catalog.workflow(&id).cloned() {
                copy.id = apicraft_core::ids::new_id();
                copy.name = format!("{} (copy)", copy.name);
                for s in &mut copy.steps {
                    s.id = apicraft_core::ids::new_id();
                }
                let wid = copy.id.clone();
                app.catalog.workflows.push(copy);
                app.view = View::Workflow(wid);
            }
        }
        Action::OAuthGet(owner) => oauth_get(app, &owner),
        Action::McpDiscover(id) => {
            if app.discovering.insert(id.clone()) {
                app.spawn(Job::McpDiscover { catalog: app.job_catalog(), sources: app.source_ctx(), request_id: id });
            }
        }
        Action::OpenFolder => app.platform.pick_folder(FolderPurpose::Open, app.inbox.clone()),
        Action::LinkFolder(id) => app.platform.pick_folder(FolderPurpose::Link(id), app.inbox.clone()),
        Action::ReloadFolder(id) => {
            if let Some(dir) = app.catalog.collection(&id).and_then(|c| c.directory.clone()) {
                app.platform.load_folder(dir, FolderPurpose::Reload(id), app.inbox.clone());
            }
        }
        Action::UnlinkFolder(id) => {
            if let Some(c) = app.catalog.collections.iter_mut().find(|c| c.id == id) {
                let dir = c.directory.take().unwrap_or_default();
                let note = format!("{} is kept in the catalog again; the files in {dir} were left as they are", c.name);
                app.toast(note, false);
            }
        }
        Action::FetchSchema(id) => {
            if app.discovering.insert(id.clone()) {
                app.spawn(Job::FetchSchema { catalog: app.job_catalog(), sources: app.source_ctx(), request_id: id });
            }
        }
        Action::RequestsFromSchema(id) => requests_from_schema(app, &id),
        Action::AddExamples => {
            let sample = crate::sample::catalog();
            for c in &sample.collections {
                app.expanded.insert(c.id.clone());
            }
            // Keep an existing environment of the same name rather than adding a twin.
            let mut sample = sample;
            sample.environments.retain(|e| !app.catalog.environments.iter().any(|x| x.name == e.name));
            if app.catalog.active_environment.is_some() {
                sample.active_environment = None;
            }
            app.catalog.merge(sample);
            app.modal = None;
            app.toast("Added the example collection and workflow", false);
        }
        Action::OpenFiles => app.platform.open_files(app.inbox.clone()),
        Action::ImportText => import_text(app),
        Action::ImportInto(target) => app.modal = Some(Modal::import(target)),
        Action::MoveItem(id, to) => {
            if app.catalog.move_item(&id, &to) {
                if let MoveTo::Into(container) = to {
                    app.expanded.insert(container);
                }
            } else {
                app.toast("A folder can't be moved inside itself", true);
            }
        }
        Action::ImportUrl => {
            let Some(Modal::Import { url, target, auth, headers, login, .. }) = &app.modal else { return };
            let url = url.trim().to_string();
            let (collection, auth, login) = (target.clone(), auth.clone(), login.clone());
            let headers = headers.iter().filter(|h| !h.key.trim().is_empty()).cloned().collect();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                app.toast("Enter an http(s) URL to import from", true);
                return;
            }
            app.toast(format!("Downloading {url}\u{2026}"), false);
            app.spawn(Job::FetchImport { catalog: app.job_catalog(), sources: app.source_ctx(), collection, url, auth, headers, login });
        }
        Action::SaveResponse => {
            let View::Request(id) = &app.view else { return };
            let Some(o) = app.outcomes.get(id) else { return };
            let name = app.catalog.find_request(id).map(|(_, r)| r.name.clone()).unwrap_or_default();
            let ct = o.response.as_ref().map(|r| r.content_type().to_string()).unwrap_or_default();
            let ext = if o.grpc.is_some() || ct.contains("json") {
                "json"
            } else if ct.contains("xml") {
                "xml"
            } else if ct.contains("html") {
                "html"
            } else {
                "txt"
            };
            let bytes = if o.grpc.is_some() { o.body_text.clone().into_bytes() } else { o.response.as_ref().map(|r| r.body.clone()).unwrap_or_default() };
            app.platform.save_file(&format!("{}-response.{ext}", file_stem(&name)), bytes, app.inbox.clone());
        }
    }
}

// ---------------------------------------------------------------- collection folders

pub fn folder_loaded(app: &mut ApiCraftApp, path: &str, purpose: FolderPurpose, result: Result<FolderContents, String>) {
    let contents = match result {
        Ok(c) => c,
        Err(e) => return app.toast(e, true),
    };
    for w in &contents.warnings {
        app.toast(format!("Skipped {w}"), true);
    }
    match purpose {
        FolderPurpose::Open => {
            if let Some(c) = app.catalog.collections.iter().find(|c| c.directory.as_deref() == Some(path)) {
                let name = c.name.clone();
                app.expanded.insert(c.id.clone());
                return app.toast(format!("{name} is already open from that folder"), false);
            }
            let mut col = contents.collection;
            col.directory = Some(path.to_string());
            // A copied folder keeps its ids; give it new ones if they clash.
            if app.catalog.collection(&col.id).is_some() {
                col.id = apicraft_core::ids::new_id();
            }
            let count = count_requests(&col.items);
            app.expanded.insert(col.id.clone());
            let name = col.name.clone();
            app.catalog.collections.push(col);
            let existing: std::collections::HashSet<String> = app.catalog.workflows.iter().map(|w| w.id.clone()).collect();
            let wfs = contents.workflows.len();
            app.catalog.workflows.extend(contents.workflows.into_iter().filter(|w| !existing.contains(&w.id)));
            app.toast(format!("Opened {name} from {path}: {count} request(s), {wfs} workflow(s)"), false);
        }
        FolderPurpose::Link(id) => {
            if let Some(c) = app.catalog.collections.iter_mut().find(|c| c.id == id) {
                c.directory = Some(path.to_string());
                let note = format!("{} is now saved as files in {path}", c.name);
                app.toast(note, false);
                // Write the files now, so Git sees them straight away.
                app.flush();
                app.run_git(apicraft_core::git::GitRequest::Status { dir: path.to_string() });
            }
        }
        FolderPurpose::Reload(id) => {
            apicraft_core::folder::merge_linked(&mut app.catalog, &id, contents.collection, contents.workflows);
            app.toast("Reloaded from the folder", false);
            app.flush();
        }
    }
}

fn count_requests(items: &[Item]) -> usize {
    items
        .iter()
        .map(|i| match i {
            Item::Request(_) => 1,
            Item::Folder(f) => count_requests(&f.items),
        })
        .sum()
}

// ---------------------------------------------------------------- editing helpers

/// A folder next to the request with one request per GraphQL operation or
/// OData entity set, using the request's URL.
fn requests_from_schema(app: &mut ApiCraftApp, id: &str) {
    use apicraft_core::engine::ApiSchema;
    let (Some((col, req)), Some(Ok(schema))) = (app.catalog.find_request(id), app.schemas.get(id)) else { return };
    let (col_id, url, name) = (col.id.clone(), req.url.clone(), req.name.clone());
    let (title, items) = match schema {
        ApiSchema::GraphQl(s) => (format!("{name} operations"), apicraft_core::graphql::request_folders(s, &url)),
        ApiSchema::OData(m) => (format!("{name} entity sets"), apicraft_core::odata::request_folders(m, &url)),
    };
    let count = items.iter().map(|i| if let Item::Folder(f) = i { f.items.len() } else { 1 }).sum::<usize>();
    let mut folder = Folder::new(title);
    folder.items = items;
    let fid = folder.id.clone();
    if let Some(items) = app.catalog.container_items_mut(&col_id) {
        items.push(Item::Folder(folder));
    }
    app.expanded.insert(col_id);
    app.expanded.insert(fid);
    app.toast(format!("Added {count} request(s)"), false);
}

pub fn switch_protocol(r: &mut Request, p: Protocol) {
    if r.protocol == p {
        return;
    }
    let body_empty = r.body.raw_text().trim().is_empty();
    r.protocol = p;
    match p {
        Protocol::Soap => {
            r.method = "POST".into();
            if body_empty || !r.body.raw_text().trim_start().starts_with('<') {
                r.body = Body::Raw { content_type: String::new(), text: soap::envelope_template(r.soap.version) };
            }
        }
        Protocol::Grpc => {
            r.method = "POST".into();
            if body_empty || serde_json::from_str::<serde_json::Value>(r.body.raw_text()).is_err() {
                r.body = Body::Raw { content_type: "application/json".into(), text: "{}".into() };
            }
        }
        Protocol::WebSocket => {
            if let Some(rest) = r.url.strip_prefix("https://") {
                r.url = format!("wss://{rest}");
            } else if let Some(rest) = r.url.strip_prefix("http://") {
                r.url = format!("ws://{rest}");
            }
        }
        Protocol::Mcp => {
            r.method = "POST".into();
            if r.url.trim().is_empty() || r.url.starts_with("ws") {
                r.url = "http://127.0.0.1:8787/mcp".into();
            }
            if body_empty || serde_json::from_str::<serde_json::Value>(r.body.raw_text()).is_err() {
                r.body = Body::Raw { content_type: "application/json".into(), text: "{}".into() };
            }
        }
        Protocol::GraphQl => {
            r.method = "POST".into();
            if r.graphql.query.trim().is_empty() {
                r.graphql.query = "query {\n  __typename\n}\n".into();
            }
            if let Some(rest) = r.url.strip_prefix("wss://") {
                r.url = format!("https://{rest}");
            } else if let Some(rest) = r.url.strip_prefix("ws://") {
                r.url = format!("http://{rest}");
            }
        }
        Protocol::Rest | Protocol::OData => {
            if !["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "MERGE"].contains(&r.method.as_str()) {
                r.method = "GET".into();
            }
            if let Some(rest) = r.url.strip_prefix("wss://") {
                r.url = format!("https://{rest}");
            } else if let Some(rest) = r.url.strip_prefix("ws://") {
                r.url = format!("http://{rest}");
            }
        }
    }
}

pub fn beautify(r: &mut Request) {
    if let Body::Raw { text, .. } = &mut r.body {
        if let Some(p) = format::pretty_json(text).or_else(|| format::pretty_xml(text)) {
            *text = p;
        }
    }
}

// ---------------------------------------------------------------- import

pub fn import_bytes(app: &mut ApiCraftApp, bytes: &[u8], name: &str) {
    let source = (name.starts_with("http://") || name.starts_with("https://")).then_some(name);
    match store::decode_from(bytes, source) {
        Ok(imported) => {
            let what = imported.describe();
            let cat = imported.into_catalog();
            let first_req = cat.all_requests().first().map(|(_, r)| r.id.clone());
            let first_env = cat.environments.first().map(|e| e.id.clone());
            for c in &cat.collections {
                app.expanded.insert(c.id.clone());
            }
            let had_env = app.catalog.active_environment.is_some();
            let target = match &app.modal {
                Some(Modal::Import { target: Some(t), .. }) if app.catalog.collection(t).is_some() => Some(t.clone()),
                _ => None,
            };
            match &target {
                Some(t) => {
                    app.expanded.insert(t.clone());
                    app.catalog.merge_into(cat, t);
                }
                None => app.catalog.merge(cat),
            }
            if !had_env {
                app.catalog.active_environment = first_env;
            }
            if let Some(r) = first_req {
                app.open_request(&r);
            }
            app.modal = None;
            app.toast(format!("Imported {what}"), false);
        }
        Err(e) => app.toast(format!("Could not import {name}: {e}"), true),
    }
}

fn import_text(app: &mut ApiCraftApp) {
    let Some(Modal::Import { text, target, .. }) = app.modal.clone() else { return };
    let t = text.trim();
    if t.is_empty() {
        app.toast("Paste a curl command or JSON first", true);
        return;
    }
    if t.starts_with("curl") {
        match curl_import::parse_curl(t) {
            Ok(req) => {
                app.modal = None;
                let target = target.filter(|t| app.catalog.collection(t).is_some()).unwrap_or_else(|| {
                    let c = Collection::new("Imported requests");
                    let id = c.id.clone();
                    app.catalog.collections.push(c);
                    id
                });
                insert_request(app, Some(target), req);
                app.toast("Imported curl command", false);
            }
            Err(e) => app.toast(format!("Could not read the curl command: {e}"), true),
        }
    } else {
        import_bytes(app, t.as_bytes(), "pasted text");
    }
}

pub fn handle_dropped_files(app: &mut ApiCraftApp, ctx: &egui::Context) {
    let files = ctx.input(|i| i.raw.dropped_files.clone());
    for f in files {
        app.platform.read_dropped_file(f, app.inbox.clone());
    }
}

// ---------------------------------------------------------------- WebSocket

fn ws_connect(app: &mut ApiCraftApp, id: &str) {
    if let Some(w) = &app.ws {
        if !w.closed {
            w.conn.close();
        }
    }
    let now = app.platform.now_ms();
    let wire = engine::build_wire(&app.job_catalog(), id, &[], now, false).and_then(|mut w| {
        auth::apply(&w.auth, &mut w.http, now)?;
        Ok(w)
    });
    let wire = match wire {
        Ok(w) => w,
        Err(e) => return app.toast(e, true),
    };
    let seq = app.next_ws_seq();
    let url = wire.http.url.clone();
    match app.platform.ws_connect(&url, &wire.http.headers, &wire.prepared.subprotocols, seq, app.inbox.clone()) {
        Ok(conn) => {
            let mut log = vec![WsEntry { dir: WsDir::Info, text: format!("Connecting to {url}"), at: app.now }];
            if !wire.prepared.missing_vars.is_empty() {
                log.push(WsEntry { dir: WsDir::Error, text: format!("Undefined variables: {}", wire.prepared.missing_vars.join(", ")), at: app.now });
            }
            app.ws = Some(WsSession { seq, request_id: id.to_string(), conn, open: false, closed: false, log, started: app.now });
        }
        Err(e) => app.toast(format!("Could not open the WebSocket: {e}"), true),
    }
}

fn ws_send(app: &mut ApiCraftApp, text: &str) {
    let Some(w) = app.ws.as_ref().filter(|w| w.open) else {
        app.toast("Not connected", true);
        return;
    };
    let col = app.catalog.collection_of(&w.request_id);
    let vars = apicraft_core::vars::Vars::for_request(&app.catalog, col, &[], app.platform.now_ms());
    let resolved = vars.apply(text);
    match w.conn.send_text(&resolved) {
        Ok(()) => {
            let at = app.now;
            if let Some(w) = &mut app.ws {
                w.log.push(WsEntry { dir: WsDir::Out, text: resolved, at });
            }
        }
        Err(e) => app.toast(format!("Send failed: {e}"), true),
    }
}

// ---------------------------------------------------------------- OAuth

fn resolved_oauth(app: &ApiCraftApp, owner: &str) -> Option<OAuth2> {
    let auth = app.catalog.collection(owner).map(|c| c.auth.clone()).or_else(|| app.catalog.find_request(owner).map(|(_, r)| r.auth.clone()))?;
    let col = app.catalog.collection(owner).or_else(|| app.catalog.collection_of(owner));
    let vars = apicraft_core::vars::Vars::for_request(&app.catalog, col, &[], app.platform.now_ms());
    match auth::resolve(&auth, &vars) {
        Auth::OAuth2(o) => Some(o),
        _ => None,
    }
}

fn oauth_get(app: &mut ApiCraftApp, owner: &str) {
    let Some(o) = resolved_oauth(app, owner) else { return };
    if o.grant == GrantType::AuthorizationCode {
        let redirect = if o.redirect_uri.trim().is_empty() { app.platform.default_redirect_uri() } else { o.redirect_uri.trim().to_string() };
        let state = apicraft_core::ids::new_id();
        let (verifier, challenge) = if o.use_pkce { auth::pkce_pair() } else { (String::new(), String::new()) };
        let url = match auth::authorize_url(&o, &redirect, &state, o.use_pkce.then_some(challenge.as_str())) {
            Ok(u) => u,
            Err(e) => return app.toast(e, true),
        };
        match app.platform.authorize(&url, &redirect, app.inbox.clone()) {
            Ok(()) => {
                app.oauth = Some(crate::app::PendingOAuth { owner: owner.to_string(), state, verifier, redirect_uri: redirect });
                app.toast("Sign in in the browser window that opened\u{2026}", false);
            }
            Err(e) => app.toast(e, true),
        }
        return;
    }
    let connect = network_options(app);
    app.spawn(Job::Token { owner: owner.to_string(), oauth: Box::new(o), code: None, connect });
}

/// The provider sent the browser back with `?code=...&state=...`.
pub fn finish_authorization(app: &mut ApiCraftApp, query: &str) {
    let params = apicraft_core::url::parse_query(query.trim_start_matches('?'));
    let get = |k: &str| params.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    let Some(pending) = app.oauth.take() else { return };
    if let Some(err) = get("error") {
        return app.toast(format!("Sign-in failed: {err} {}", get("error_description").unwrap_or_default()), true);
    }
    if get("state").as_deref() != Some(pending.state.as_str()) {
        return app.toast("Sign-in response did not match (state mismatch); try again", true);
    }
    let Some(code) = get("code") else { return app.toast("Sign-in response had no code", true) };
    let Some(o) = resolved_oauth(app, &pending.owner) else { return };
    let connect = network_options(app);
    app.spawn(Job::Token {
        owner: pending.owner,
        oauth: Box::new(o),
        code: Some(CodeExchange { code, verifier: pending.verifier, redirect_uri: pending.redirect_uri }),
        connect,
    });
}

/// The catalog's proxy and certificate settings, with variables applied.
fn network_options(app: &ApiCraftApp) -> apicraft_core::http::ConnectOptions {
    let vars = apicraft_core::vars::Vars::for_request(&app.catalog, None, &[], app.platform.now_ms());
    app.catalog.network.connect_options(&vars)
}
