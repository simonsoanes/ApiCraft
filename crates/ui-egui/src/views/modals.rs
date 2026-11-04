//! Dialogs.

use apicraft_core::codegen::{self, Target};
use apicraft_core::format::human_size;
use apicraft_core::git::{GitReply, GitRequest};
use apicraft_core::model::{Auth, ProxyMode, SourceKind, VarSource};
use egui::{Align, Layout, RichText, TextEdit, Ui};

use super::response;
use super::widgets::{self, accent_button, alert, danger_button, help, kv_table, AlertKind, AuthCtx};
use crate::actions::Action;
use crate::app::{ApiCraftApp, CollectionTab, Confirm, Modal, RespTab, ThemeChoice};
use crate::highlight::Lang;
use crate::icons;
use crate::theme::pal;

/// The dialog's title bar, which drags it around.
pub(crate) fn title_bar_id() -> egui::Id {
    egui::Id::unique("modal-title")
}

/// How far the open dialog has been dragged from the middle of the window.
fn offset_id() -> egui::Id {
    title_bar_id().with("offset")
}

pub fn show(app: &mut ApiCraftApp, ctx: &egui::Context) {
    let Some(modal) = app.modal.clone() else {
        // The next dialog opens in the middle again.
        ctx.data_mut(|d| d.remove::<egui::Vec2>(offset_id()));
        return;
    };
    let wide = matches!(modal, Modal::Code { .. } | Modal::Environments { .. } | Modal::Runtime | Modal::Collection { .. } | Modal::StepResult { .. });
    let max_w = ctx.content_rect().width() - 40.0;
    let width = match modal {
        Modal::Confirm { .. } => 440.0_f32,
        Modal::Import { .. } => 640.0,
        _ if wide => 820.0,
        _ => 520.0,
    }
    .min(max_w);
    let max_h = ctx.content_rect().height() - 80.0;
    let id = egui::Id::unique("modal");
    let offset = ctx.data(|d| d.get_temp::<egui::Vec2>(offset_id())).unwrap_or_default();
    let area = egui::Modal::default_area(id).anchor(egui::Align2::CENTER_CENTER, offset);
    let resp = egui::Modal::new(id).area(area).show(ctx, |ui| {
        ui.set_width(width);
        ui.set_max_height(max_h);
        match modal {
            Modal::Code { request_id, curl } => code(app, ui, &request_id, curl),
            Modal::Environments { selected } => environments(app, ui, selected),
            Modal::Runtime => runtime(app, ui),
            Modal::Collection { id, tab } => collection(app, ui, &id, tab),
            Modal::Rename { .. } => rename(app, ui),
            Modal::Import { .. } => import(app, ui),
            Modal::Confirm { message, .. } => confirm(app, ui, &message),
            Modal::Settings => settings(app, ui),
            Modal::StepResult { index } => step_result(app, ui, index),
        }
    });
    // Keep the whole dialog in the window, however far it was dragged.
    let room = ((ctx.content_rect().size() - resp.response.rect.size()) / 2.0).max(egui::Vec2::ZERO);
    ctx.data_mut(|d| {
        let o = d.get_temp_mut_or_default::<egui::Vec2>(offset_id());
        *o = o.clamp(-room, room);
    });
    if resp.should_close() {
        app.modal = None;
    }
}

fn head(app: &mut ApiCraftApp, ui: &mut Ui, title: &str) {
    let bar = ui.horizontal(|ui| {
        ui.heading(title);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::icon_button(ui, icons::X, "Close").clicked() {
                app.modal = None;
            }
        });
    });
    let drag = ui.interact(bar.response.rect, title_bar_id(), egui::Sense::drag());
    // Measured from the press, so the dialog stays under the pointer even if
    // frames are slow or it was held against the edge of the window.
    let start_id = offset_id().with("start");
    if drag.drag_started() {
        ui.ctx().data_mut(|d| {
            let offset = d.get_temp::<egui::Vec2>(offset_id()).unwrap_or_default();
            d.insert_temp(start_id, offset);
        });
    }
    if drag.dragged() {
        if let Some(moved) = ui.input(|i| Some(i.pointer.latest_pos()? - i.pointer.press_origin()?)) {
            ui.ctx().data_mut(|d| {
                let start = d.get_temp::<egui::Vec2>(start_id).unwrap_or_default();
                d.insert_temp(offset_id(), start + moved);
            });
        }
    }
    let cursor = if drag.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab };
    drag.on_hover_cursor(cursor);
    ui.separator();
    ui.add_space(4.0);
}

/// Right-aligned dialog buttons. The row is kept to one line: a bare
/// `with_layout` here would take the modal's whole available height.
fn foot(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(8.0);
    ui.separator();
    ui.horizontal(|ui| ui.with_layout(Layout::right_to_left(Align::Center), add));
}

fn code(app: &mut ApiCraftApp, ui: &mut Ui, request_id: &str, curl: bool) {
    head(app, ui, "Save as code");
    let mut is_curl = curl;
    widgets::segmented(ui, &mut is_curl, &[(true, "curl"), (false, "PowerShell")]);
    if is_curl != curl {
        app.modal = Some(Modal::Code { request_id: request_id.to_string(), curl: is_curl });
    }
    ui.add_space(6.0);
    let target = if is_curl { Target::Curl } else { Target::PowerShell };
    let code = codegen::generate(&app.job_catalog(), request_id, app.platform.now_ms(), target).unwrap_or_else(|e| format!("# {e}"));
    egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| widgets::code_view(ui, "code-out", &code, Lang::Plain, &app.highlighter));
    help(ui, "Variables, params and auth are applied, so this runs the same request as Send.");
    foot(ui, |ui| {
        if accent_button(ui, format!("{} Copy", icons::COPY)).clicked() {
            app.actions.push(Action::CopyCode(request_id.to_string(), target));
        }
        if ui.button(format!("{} Save…", icons::FLOPPY_DISK)).clicked() {
            app.actions.push(Action::SaveCode(request_id.to_string(), target));
        }
    });
}

fn environments(app: &mut ApiCraftApp, ui: &mut Ui, selected: Option<String>) {
    head(app, ui, "Environments");
    let p = pal(ui);
    let sel = selected.or_else(|| app.catalog.active_environment.clone()).or_else(|| app.catalog.environments.first().map(|e| e.id.clone()));
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(190.0);
            for e in &app.catalog.environments {
                let active = app.catalog.active_environment.as_deref() == Some(&e.id);
                let label = if active { format!("{}   \u{2022} active", e.name) } else { e.name.clone() };
                if ui.add_sized([190.0, 28.0], egui::Button::selectable(sel.as_deref() == Some(&e.id), label)).clicked() {
                    app.modal = Some(Modal::Environments { selected: Some(e.id.clone()) });
                }
            }
            ui.add_space(6.0);
            if ui.button(format!("{} New environment", icons::PLUS)).clicked() {
                let e = apicraft_core::model::Environment::new(format!("Environment {}", app.catalog.environments.len() + 1));
                let id = e.id.clone();
                app.catalog.environments.push(e);
                if app.catalog.active_environment.is_none() {
                    app.catalog.active_environment = Some(id.clone());
                }
                app.modal = Some(Modal::Environments { selected: Some(id) });
            }
        });
        ui.separator();
        ui.vertical(|ui| {
            let Some(id) = sel.clone() else {
                ui.label(RichText::new("Environments hold variables like {{base_url}} that change between dev, staging and production.").color(p.muted));
                return;
            };
            let active = app.catalog.active_environment.as_deref() == Some(&id);
            let ApiCraftApp { catalog, source_values, source_errors, bitwarden, actions, fetching_sources, .. } = app;
            let Some(e) = catalog.environments.iter_mut().find(|e| e.id == id) else { return };
            ui.label(RichText::new("Name").strong());
            let r = ui.add(TextEdit::singleline(&mut e.name).desired_width(f32::INFINITY));
            widgets::replace_default(ui, &r, &e.name);
            ui.add_space(6.0);
            let room = (ui.ctx().content_rect().height() - 280.0).max(340.0);
            egui::ScrollArea::vertical().max_height(room).show(ui, |ui| {
                kv_table(ui, &format!("env-{id}"), &mut e.variables, "variable", "value");
                ui.add_space(12.0);
                let known = source_values.get(&id);
                sources_editor(ui, &id, &mut e.sources, known, source_errors, active, *fetching_sources, actions);
                if e.sources.iter().any(|s| s.kind == SourceKind::Bitwarden) {
                    ui.add_space(8.0);
                    bitwarden_panel(ui, bitwarden, actions);
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if !active && accent_button(ui, "Make active").clicked() {
                    app.catalog.active_environment = Some(id.clone());
                }
                if ui.button(format!("{} Export as Postman", icons::DOWNLOAD)).clicked() {
                    app.actions.push(Action::ExportEnvironment(id.clone()));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if danger_button(ui, format!("{} Delete", icons::TRASH)).clicked() {
                        app.modal =
                            Some(Modal::Confirm { message: "Delete this environment and its variables?".into(), action: Confirm::DeleteEnv(id.clone()) });
                    }
                });
            });
        });
    });
}

/// Variables whose values come from running a command, or from Bitwarden.
#[allow(clippy::too_many_arguments)]
fn sources_editor(
    ui: &mut Ui,
    env_id: &str,
    sources: &mut Vec<VarSource>,
    known: Option<&std::collections::HashMap<String, String>>,
    errors: &std::collections::HashMap<String, String>,
    active: bool,
    fetching: bool,
    actions: &mut Vec<Action>,
) {
    let p = pal(ui);
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{}  Values from commands", icons::TERMINAL_WINDOW)).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if !sources.is_empty() {
                let b = ui.add_enabled(active && !fetching, egui::Button::new(format!("{} Fetch now", icons::ARROWS_CLOCKWISE)));
                let b = if active {
                    b.on_hover_text("Run every command again")
                } else {
                    b.on_disabled_hover_text("Make this environment active to fetch its values")
                };
                if b.clicked() {
                    actions.push(Action::FetchSources);
                }
                if fetching {
                    ui.spinner();
                }
            }
        });
    });
    help(
        ui,
        "Each value is fetched by running a command (or reading Bitwarden) the first time a request needs it, then kept in memory for this session. Fetched values are never saved, and override variables of the same name above.",
    );
    ui.add_space(4.0);
    let mut remove = None;
    let fixed = 22.0 + 110.0 + 100.0 + 22.0 + 24.0 + 5.0 * 8.0;
    let wide = (ui.available_width() - fixed).max(160.0);
    for (i, s) in sources.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add_sized([22.0, 24.0], egui::Checkbox::without_text(&mut s.enabled)).on_hover_text("Enabled");
            ui.add(TextEdit::singleline(&mut s.key).id_salt((env_id, i, "sk")).hint_text("variable").desired_width(110.0).font(egui::TextStyle::Monospace));
            egui::ComboBox::from_id_salt((env_id, i, "skind"))
                .selected_text(match s.kind {
                    SourceKind::Command => "Command",
                    SourceKind::Bitwarden => "Bitwarden",
                })
                .width(100.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.kind, SourceKind::Command, "Command");
                    ui.selectable_value(&mut s.kind, SourceKind::Bitwarden, "Bitwarden");
                });
            match s.kind {
                SourceKind::Command => {
                    ui.add(
                        TextEdit::singleline(&mut s.command)
                            .id_salt((env_id, i, "scmd"))
                            .hint_text("op read op://vault/item/field   or   az account get-access-token --query accessToken -o tsv")
                            .desired_width(wide)
                            .font(egui::TextStyle::Monospace),
                    );
                }
                SourceKind::Bitwarden => {
                    // The field picker's arrow and padding sit outside its width.
                    let fw = 84.0;
                    let w = (wide - fw - ui.spacing().item_spacing.x - 2.0 * ui.spacing().button_padding.x - ui.spacing().icon_width).max(80.0);
                    ui.add(TextEdit::singleline(&mut s.item).id_salt((env_id, i, "sitem")).hint_text("item name or id").desired_width(w));
                    egui::ComboBox::from_id_salt((env_id, i, "sfield")).selected_text(&s.field).width(fw).show_ui(ui, |ui| {
                        for f in apicraft_core::sources::BITWARDEN_FIELDS {
                            ui.selectable_value(&mut s.field, f.to_string(), f);
                        }
                        ui.separator();
                        ui.label(RichText::new("or a custom field's name:").size(11.5).color(p.faint));
                        let r = ui.add(TextEdit::singleline(&mut s.field).desired_width(160.0));
                        widgets::replace_default(ui, &r, &s.field);
                    });
                }
            }
            let key = s.key.trim().to_string();
            let (icon, color, tip) = if let Some(err) = errors.get(&format!("{env_id}/{key}")) {
                (icons::WARNING, p.err_text, format!("Couldn't fetch: {err}"))
            } else if let Some(v) = known.and_then(|k| k.get(&key)) {
                (icons::CHECK, p.ok_text, format!("Fetched: {} characters, kept in memory only", v.chars().count()))
            } else {
                ("\u{2013}", p.faint, "Not fetched yet: fetched when a request first uses it".to_string())
            };
            ui.add_sized([22.0, 24.0], egui::Label::new(RichText::new(icon).color(color))).on_hover_text(tip);
            if widgets::icon_button(ui, icons::X, "Remove").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        sources.remove(i);
    }
    ui.horizontal(|ui| {
        if ui.button(format!("{} Add a command", icons::TERMINAL_WINDOW)).clicked() {
            sources.push(VarSource::new(SourceKind::Command));
        }
        if ui.button(format!("{} Add a Bitwarden secret", icons::LOCK_KEY)).clicked() {
            sources.push(VarSource::new(SourceKind::Bitwarden));
        }
    });
    if sources.iter().any(|s| s.kind == SourceKind::Command) {
        help(ui, "Commands run in the system shell (cmd on Windows, sh elsewhere) on this computer; the output, trimmed, is the value.");
    }
}

/// Unlocking the Bitwarden CLI for this session.
fn bitwarden_panel(ui: &mut Ui, bw: &mut crate::app::Bitwarden, actions: &mut Vec<Action>) {
    let p = pal(ui);
    egui::Frame::new().fill(p.surface2).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{}  Bitwarden", icons::LOCK_KEY)).strong());
            let (text, color) = match &bw.status {
                _ if !bw.session.is_empty() => ("unlocked for this session".to_string(), p.ok_text),
                Some((s, email)) if s == "unlocked" => (format!("unlocked{}", if email.is_empty() { String::new() } else { format!(" ({email})") }), p.ok_text),
                Some((s, email)) if s == "locked" => (format!("locked{}", if email.is_empty() { String::new() } else { format!(" ({email})") }), p.warn_text),
                Some((s, _)) if s == "unauthenticated" => ("not logged in".to_string(), p.err_text),
                Some((s, _)) => (s.clone(), p.muted),
                None => ("status not checked".to_string(), p.muted),
            };
            ui.label(RichText::new(text).size(12.5).color(color));
            if bw.busy {
                ui.spinner();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !bw.session.is_empty() && ui.button(format!("{} Lock", icons::LOCK_KEY)).on_hover_text("Forget the session key and fetched values").clicked() {
                    actions.push(Action::BitwardenLock);
                }
                if ui.add_enabled(!bw.busy, egui::Button::new(format!("{} Sync", icons::CLOUD_ARROW_DOWN))).on_hover_text("bw sync: get the latest vault").clicked() {
                    actions.push(Action::BitwardenSync);
                }
                if ui.add_enabled(!bw.busy, egui::Button::new("Check")).on_hover_text("bw status").clicked() {
                    actions.push(Action::BitwardenStatus);
                }
            });
        });
        if bw.session.is_empty() {
            ui.horizontal(|ui| {
                let (label, tip) = if bw.logged_in() {
                    (format!("{} Unlock\u{2026}", icons::LOCK_OPEN), "Opens a terminal window running `bw unlock`; type your master password there")
                } else {
                    (format!("{} Log in\u{2026}", icons::LOCK_OPEN), "Opens a terminal window running `bw login`; sign in there")
                };
                if ui.add_enabled(!bw.busy, egui::Button::new(label)).on_hover_text(tip).clicked() {
                    actions.push(Action::BitwardenUnlock);
                }
                if bw.busy {
                    ui.label(RichText::new("waiting for the Bitwarden window\u{2026}").size(12.5).color(p.muted));
                }
            });
            help(
                ui,
                "Uses the Bitwarden CLI (bw). Your master password is typed into bw in its own window, never into ApiCraft; only the session key comes back, and it's kept in memory. Skip this if BW_SESSION is already set where the app runs.",
            );
        }
    });
}

fn runtime(app: &mut ApiCraftApp, ui: &mut Ui) {
    head(app, ui, "Runtime variables");
    help(ui, "Values captured from responses. They override environment and collection variables until cleared, and are saved with the catalog.");
    ui.add_space(6.0);
    egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| kv_table(ui, "runtime", &mut app.catalog.runtime, "variable", "value"));
    foot(ui, |ui| {
        if accent_button(ui, "Done").clicked() {
            app.modal = None;
        }
        if danger_button(ui, format!("{} Clear all", icons::TRASH)).clicked() {
            app.catalog.runtime.clear();
        }
    });
}

fn collection(app: &mut ApiCraftApp, ui: &mut Ui, id: &str, tab: CollectionTab) {
    let name = app.catalog.collection(id).map(|c| c.name.clone()).unwrap_or_default();
    head(app, ui, &name);
    let mut t = tab;
    let dir = app.catalog.collection(id).and_then(|c| c.directory.clone());
    let changed = dir.as_ref().and_then(|d| app.git.get(d)).and_then(|g| match &g.status {
        Some(Ok(GitReply::Status(st))) => Some(st.files.len()),
        _ => None,
    });
    if widgets::tabs(
        ui,
        &mut t,
        &[
            (CollectionTab::General, "General".into(), 0),
            (CollectionTab::Variables, "Variables".into(), 0),
            (CollectionTab::Auth, "Auth".into(), 0),
            (CollectionTab::Files, "Folder & Git".into(), changed.unwrap_or(0)),
        ],
    ) {
        if t == CollectionTab::Files {
            if let Some(dir) = &dir {
                app.actions.push(Action::Git(GitRequest::Status { dir: dir.clone() }));
            }
        }
        app.modal = Some(Modal::Collection { id: id.to_string(), tab: t });
    }
    if t == CollectionTab::Files {
        files_tab(app, ui, id, dir);
        foot(ui, |ui| {
            if accent_button(ui, "Done").clicked() {
                app.modal = None;
            }
        });
        return;
    }
    let native = app.platform.native_grpc();
    let default_redirect = app.platform.default_redirect_uri();
    let now = app.platform.now_ms();
    let ApiCraftApp { catalog, actions, .. } = app;
    let Some(c) = catalog.collection_mut(id) else { return };
    egui::ScrollArea::vertical().max_height(460.0).show(ui, |ui| match t {
        CollectionTab::General => {
            ui.label(RichText::new("Name").strong());
            let r = ui.add(TextEdit::singleline(&mut c.name).desired_width(f32::INFINITY));
            widgets::replace_default(ui, &r, &c.name);
            ui.add_space(6.0);
            ui.label(RichText::new("Description").strong());
            ui.add(TextEdit::multiline(&mut c.description).desired_rows(5).desired_width(f32::INFINITY));
        }
        CollectionTab::Variables => {
            help(ui, "Available to every request in this collection as {{name}}. Environments override them.");
            ui.add_space(4.0);
            kv_table(ui, &format!("cvars-{id}"), &mut c.variables, "variable", "value");
        }
        CollectionTab::Auth => {
            help(ui, "Requests set to \u{201c}Inherit\u{201d} use this.");
            ui.add_space(4.0);
            let cx = AuthCtx { owner: id, allow_inherit: false, inherited: None, native, default_redirect, now, can_get_token: true };
            let mut ignore = None;
            widgets::auth_editor(ui, &mut c.auth, &cx, actions, &mut ignore);
        }
        CollectionTab::Files => {}
    });
    foot(ui, |ui| {
        if accent_button(ui, "Done").clicked() {
            app.modal = None;
        }
    });
}

/// Where the collection is kept on disk, and Git for that folder.
fn files_tab(app: &mut ApiCraftApp, ui: &mut Ui, id: &str, dir: Option<String>) {
    let p = pal(ui);
    let Some(dir) = dir else {
        help(
            ui,
            "This collection is kept inside the app's catalog. Link it to a folder to keep it as one text file per request and workflow instead, ready for Git: changes you make here are written to the files, and edits made to the files (by hand, or by a git pull) are picked up.",
        );
        ui.add_space(8.0);
        if accent_button(ui, format!("{} Link to a folder\u{2026}", icons::FOLDER_OPEN)).clicked() {
            app.actions.push(Action::LinkFolder(id.to_string()));
        }
        help(ui, "Choose an empty folder, or one that already holds this collection. If it's inside a Git repository, its changes can be committed from here.");
        return;
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(icons::FOLDER_OPEN).color(p.muted));
        let r = ui.add(egui::Label::new(RichText::new(&dir).monospace()).truncate().selectable(true));
        crate::textmenu::copy_menu(&r, &dir);
    });
    ui.horizontal(|ui| {
        if ui.button(format!("{} Reload from folder", icons::ARROWS_CLOCKWISE)).clicked() {
            app.actions.push(Action::ReloadFolder(id.to_string()));
        }
        if ui.button(format!("{} Link to another folder\u{2026}", icons::FOLDER_OPEN)).clicked() {
            app.actions.push(Action::LinkFolder(id.to_string()));
        }
        if ui.button(format!("{} Keep in the catalog instead", icons::LINK_BREAK)).on_hover_text("The files stay where they are").clicked() {
            app.actions.push(Action::UnlinkFolder(id.to_string()));
        }
    });
    ui.add_space(6.0);
    ui.separator();
    let view = app.git.entry(dir.clone()).or_default();
    let busy = view.busy;
    if view.status.is_none() && busy.is_none() {
        app.actions.push(Action::Git(GitRequest::Status { dir: dir.clone() }));
    }
    let mut acts: Vec<Action> = Vec::new();
    let mut confirm = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{}  Git", icons::GIT_BRANCH)).strong());
        if let Some(what) = busy {
            ui.spinner();
            ui.label(RichText::new(format!("{what}\u{2026}")).size(12.5).color(p.muted));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add_enabled(busy.is_none(), egui::Button::new(format!("{} Refresh", icons::ARROWS_CLOCKWISE))).clicked() {
                acts.push(Action::Git(GitRequest::Status { dir: dir.clone() }));
            }
        });
    });
    match &view.status {
        None => {}
        Some(Err(e)) => alert(ui, AlertKind::Error, e),
        Some(Ok(GitReply::NotRepo)) => {
            help(ui, "This folder isn't in a Git repository.");
            if ui.add_enabled(busy.is_none(), egui::Button::new(format!("{} Create a repository here", icons::GIT_BRANCH))).clicked() {
                acts.push(Action::Git(GitRequest::Init { dir: dir.clone() }));
            }
        }
        Some(Ok(GitReply::Status(st))) => {
            ui.horizontal_wrapped(|ui| {
                widgets::chip(ui, format!("{} {}", icons::GIT_BRANCH, if st.branch.is_empty() { "(no branch)" } else { &st.branch }));
                match &st.upstream {
                    Some(u) => {
                        ui.label(RichText::new(format!("tracking {u}")).size(12.0).color(p.muted));
                        if st.ahead > 0 {
                            widgets::pill(ui, format!("{} {} to push", icons::ARROW_UP, st.ahead), p.accent_soft, p.accent);
                        }
                        if st.behind > 0 {
                            widgets::pill(ui, format!("{} {} to pull", icons::ARROW_DOWN, st.behind), p.warn_bg, p.warn_text);
                        }
                    }
                    None => {
                        ui.label(RichText::new("no upstream branch").size(12.0).color(p.faint));
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add_enabled(busy.is_none(), egui::Button::new(format!("{} Push", icons::CLOUD_ARROW_UP))).clicked() {
                        acts.push(Action::Git(GitRequest::Push { dir: dir.clone() }));
                    }
                    if ui.add_enabled(busy.is_none() && st.upstream.is_some(), egui::Button::new(format!("{} Pull", icons::CLOUD_ARROW_DOWN))).clicked() {
                        acts.push(Action::Git(GitRequest::Pull { dir: dir.clone() }));
                    }
                });
            });
            ui.label(RichText::new(format!("Repository: {}", st.root)).size(11.5).color(p.faint));
            ui.add_space(6.0);
            if st.files.is_empty() {
                ui.label(RichText::new(format!("{} No changes in this folder.", icons::CHECK)).color(p.ok_text));
            } else {
                let included = st.files.iter().filter(|f| !view.unchecked.contains(&f.path)).count();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Changes  {}", st.files.len())).strong().size(12.5));
                    let mut all = included == st.files.len();
                    if ui.checkbox(&mut all, "all").changed() {
                        if all {
                            view.unchecked.clear();
                        } else {
                            view.unchecked = st.files.iter().map(|f| f.path.clone()).collect();
                        }
                    }
                });
                egui::ScrollArea::vertical().id_salt("git-files").max_height(150.0).show(ui, |ui| {
                    for f in &st.files {
                        ui.horizontal(|ui| {
                            let mut on = !view.unchecked.contains(&f.path);
                            if ui.add(egui::Checkbox::without_text(&mut on)).on_hover_text("Include in the commit").changed() {
                                if on {
                                    view.unchecked.remove(&f.path);
                                } else {
                                    view.unchecked.insert(f.path.clone());
                                }
                            }
                            let color = match f.letter() {
                                'U' | 'A' => p.ok_text,
                                'D' | '!' => p.err_text,
                                _ => p.warn_text,
                            };
                            ui.label(RichText::new(f.letter().to_string()).monospace().strong().color(color)).on_hover_text(f.label());
                            let shown = view.diff.as_ref().is_some_and(|(d, _)| d == &f.path);
                            let name = RichText::new(&f.shown).monospace().size(12.5);
                            if ui.add(egui::Button::selectable(shown, name)).on_hover_text("Show the changes").clicked() {
                                if shown {
                                    view.diff = None;
                                } else {
                                    acts.push(Action::Git(GitRequest::Diff { dir: dir.clone(), path: f.path.clone() }));
                                }
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if widgets::icon_button(ui, icons::ARROW_COUNTER_CLOCKWISE, "Discard the changes to this file").clicked() {
                                    confirm = Some(vec![f.path.clone()]);
                                }
                            });
                        });
                    }
                });
                if let Some((path, text)) = &view.diff {
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("{}  {}", icons::GIT_DIFF, path.strip_prefix(&st.prefix).unwrap_or(path))).size(12.0).color(p.muted));
                    egui::Frame::new().fill(p.surface2).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(6).inner_margin(egui::Margin::same(8)).show(
                        ui,
                        |ui| {
                            egui::ScrollArea::both().id_salt("git-diff").max_height(200.0).auto_shrink([false, true]).show(ui, |ui| {
                                diff_view(ui, text);
                            });
                        },
                    );
                }
                ui.add_space(6.0);
                ui.add(
                    TextEdit::multiline(&mut view.message)
                        .hint_text("Commit message: what changed in these requests?")
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                );
                ui.horizontal(|ui| {
                    let can = busy.is_none() && included > 0 && !view.message.trim().is_empty();
                    let label = format!("{} Commit {included} file{}", icons::GIT_COMMIT, if included == 1 { "" } else { "s" });
                    let b = ui.add_enabled(
                        can,
                        egui::Button::new(RichText::new(label).color(if can { p.accent_text } else { p.muted })).fill(if can { p.accent } else { p.surface3 }),
                    );
                    if b.on_disabled_hover_text("Choose files and write a message").clicked() {
                        let paths = st.files.iter().filter(|f| !view.unchecked.contains(&f.path)).map(|f| f.path.clone()).collect();
                        acts.push(Action::Git(GitRequest::Commit { dir: dir.clone(), message: view.message.trim().to_string(), paths }));
                    }
                    if included > 0
                        && ui
                            .button(format!("{} Discard {included}", icons::ARROW_COUNTER_CLOCKWISE))
                            .on_hover_text("Throw away the changes to the chosen files")
                            .clicked()
                    {
                        confirm = Some(st.files.iter().filter(|f| !view.unchecked.contains(&f.path)).map(|f| f.path.clone()).collect());
                    }
                });
                help(ui, "Only files in this collection's folder are staged and committed; anything else in the repository is left alone.");
            }
            if !st.log.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new("Recent commits").strong().size(12.5));
                egui::ScrollArea::vertical().id_salt("git-log").max_height(110.0).show(ui, |ui| {
                    for c in &st.log {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&c.hash).monospace().size(12.0).color(p.faint));
                            ui.add(egui::Label::new(RichText::new(&c.subject).size(12.5)).truncate());
                            ui.label(RichText::new(format!("{} \u{b7} {}", c.author, c.when)).size(11.5).color(p.muted));
                        });
                    }
                });
            }
        }
        Some(Ok(_)) => {}
    }
    app.actions.extend(acts);
    if let Some(paths) = confirm {
        let n = paths.len();
        app.modal = Some(Modal::Confirm {
            message: format!("Throw away the uncommitted changes to {n} file{}? New files are deleted.", if n == 1 { "" } else { "s" }),
            action: Confirm::GitDiscard { dir, paths },
        });
    }
}

/// A unified diff with added and removed lines coloured.
fn diff_view(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    ui.spacing_mut().item_spacing.y = 0.0;
    for line in text.lines().take(2000) {
        let color = if line.starts_with("+++") || line.starts_with("---") || line.starts_with("diff ") || line.starts_with("index ") {
            p.faint
        } else if line.starts_with('+') {
            p.ok_text
        } else if line.starts_with('-') {
            p.err_text
        } else if line.starts_with("@@") {
            p.accent
        } else {
            p.text2
        };
        ui.add(egui::Label::new(RichText::new(line).monospace().size(12.0).color(color)).extend());
    }
}

fn rename(app: &mut ApiCraftApp, ui: &mut Ui) {
    head(app, ui, "Rename");
    let mut save = false;
    if let Some(Modal::Rename { value, .. }) = &mut app.modal {
        let r = ui.add(TextEdit::singleline(value).desired_width(f32::INFINITY));
        if !r.has_focus() && !r.lost_focus() {
            widgets::focus_selected(ui, &r, value.chars().count());
        }
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            save = true;
        }
    }
    foot(ui, |ui| {
        if accent_button(ui, "Save").clicked() {
            save = true;
        }
        if ui.button("Cancel").clicked() {
            app.modal = None;
        }
    });
    if save {
        app.actions.push(Action::RenameSave);
    }
}

/// Where the import dialog reads from.
#[derive(Clone, Copy, PartialEq)]
enum ImportSource {
    File,
    Url,
    Paste,
}

fn import(app: &mut ApiCraftApp, ui: &mut Ui) {
    head(app, ui, "Import");
    let p = pal(ui);
    let cols: Vec<(String, String)> = app.catalog.collections.iter().map(|c| (c.id.clone(), c.name.clone())).collect();
    let col_auth: Vec<(String, String)> = app
        .catalog
        .collections
        .iter()
        .map(|c| (c.id.clone(), Auth::KINDS.iter().find(|(k, _)| *k == c.auth.key()).map(|(_, l)| l.to_string()).unwrap_or_default()))
        .collect();
    let (native, default_redirect, now) = (app.platform.native_grpc(), app.platform.default_redirect_uri(), app.platform.now_ms());
    let mut acts = Vec::new();
    let source_id = egui::Id::unique("import-source");
    let mut source = ui.ctx().data(|d| d.get_temp::<ImportSource>(source_id)).unwrap_or(ImportSource::File);
    let Some(Modal::Import { text, target, url, auth, headers, login }) = &mut app.modal else { return };

    ui.horizontal(|ui| {
        ui.label(RichText::new("Import into").strong());
        let label = target.as_ref().and_then(|t| cols.iter().find(|c| &c.0 == t)).map(|c| c.1.clone()).unwrap_or_else(|| "New collection".into());
        egui::ComboBox::from_id_salt("import-target").selected_text(label).width(260.0).show_ui(ui, |ui| {
            if ui.selectable_label(target.is_none(), "New collection").clicked() {
                *target = None;
            }
            for (id, name) in &cols {
                if ui.selectable_label(target.as_deref() == Some(id), name).clicked() {
                    *target = Some(id.clone());
                }
            }
        });
    });
    ui.add_space(8.0);
    widgets::segmented(ui, &mut source, &[(ImportSource::File, "File"), (ImportSource::Url, "URL"), (ImportSource::Paste, "Paste text")]);
    ui.ctx().data_mut(|d| d.insert_temp(source_id, source));
    ui.add_space(8.0);

    let mut go = false;
    egui::Frame::new().fill(p.surface2).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(10).inner_margin(16).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.set_min_height(150.0);
        match source {
            ImportSource::File => {
                ui.vertical_centered(|ui| {
                    ui.add_space(8.0);
                    ui.label(RichText::new(icons::UPLOAD).size(28.0).color(p.faint));
                    ui.label(RichText::new("Choose a file, or drop one anywhere on the window").strong());
                    ui.add_space(8.0);
                    if accent_button(ui, format!("{} Choose file\u{2026}", icons::FOLDER_OPEN)).clicked() {
                        go = true;
                    }
                });
            }
            ImportSource::Url => {
                ui.label(RichText::new("Address of the document").strong());
                let r = ui.add(TextEdit::singleline(url).hint_text("https://petstore3.swagger.io/api/v3/openapi.json").desired_width(f32::INFINITY));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = true;
                }
                ui.add_space(4.0);
                help(ui, "Relative server URLs in an OpenAPI document are resolved against this address. For a WSDL, include ?wsdl; for OData, use the service\u{2019}s $metadata URL.");
                ui.add_space(6.0);
                // Only a collection being imported into has auth to inherit.
                if target.is_none() && **auth == Auth::Inherit {
                    **auth = Auth::None;
                }
                let inherited = target.as_ref().and_then(|t| col_auth.iter().find(|c| &c.0 == t)).map(|c| (c.0.as_str(), c.1.clone()));
                egui::CollapsingHeader::new(RichText::new(format!("{} Authentication", icons::KEY)).strong()).id_salt("import-auth").show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        let cx = AuthCtx { owner: "import", allow_inherit: target.is_some(), inherited, native, default_redirect, now, can_get_token: false };
                        let mut open_collection = None;
                        widgets::auth_editor(ui, auth, &cx, &mut acts, &mut open_collection);
                        if let Some(c) = open_collection {
                            acts.push(Action::CollectionSettings(c));
                        }
                        ui.add_space(8.0);
                        let mut sign_in = login.is_some();
                        if ui.checkbox(&mut sign_in, "Sign in through a web page first").changed() {
                            *login = sign_in.then(Default::default);
                        }
                        if let Some(l) = login {
                            ui.add_space(4.0);
                            widgets::field(ui, "Username", &mut l.username, false, "Leave blank if the page only asks for a password");
                            widgets::field(ui, "Password", &mut l.password, true, "");
                            widgets::field(ui, "Sign-in page", &mut l.page, false, "Optional: found from the address above");
                        }
                        ui.add_space(8.0);
                        ui.label(RichText::new("Headers").strong());
                        widgets::kv_table(ui, "import-headers", headers, "Header", "Value");
                        ui.add_space(4.0);
                        let cookies = if native { "" } else { " In a browser, signing in and Cookie headers need the local agent." };
                        help(ui, &format!("Used for this download only, and not saved. A sign-in with several steps, or one built with JavaScript, can\u{2019}t be filled in here: sign in with your browser, copy the Cookie header of a request from its developer tools, and add it as a header.{cookies}"));
                    });
                });
            }
            ImportSource::Paste => {
                ui.add(
                    TextEdit::multiline(text)
                        .code_editor()
                        .desired_rows(7)
                        .desired_width(f32::INFINITY)
                        .hint_text("curl https://api.example.com -H 'Accept: application/json'\n\nor a Postman, OpenAPI or catalog JSON document"),
                );
            }
        }
    });
    ui.add_space(6.0);
    help(ui, "Reads OpenAPI / Swagger (JSON or YAML), WSDL, OData $metadata, GraphQL introspection results, Postman collections and environments, Fizl ApiCraft catalogs (.json, .json.gz) and curl commands.");
    foot(ui, |ui| {
        let label = match source {
            ImportSource::File => None,
            ImportSource::Url => Some("Import from URL"),
            ImportSource::Paste => Some("Import"),
        };
        if label.is_some_and(|l| accent_button(ui, l).clicked()) {
            go = true;
        }
        if ui.button("Cancel").clicked() {
            app.modal = None;
        }
    });
    app.actions.append(&mut acts);
    if go {
        app.actions.push(match source {
            ImportSource::File => Action::OpenFiles,
            ImportSource::Url => Action::ImportUrl,
            ImportSource::Paste => Action::ImportText,
        });
    }
}

fn confirm(app: &mut ApiCraftApp, ui: &mut Ui, message: &str) {
    let git = matches!(&app.modal, Some(Modal::Confirm { action: Confirm::GitDiscard { .. }, .. }));
    head(app, ui, if git { "Discard changes" } else { "Delete" });
    let p = pal(ui);
    ui.horizontal_top(|ui| {
        ui.label(RichText::new(icons::TRASH).size(22.0).color(p.danger));
        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.label(RichText::new(message).color(p.text));
            ui.label(
                RichText::new(if git { "This can't be undone." } else { "Undo (Ctrl+Z, or the arrow at the top) brings it back." }).size(12.0).color(p.muted),
            );
        });
    });
    foot(ui, |ui| {
        if danger_button(ui, if git { "Discard" } else { "Delete" }).clicked() {
            app.actions.push(Action::Confirm);
        }
        if ui.button("Cancel").clicked() {
            app.modal = None;
        }
    });
}

fn settings(app: &mut ApiCraftApp, ui: &mut Ui) {
    head(app, ui, "Settings");
    let tab_id = egui::Id::unique("settings-tab");
    let mut network = ui.ctx().data(|d| d.get_temp::<bool>(tab_id)).unwrap_or(false);
    widgets::segmented(ui, &mut network, &[(false, "General"), (true, "Network")]);
    ui.ctx().data_mut(|d| d.insert_temp(tab_id, network));
    ui.add_space(8.0);
    if network {
        network_settings(app, ui);
        foot(ui, |ui| {
            if accent_button(ui, "Done").clicked() {
                app.modal = None;
            }
        });
        return;
    }
    let p = pal(ui);
    let st = app.platform.status();
    alert(ui, if st.level == crate::platform::Level::Warn { AlertKind::Warn } else { AlertKind::Ok }, &st.detail);
    ui.add_space(6.0);
    let inbox = app.inbox.clone();
    app.platform.settings_ui(ui, &inbox);
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Theme").strong());
        let mut t = app.prefs.theme;
        widgets::segmented(ui, &mut t, &[(ThemeChoice::System, "System"), (ThemeChoice::Light, "Light"), (ThemeChoice::Dark, "Dark")]);
        if t != app.prefs.theme {
            app.prefs.theme = t;
            app.apply_theme(ui.ctx());
        }
    });
    ui.add_space(8.0);
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Examples").strong());
        if ui.button("Add the example collection").on_hover_text("REST, SOAP, gRPC, WebSocket and MCP examples, including the MCP test mode").clicked() {
            app.actions.push(Action::AddExamples);
        }
    });
    ui.add_space(6.0);
    ui.label(RichText::new("Storage").strong());
    let gz = apicraft_core::store::to_gzip(&app.catalog).len();
    let json = apicraft_core::store::to_json(&app.catalog).len();
    ui.label(
        RichText::new(format!(
            "{}\nCompressed JSON: {} (from {} of JSON). Export from the sidebar to keep a copy elsewhere.",
            app.platform.storage_description(),
            human_size(gz),
            human_size(json)
        ))
        .color(p.muted),
    );
    ui.add_space(10.0);
    ui.label(RichText::new(format!("Fizl ApiCraft {}  \u{b7}  \u{a9} 2026 Fizl Ltd & Simon Soanes", env!("CARGO_PKG_VERSION"))).size(12.0).color(p.faint));
    foot(ui, |ui| {
        if accent_button(ui, "Done").clicked() {
            app.modal = None;
        }
    });
}

/// Proxy and TLS settings (kept in the catalog, so the CLI uses them too).
fn network_settings(app: &mut ApiCraftApp, ui: &mut Ui) {
    let p = pal(ui);
    let honoured = app.platform.native_grpc();
    let n = &mut app.catalog.network;
    if !honoured {
        alert(
            ui,
            AlertKind::Warn,
            "The browser uses its own proxy and certificate settings. These apply when requests go through the local agent, and in the desktop app and CLI.",
        );
        ui.add_space(6.0);
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new("Proxy").strong());
        widgets::segmented(ui, &mut n.proxy, &[(ProxyMode::System, "System"), (ProxyMode::None, "None"), (ProxyMode::Manual, "Manual")]);
    });
    match n.proxy {
        ProxyMode::System => help(ui, "Uses the HTTPS_PROXY, HTTP_PROXY, ALL_PROXY and NO_PROXY environment variables, when set."),
        ProxyMode::None => help(ui, "Connects directly, ignoring any proxy environment variables."),
        ProxyMode::Manual => {
            ui.add_space(4.0);
            egui::Grid::new("proxy-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Proxy URL");
                ui.add(TextEdit::singleline(&mut n.proxy_url).hint_text("http://proxy.corp:8080   or   socks5://127.0.0.1:1080").desired_width(360.0));
                ui.end_row();
                ui.label("Username");
                ui.add(TextEdit::singleline(&mut n.proxy_username).hint_text("optional").desired_width(360.0));
                ui.end_row();
                ui.label("Password");
                ui.add(TextEdit::singleline(&mut n.proxy_password).password(true).desired_width(360.0));
                ui.end_row();
            });
        }
    }
    if n.proxy != ProxyMode::None {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Bypass for");
            ui.add(TextEdit::singleline(&mut n.no_proxy).hint_text("localhost, 127.0.0.1, .internal.example.com").desired_width(360.0));
        });
    }
    ui.add_space(10.0);
    ui.separator();
    ui.label(RichText::new("Certificates").strong());
    ui.checkbox(&mut n.verify_tls, "Verify server certificates");
    if !n.verify_tls {
        alert(ui, AlertKind::Warn, "Any certificate is accepted, so connections can be intercepted. Use this only for local and test servers.");
    }
    ui.add_space(6.0);
    ui.label("Trusted CA certificates");
    ui.add(
        TextEdit::multiline(&mut n.ca_certificates)
            .hint_text("PEM text (-----BEGIN CERTIFICATE-----\u{2026}), or the path of a .pem / .crt file")
            .desired_rows(3)
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Monospace),
    );
    help(ui, "When set, servers are checked against these certificates instead of the built-in roots: for a company CA, or a test server's own certificate.");
    ui.add_space(8.0);
    ui.label(
        RichText::new(format!(
            "{}  Client certificates (mutual TLS) are set in a request's or collection's Auth tab. {{{{variables}}}} work in these fields.",
            icons::SHIELD_CHECK
        ))
        .size(12.0)
        .color(p.muted),
    );
}

fn step_result(app: &mut ApiCraftApp, ui: &mut Ui, index: usize) {
    let Some(rep) = app.run.as_ref().and_then(|r| r.reports.get(index)) else {
        app.modal = None;
        return;
    };
    let title = format!("Step {}: {}", rep.index + 1, rep.name);
    let request_id = rep.request_id.clone();
    let Some(o) = rep.outcome.clone() else { return };
    head(app, ui, &title);
    let pretty = apicraft_core::format::pretty_body(o.response.as_ref().map(|r| r.content_type()).unwrap_or(""), &o.body_text);
    let mut tab = ui.ctx().data(|d| d.get_temp::<RespTab>(egui::Id::unique("step-tab"))).unwrap_or(RespTab::Body);
    let mut raw = false;
    ui.allocate_ui(egui::vec2(ui.available_width(), 460.0), |ui| {
        response::outcome_view(ui, &o, &mut tab, &mut raw, &pretty, &app.highlighter, None, "step");
    });
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("step-tab"), tab));
    foot(ui, |ui| {
        if accent_button(ui, "Close").clicked() {
            app.modal = None;
        }
        if ui.button("Open request").clicked() {
            app.actions.push(Action::Open(request_id.clone()));
        }
    });
}
