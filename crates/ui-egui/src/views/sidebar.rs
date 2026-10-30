//! The sidebar: collections tree, workflows, import/export.

use apicraft_core::codegen::Target;
use apicraft_core::model::{Item, MoveTo, Protocol, Request};
use egui::{Align, CornerRadius, Layout, Rect, RichText, Sense, Stroke, Ui};

use super::widgets::{icon_button, section_label};
use crate::actions::Action;
use crate::app::{ApiCraftApp, Modal, View};
use crate::icons;
use crate::theme::{method_color, method_label, pal, RADIUS_SM};

const ROW_H: f32 = 28.0;

pub fn show(app: &mut ApiCraftApp, ui: &mut Ui) {
    let p = pal(ui);
    // brand
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), Sense::hover());
        logo(ui, rect);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(RichText::new("Fizl ApiCraft").strong().size(15.0).color(p.text));
            ui.label(RichText::new("REST \u{b7} SOAP \u{b7} gRPC \u{b7} WebSocket").size(11.0).color(p.muted));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icon_button(ui, icons::CARET_DOUBLE_LEFT, "Hide sidebar").clicked() {
                app.prefs.sidebar_open = false;
                app.sidebar_forced = false;
            }
        });
    });
    ui.add_space(10.0);
    let w = ui.available_width();
    ui.scope(|ui| {
        ui.spacing_mut().button_padding = egui::vec2(10.0, 8.0);
        if ui
            .add_sized(
                [w, 36.0],
                egui::Button::new(RichText::new(format!("{}  New request", icons::PLUS)).strong().color(p.accent_text))
                    .fill(p.accent)
                    .corner_radius(CornerRadius::same(10)),
            )
            .clicked()
        {
            app.actions.push(Action::NewRequest(None));
        }
    });
    ui.add_space(8.0);
    let filter =
        ui.add(egui::TextEdit::singleline(&mut app.filter).hint_text(format!("{} Filter requests", icons::MAGNIFYING_GLASS)).desired_width(f32::INFINITY));
    if std::mem::take(&mut app.focus_filter) {
        filter.request_focus();
    }
    ui.add_space(4.0);

    let footer_h = 40.0;
    let list_h = (ui.available_height() - footer_h).max(80.0);
    egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        let mut acts = Vec::new();
        header(ui, "Collections", "New collection", || acts.push(Action::NewCollection)).context_menu(|ui| {
            ui.set_min_width(190.0);
            if ui.button(format!("{}  New collection", icons::PLUS)).clicked() {
                acts.push(Action::NewCollection);
                ui.close();
            }
            if ui.button(format!("{}  Import\u{2026}", icons::UPLOAD)).clicked() {
                acts.push(Action::ImportInto(None));
                ui.close();
            }
            if ui.button(format!("{}  Open folder\u{2026}", icons::FOLDER_OPEN)).on_hover_text("A folder with a text file per request and workflow").clicked() {
                acts.push(Action::OpenFolder);
                ui.close();
            }
        });
        let filter = app.filter.trim().to_lowercase();
        if app.catalog.collections.is_empty() {
            ui.label(RichText::new("No collections yet. Create one, or import a Postman collection.").size(12.0).color(p.faint));
        }
        for c in &app.catalog.collections {
            let open = app.expanded.contains(&c.id) || !filter.is_empty();
            if !filter.is_empty() && !c.name.to_lowercase().contains(&filter) && !has_match(&c.items, &filter) {
                continue;
            }
            let label = if c.directory.is_some() { format!("{}  {}", c.name, icons::FOLDER_OPEN) } else { c.name.clone() };
            let resp = row(ui, 0, Some(open), None, &label, false, &c.id, &mut acts, MenuKind::Collection(c.directory.is_some()), Dnd::Into(c.id.clone()));
            let resp = match &c.directory {
                Some(dir) => resp.on_hover_text(format!("Saved as files in {dir}")),
                None => resp,
            };
            if resp.clicked() {
                acts.push(Action::Toggle(c.id.clone()));
            }
            if open {
                if c.items.is_empty() {
                    let hint = format!("{}-hint", c.id);
                    let r = row(ui, 1, None, None, "+ Add a request", false, &hint, &mut acts, MenuKind::None, Dnd::Into(c.id.clone()));
                    if r.clicked() {
                        acts.push(Action::NewRequest(Some(c.id.clone())));
                    }
                }
                items(app, ui, &c.items, 1, &filter, &mut acts);
            }
        }
        ui.add_space(10.0);
        header(ui, "Workflows", "New workflow", || acts.push(Action::NewWorkflow));
        if app.catalog.workflows.is_empty() {
            ui.label(RichText::new("Chain requests and pass values between them.").size(12.0).color(p.faint));
        }
        for wf in &app.catalog.workflows {
            let active = app.view == View::Workflow(wf.id.clone());
            let running = app.run.as_ref().map(|r| r.running && r.workflow_id == wf.id).unwrap_or(false);
            let badge = Badge { text: if running { icons::PLAY.into() } else { icons::FLOW.into() }, color: if running { p.m_get } else { p.muted } };
            let resp = row(ui, 0, None, Some(badge), &wf.name, active, &wf.id, &mut acts, MenuKind::Workflow, Dnd::None);
            if resp.clicked() {
                acts.push(Action::OpenWorkflow(wf.id.clone()));
            }
            if resp.double_clicked() {
                acts.push(Action::Rename(wf.id.clone()));
            }
        }
        app.actions.extend(acts);
    });

    // footer
    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().button_padding = egui::vec2(8.0, 6.0);
            if ui.add(egui::Button::new(footer_text(icons::UPLOAD, "Import")).frame(false)).on_hover_text("Postman, OpenAPI, catalog files or curl").clicked() {
                app.actions.push(Action::ImportInto(None));
            }
            let export = ui.add(egui::Button::new(footer_text(icons::DOWNLOAD, "Export")).frame(false));
            egui::Popup::menu(&export).show(|ui| {
                ui.label(RichText::new("WHOLE CATALOG").size(11.0).color(p.faint));
                if ui.button("Save .json.gz (compressed)").clicked() {
                    app.actions.push(Action::ExportCatalogGz);
                    ui.close();
                }
                if ui.button("Save .json (text)").clicked() {
                    app.actions.push(Action::ExportCatalogJson);
                    ui.close();
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(footer_text(icons::GEAR, "Settings")).frame(false))
                    .on_hover_text("Theme, network, proxy and certificates")
                    .clicked()
                {
                    app.modal = Some(Modal::Settings);
                }
            });
        });
        let r = ui.min_rect();
        ui.painter().hline(r.x_range(), r.top() - 4.0, Stroke::new(1.0, p.border));
    });
}

/// A footer label with a larger icon.
fn footer_text(icon: &str, label: &str) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(icon, 0.0, egui::TextFormat { font_id: egui::FontId::proportional(17.0), valign: Align::Center, ..Default::default() });
    job.append(label, 6.0, egui::TextFormat { font_id: egui::FontId::proportional(13.5), valign: Align::Center, ..Default::default() });
    job
}

/// The sidebar folded away: a strip of icons for the things used most.
pub fn rail(app: &mut ApiCraftApp, ui: &mut Ui) {
    let p = pal(ui);
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), Sense::click());
        logo(ui, rect);
        if resp.on_hover_text("Show the sidebar").clicked() {
            show_sidebar(app);
        }
        ui.add_space(2.0);
        if rail_button(ui, icons::CARET_DOUBLE_RIGHT, "Show the sidebar", false).clicked() {
            show_sidebar(app);
        }
        ui.add_space(4.0);
        if rail_button(ui, icons::PLUS, "New request (Ctrl+N)", true).clicked() {
            app.actions.push(Action::NewRequest(None));
        }
        let n = app.catalog.collections.len();
        if rail_button(ui, icons::STACK, &format!("Collections ({n})"), false).clicked() {
            show_sidebar(app);
        }
        let n = app.catalog.workflows.len();
        if rail_button(ui, icons::FLOW, &format!("Workflows ({n})"), false).clicked() {
            show_sidebar(app);
        }
        if rail_button(ui, icons::MAGNIFYING_GLASS, "Filter requests", false).clicked() {
            show_sidebar(app);
            app.focus_filter = true;
        }
        ui.add_space(2.0);
        let r = ui.min_rect();
        ui.painter().hline(r.shrink2(egui::vec2(8.0, 0.0)).x_range(), ui.cursor().top(), Stroke::new(1.0, p.border));
        ui.add_space(4.0);
        if rail_button(ui, icons::UPLOAD, "Import: Postman, OpenAPI, WSDL, catalogs or curl", false).clicked() {
            app.actions.push(Action::ImportInto(None));
        }
        let export = rail_button(ui, icons::DOWNLOAD, "Export the whole catalog", false);
        egui::Popup::menu(&export).show(|ui| {
            ui.set_min_width(200.0);
            ui.label(RichText::new("WHOLE CATALOG").size(11.0).color(p.faint));
            if ui.button("Save .json.gz (compressed)").clicked() {
                app.actions.push(Action::ExportCatalogGz);
                ui.close();
            }
            if ui.button("Save .json (text)").clicked() {
                app.actions.push(Action::ExportCatalogJson);
                ui.close();
            }
        });
        if rail_button(ui, icons::FOLDER_OPEN, "Open a collection folder", false).clicked() {
            app.actions.push(Action::OpenFolder);
        }
    });
    ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        if rail_button(ui, icons::GEAR, "Settings", false).clicked() {
            app.modal = Some(Modal::Settings);
        }
        if rail_button(ui, icons::GLOBE, "Environments", false).clicked() {
            app.modal = Some(Modal::Environments { selected: None });
        }
    });
}
/// Reveal the full sidebar. In a narrow window that is a deliberate request, so
/// it stays open at a reduced width until there is room for it anyway.
fn show_sidebar(app: &mut ApiCraftApp) {
    app.prefs.sidebar_open = true;
    app.sidebar_forced = true;
}

/// A square icon button for the folded sidebar, with its tooltip to the side.
fn rail_button(ui: &mut Ui, icon: &str, tip: &str, accent: bool) -> egui::Response {
    let p = pal(ui);
    let size = egui::vec2(38.0, 36.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if accent {
        if resp.hovered() {
            p.accent.gamma_multiply(0.9)
        } else {
            p.accent
        }
    } else if resp.hovered() || resp.is_pointer_button_down_on() {
        p.surface3
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
    let color = if accent {
        p.accent_text
    } else if resp.hovered() {
        p.text
    } else {
        p.muted
    };
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icon, egui::FontId::proportional(19.0), color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip)
}

/// A section heading with a "+" button; the returned response takes a context menu.
fn header(ui: &mut Ui, title: &str, tip: &str, on_add: impl FnOnce()) -> egui::Response {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        section_label(ui, title);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icon_button(ui, icons::PLUS, tip).clicked() {
                on_add();
            }
        });
    })
    .response
    .interact(Sense::click())
}

fn has_match(items: &[Item], filter: &str) -> bool {
    items.iter().any(|i| match i {
        Item::Request(r) => matches(r, filter),
        Item::Folder(f) => f.name.to_lowercase().contains(filter) || has_match(&f.items, filter),
    })
}

fn matches(r: &Request, filter: &str) -> bool {
    filter.is_empty() || r.name.to_lowercase().contains(filter) || r.url.to_lowercase().contains(filter)
}

fn items(app: &ApiCraftApp, ui: &mut Ui, list: &[Item], depth: usize, filter: &str, acts: &mut Vec<Action>) {
    let p = pal(ui);
    for it in list {
        match it {
            Item::Folder(f) => {
                if !filter.is_empty() && !f.name.to_lowercase().contains(filter) && !has_match(&f.items, filter) {
                    continue;
                }
                let open = app.expanded.contains(&f.id) || !filter.is_empty();
                let resp = row(
                    ui,
                    depth,
                    Some(open),
                    Some(Badge { text: icons::FOLDER.into(), color: p.muted }),
                    &f.name,
                    false,
                    &f.id,
                    acts,
                    MenuKind::Folder,
                    Dnd::Folder,
                );
                if resp.clicked() {
                    acts.push(Action::Toggle(f.id.clone()));
                }
                if resp.double_clicked() {
                    acts.push(Action::Rename(f.id.clone()));
                }
                if open {
                    items(app, ui, &f.items, depth + 1, filter, acts);
                }
            }
            Item::Request(r) => {
                if !matches(r, filter) {
                    continue;
                }
                let active = app.view == View::Request(r.id.clone());
                let live = app.sending.contains(&r.id) || app.ws.as_ref().map(|w| w.request_id == r.id && w.open).unwrap_or(false);
                let badge = Badge { text: method_label(r.protocol, &r.method), color: method_color(p, r.protocol, &r.method) };
                let label = if live { format!("{}  \u{25cf}", r.name) } else { r.name.clone() };
                let resp = row(ui, depth, None, Some(badge), &label, active, &r.id, acts, MenuKind::Request(r.protocol), Dnd::Item);
                let resp = if resp.dragged() { resp } else { resp.on_hover_text(&r.url) };
                if resp.clicked() {
                    acts.push(Action::Open(r.id.clone()));
                }
                if resp.double_clicked() {
                    acts.push(Action::Rename(r.id.clone()));
                }
            }
        }
    }
}

struct Badge {
    text: String,
    color: egui::Color32,
}

#[derive(Clone, Copy)]
enum MenuKind {
    None,
    /// `true` when it's kept in a folder of files.
    Collection(bool),
    Folder,
    Request(Protocol),
    Workflow,
}

/// How a row takes part in drag and drop.
enum Dnd {
    None,
    /// A drop target only: dropped items go to the end of this container.
    Into(String),
    /// A draggable folder: drop above, into or below it.
    Folder,
    /// A draggable request: drop above or below it.
    Item,
}

/// The id of the request or folder being dragged.
#[derive(Clone)]
struct Dragged(String);

/// A tree row: indentation, optional chevron, badge, label, and a "⋯" menu on hover.
#[allow(clippy::too_many_arguments)]
fn row(
    ui: &mut Ui,
    depth: usize,
    chevron: Option<bool>,
    badge: Option<Badge>,
    label: &str,
    active: bool,
    id: &str,
    acts: &mut Vec<Action>,
    kind: MenuKind,
    dnd: Dnd,
) -> egui::Response {
    let p = pal(ui);
    let draggable = matches!(dnd, Dnd::Folder | Dnd::Item);
    let sense = if draggable { Sense::click_and_drag() } else { Sense::click() };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), sense);
    let hovered = ui.rect_contains_pointer(rect);
    if active {
        ui.painter().rect_filled(rect, CornerRadius::same(RADIUS_SM), p.selected);
    } else if hovered {
        ui.painter().rect_filled(rect, CornerRadius::same(RADIUS_SM), p.surface3);
    }
    let mut x = rect.left() + 6.0 + depth as f32 * 14.0;
    let cy = rect.center().y;
    let small = egui::FontId::proportional(11.0);
    if let Some(open) = chevron {
        chevron_shape(ui, egui::pos2(x + 5.0, cy), open, p.muted);
        x += 16.0;
    } else if depth > 0 {
        x += 16.0;
    }
    if let Some(b) = badge {
        let font = egui::FontId::monospace(10.5);
        ui.painter().text(egui::pos2(x, cy), egui::Align2::LEFT_CENTER, &b.text, font, b.color);
        x += 38.0;
    } else if chevron.is_some() && depth == 0 {
        ui.painter().text(egui::pos2(x, cy), egui::Align2::LEFT_CENTER, icons::STACK, small, p.muted);
        x += 18.0;
    }
    let show_menu = !matches!(kind, MenuKind::None) && (hovered || resp.context_menu_opened());
    let label_right = rect.right() - if show_menu { 28.0 } else { 6.0 };
    let label_rect = Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2(label_right.max(x + 10.0), rect.bottom()));
    let color = if active { p.accent } else { p.text2 };
    let text = RichText::new(label).color(color).size(13.5);
    let text = if active { text.strong() } else { text };
    // Child uis are laid out inside the row without growing it, so hovering
    // (which adds the menu button) never changes the row's height.
    let mut label_ui = ui.new_child(egui::UiBuilder::new().max_rect(label_rect).layout(Layout::left_to_right(Align::Center)));
    label_ui.add(egui::Label::new(text).truncate().selectable(false).sense(Sense::hover()));

    if !matches!(kind, MenuKind::None) {
        resp.context_menu(|ui| menu(ui, kind, id, acts));
        if show_menu {
            let br = Rect::from_center_size(egui::pos2(rect.right() - 14.0, cy), egui::vec2(24.0, 22.0));
            let mut button_ui = ui.new_child(egui::UiBuilder::new().max_rect(br).layout(Layout::centered_and_justified(egui::Direction::LeftToRight)));
            let b = button_ui.add(egui::Button::new(RichText::new(icons::DOTS_THREE).color(p.muted)).frame(false));
            egui::Popup::menu(&b).show(|ui| menu(ui, kind, id, acts));
        }
    }
    if draggable {
        resp.dnd_set_drag_payload(Dragged(id.to_string()));
        if resp.dragged() {
            drag_ghost(ui, label);
        }
    }
    drop_target(ui, &resp, rect, id, dnd, acts);
    resp
}

/// The dragged row's name, following the pointer.
fn drag_ghost(ui: &Ui, label: &str) {
    let p = pal(ui);
    let Some(pos) = ui.ctx().pointer_interact_pos() else { return };
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("sidebar-drag")));
    let galley = painter.layout_no_wrap(label.to_string(), egui::FontId::proportional(13.0), p.text);
    let r = Rect::from_min_size(pos + egui::vec2(14.0, 6.0), galley.size()).expand2(egui::vec2(8.0, 4.0));
    painter.rect(r, CornerRadius::same(RADIUS_SM), p.surface, Stroke::new(1.0, p.border_strong), egui::StrokeKind::Inside);
    painter.galley(r.min + egui::vec2(8.0, 4.0), galley, p.text);
}

/// Shows where a dragged item would land on this row, and queues the move on release.
fn drop_target(ui: &Ui, resp: &egui::Response, rect: Rect, id: &str, dnd: Dnd, acts: &mut Vec<Action>) {
    let Some(dragged) = resp.dnd_hover_payload::<Dragged>() else { return };
    if dragged.0 == id {
        return;
    }
    let Some(pointer) = ui.ctx().pointer_interact_pos() else { return };
    let t = (pointer.y - rect.top()) / rect.height();
    let to = match dnd {
        Dnd::None => return,
        Dnd::Into(container) => MoveTo::Into(container),
        Dnd::Folder if t < 0.25 => MoveTo::Before(id.to_string()),
        Dnd::Folder if t > 0.75 => MoveTo::After(id.to_string()),
        Dnd::Folder => MoveTo::Into(id.to_string()),
        Dnd::Item if t < 0.5 => MoveTo::Before(id.to_string()),
        Dnd::Item => MoveTo::After(id.to_string()),
    };
    let p = pal(ui);
    let line = Stroke::new(2.0, p.accent);
    match &to {
        MoveTo::Into(_) => {
            ui.painter().rect_stroke(rect.shrink(1.0), CornerRadius::same(RADIUS_SM), line, egui::StrokeKind::Inside);
        }
        MoveTo::Before(_) => {
            ui.painter().hline(rect.x_range(), rect.top(), line);
        }
        MoveTo::After(_) => {
            ui.painter().hline(rect.x_range(), rect.bottom(), line);
        }
    }
    if let Some(dragged) = resp.dnd_release_payload::<Dragged>() {
        acts.push(Action::MoveItem(dragged.0.clone(), to));
    }
}

fn menu(ui: &mut Ui, kind: MenuKind, id: &str, acts: &mut Vec<Action>) {
    ui.set_min_width(210.0);
    let mut item = |ui: &mut Ui, text: &str, a: Action| {
        if ui.button(text).clicked() {
            acts.push(a);
            ui.close();
        }
    };
    let id = id.to_string();
    match kind {
        MenuKind::Collection(linked) => {
            item(ui, &format!("{}  New request", icons::PLUS), Action::NewRequest(Some(id.clone())));
            item(ui, &format!("{}  New folder", icons::FOLDER), Action::NewFolder(id.clone()));
            ui.separator();
            item(ui, &format!("{}  Import into this collection\u{2026}", icons::UPLOAD), Action::ImportInto(Some(id.clone())));
            if linked {
                item(ui, &format!("{}  Folder & Git\u{2026}", icons::GIT_BRANCH), Action::CollectionFiles(id.clone()));
                item(ui, &format!("{}  Reload from folder", icons::ARROWS_CLOCKWISE), Action::ReloadFolder(id.clone()));
                item(ui, &format!("{}  Keep in the catalog instead", icons::FOLDER), Action::UnlinkFolder(id.clone()));
            } else {
                item(ui, &format!("{}  Link to a folder of files\u{2026}", icons::FOLDER_OPEN), Action::LinkFolder(id.clone()));
            }
            ui.separator();
            item(ui, &format!("{}  Variables & auth", icons::GEAR), Action::CollectionSettings(id.clone()));
            item(ui, &format!("{}  Rename", icons::PENCIL), Action::Rename(id.clone()));
            item(ui, &format!("{}  Duplicate collection", icons::COPY), Action::DuplicateCollection(id.clone()));
            item(ui, &format!("{}  Export (.json.gz)", icons::DOWNLOAD), Action::ExportCollectionGz(id.clone()));
            item(ui, &format!("{}  Export as Postman v2.1", icons::DOWNLOAD), Action::ExportCollectionPostman(id.clone()));
            ui.separator();
            danger(ui, &format!("{}  Delete collection", icons::TRASH), Action::AskDelete(id), acts);
        }
        MenuKind::Folder => {
            item(ui, &format!("{}  New request", icons::PLUS), Action::NewRequest(Some(id.clone())));
            item(ui, &format!("{}  New folder", icons::FOLDER), Action::NewFolder(id.clone()));
            item(ui, &format!("{}  Rename", icons::PENCIL), Action::Rename(id.clone()));
            item(ui, &format!("{}  Duplicate folder", icons::COPY), Action::DuplicateFolder(id.clone()));
            ui.separator();
            danger(ui, &format!("{}  Delete folder", icons::TRASH), Action::AskDelete(id), acts);
        }
        MenuKind::Request(protocol) => {
            item(ui, &format!("{}  Duplicate", icons::COPY), Action::Duplicate(id.clone()));
            item(ui, &format!("{}  Rename", icons::PENCIL), Action::Rename(id.clone()));
            ui.separator();
            item(ui, &format!("{}  Copy as curl", icons::COPY), Action::CopyCode(id.clone(), Target::Curl));
            item(ui, &format!("{}  Copy as PowerShell", icons::COPY), Action::CopyCode(id.clone(), Target::PowerShell));
            if protocol != Protocol::WebSocket {
                item(ui, &format!("{}  Add to a new workflow", icons::FLOW), Action::WorkflowFrom(id.clone()));
            }
            ui.separator();
            danger(ui, &format!("{}  Delete request", icons::TRASH), Action::AskDelete(id), acts);
        }
        MenuKind::Workflow => {
            item(ui, &format!("{}  Run", icons::PLAY), Action::RunWorkflow(id.clone()));
            item(ui, &format!("{}  Rename", icons::PENCIL), Action::Rename(id.clone()));
            item(ui, &format!("{}  Duplicate", icons::COPY), Action::DuplicateWorkflow(id.clone()));
            ui.separator();
            danger(ui, &format!("{}  Delete workflow", icons::TRASH), Action::AskDelete(id), acts);
        }
        MenuKind::None => {}
    }
}

fn danger(ui: &mut Ui, text: &str, a: Action, acts: &mut Vec<Action>) {
    let p = pal(ui);
    if ui.button(RichText::new(text).color(p.danger)).clicked() {
        acts.push(a);
        ui.close();
    }
}

/// A small filled triangle: pointing right when closed, down when open.
fn chevron_shape(ui: &Ui, c: egui::Pos2, open: bool, color: egui::Color32) {
    let s = 3.6;
    let pts = if open {
        vec![egui::pos2(c.x - s, c.y - s * 0.55), egui::pos2(c.x + s, c.y - s * 0.55), egui::pos2(c.x, c.y + s * 0.75)]
    } else {
        vec![egui::pos2(c.x - s * 0.55, c.y - s), egui::pos2(c.x + s * 0.75, c.y), egui::pos2(c.x - s * 0.55, c.y + s)]
    };
    ui.painter().add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// The brand mark: the app icon, rasterised once at the screen's scale.
fn logo(ui: &Ui, rect: Rect) {
    let id = egui::Id::unique("apicraft_brand_logo");
    let size = (rect.width() * ui.ctx().pixels_per_point()).ceil() as u32;
    let cached = ui.ctx().data(|d| d.get_temp::<(u32, egui::TextureHandle)>(id));
    let tex = match cached {
        Some((s, tex)) if s == size => tex,
        _ => {
            let image = egui::ColorImage::from_rgba_unmultiplied([size as usize; 2], &crate::brand::icon_rgba(size));
            let tex = ui.ctx().load_texture("brand-logo", image, egui::TextureOptions::LINEAR);
            ui.ctx().data_mut(|d| d.insert_temp(id, (size, tex.clone())));
            tex
        }
    };
    let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    ui.painter().image(tex.id(), rect, uv, egui::Color32::WHITE);
}
