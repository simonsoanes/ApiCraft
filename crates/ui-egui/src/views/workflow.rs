//! The workflow editor and its run results.

use apicraft_core::codegen::Target;
use apicraft_core::model::{ForEach, Protocol, WorkflowStep};
use egui::{Align, CornerRadius, Frame, Layout, Margin, RichText, Stroke, TextEdit, Ui};

use super::topbar;
use super::widgets::{self, accent_button, help, kv_table, pill};
use crate::actions::Action;
use crate::app::{ApiCraftApp, Modal};
use crate::icons;
use crate::theme::{method_color, method_label, pal, status_colors};

pub fn show(app: &mut ApiCraftApp, ui: &mut Ui, id: &str) {
    topbar(app, ui, |app, ui| {
        let p = pal(ui);
        ui.label(RichText::new("Workflow").size(11.5).color(p.muted));
        if let Some(w) = app.catalog.workflow_mut(id) {
            ui.add(
                TextEdit::singleline(&mut w.name)
                    .font(egui::FontId::proportional(17.0))
                    .frame(egui::Frame::NONE)
                    .desired_width(ui.available_width().min(520.0))
                    .margin(Margin::ZERO),
            );
        }
    });
    let p = pal(ui);
    let running = app.run.as_ref().map(|r| r.running && r.workflow_id == id).unwrap_or(false);
    Frame::new().inner_margin(Margin { left: 14, right: 14, top: 10, bottom: 10 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().button_padding = egui::vec2(14.0, 7.0);
            if running {
                if ui.button(format!("{} Stop", icons::STOP)).clicked() {
                    app.actions.push(Action::StopWorkflow);
                }
            } else if accent_button(ui, format!("{} Run workflow", icons::PLAY)).on_hover_text("Ctrl+Enter").clicked() {
                app.actions.push(Action::RunWorkflow(id.to_string()));
            }
            if let Some(w) = app.catalog.workflow_mut(id) {
                ui.label("Iterations");
                ui.add(egui::DragValue::new(&mut w.iterations).range(1..=1000));
                ui.checkbox(&mut w.stop_on_failure, "Stop at the first failure");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.menu_button(format!("{} Export script", icons::DOWNLOAD), |ui| {
                        ui.set_min_width(230.0);
                        ui.label(RichText::new("Runs the steps, captures and checks").size(11.0).color(p.faint));
                        let mut item = |ui: &mut egui::Ui, text: &str, a: Action| {
                            if ui.button(text).clicked() {
                                app.actions.push(a);
                                ui.close();
                            }
                        };
                        item(ui, &format!("{}  Save as Bash (.sh)\u{2026}", icons::FLOPPY_DISK), Action::SaveWorkflowScript(id.to_string(), Target::Curl));
                        item(
                            ui,
                            &format!("{}  Save as PowerShell (.ps1)\u{2026}", icons::FLOPPY_DISK),
                            Action::SaveWorkflowScript(id.to_string(), Target::PowerShell),
                        );
                        ui.separator();
                        item(ui, &format!("{}  Copy as Bash", icons::COPY), Action::CopyWorkflowScript(id.to_string(), Target::Curl));
                        item(ui, &format!("{}  Copy as PowerShell", icons::COPY), Action::CopyWorkflowScript(id.to_string(), Target::PowerShell));
                    });
                    ui.label(RichText::new(format!("{} steps", w.steps.len())).color(p.muted));
                });
            }
        });
    });
    ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0, p.border));

    let pane = Frame::new().fill(p.surface).inner_margin(Margin { left: 14, right: 14, top: 10, bottom: 10 });
    let wide = ui.available_width() > 1000.0;
    if wide {
        let w = ui.available_width();
        egui::Panel::right("wf-results-side")
            .resizable(true)
            .default_size((w * 0.45).clamp(320.0, (w - 360.0).max(320.0)))
            .size_range(320.0..=(w - 360.0).max(320.0))
            .frame(pane)
            .show(ui, |ui| widgets::fixed_size(ui, |ui| results(app, ui, id)));
    } else {
        let h = ui.available_height();
        egui::Panel::bottom("wf-results-below")
            .resizable(true)
            .default_size((h * 0.45).clamp(140.0, (h - 160.0).max(140.0)))
            .size_range(140.0..=(h - 160.0).max(140.0))
            .frame(pane)
            .show(ui, |ui| widgets::fixed_size(ui, |ui| results(app, ui, id)));
    }
    egui::CentralPanel::default().frame(pane).show(ui, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| steps(app, ui, id));
    });
}

/// What a step row shows about its request.
#[derive(Clone)]
struct StepInfo {
    protocol: Protocol,
    method: String,
    name: String,
    url: String,
    captures: usize,
    checks: usize,
}

fn steps(app: &mut ApiCraftApp, ui: &mut Ui, id: &str) {
    let p = pal(ui);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Steps").strong());
        ui.label(
            RichText::new(
                "run top to bottom; values captured by one step are available to the next as {{name}}. A step can repeat for each item of a captured list.",
            )
            .size(12.0)
            .color(p.muted),
        );
    });
    ui.add_space(6.0);

    // Info about each step's request, gathered before borrowing the workflow mutably.
    let info: Vec<Option<StepInfo>> = app
        .catalog
        .workflow(id)
        .map(|w| {
            w.steps
                .iter()
                .map(|s| {
                    app.catalog.find_request(&s.request_id).map(|(_, r)| StepInfo {
                        protocol: r.protocol,
                        method: r.method.clone(),
                        name: r.name.clone(),
                        url: r.url.clone(),
                        captures: r.extract.len(),
                        checks: r.checks.len(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let last_state: Vec<Option<bool>> = app
        .catalog
        .workflow(id)
        .map(|w| {
            w.steps
                .iter()
                .map(|s| {
                    app.run
                        .as_ref()
                        .filter(|r| r.workflow_id == id)
                        .and_then(|r| r.reports.iter().rev().find(|rep| rep.step_id == s.id))
                        .map(|rep| rep.passed() && rep.skipped.is_none())
                })
                .collect()
        })
        .unwrap_or_default();

    let ApiCraftApp { catalog, expanded, actions, add_step, toasts, now, .. } = app;
    let requests: Vec<(String, String, Protocol)> =
        catalog.all_requests().into_iter().filter(|(_, r)| r.protocol != Protocol::WebSocket).map(|(l, r)| (r.id.clone(), l, r.protocol)).collect();
    let Some(w) = catalog.workflow_mut(id) else { return };
    if w.steps.is_empty() {
        help(ui, "No steps yet. Add saved requests below, in the order they should run.");
    }
    let n = w.steps.len();
    // Which loop (by its first step) covers each step; loops don't nest.
    let mut covered: Vec<Option<usize>> = vec![None; n];
    let mut i = 0;
    while i < n {
        match w.steps[i].for_each.as_ref().filter(|_| w.steps[i].enabled) {
            Some(fe) => {
                let end = (i + fe.steps.max(1) as usize).min(n);
                covered[i..end].iter_mut().for_each(|c| *c = Some(i));
                i = end;
            }
            None => i += 1,
        }
    }
    let mut op: Option<(usize, i32)> = None; // (index, -1 up / +1 down / 0 delete)
    for (i, s) in w.steps.iter_mut().enumerate() {
        let open = expanded.contains(&s.id);
        let in_loop = covered[i].is_some();
        if let (Some(start), Some(fe)) = (covered[i].filter(|c| *c == i), s.for_each.as_ref()) {
            let end = (start + fe.steps.max(1) as usize).min(n);
            let item = if fe.item.trim().is_empty() { "item" } else { fe.item.trim() };
            let path = if fe.path.trim().is_empty() { String::new() } else { format!(" at {}", fe.path.trim()) };
            let range = if end - start == 1 { format!("step {}", start + 1) } else { format!("steps {}\u{2013}{end}", start + 1) };
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(match fe.list.trim().trim_matches(['{', '}']) {
                        "" => format!("{}  For each {{{{{item}}}}} in a list (choose it below)", icons::REPEAT),
                        list => format!("{}  For each {{{{{item}}}}} in {{{{{list}}}}}{path}", icons::REPEAT),
                    })
                    .strong()
                    .color(p.accent),
                );
                ui.label(RichText::new(format!("repeats {range}; {{{{{item}.field}}}} and {{{{$index}}}} work in them")).size(12.0).color(p.muted));
            });
        }
        let frame = Frame::new()
            .stroke(Stroke::new(1.0, if in_loop { p.accent.gamma_multiply(0.55) } else { p.border }))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(10, 6))
            .outer_margin(Margin { left: if in_loop { 18 } else { 0 }, ..Margin::ZERO })
            .fill(p.surface);
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (fill, color, mark) = match last_state.get(i).copied().flatten() {
                    Some(true) => (p.ok_bg, p.ok_text, icons::CHECK.to_string()),
                    Some(false) => (p.err_bg, p.err_text, icons::X.to_string()),
                    None => (p.surface3, p.muted, (i + 1).to_string()),
                };
                pill(ui, mark, fill, color);
                ui.checkbox(&mut s.enabled, "").on_hover_text("Enabled");
                match info.get(i).cloned().flatten() {
                    Some(StepInfo { protocol: proto, method, name, url, captures: caps, checks }) => {
                        ui.label(RichText::new(method_label(proto, &method)).monospace().strong().size(11.0).color(method_color(p, proto, &method)));
                        if ui.link(RichText::new(&name).strong()).clicked() {
                            actions.push(Action::Open(s.request_id.clone()));
                        }
                        // Leave room for the controls on the right, or the URL runs under them.
                        let room = (ui.available_width() - 240.0).max(40.0);
                        ui.scope(|ui| {
                            ui.set_max_width(room);
                            ui.add(egui::Label::new(RichText::new(url).monospace().size(11.5).color(p.muted)).truncate());
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if widgets::icon_button(ui, icons::TRASH, "Remove step").clicked() {
                                op = Some((i, 0));
                            }
                            if ui.add_enabled(i + 1 < n, egui::Button::new(icons::ARROW_DOWN).frame(false)).on_hover_text("Move down").clicked() {
                                op = Some((i, 1));
                            }
                            if ui.add_enabled(i > 0, egui::Button::new(icons::ARROW_UP).frame(false)).on_hover_text("Move up").clicked() {
                                op = Some((i, -1));
                            }
                            if widgets::icon_button(ui, if open { icons::CARET_DOWN } else { icons::CARET_RIGHT }, "Loop, captures and checks for this step")
                                .clicked()
                            {
                                if open {
                                    expanded.remove(&s.id);
                                } else {
                                    expanded.insert(s.id.clone());
                                }
                            }
                            ui.label(RichText::new("ms").size(12.0).color(p.muted));
                            ui.add(egui::DragValue::new(&mut s.delay_ms).range(0..=600_000).speed(50));
                            ui.label(RichText::new("wait").size(12.0).color(p.muted));
                            ui.label(RichText::new(format!("{} {}", icons::CHECK, checks + s.checks.len())).size(12.0).color(p.muted)).on_hover_text("checks");
                            ui.label(RichText::new(format!("{} {}", icons::VARIABLE, caps + s.extract.len())).size(12.0).color(p.muted))
                                .on_hover_text("captures");
                        });
                    }
                    None => {
                        ui.label(RichText::new("Deleted request").color(p.danger));
                        if widgets::icon_button(ui, icons::TRASH, "Remove step").clicked() {
                            op = Some((i, 0));
                        }
                    }
                }
            });
            if open {
                ui.separator();
                loop_editor(ui, s, n - i);
                ui.add_space(6.0);
                help(ui, "Extra captures and checks for this step only, on top of the request's own.");
                widgets::capture_table(ui, &format!("step-cap-{}", s.id), &mut s.extract, None, apicraft_core::model::ExtractSource::Json);
                ui.add_space(8.0);
                widgets::checks_table(ui, &format!("step-chk-{}", s.id), &mut s.checks);
            }
        });
        ui.add_space(4.0);
    }
    if let Some((i, d)) = op {
        match d {
            0 => {
                w.steps.remove(i);
                toasts.push(crate::app::Toast {
                    text: format!("Removed step {}. Press Ctrl+Z (or Undo at the top) to bring it back.", i + 1),
                    error: false,
                    until: *now + 5.0,
                });
            }
            -1 => w.steps.swap(i, i - 1),
            _ => w.steps.swap(i, i + 1),
        }
    }

    // Add a step
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let label = requests.iter().find(|(rid, _, _)| rid == add_step).map(|(_, l, _)| l.clone()).unwrap_or_else(|| "Choose a request to add\u{2026}".into());
        egui::ComboBox::from_id_salt("add-step").selected_text(label).width((ui.available_width() - 120.0).max(200.0)).show_ui(ui, |ui| {
            for (rid, l, _) in &requests {
                ui.selectable_value(add_step, rid.clone(), l);
            }
        });
        if ui.add_enabled(!add_step.is_empty(), egui::Button::new(format!("{} Add step", icons::PLUS))).clicked() {
            w.steps.push(WorkflowStep::new(std::mem::take(add_step)));
        }
    });

    ui.add_space(14.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Workflow variables").strong());
        ui.label(RichText::new("starting values; environment and captured values override them").size(12.0).color(p.muted));
    });
    kv_table(ui, "wfvars", &mut w.variables, "name", "value");
}

/// "Repeat this step for each item of a list".
fn loop_editor(ui: &mut Ui, s: &mut WorkflowStep, remaining: usize) {
    let p = pal(ui);
    let mut on = s.for_each.is_some();
    if ui.checkbox(&mut on, RichText::new("Repeat for each item of a list").strong()).changed() {
        s.for_each = on.then(ForEach::default);
    }
    let Some(fe) = s.for_each.as_mut() else { return };
    egui::Grid::new(("loop", &s.id)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        ui.label("List");
        ui.horizontal(|ui| {
            ui.add(
                TextEdit::singleline(&mut fe.list).hint_text("people   (a variable captured earlier)").desired_width(200.0).font(egui::TextStyle::Monospace),
            );
            ui.label(RichText::new("at").color(p.muted));
            ui.add(
                TextEdit::singleline(&mut fe.path).hint_text("$.value   or   $.items[*].id   (optional)").desired_width(230.0).font(egui::TextStyle::Monospace),
            );
        });
        ui.end_row();
        ui.label("Each item as");
        ui.horizontal(|ui| {
            ui.add(TextEdit::singleline(&mut fe.item).hint_text("item").desired_width(120.0).font(egui::TextStyle::Monospace));
            ui.label(RichText::new("Steps to repeat").color(p.muted));
            ui.add(egui::DragValue::new(&mut fe.steps).range(1..=remaining.max(1) as u32));
            ui.label(RichText::new("(this one and the ones after it)").size(12.0).color(p.muted));
        });
        ui.end_row();
    });
    let item = if fe.item.trim().is_empty() { "item" } else { fe.item.trim() };
    help(
        ui,
        &format!(
            "The list is a JSON array (or lines or comma-separated text). Use {{{{{item}}}}} for the item, {{{{{item}.id}}}} for its fields and {{{{$index}}}} for its position (from 0)."
        ),
    );
}

fn results(app: &mut ApiCraftApp, ui: &mut Ui, id: &str) {
    let p = pal(ui);
    let Some(run) = app.run.as_ref().filter(|r| r.workflow_id == id) else {
        ui.add_space(ui.available_height() * 0.3);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(icons::FLOW).size(30.0).color(p.faint));
            ui.label(RichText::new("Not run yet").strong());
            ui.label(RichText::new("Run the workflow to see each step's result, captured values and checks.").color(p.muted));
            ui.label(RichText::new("Ctrl + Enter").size(12.0).color(p.faint));
        });
        return;
    };
    ui.horizontal_wrapped(|ui| {
        if run.running {
            ui.spinner();
            pill(ui, "Running\u{2026}", p.accent_soft, p.accent);
        }
        if let Some(s) = &run.summary {
            if s.failed == 0 {
                pill(ui, "Passed", p.ok_bg, p.ok_text);
            } else {
                pill(ui, if s.stopped_early { "Stopped" } else { "Failed" }, p.err_bg, p.err_text);
            }
            ui.label(RichText::new(format!("{} passed", s.passed)).color(p.ok_text).size(12.5));
            ui.label(RichText::new(format!("{} failed", s.failed)).color(if s.failed > 0 { p.err_text } else { p.muted }).size(12.5));
            ui.label(RichText::new(format!("{} skipped", s.skipped)).color(p.muted).size(12.5));
            ui.label(RichText::new(format!("{:.0} ms", s.elapsed_ms)).color(p.muted).size(12.5));
        }
    });
    if let Some(e) = &run.error {
        widgets::alert(ui, widgets::AlertKind::Error, e);
    }
    ui.add_space(6.0);
    let multi = app.catalog.workflow(id).map(|w| w.iterations > 1).unwrap_or(false);
    let mut open_step = None;
    egui::ScrollArea::vertical().id_salt("wf-results").auto_shrink([false, false]).stick_to_bottom(run.running).show(ui, |ui| {
        for (i, rep) in run.reports.iter().enumerate() {
            let (fill, color, mark) = match (&rep.outcome, &rep.skipped) {
                (_, Some(_)) => (p.surface3, p.faint, "\u{2193}"),
                (Some(o), _) if o.passed() => (p.ok_bg, p.ok_text, icons::CHECK),
                _ => (p.err_bg, p.err_text, icons::X),
            };
            let border = if rep.passed() { p.border } else { p.danger.gamma_multiply(0.5) };
            let resp =
                Frame::new().stroke(Stroke::new(1.0, border)).corner_radius(CornerRadius::same(6)).inner_margin(Margin::symmetric(8, 6)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        pill(ui, mark, fill, color);
                        let name =
                            if multi { format!("#{} {}. {}", rep.iteration, rep.index + 1, rep.name) } else { format!("{}. {}", rep.index + 1, rep.name) };
                        ui.label(RichText::new(name).strong());
                        if let Some(it) = &rep.item {
                            widgets::chip(ui, format!("{} {}/{}  {}", icons::REPEAT, it.index + 1, it.count, it.label));
                        }
                        match (&rep.outcome, &rep.skipped) {
                            (Some(o), _) => {
                                if let Some(r) = &o.response {
                                    let (f, c) = status_colors(p, r.status);
                                    pill(ui, r.status.to_string(), f, c);
                                    ui.label(RichText::new(format!("{:.0} ms", r.elapsed_ms)).color(p.muted).size(12.0));
                                }
                                for (_, k, v) in &o.extracted {
                                    widgets::chip(ui, format!("{{{{{k}}}}} = {}", v.chars().take(32).collect::<String>()));
                                }
                                for c in o.checks.iter().filter(|c| !c.passed) {
                                    pill(ui, format!("{} {}", c.label, c.detail), p.err_bg, p.err_text);
                                }
                                if let Some(e) = &o.error {
                                    pill(ui, e.chars().take(140).collect::<String>(), p.err_bg, p.err_text);
                                }
                                for w in &o.warnings {
                                    pill(ui, w.as_str(), p.warn_bg, p.warn_text);
                                }
                                for e in &o.extract_errors {
                                    pill(ui, e.as_str(), p.warn_bg, p.warn_text);
                                }
                            }
                            (None, Some(why)) => {
                                ui.label(RichText::new(format!("skipped ({why})")).color(p.muted));
                            }
                            _ => {}
                        }
                    });
                });
            if rep.outcome.is_some() && resp.response.interact(egui::Sense::click()).on_hover_text("Show details").clicked() {
                open_step = Some(i);
            }
            ui.add_space(4.0);
        }
    });
    if let Some(i) = open_step {
        app.modal = Some(Modal::StepResult { index: i });
    }
}
