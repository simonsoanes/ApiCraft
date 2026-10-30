//! Reusable pieces: buttons, pills, tabs, key/value tables, the auth editor,
//! capture and check tables, and code editors.

use apicraft_core::model::*;
use egui::{Color32, CornerRadius, FontId, Margin, RichText, Stroke, TextEdit, Ui, Vec2};

use crate::actions::Action;
use crate::highlight::{Highlighter, Lang};
use crate::icons;
use crate::theme::{pal, RADIUS_SM};

pub fn accent_button(ui: &mut Ui, text: impl Into<String>) -> egui::Response {
    let p = pal(ui);
    ui.add(
        egui::Button::new(RichText::new(text.into()).color(p.accent_text).strong())
            .fill(p.accent)
            .stroke(Stroke::new(1.0, p.accent))
            .corner_radius(CornerRadius::same(RADIUS_SM)),
    )
}

pub fn danger_button(ui: &mut Ui, text: impl Into<String>) -> egui::Response {
    let p = pal(ui);
    ui.add(egui::Button::new(RichText::new(text.into()).color(p.danger)))
}

/// A small icon-only button with a tooltip.
/// A standard button with a count after its label, highlighted when non-zero.
pub fn count_button(ui: &mut Ui, icon: &str, label: &str, count: usize) -> egui::Response {
    let p = pal(ui);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    let fmt = |color| egui::text::TextFormat { font_id: font.clone(), color, ..Default::default() };
    job.append(&format!("{icon}  {label}"), 0.0, fmt(p.text));
    job.append(&count.to_string(), 8.0, fmt(if count > 0 { p.accent } else { p.faint }));
    ui.add(egui::Button::new(job))
}

/// A standard button led by a small status dot.
pub fn status_button(ui: &mut Ui, dot: Color32, label: &str) -> egui::Response {
    let p = pal(ui);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    // Room for the dot, painted over the button below.
    job.append(label, 14.0, egui::text::TextFormat { font_id: font, color: p.text, ..Default::default() });
    let resp = ui.add(egui::Button::new(job));
    let pad = ui.spacing().button_padding.x;
    ui.painter().circle_filled(egui::pos2(resp.rect.left() + pad + 4.0, resp.rect.center().y), 4.0, dot);
    resp
}

pub fn icon_button(ui: &mut Ui, icon: &str, tip: &str) -> egui::Response {
    let p = pal(ui);
    ui.add(egui::Button::new(RichText::new(icon).color(p.muted).size(16.0)).frame(false).min_size(Vec2::new(24.0, 24.0))).on_hover_text(tip)
}

pub fn pill(ui: &mut Ui, text: impl Into<String>, fill: Color32, color: Color32) -> egui::Response {
    egui::Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::symmetric(9, 3))
        .show(ui, |ui| ui.label(RichText::new(text.into()).color(color).strong().size(12.5)))
        .response
}

pub fn chip(ui: &mut Ui, text: impl Into<String>) -> egui::Response {
    let p = pal(ui);
    pill(ui, text, p.surface3, p.muted)
}

pub fn section_label(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    ui.label(RichText::new(text.to_uppercase()).size(11.0).strong().color(p.faint));
}

pub fn help(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    ui.label(RichText::new(text).size(12.0).color(p.muted));
}

pub fn alert(ui: &mut Ui, kind: AlertKind, text: &str) {
    let p = pal(ui);
    let (fill, color, icon) = match kind {
        AlertKind::Error => (p.err_bg, p.err_text, icons::WARNING),
        AlertKind::Warn => (p.warn_bg, p.warn_text, icons::WARNING),
        AlertKind::Ok => (p.ok_bg, p.ok_text, icons::CHECK),
    };
    egui::Frame::new().fill(fill).corner_radius(CornerRadius::same(RADIUS_SM)).inner_margin(Margin::symmetric(10, 7)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(icon).color(color));
            ui.label(RichText::new(text).color(color));
        });
    });
}

#[derive(Clone, Copy)]
pub enum AlertKind {
    Error,
    Warn,
    Ok,
}

/// An underlined tab strip. Returns true when the selection changed.
pub fn tabs<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, items: &[(T, String, usize)]) -> bool {
    let p = pal(ui);
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        ui.spacing_mut().item_spacing.y = 4.0;
        for (value, label, count) in items {
            let on = *current == *value;
            let label_text = if *count > 0 { format!("{label}  {count}") } else { label.clone() };
            let text = RichText::new(label_text).color(if on { p.accent } else { p.muted });
            let text = if on { text.strong() } else { text };
            let resp = ui.add(egui::Button::new(text).frame(false).min_size(Vec2::new(0.0, 30.0)));
            if on {
                let r = resp.rect;
                ui.painter().line_segment([egui::pos2(r.left(), r.bottom() + 2.0), egui::pos2(r.right(), r.bottom() + 2.0)], Stroke::new(2.0, p.accent));
            }
            if resp.clicked() && !on {
                *current = *value;
                changed = true;
            }
        }
    });
    let rect = ui.min_rect();
    // min_rect covers every row the strip wrapped onto, so the rule sits under
    // the whole of it rather than under the first row only.
    ui.painter()
        .line_segment([egui::pos2(rect.left(), rect.bottom() + 3.0), egui::pos2(ui.max_rect().right(), rect.bottom() + 3.0)], Stroke::new(1.0, p.border));
    ui.add_space(8.0);
    changed
}

/// A segmented control.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, items: &[(T, &str)]) -> bool {
    let p = pal(ui);
    let mut changed = false;
    egui::Frame::new().fill(p.surface3).corner_radius(CornerRadius::same(RADIUS_SM)).inner_margin(Margin::same(3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (value, label) in items {
                let on = *current == *value;
                let btn = if on {
                    egui::Button::new(RichText::new(*label).color(p.text).strong()).fill(p.surface).stroke(Stroke::NONE)
                } else {
                    egui::Button::new(RichText::new(*label).color(p.muted)).fill(egui::Color32::TRANSPARENT).stroke(Stroke::NONE)
                };
                if ui.add(btn.min_size(Vec2::new(0.0, 24.0))).clicked() && !on {
                    *current = *value;
                    changed = true;
                }
            }
        });
    });
    changed
}

/// Key/value rows; typing in the trailing blank row adds an entry.
pub fn kv_table(ui: &mut Ui, id: &str, items: &mut Vec<KeyValue>, key_ph: &str, value_ph: &str) {
    let mut remove = None;
    // After the trailing row grew the list, keep typing in the new row's field.
    let refocus_key = egui::Id::unique((id, "refocus"));
    let refocus: Option<(usize, bool)> = ui.ctx().memory_mut(|m| m.data.remove_temp(refocus_key));
    let field_w = |ui: &Ui| ((ui.available_width() - 34.0) / 2.0 - ui.spacing().item_spacing.x).max(80.0);
    for (i, kv) in items.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add_sized([22.0, 24.0], egui::Checkbox::without_text(&mut kv.enabled)).on_hover_text("Enabled");
            let w = field_w(ui);
            let color = if kv.enabled { None } else { Some(pal(ui).faint) };
            let mut k = TextEdit::singleline(&mut kv.key).id_salt((id, i, "k")).hint_text(key_ph).desired_width(w).font(egui::TextStyle::Monospace);
            let mut v = TextEdit::singleline(&mut kv.value).id_salt((id, i, "v")).hint_text(value_ph).desired_width(w).font(egui::TextStyle::Monospace);
            if let Some(c) = color {
                k = k.text_color(c);
                v = v.text_color(c);
            }
            let rk = ui.add(k);
            let rv = ui.add(v);
            if let Some((row, value_field)) = refocus {
                if row == i {
                    let len = if value_field { kv.value.chars().count() } else { kv.key.chars().count() };
                    focus_at_end(ui, if value_field { &rv } else { &rk }, len);
                }
            }
            if icon_button(ui, icons::X, "Remove").clicked() {
                remove = Some(i);
            }
        });
    }
    ui.horizontal(|ui| {
        ui.add_space(22.0);
        let w = field_w(ui);
        let mut nk = String::new();
        let mut nv = String::new();
        ui.add(TextEdit::singleline(&mut nk).id_salt((id, "new-k")).hint_text(key_ph).desired_width(w).font(egui::TextStyle::Monospace));
        let rv = ui.add(TextEdit::singleline(&mut nv).id_salt((id, "new-v")).hint_text(value_ph).desired_width(w).font(egui::TextStyle::Monospace));
        if !nk.is_empty() || !nv.is_empty() {
            let value_field = rv.changed();
            items.push(KeyValue::new(nk, nv));
            let row = items.len() - 1;
            ui.ctx().memory_mut(|m| m.data.insert_temp(refocus_key, (row, value_field)));
            ui.ctx().request_repaint();
        }
    });
    if let Some(i) = remove {
        items.remove(i);
    }
}

/// Focuses a text field with the cursor after its last character.
fn focus_at_end(ui: &Ui, resp: &egui::Response, len: usize) {
    resp.request_focus();
    if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), resp.id) {
        let end = egui::text::CCursor::new(len);
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
        state.store(ui.ctx(), resp.id);
    }
}

fn field(ui: &mut Ui, label: &str, value: &mut String, password: bool, hint: &str) {
    ui.label(RichText::new(label).strong().size(12.5));
    ui.add(TextEdit::singleline(value).password(password).hint_text(hint).desired_width(f32::INFINITY));
    ui.add_space(4.0);
}

fn field_multiline(ui: &mut Ui, label: &str, value: &mut String, rows: usize) {
    ui.label(RichText::new(label).strong().size(12.5));
    ui.add(TextEdit::multiline(value).code_editor().desired_rows(rows).desired_width(f32::INFINITY));
    ui.add_space(4.0);
}

fn two<R>(ui: &mut Ui, left: impl FnOnce(&mut Ui) -> R, right: impl FnOnce(&mut Ui) -> R) {
    ui.columns(2, |cols| {
        left(&mut cols[0]);
        right(&mut cols[1]);
    });
}

fn expiry(expires_at: u64, now: u64) -> String {
    if expires_at == 0 {
        return "no expiry given".into();
    }
    if expires_at <= now {
        return "expired".into();
    }
    let secs = (expires_at - now) / 1000;
    if secs > 3600 {
        format!("expires in {}h {}m", secs / 3600, secs % 3600 / 60)
    } else {
        format!("expires in {}m {}s", secs / 60, secs % 60)
    }
}

/// The auth editor. `inherited` describes the collection's auth when the
/// request inherits it; `owner` is the request or collection id.
pub struct AuthCtx<'a> {
    pub owner: &'a str,
    pub allow_inherit: bool,
    pub inherited: Option<(&'a str, String)>,
    pub native: bool,
    pub default_redirect: String,
    pub now: u64,
}

pub fn auth_editor(ui: &mut Ui, auth: &mut Auth, cx: &AuthCtx, actions: &mut Vec<Action>, open_collection: &mut Option<String>) {
    let p = pal(ui);
    let kinds: Vec<(&str, &str)> = Auth::KINDS.iter().filter(|(k, _)| cx.allow_inherit || *k != "inherit").map(|(k, l)| (*k, *l)).collect();
    let current = auth.key();
    let current_label = kinds.iter().find(|(k, _)| *k == current).map(|(_, l)| *l).unwrap_or("None");
    let mut chosen = current.to_string();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Type").strong());
        egui::ComboBox::from_id_salt(("auth-kind", cx.owner)).selected_text(current_label).width(220.0).show_ui(ui, |ui| {
            for (k, l) in &kinds {
                ui.selectable_value(&mut chosen, k.to_string(), *l);
            }
        });
    });
    if chosen != current {
        *auth = Auth::of_kind(&chosen, auth);
    }
    ui.add_space(8.0);
    ui.set_max_width(ui.available_width().min(760.0));
    match auth {
        Auth::Inherit => {
            ui.horizontal_wrapped(|ui| {
                match &cx.inherited {
                    Some((_, label)) => ui.label(RichText::new(format!("Uses the collection's auth: {label}.")).color(p.muted)),
                    None => ui.label(RichText::new("Uses the collection's auth.").color(p.muted)),
                };
                if let Some((col, _)) = &cx.inherited {
                    if ui.link("Edit collection auth").clicked() {
                        *open_collection = Some(col.to_string());
                    }
                }
            });
        }
        Auth::None => {
            ui.label(RichText::new("No authorization is sent.").color(p.muted));
        }
        Auth::Basic { username, password } => {
            field(ui, "Username", username, false, "");
            field(ui, "Password", password, true, "");
            help(ui, "Sent as Authorization: Basic \u{2026}. {{variables}} work in every field.");
        }
        Auth::Digest { username, password } => {
            field(ui, "Username", username, false, "");
            field(ui, "Password", password, true, "");
            help(
                ui,
                if cx.native {
                    "Answers the server's 401 challenge (MD5, SHA-256, -sess, qop auth/auth-int)."
                } else {
                    "Answers the server's 401 challenge. Browsers only expose the challenge when the server allows it via CORS; use the local agent if this fails."
                },
            );
        }
        Auth::Bearer { token } => {
            field(ui, "Token", token, false, "{{token}}");
            help(ui, "Sent as Authorization: Bearer \u{2026}. Use {{token}} to take it from a captured value.");
        }
        Auth::ApiKey { key, value, in_header } => {
            field(ui, "Key", key, false, "X-API-Key");
            field(ui, "Value", value, false, "");
            ui.horizontal(|ui| {
                ui.label(RichText::new("Add to").strong());
                ui.radio_value(in_header, true, "Header");
                ui.radio_value(in_header, false, "Query params");
            });
        }
        Auth::OAuth2(o) => {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Grant type").strong());
                egui::ComboBox::from_id_salt(("grant", cx.owner))
                    .selected_text(GrantType::ALL.iter().find(|g| g.0 == o.grant).map(|g| g.2).unwrap_or(""))
                    .width(240.0)
                    .show_ui(ui, |ui| {
                        for (g, _, label) in GrantType::ALL {
                            ui.selectable_value(&mut o.grant, g, label);
                        }
                    });
            });
            ui.add_space(6.0);
            if o.grant == GrantType::AuthorizationCode {
                field(ui, "Authorization URL", &mut o.auth_url, false, "https://login.example.com/authorize");
            }
            if o.grant != GrantType::Manual {
                field(ui, "Access token URL", &mut o.token_url, false, "https://login.example.com/token");
                two(ui, |ui| field(ui, "Client ID", &mut o.client_id, false, ""), |ui| field(ui, "Client secret", &mut o.client_secret, true, ""));
            } else {
                field(ui, "Token URL (for refresh)", &mut o.token_url, false, "optional");
            }
            if o.grant == GrantType::Password {
                two(ui, |ui| field(ui, "Username", &mut o.username, false, ""), |ui| field(ui, "Password", &mut o.password, true, ""));
            }
            if o.grant != GrantType::Manual {
                two(ui, |ui| field(ui, "Scope", &mut o.scope, false, ""), |ui| field(ui, "Audience", &mut o.audience, false, ""));
            }
            if o.grant == GrantType::AuthorizationCode {
                field(ui, "Redirect URI", &mut o.redirect_uri, false, &cx.default_redirect);
                help(ui, "Register this with the provider. Leave blank to use the default shown.");
                ui.checkbox(&mut o.use_pkce, "Use PKCE (S256)");
            }
            if o.grant != GrantType::Manual {
                ui.checkbox(&mut o.client_auth_basic, "Send client credentials as a Basic auth header (otherwise in the body)");
            }
            field(ui, "Header prefix", &mut o.header_prefix, false, "Bearer");
            egui::Frame::new().fill(p.surface2).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(12)).show(
                ui,
                |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{} Token", icons::KEY)).strong());
                        ui.label(
                            RichText::new(if o.access_token.is_empty() { "none yet".to_string() } else { expiry(o.expires_at, cx.now) })
                                .color(p.muted)
                                .size(12.5),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if !o.access_token.is_empty() && ui.button("Clear").clicked() {
                                o.access_token.clear();
                                o.refresh_token.clear();
                                o.expires_at = 0;
                            }
                            if o.grant != GrantType::Manual && accent_button(ui, "Get new token").clicked() {
                                actions.push(Action::OAuthGet(cx.owner.to_string()));
                            }
                        });
                    });
                    ui.add_space(4.0);
                    field_multiline(ui, "Access token", &mut o.access_token, 3);
                    field(ui, "Refresh token", &mut o.refresh_token, false, "");
                    help(ui, "Fetched automatically before sending when the grant allows it (client credentials, password, refresh token).");
                },
            );
        }
        Auth::OAuth1(o) => {
            two(ui, |ui| field(ui, "Consumer key", &mut o.consumer_key, false, ""), |ui| field(ui, "Consumer secret", &mut o.consumer_secret, true, ""));
            two(ui, |ui| field(ui, "Access token", &mut o.token, false, ""), |ui| field(ui, "Token secret", &mut o.token_secret, true, ""));
            ui.horizontal(|ui| {
                ui.label(RichText::new("Signature method").strong());
                egui::ComboBox::from_id_salt(("sig", cx.owner)).selected_text(o.signature_method.name()).show_ui(ui, |ui| {
                    for m in [OAuth1Method::HmacSha1, OAuth1Method::HmacSha256, OAuth1Method::Plaintext] {
                        ui.selectable_value(&mut o.signature_method, m, m.name());
                    }
                });
            });
            field(ui, "Realm", &mut o.realm, false, "optional");
            help(ui, "Signed per request (RFC 5849) with a fresh nonce and timestamp; form bodies are included in the signature.");
        }
        Auth::AwsSigV4(a) => {
            two(ui, |ui| field(ui, "Access key", &mut a.access_key, false, ""), |ui| field(ui, "Secret key", &mut a.secret_key, true, ""));
            two(ui, |ui| field(ui, "Region", &mut a.region, false, "us-east-1"), |ui| field(ui, "Service", &mut a.service, false, "execute-api"));
            field(ui, "Session token", &mut a.session_token, true, "only for temporary credentials");
        }
        Auth::Jwt(j) => {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Algorithm").strong());
                egui::ComboBox::from_id_salt(("jwt", cx.owner)).selected_text(j.algorithm.clone()).show_ui(ui, |ui| {
                    for a in ["HS256", "HS384", "HS512"] {
                        ui.selectable_value(&mut j.algorithm, a.to_string(), a);
                    }
                });
                ui.label(RichText::new("Expires in (s)").strong());
                ui.add(egui::DragValue::new(&mut j.expires_in_secs).range(0..=31_536_000));
            });
            field(ui, "Secret", &mut j.secret, true, "");
            field_multiline(ui, "Payload (JSON claims)", &mut j.payload, 4);
            field(ui, "Header prefix", &mut j.header_prefix, false, "Bearer");
            help(ui, "iat/exp are added when missing and an expiry is set.");
        }
        Auth::ClientCert(c) => {
            field_multiline(ui, "Certificate (PEM text, or the path of a .pem/.crt file)", &mut c.certificate, 3);
            field_multiline(ui, "Private key (PEM text, or the path of a .key/.pem file)", &mut c.key, 3);
            help(ui, "Sent during the TLS handshake (mutual TLS). The key must be unencrypted (PKCS#8, PKCS#1 or SEC1). Paths are read when the request is sent, so the key needn't live in the catalog.");
            if !cx.native {
                alert(ui, AlertKind::Warn, "Browsers can't present client certificates; these requests need the desktop app or the local agent.");
            }
        }
    }
}

/// Capture rules: "take X from the response, store as {{var}}".
pub fn capture_table(ui: &mut Ui, id: &str, rules: &mut Vec<Extraction>, last: Option<&[(VarScope, String, String)]>, default_source: ExtractSource) {
    let p = pal(ui);
    if rules.is_empty() {
        ui.label(RichText::new("Capture values from the response into {{variables}} that later requests and workflow steps can use.").color(p.muted));
        ui.add_space(6.0);
    } else {
        let mut remove = None;
        let fixed = 24.0 + 130.0 + 110.0 + 108.0 + 120.0 + 28.0 + 6.0 * 8.0;
        let expr_w = (ui.available_width() - fixed).max(120.0);
        ui.horizontal(|ui| {
            for (h, w) in [("", 24.0), ("Variable", 130.0), ("From", 110.0), ("Expression", expr_w), ("Store in", 108.0), ("Last value", 120.0)] {
                ui.add_sized([w, 16.0], egui::Label::new(RichText::new(h).size(11.0).strong().color(p.faint)));
            }
        });
        for (i, r) in rules.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add_sized([24.0, 24.0], egui::Checkbox::without_text(&mut r.enabled));
                ui.add(TextEdit::singleline(&mut r.variable).hint_text("token").desired_width(130.0).font(egui::TextStyle::Monospace));
                egui::ComboBox::from_id_salt((id, i, "src"))
                    .selected_text(ExtractSource::ALL.iter().find(|s| s.0 == r.source).map(|s| s.2).unwrap_or(""))
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        for (s, _, label) in ExtractSource::ALL {
                            ui.selectable_value(&mut r.source, s, label);
                        }
                    });
                let hint = match r.source {
                    ExtractSource::Json => "$.data.token",
                    ExtractSource::Xml => "Envelope/Body/Result",
                    ExtractSource::Header => "Location",
                    ExtractSource::Status => "(status code)",
                    ExtractSource::Regex => "id=(\\d+)",
                };
                ui.add_enabled(
                    r.source != ExtractSource::Status,
                    TextEdit::singleline(&mut r.expression).hint_text(hint).desired_width(expr_w).font(egui::TextStyle::Monospace),
                );
                egui::ComboBox::from_id_salt((id, i, "scope"))
                    .selected_text(if r.scope == VarScope::Environment { "Environment" } else { "Runtime" })
                    .width(108.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut r.scope, VarScope::Runtime, "Runtime");
                        ui.selectable_value(&mut r.scope, VarScope::Environment, "Environment");
                    });
                let last_val = last.and_then(|l| l.iter().find(|(_, k, _)| *k == r.variable.trim()).map(|(_, _, v)| v.clone()));
                let label = match &last_val {
                    Some(v) => RichText::new(v.chars().take(18).collect::<String>()).monospace().color(p.ok_text),
                    None => RichText::new("\u{2014}").color(p.faint),
                };
                let resp = ui.add_sized([120.0, 24.0], egui::Label::new(label).truncate());
                if let Some(v) = last_val {
                    resp.on_hover_text(v);
                }
                if icon_button(ui, icons::X, "Remove").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            rules.remove(i);
        }
    }
    if ui.button(format!("{} Add capture", icons::PLUS)).clicked() {
        rules.push(Extraction { variable: String::new(), source: default_source, expression: String::new(), scope: VarScope::Runtime, enabled: true });
    }
    ui.add_space(6.0);
    help(ui, "JSON path: $.items[0].id, data.user.name, items.length. XML path: element names (prefixes ignored), @attr for attributes. Runtime values override the environment.");
}

pub fn checks_table(ui: &mut Ui, id: &str, checks: &mut Vec<Check>) {
    let p = pal(ui);
    if checks.is_empty() {
        ui.label(RichText::new("Checks mark a response as passed or failed. In a workflow, a failed check can stop the run.").color(p.muted));
        ui.add_space(6.0);
    } else {
        let mut remove = None;
        let w = ((ui.available_width() - (24.0 + 140.0 + 28.0 + 6.0 * 5.0)) / 2.0).max(100.0);
        ui.horizontal(|ui| {
            for (h, width) in [("", 24.0), ("Check", 140.0), ("Target", w), ("Expected", w)] {
                ui.add_sized([width, 16.0], egui::Label::new(RichText::new(h).size(11.0).strong().color(p.faint)));
            }
        });
        for (i, c) in checks.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add_sized([24.0, 24.0], egui::Checkbox::without_text(&mut c.enabled));
                egui::ComboBox::from_id_salt((id, i, "kind"))
                    .selected_text(CheckKind::ALL.iter().find(|k| k.0 == c.kind).map(|k| k.2).unwrap_or(""))
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        for (k, _, label) in CheckKind::ALL {
                            ui.selectable_value(&mut c.kind, k, label);
                        }
                    });
                let needs_target = matches!(c.kind, CheckKind::JsonEquals | CheckKind::Header);
                let (tph, eph) = match c.kind {
                    CheckKind::Status => ("", "200, 2xx, 200-204"),
                    CheckKind::BodyContains => ("", "text"),
                    CheckKind::JsonEquals => ("$.status", "value (empty = exists)"),
                    CheckKind::Header => ("Content-Type", "value (empty = present)"),
                    CheckKind::TimeBelow => ("", "500"),
                };
                ui.add_enabled(needs_target, TextEdit::singleline(&mut c.target).hint_text(tph).desired_width(w).font(egui::TextStyle::Monospace));
                ui.add(TextEdit::singleline(&mut c.expected).hint_text(eph).desired_width(w).font(egui::TextStyle::Monospace));
                if icon_button(ui, icons::X, "Remove").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            checks.remove(i);
        }
    }
    if ui.button(format!("{} Add check", icons::PLUS)).clicked() {
        checks.push(Check { kind: CheckKind::Status, target: String::new(), expected: "2xx".into(), enabled: true });
    }
}

/// An editable, syntax-coloured code area.
pub fn code_editor(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, text: &mut String, lang: Lang, rows: usize, hl: &Highlighter) -> egui::Response {
    let p = pal(ui);
    let font = FontId::monospace(13.0);
    let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
        let job = hl.job(buf.as_str(), lang, p, &font, wrap);
        ui.fonts_mut(|f| f.layout_job(job))
    };
    egui::Frame::new()
        .fill(p.surface2)
        .stroke(Stroke::new(1.0, p.border_strong))
        .corner_radius(CornerRadius::same(RADIUS_SM))
        .show(ui, |ui| {
            ui.add(
                TextEdit::multiline(text)
                    .id_salt(id)
                    .code_editor()
                    .frame(egui::Frame::NONE)
                    .margin(Margin::symmetric(10, 8))
                    .desired_rows(rows)
                    .desired_width(f32::INFINITY)
                    .lock_focus(true)
                    .layouter(&mut layouter),
            )
        })
        .inner
}

/// Read-only, selectable, syntax-coloured text.
pub fn code_view(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, text: &str, lang: Lang, hl: &Highlighter) {
    let p = pal(ui);
    let font = FontId::monospace(13.0);
    let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
        let job = hl.job(buf.as_str(), lang, p, &font, wrap);
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let mut s: &str = text;
    egui::Frame::new().fill(p.surface2).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(RADIUS_SM)).show(ui, |ui| {
        ui.add(
            TextEdit::multiline(&mut s)
                .id_salt(id)
                .code_editor()
                .frame(egui::Frame::NONE)
                .margin(Margin::symmetric(10, 8))
                .desired_width(f32::INFINITY)
                .layouter(&mut layouter),
        );
    });
}

/// Lays out `add` in exactly the space available and clips whatever doesn't
/// fit, rather than letting wide content (a long header, an unwrapped line)
/// push the panel wider. Resizable panels remember their size from their
/// content, so without this they jump around as tabs change.
pub fn fixed_size<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let rect = ui.available_rect_before_wrap();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
    child.shrink_clip_rect(rect);
    child.set_max_width(rect.width());
    let r = add(&mut child);
    ui.allocate_rect(rect, egui::Sense::hover());
    r
}

/// A two-column table of header-like pairs.
pub fn pairs_table(ui: &mut Ui, id: &str, pairs: &[(String, String)]) {
    let p = pal(ui);
    if pairs.is_empty() {
        ui.label(RichText::new("None.").color(p.muted));
        return;
    }
    egui::Grid::new(id).num_columns(2).striped(true).spacing([16.0, 6.0]).show(ui, |ui| {
        for (k, v) in pairs {
            let rk = ui.add(egui::Label::new(RichText::new(k).monospace().strong().color(p.text2)).sense(egui::Sense::click()));
            let rv = ui.add(egui::Label::new(RichText::new(v).monospace()).wrap().selectable(true));
            for r in [rk, rv] {
                r.context_menu(|ui| {
                    ui.set_min_width(180.0);
                    if ui.button(format!("{}  Copy value", icons::COPY)).clicked() {
                        ui.ctx().copy_text(v.clone());
                        ui.close();
                    }
                    if ui.button(format!("{}  Copy name", icons::COPY)).clicked() {
                        ui.ctx().copy_text(k.clone());
                        ui.close();
                    }
                    if ui.button(format!("{}  Copy as \u{201c}{k}: \u{2026}\u{201d}", icons::COPY)).clicked() {
                        ui.ctx().copy_text(format!("{k}: {v}"));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(format!("{}  Copy all", icons::COPY)).clicked() {
                        ui.ctx().copy_text(pairs.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n"));
                        ui.close();
                    }
                });
            }
            ui.end_row();
        }
    });
}
