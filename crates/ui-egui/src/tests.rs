//! Headless smoke tests: draw every view, tab and dialog with a fake
//! platform, so a panic in view code fails the build instead of the app.

use apicraft_core::engine::Outcome;
use apicraft_core::http::{HttpRequest, HttpResponse};
use apicraft_core::model::*;

use crate::app::{ApiCraftApp, CollectionTab, Confirm, Modal, ReqTab, RespTab, ThemeChoice, View};
use crate::platform::{Inbox, Job, Level, Platform, Status, WsConn};

struct FakePlatform;

impl Platform for FakePlatform {
    fn status(&self) -> Status {
        Status { label: "Test".into(), detail: "headless".into(), level: Level::Good }
    }
    fn now_ms(&self) -> u64 {
        1_700_000_000_000
    }
    fn spawn(&self, _job: Job, _inbox: Inbox) {}
    fn load_catalog(&self) -> Option<Catalog> {
        None
    }
    fn save_catalog(&self, _catalog: &Catalog) -> Result<(), String> {
        Ok(())
    }
    fn storage_description(&self) -> String {
        "nowhere".into()
    }
    fn save_file(&self, _name: &str, _bytes: Vec<u8>, _inbox: Inbox) {}
    fn open_files(&self, _inbox: Inbox) {}
    fn ws_connect(&self, _url: &str, _h: &[(String, String)], _p: &[String], _seq: u32, _inbox: Inbox) -> Result<Box<dyn WsConn>, String> {
        Err("no sockets in tests".into())
    }
    fn ws_sends_headers(&self) -> bool {
        true
    }
    fn default_redirect_uri(&self) -> String {
        "http://127.0.0.1:7878/callback".into()
    }
    fn authorize(&self, _url: &str, _redirect: &str, _inbox: Inbox) -> Result<(), String> {
        Ok(())
    }
    fn native_grpc(&self) -> bool {
        true
    }
}

fn frame(ctx: &egui::Context, app: &mut ApiCraftApp, w: f32, h: f32) {
    let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h))), ..Default::default() };
    // Two passes: egui sizes some things on the first.
    for _ in 0..2 {
        let mut out = ctx.run_ui(raw.clone(), |ui| app.ui(ui));
        // No renderer here: drop the texture updates deliberately.
        out.textures_delta.clear();
    }
}

fn frame_with(ctx: &egui::Context, app: &mut ApiCraftApp, events: Vec<egui::Event>) {
    let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))), events, ..Default::default() };
    let mut out = ctx.run_ui(raw, |ui| app.ui(ui));
    out.textures_delta.clear();
}

/// Drags with the mouse from one point to another over several frames, as a person would.
fn drag(ctx: &egui::Context, app: &mut ApiCraftApp, from: egui::Pos2, to: egui::Pos2) {
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    frame_with(ctx, app, vec![egui::Event::PointerMoved(from)]);
    frame_with(ctx, app, vec![button(from, true)]);
    for i in 1..=8 {
        frame_with(ctx, app, vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0))]);
    }
    frame_with(ctx, app, vec![button(to, false)]);
    frame_with(ctx, app, vec![egui::Event::PointerMoved(egui::pos2(900.0, 600.0))]);
    frame_with(ctx, app, Vec::new());
}

/// The first sidebar row labelled `name`: its centre.
fn row_center(ctx: &egui::Context, name: &str) -> egui::Pos2 {
    let mut found = None;
    ctx.viewport(|v| {
        let widgets = &v.prev_pass.widgets;
        for (_, rects) in widgets.layers() {
            for w in rects {
                if found.is_none() && widgets.info(w.id).and_then(|i| i.label.as_deref()) == Some(name) && w.rect.left() < 300.0 {
                    found = Some(w.rect.center());
                }
            }
        }
    });
    found.unwrap_or_else(|| panic!("no sidebar row labelled {name}"))
}

#[test]
fn sidebar_drag_and_drop_reorders_and_moves() {
    let ctx = egui::Context::default();
    let mut app = ApiCraftApp::new(&ctx, Box::new(FakePlatform), None, None);
    let col = app.catalog.collections[0].id.clone();
    let rest = app.catalog.collections[0].items.iter().find(|i| i.name() == "REST").map(|i| i.id().to_string()).unwrap();
    app.expanded = [col.clone(), rest.clone()].into_iter().collect();
    // egui keeps widget labels (used to find the rows) only with this on.
    ctx.all_styles_mut(|s| s.debug.show_interactive_widgets = true);
    for _ in 0..3 {
        frame_with(&ctx, &mut app, Vec::new());
    }
    let folder = |app: &ApiCraftApp, name: &str| match app.catalog.collections[0].items.iter().find(|i| i.name() == name) {
        Some(Item::Folder(f)) => f.items.iter().map(|i| i.name().to_string()).collect::<Vec<_>>(),
        _ => panic!("no folder {name}"),
    };
    let before = folder(&app, "REST");
    assert_eq!(before, ["Get with query", "Post JSON", "Basic auth", "Digest auth"]);

    // Onto the top edge of a sibling: reorders.
    let from = row_center(&ctx, "Basic auth");
    let get = row_center(&ctx, "Get with query");
    drag(&ctx, &mut app, from, get - egui::vec2(0.0, 10.0));
    assert_eq!(folder(&app, "REST"), ["Basic auth", "Get with query", "Post JSON", "Digest auth"]);

    // Onto the middle of a folder: moves into it, at the end.
    let from = row_center(&ctx, "Digest auth");
    let to = row_center(&ctx, "Token flow");
    drag(&ctx, &mut app, from, to);
    assert_eq!(folder(&app, "REST"), ["Basic auth", "Get with query", "Post JSON"]);
    assert_eq!(folder(&app, "Token flow").last().map(String::as_str), Some("Digest auth"));
}

fn outcome_for(app: &ApiCraftApp, id: &str, body: &str, content_type: &str) -> Outcome {
    let name = app.catalog.find_request(id).map(|(_, r)| r.name.clone()).unwrap_or_default();
    Outcome {
        request_id: id.into(),
        request_name: name,
        sent: Some(HttpRequest {
            connect: Default::default(),
            method: "POST".into(),
            url: "https://api.test/x".into(),
            headers: vec![("Accept".into(), "*/*".into())],
            body: b"{\"a\":1}".to_vec(),
            grpc: false,
            timeout_ms: 0,
        }),
        response: Some(HttpResponse {
            status: 201,
            status_text: "Created".into(),
            headers: vec![("Content-Type".into(), content_type.into())],
            body: body.as_bytes().to_vec(),
            trailers: vec![("grpc-status".into(), "0".into())],
            elapsed_ms: 12.0,
        }),
        body_text: body.into(),
        extracted: vec![(VarScope::Runtime, "token".into(), "abc".into())],
        checks: vec![apicraft_core::extract::CheckResult { label: "status is 2xx".into(), passed: true, detail: "got 201".into() }],
        warnings: vec!["sent without a token".into()],
        ..Default::default()
    }
}

#[test]
fn renders_every_view_tab_and_dialog() {
    let ctx = egui::Context::default();
    let mut app = ApiCraftApp::new(&ctx, Box::new(FakePlatform), None, None);
    let ids: Vec<String> = app.catalog.all_requests().iter().map(|(_, r)| r.id.clone()).collect();
    assert!(ids.len() >= 10, "the sample catalog covers every protocol");
    let tabs = [
        ReqTab::Params,
        ReqTab::Headers,
        ReqTab::Body,
        ReqTab::Auth,
        ReqTab::Soap,
        ReqTab::Proto,
        ReqTab::Messages,
        ReqTab::Mcp,
        ReqTab::Capture,
        ReqTab::Checks,
        ReqTab::Docs,
    ];
    for (n, id) in ids.iter().enumerate() {
        app.open_request(id);
        for tab in tabs {
            app.req_tab = tab;
            frame(&ctx, &mut app, 1400.0, 900.0);
        }
        // With a response, in every response tab; JSON and XML bodies alternate.
        let (body, ct) =
            if n % 2 == 0 { ("{\"ok\":true,\"items\":[1,2.5,null,\"x\"]}", "application/json") } else { ("<a x=\"1\"><b>t</b><!-- c --></a>", "text/xml") };
        let o = outcome_for(&app, id, body, ct);
        app.outcomes.insert(id.clone(), o);
        for rt in [RespTab::Body, RespTab::Headers, RespTab::Checks, RespTab::Captured, RespTab::Sent] {
            app.resp_tab = rt;
            app.resp_raw = rt == RespTab::Headers;
            frame(&ctx, &mut app, 1400.0, 900.0);
        }
        // Narrow layout puts the response below.
        frame(&ctx, &mut app, 700.0, 800.0);
    }

    // Every auth editor.
    let first = ids[0].clone();
    app.open_request(&first);
    app.req_tab = ReqTab::Auth;
    for (kind, _) in Auth::KINDS {
        if let Some(r) = app.catalog.find_request_mut(&first) {
            r.auth = Auth::of_kind(kind, &r.auth);
        }
        frame(&ctx, &mut app, 1400.0, 900.0);
    }

    // Workflows, with and without a run.
    let wid = app.catalog.workflows[0].id.clone();
    app.view = View::Workflow(wid.clone());
    frame(&ctx, &mut app, 1400.0, 900.0);
    app.expanded.insert(app.catalog.workflows[0].steps[0].id.clone());
    frame(&ctx, &mut app, 700.0, 800.0);

    // Every dialog.
    let col = app.catalog.collections[0].id.clone();
    let env = app.catalog.environments[0].id.clone();
    let modals = vec![
        Modal::Code { request_id: first.clone(), curl: true },
        Modal::Code { request_id: first.clone(), curl: false },
        Modal::Environments { selected: Some(env) },
        Modal::Runtime,
        Modal::Collection { id: col.clone(), tab: CollectionTab::General },
        Modal::Collection { id: col.clone(), tab: CollectionTab::Variables },
        Modal::Collection { id: col.clone(), tab: CollectionTab::Auth },
        Modal::Collection { id: col.clone(), tab: CollectionTab::Files },
        Modal::Rename { id: col.clone(), value: "x".into() },
        Modal::Import { text: String::new(), target: None, url: String::new() },
        Modal::Confirm { message: "Delete?".into(), action: Confirm::Delete(col) },
        Modal::Settings,
        Modal::StepResult { index: 0 },
    ];
    for m in modals {
        app.modal = Some(m);
        frame(&ctx, &mut app, 1400.0, 900.0);
    }

    // Values from commands and Bitwarden in an environment.
    let env = app.catalog.environments[0].id.clone();
    app.catalog.active_environment = Some(env.clone());
    let mut bw = VarSource::new(SourceKind::Bitwarden);
    bw.key = "token".into();
    bw.item = "My API".into();
    app.catalog.environments[0].sources = vec![VarSource { key: "cmd".into(), command: "echo hi".into(), ..VarSource::new(SourceKind::Command) }, bw];
    app.source_values.insert(env.clone(), [("cmd".to_string(), "hi".to_string())].into());
    app.source_errors.insert(format!("{env}/token"), "Bitwarden is locked".into());
    app.modal = Some(Modal::Environments { selected: Some(env) });
    frame(&ctx, &mut app, 1400.0, 900.0);

    // A collection kept in a folder, with Git changes and a diff open.
    let col = app.catalog.collections[0].id.clone();
    app.catalog.collections[0].directory = Some("C:/work/api".into());
    let file = |p: &str, x: char, y: char| apicraft_core::git::GitFile { path: format!("api/{p}"), shown: p.into(), index: x, worktree: y };
    let status = apicraft_core::git::GitStatus {
        root: "C:/work".into(),
        prefix: "api/".into(),
        branch: "main".into(),
        upstream: Some("origin/main".into()),
        ahead: 1,
        behind: 2,
        files: vec![file("Get.request.json", ' ', 'M'), file("New.request.json", '?', '?')],
        log: vec![apicraft_core::git::GitCommit { hash: "abc1234".into(), author: "Sam".into(), when: "2 days ago".into(), subject: "Add requests".into() }],
    };
    let view = app.git.entry("C:/work/api".into()).or_default();
    view.status = Some(Ok(apicraft_core::git::GitReply::Status(status)));
    view.diff = Some((
        "api/Get.request.json".into(),
        "@@ -1 +1 @@
-{}
+{\"a\":1}"
            .into(),
    ));
    app.modal = Some(Modal::Collection { id: col.clone(), tab: CollectionTab::Files });
    frame(&ctx, &mut app, 1400.0, 900.0);
    app.git.get_mut("C:/work/api").unwrap().status = Some(Ok(apicraft_core::git::GitReply::NotRepo));
    frame(&ctx, &mut app, 1400.0, 900.0);

    // Dark theme, collapsed sidebar, welcome view.
    app.modal = None;
    app.prefs.theme = ThemeChoice::Dark;
    app.apply_theme(&ctx);
    app.prefs.sidebar_open = false;
    app.view = View::Welcome;
    frame(&ctx, &mut app, 1400.0, 900.0);
    app.toast("hello", false);
    frame(&ctx, &mut app, 1400.0, 900.0);
}

/// The x position of the widget labelled `name` in the last frame.
fn widget_x(ctx: &egui::Context, name: &str) -> f32 {
    let mut found = None;
    ctx.viewport(|v| {
        let widgets = &v.prev_pass.widgets;
        for (_, rects) in widgets.layers() {
            for w in rects {
                if found.is_none() && widgets.info(w.id).and_then(|i| i.label.as_deref()) == Some(name) {
                    found = Some(w.rect.left());
                }
            }
        }
    });
    found.unwrap_or_else(|| panic!("no widget labelled {name}"))
}

#[test]
fn response_pane_keeps_its_width_across_tabs() {
    let ctx = egui::Context::default();
    let mut app = ApiCraftApp::new(&ctx, Box::new(FakePlatform), None, None);
    ctx.all_styles_mut(|s| s.debug.show_interactive_widgets = true);
    let id = app.catalog.all_requests()[0].1.id.clone();
    app.open_request(&id);
    // Panels remember the size of what they last showed: a short Checks
    // tab used to shrink the pane, and a long header to stretch it.
    let mut short = outcome_for(&app, &id, "{}", "application/json");
    // Alerts fill the width, which would hide the problem.
    short.warnings.clear();
    let mut long = short.clone();
    if let Some(r) = &mut long.response {
        r.headers.push(("X-Long".into(), "x".repeat(600)));
    }
    long.checks[0].detail = "y".repeat(400);
    for o in [short, long] {
        app.outcomes.insert(id.clone(), o);
        let mut xs = Vec::new();
        for tab in [RespTab::Body, RespTab::Headers, RespTab::Checks, RespTab::Body, RespTab::Checks] {
            app.resp_tab = tab;
            frame(&ctx, &mut app, 1400.0, 900.0);
            frame(&ctx, &mut app, 1400.0, 900.0);
            xs.push(widget_x(&ctx, "Request"));
        }
        assert!(xs.windows(2).all(|w| (w[0] - w[1]).abs() < 0.5), "the response pane moved between tabs: {xs:?}");
    }
}

#[test]
fn undo_brings_back_a_deleted_workflow_step() {
    let ctx = egui::Context::default();
    let mut app = ApiCraftApp::new(&ctx, Box::new(FakePlatform), None, None);
    let before = app.catalog.workflows[0].steps.len();
    app.catalog.workflows[0].steps.remove(0);
    app.undo();
    assert_eq!(app.catalog.workflows[0].steps.len(), before);
    app.redo();
    assert_eq!(app.catalog.workflows[0].steps.len(), before - 1);
}

#[test]
fn duplicates_collections_and_folders_with_new_ids() {
    let mut cat = crate::sample::catalog();
    let col = cat.collections[0].clone();
    let copy_id = cat.duplicate_collection(&col.id).unwrap();
    let copy = cat.collection(&copy_id).unwrap();
    assert_eq!(copy.name, format!("{} (copy)", col.name));
    let ids = |c: &Collection| c.items.iter().map(|i| i.id().to_string()).collect::<Vec<_>>();
    assert_eq!(copy.items.len(), col.items.len());
    assert!(ids(copy).iter().all(|i| !ids(&col).contains(i)), "fresh ids");
    let folder = col.items.iter().find(|i| matches!(i, Item::Folder(_))).unwrap().id().to_string();
    let new_folder = cat.duplicate_folder(&folder).unwrap();
    assert_eq!(cat.parent_of(&new_folder).map(|(c, _)| c), Some(col.id.clone()));
}

/// The centre of the last frame's widget matching `pick`.
fn find_widget(ctx: &egui::Context, pick: impl Fn(&egui::WidgetInfo) -> bool) -> Option<egui::Pos2> {
    let mut found = None;
    ctx.viewport(|v| {
        let widgets = &v.prev_pass.widgets;
        for (_, rects) in widgets.layers() {
            for w in rects {
                if found.is_none() && widgets.info(w.id).is_some_and(&pick) {
                    found = Some(w.rect.center());
                }
            }
        }
    });
    found
}

fn click(ctx: &egui::Context, app: &mut ApiCraftApp, pos: egui::Pos2, button: egui::PointerButton) {
    let ev = |pressed| egui::Event::PointerButton { pos, button, pressed, modifiers: Default::default() };
    frame_with(ctx, app, vec![egui::Event::PointerMoved(pos)]);
    frame_with(ctx, app, vec![ev(true)]);
    frame_with(ctx, app, vec![ev(false)]);
    frame_with(ctx, app, Vec::new());
    frame_with(ctx, app, Vec::new());
}

fn menu_item(ctx: &egui::Context, name: &str) -> egui::Pos2 {
    find_widget(ctx, |i| i.label.as_deref().is_some_and(|l| l.contains(name))).unwrap_or_else(|| panic!("no menu item {name}"))
}

#[test]
fn text_fields_have_a_working_context_menu() {
    let ctx = egui::Context::default();
    let mut app = ApiCraftApp::new(&ctx, Box::new(FakePlatform), None, None);
    ctx.all_styles_mut(|s| s.debug.show_interactive_widgets = true);
    app.filter = "basic".into();
    for _ in 0..3 {
        frame_with(&ctx, &mut app, Vec::new());
    }
    let field = find_widget(&ctx, |i| i.current_text_value.as_deref() == Some("basic")).expect("the filter field");
    click(&ctx, &mut app, field, egui::PointerButton::Primary);
    click(&ctx, &mut app, field, egui::PointerButton::Secondary);
    click(&ctx, &mut app, menu_item(&ctx, "Select all"), egui::PointerButton::Primary);
    click(&ctx, &mut app, field, egui::PointerButton::Secondary);
    click(&ctx, &mut app, menu_item(&ctx, "Cut"), egui::PointerButton::Primary);
    assert_eq!(app.filter, "", "Select all then Cut empties the field");
    // Undo in the menu goes through the field's own undo.
    click(&ctx, &mut app, field, egui::PointerButton::Secondary);
    click(&ctx, &mut app, menu_item(&ctx, "Undo"), egui::PointerButton::Primary);
    assert_eq!(app.filter, "basic");
}

#[test]
fn sidebar_menus_offer_duplicate_and_git() {
    let ctx = egui::Context::default();
    let mut app = ApiCraftApp::new(&ctx, Box::new(FakePlatform), None, None);
    ctx.all_styles_mut(|s| s.debug.show_interactive_widgets = true);
    for _ in 0..3 {
        frame_with(&ctx, &mut app, Vec::new());
    }
    let row = row_center(&ctx, "Examples");
    click(&ctx, &mut app, row, egui::PointerButton::Secondary);
    let before = app.catalog.collections.len();
    click(&ctx, &mut app, menu_item(&ctx, "Duplicate collection"), egui::PointerButton::Primary);
    assert_eq!(app.catalog.collections.len(), before + 1);
}
/// One frame at a chosen window size. `frame` fixes the size because nothing in
/// the other tests depends on it; the responsive layout does. A pass populates
/// `prev_pass` only for the *next* pass, so settle with two before looking.
fn frames(ctx: &egui::Context, app: &mut ApiCraftApp, w: f32, h: f32, events: Vec<egui::Event>) {
    let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h))), events, ..Default::default() };
    let mut out = ctx.run_ui(raw, |ui| app.ui(ui));
    // No renderer here: drop the texture updates deliberately.
    out.textures_delta.clear();
}

/// Clicks at `pos` in a window of a chosen size. `click` cannot be used for the
/// responsive tests: it reframes at 1280px, and a frame that wide folds the
/// forced sidebar straight back away.
fn click_at(ctx: &egui::Context, app: &mut ApiCraftApp, w: f32, h: f32, pos: egui::Pos2) {
    let ev = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    frames(ctx, app, w, h, vec![egui::Event::PointerMoved(pos)]);
    frames(ctx, app, w, h, vec![ev(true)]);
    frames(ctx, app, w, h, vec![ev(false)]);
    frames(ctx, app, w, h, Vec::new());
    frames(ctx, app, w, h, Vec::new());
}

/// Is the full sidebar on screen? The button's label is an icon glyph followed
/// by the words, so match the tail. The rail's equivalents allocate their own
/// hit areas and register no label at all, which makes this a clean test.
fn sidebar_is_open(ctx: &egui::Context) -> bool {
    find_widget(ctx, |i| i.label.as_deref().is_some_and(|l| l.ends_with("New request"))).is_some()
}

/// Widgets with a labelled hit area that leaves the window sideways: content a
/// person cannot reach without resizing. Clipped top or bottom is normal for a
/// scroll area; running off the left or right edge was the bug.
fn offscreen_widgets(ctx: &egui::Context, w: f32) -> Vec<(String, f32, f32)> {
    let mut out = Vec::new();
    ctx.viewport(|v| {
        let widgets = &v.prev_pass.widgets;
        for (_, rects) in widgets.layers() {
            for r in rects {
                let info = widgets.info(r.id);
                let name = info.as_ref().and_then(|i| i.label.clone()).unwrap_or_default();
                // egui's own panel splitter spans both panes on purpose: it is a
                // handle, not content, so a wide rect here means nothing.
                let splitter = info.as_ref().is_some_and(|i| i.role == egui::Role::Splitter);
                if !name.is_empty() && !splitter && (r.rect.left() < -1.0 || r.rect.right() > w + 1.0) {
                    out.push((name, r.rect.left().round(), r.rect.right().round()));
                }
            }
        }
    });
    // f32 is only partially ordered, so sort by name and then by position.
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.total_cmp(&b.2)));
    out.dedup();
    out
}

#[test]
fn a_narrow_window_folds_the_sidebar_and_keeps_the_view_reachable() {
    let ctx = egui::Context::default();
    // egui only records a widget's label for tests to read when this is on.
    ctx.all_styles_mut(|s| s.debug.show_interactive_widgets = true);
    let mut app = ApiCraftApp::new(&ctx, Box::new(FakePlatform), None, None);
    let id = app.catalog.all_requests()[0].1.id.clone();
    app.open_request(&id);

    // The window cannot go below 760px (the desktop shell sets that minimum),
    // so these are the widths a person can actually reach.
    for w in [760.0, 900.0, 1100.0, 1400.0] {
        app.sidebar_forced = false;
        frames(&ctx, &mut app, w, 620.0, Vec::new());
        frames(&ctx, &mut app, w, 620.0, Vec::new());
        let off = offscreen_widgets(&ctx, w);
        assert!(off.is_empty(), "at {w}px wide, widgets sit outside the window: {off:?}");
        let open = sidebar_is_open(&ctx);
        assert_eq!(open, w >= 860.0, "sidebar visible at {w}px: {open}");
    }

    // The rail's logo is how you ask for the sidebar back in a window too narrow
    // for it. That is a deliberate request, so it comes back - but it takes a
    // slice, not the whole loaf. Taking the whole window is what used to happen.
    app.sidebar_forced = false;
    frames(&ctx, &mut app, 760.0, 620.0, Vec::new());
    frames(&ctx, &mut app, 760.0, 620.0, Vec::new());
    click_at(&ctx, &mut app, 760.0, 620.0, egui::pos2(28.0, 27.0));
    assert!(app.sidebar_forced, "the rail's logo should expand the sidebar even when it will not fit");
    frames(&ctx, &mut app, 760.0, 620.0, Vec::new());
    frames(&ctx, &mut app, 760.0, 620.0, Vec::new());
    assert!(sidebar_is_open(&ctx), "the sidebar came back at the person's request");
    let off = offscreen_widgets(&ctx, 760.0);
    assert!(off.is_empty(), "a forced sidebar crowded the view out: {off:?}");
}

#[test]
fn tab_strips_wrap_instead_of_running_off_the_edge() {
    let ctx = egui::Context::default();
    ctx.all_styles_mut(|s| s.debug.show_interactive_widgets = true);
    let mut app = ApiCraftApp::new(&ctx, Box::new(FakePlatform), None, None);
    let id = app.catalog.all_requests()[0].1.id.clone();
    app.open_request(&id);
    // Tab labels carry a count suffix when non-empty, so match the head.
    for w in [760.0, 900.0, 1200.0] {
        frames(&ctx, &mut app, w, 700.0, Vec::new());
        frames(&ctx, &mut app, w, 700.0, Vec::new());
        let mut seen = 0;
        for label in ["Params", "Headers", "Body", "Auth", "Capture", "Checks", "Docs"] {
            if find_widget(&ctx, |i| i.label.as_deref().is_some_and(|l| l.starts_with(label))).is_some() {
                seen += 1;
            }
        }
        assert!(seen >= 5, "expected most of the REST tab strip at {w}px, found {seen}");
        let off = offscreen_widgets(&ctx, w);
        assert!(off.is_empty(), "the tab strip overflowed at {w}px: {off:?}");
    }
}
