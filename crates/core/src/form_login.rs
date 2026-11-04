//! Signs in through a site's HTML form, for downloads that sit behind a
//! sign-in page rather than an auth header. It does what a browser would:
//! loads the page, keeps its cookies, posts the form with its hidden fields
//! (such as an anti-forgery token) and keeps the cookies that come back.

use regex_lite::Regex;

use crate::http::{ConnectOptions, HttpRequest, HttpResponse, Transport};
use crate::prepare::form_urlencode;
use crate::url::Url;

const MAX_REDIRECTS: usize = 10;
const TIMEOUT_MS: u32 = 60_000;
const ACCEPT_HTML: &str = "text/html,application/xhtml+xml,*/*;q=0.8";
/// Anything between a tag's name and its `>`, allowing for `>` in quotes.
const TAG_REST: &str = r#"((?:[^>"']|"[^"]*"|'[^']*')*)"#;

/// What to sign in with. `page` is optional: without it, the address being
/// downloaded is expected to lead to the sign-in form.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FormLogin {
    pub page: String,
    pub username: String,
    pub password: String,
}

struct Cookie {
    name: String,
    value: String,
    domain: String,
    /// Sent to `domain` only, not its subdomains.
    host_only: bool,
    secure: bool,
}

/// The cookies of one sign-in. Paths aren't tracked: every cookie goes to
/// every path of its host.
#[derive(Default)]
pub struct CookieJar(Vec<Cookie>);

fn in_domain(host: &str, domain: &str) -> bool {
    host == domain || host.strip_suffix(domain).is_some_and(|rest| rest.ends_with('.'))
}

impl CookieJar {
    /// Keeps a `Set-Cookie` header received from `url`.
    fn store(&mut self, url: &Url, set_cookie: &str) {
        let mut parts = set_cookie.split(';');
        let Some((name, value)) = parts.next().and_then(|p| p.split_once('=')) else { return };
        let (name, value) = (name.trim(), value.trim());
        let host = url.host.to_ascii_lowercase();
        let mut cookie = Cookie { name: name.into(), value: value.into(), domain: host.clone(), host_only: true, secure: false };
        let mut expired = value.is_empty();
        for attr in parts {
            let (k, v) = attr.split_once('=').unwrap_or((attr, ""));
            let v = v.trim();
            match k.trim().to_ascii_lowercase().as_str() {
                "domain" if !v.is_empty() => {
                    let domain = v.trim_start_matches('.').to_ascii_lowercase();
                    // A site can't set cookies for somewhere else.
                    if !in_domain(&host, &domain) {
                        return;
                    }
                    cookie.domain = domain;
                    cookie.host_only = false;
                }
                "secure" => cookie.secure = true,
                "max-age" => expired |= v.parse::<i64>().is_ok_and(|s| s <= 0),
                // How sites clear a cookie; dates in general aren't parsed.
                "expires" => expired |= v.contains("1970"),
                _ => {}
            }
        }
        self.0.retain(|c| !(c.name == cookie.name && c.domain == cookie.domain));
        if !expired {
            self.0.push(cookie);
        }
    }

    /// The `Cookie` header for a request to `url`, if any cookie applies.
    pub fn header(&self, url: &Url) -> Option<String> {
        let host = url.host.to_ascii_lowercase();
        let applies = |c: &&Cookie| (c.host_only && c.domain == host || !c.host_only && in_domain(&host, &c.domain)) && (!c.secure || url.is_tls());
        let pairs: Vec<String> = self.0.iter().filter(applies).map(|c| format!("{}={}", c.name, c.value)).collect();
        (!pairs.is_empty()).then(|| pairs.join("; "))
    }
}

/// Where a link on the page at `base` leads.
fn resolve(base: &str, link: &str) -> Result<String, String> {
    let link = link.trim().split('#').next().unwrap_or("");
    let lower = link.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Ok(link.to_string());
    }
    let b = Url::parse(base)?;
    if let Some(rest) = link.strip_prefix("//") {
        return Ok(format!("{}://{rest}", b.scheme));
    }
    let origin = format!("{}://{}", b.scheme, b.host_header());
    if link.is_empty() {
        return Ok(format!("{origin}{}", b.path_and_query()));
    }
    if link.starts_with('/') {
        return Ok(format!("{origin}{link}"));
    }
    if link.starts_with('?') {
        return Ok(format!("{origin}{}{link}", b.path));
    }
    let (path, query) = link.split_once('?').map_or((link, None), |(p, q)| (p, Some(q)));
    let dir = &b.path[..b.path.rfind('/').map_or(0, |i| i + 1)];
    let mut segments: Vec<&str> = Vec::new();
    for s in dir.split('/').chain(path.split('/')).filter(|s| !s.is_empty()) {
        match s {
            "." => {}
            ".." => {
                segments.pop();
            }
            s => segments.push(s),
        }
    }
    let query = query.map(|q| format!("?{q}")).unwrap_or_default();
    Ok(format!("{origin}/{}{query}", segments.join("/")))
}

fn decode_entities(s: &str) -> String {
    let numeric = Regex::new("&#([xX][0-9a-fA-F]+|[0-9]+);").expect("a valid pattern");
    let s = numeric.replace_all(s, |c: &regex_lite::Captures| {
        let n = &c[1];
        let code = match n.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok(),
            None => n.parse().ok(),
        };
        code.and_then(char::from_u32).map(String::from).unwrap_or_else(|| c[0].to_string())
    });
    // &amp; last, so "&amp;lt;" stays the text "&lt;".
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// The attributes of a tag, names in lower case and values decoded.
fn attributes(tag: &str) -> Vec<(String, String)> {
    let attr = Regex::new(r#"([^\s=/"'<>]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+)))?"#).expect("a valid pattern");
    attr.captures_iter(tag)
        .map(|c| {
            let value = c.get(2).or(c.get(3)).or(c.get(4)).map_or("", |m| m.as_str());
            (c[1].to_ascii_lowercase(), decode_entities(value))
        })
        .collect()
}

fn attribute<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// A form with a password box.
#[derive(Debug, PartialEq)]
struct SignInForm {
    action: String,
    method: String,
    fields: Vec<(String, String)>,
    /// Indexes into `fields`.
    username: Option<usize>,
    password: usize,
}

/// Finds the sign-in form of a page: the first form with a password box.
fn parse_form(html: &str) -> Option<SignInForm> {
    let comments = Regex::new(r"(?s)<!--.*?-->").expect("a valid pattern");
    let html = comments.replace_all(html, "");
    let forms = Regex::new(&format!(r"(?is)<form\b{TAG_REST}>(.*?)</form\s*>")).expect("a valid pattern");
    let controls = Regex::new(&format!(r"(?is)<(input|button)\b{TAG_REST}>")).expect("a valid pattern");
    let form = forms.captures_iter(&html).find_map(|form| {
        let attrs = attributes(&form[1]);
        let mut found = SignInForm {
            action: attribute(&attrs, "action").unwrap_or("").to_string(),
            method: attribute(&attrs, "method").unwrap_or("get").to_ascii_lowercase(),
            fields: Vec::new(),
            username: None,
            password: usize::MAX,
        };
        let mut submitted = false;
        for control in controls.captures_iter(&form[2]) {
            let attrs = attributes(&control[2]);
            let Some(name) = attribute(&attrs, "name").filter(|n| !n.is_empty()) else { continue };
            let is_button = control[1].eq_ignore_ascii_case("button");
            let kind = attribute(&attrs, "type").unwrap_or(if is_button { "submit" } else { "text" }).to_ascii_lowercase();
            let value = attribute(&attrs, "value").unwrap_or("");
            let index = found.fields.len();
            match kind.as_str() {
                "password" if found.password == usize::MAX => found.password = index,
                "text" | "email" | "tel" if found.username.is_none() => found.username = Some(index),
                "checkbox" | "radio" if attribute(&attrs, "checked").is_none() => continue,
                "checkbox" | "radio" => {
                    found.fields.push((name.to_string(), if value.is_empty() { "on".to_string() } else { value.to_string() }));
                    continue;
                }
                // One button is pressed, so only the first goes along.
                "submit" if !submitted => submitted = true,
                "submit" | "button" | "reset" | "image" | "file" => continue,
                _ => {}
            }
            found.fields.push((name.to_string(), value.to_string()));
        }
        (found.password != usize::MAX).then_some(found)
    });
    form
}

fn is_html(resp: &HttpResponse) -> bool {
    resp.content_type().to_ascii_lowercase().contains("html")
}

/// Sends a request and follows its redirects by hand, because each hop can
/// set cookies. Redirects always continue as a GET, so a posted password is
/// only ever sent to the form's own address.
async fn walk(mut req: HttpRequest, jar: &mut CookieJar, transport: &dyn Transport) -> Result<(String, HttpResponse), String> {
    for _ in 0..=MAX_REDIRECTS {
        let url = Url::parse(&req.url)?;
        req.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("cookie"));
        if let Some(cookie) = jar.header(&url) {
            req.headers.push(("Cookie".into(), cookie));
        }
        let resp = transport.send(req.clone()).await?;
        for (_, v) in resp.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie")) {
            jar.store(&url, v);
        }
        let location = resp.header("location").filter(|_| (300..400).contains(&resp.status) && resp.status != 304);
        let Some(location) = location else { return Ok((req.url, resp)) };
        req.url = resolve(&req.url, location)?;
        req.method = "GET".into();
        req.body.clear();
        req.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
    }
    Err(format!("too many redirects from {}", req.url))
}

fn request(method: &str, url: &str, connect: &ConnectOptions) -> HttpRequest {
    HttpRequest {
        method: method.into(),
        url: url.into(),
        headers: vec![("Accept".into(), ACCEPT_HTML.into())],
        timeout_ms: TIMEOUT_MS,
        connect: connect.clone(),
        no_redirects: true,
        ..Default::default()
    }
}

/// Signs in and returns the cookies to send afterwards. `target` is the
/// address the sign-in is for; it's where to look when `login.page` is blank.
pub async fn sign_in(login: &FormLogin, target: &str, connect: &ConnectOptions, transport: &dyn Transport) -> Result<CookieJar, String> {
    // A browser hides Set-Cookie from a page and won't send a Cookie header.
    if !transport.honours_connect_options() {
        return Err("signing in through a web page needs the desktop app or the local agent (apicraft-agent)".into());
    }
    let page = login.page.trim();
    let page = if page.is_empty() { target } else { page };
    let mut jar = CookieJar::default();
    let (page_url, resp) = walk(request("GET", page, connect), &mut jar, transport).await?;
    let Some(mut form) = parse_form(&resp.text()) else {
        // Not protected after all.
        if login.page.trim().is_empty() && (200..300).contains(&resp.status) && !is_html(&resp) {
            return Ok(jar);
        }
        return Err(format!(
            "no sign-in form at {page_url}: if the page is built with JavaScript or the form is elsewhere, give the sign-in page, or add a Cookie header from your browser"
        ));
    };
    if form.method != "post" {
        return Err(format!("the sign-in form at {page_url} would put the password in the address, so it wasn't sent"));
    }
    let action = resolve(&page_url, &form.action)?;
    if page_url.to_ascii_lowercase().starts_with("https:") && !action.to_ascii_lowercase().starts_with("https:") {
        return Err(format!("the sign-in form at {page_url} posts to an unencrypted address, so the password wasn't sent"));
    }
    match form.username {
        Some(i) if login.username.trim().is_empty() => return Err(format!("the sign-in form asks for a username ({})", form.fields[i].0)),
        Some(i) => form.fields[i].1 = login.username.trim().to_string(),
        None => {}
    }
    form.fields[form.password].1 = login.password.clone();

    let mut post = request("POST", &action, connect);
    post.headers.push(("Content-Type".into(), "application/x-www-form-urlencoded".into()));
    post.headers.push(("Referer".into(), page_url.clone()));
    post.body = form_urlencode(&form.fields).into_bytes();
    let (_, after) = walk(post, &mut jar, transport).await?;
    if matches!(after.status, 401 | 403) || (is_html(&after) && parse_form(&after.text()).is_some()) {
        return Err(format!("signing in at {page_url} didn't work: check the username and password"));
    }
    Ok(jar)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_form_with_the_password_box() {
        let html = r#"
            <form action="/search"><input name="q"></form>
            <!-- <form method="post" action="/old"><input type="password" name="x"></form> -->
            <FORM method='POST' action="/Account/Login?a=1&amp;b=2">
              <input type="hidden" name="returnUrl" value="&#x2F;swagger">
              <input name="__RequestVerificationToken" type="hidden" value="tok>en" />
              <input type="email" name="user">
              <input type="password" name="password" autofocus>
              <input type="checkbox" name="remember" value="yes">
              <input type="checkbox" name="agree" checked>
              <button name="go" value="1">Sign in</button>
              <button name="other" value="2">Other</button>
            </FORM>"#;
        let f = parse_form(html).expect("a form");
        assert_eq!(f.action, "/Account/Login?a=1&b=2");
        assert_eq!(f.method, "post");
        let fields: Vec<(&str, &str)> = f.fields.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(fields, [("returnUrl", "/swagger"), ("__RequestVerificationToken", "tok>en"), ("user", ""), ("password", ""), ("agree", "on"), ("go", "1")]);
        assert_eq!((f.username, f.password), (Some(2), 3));
        assert!(parse_form("<form><input name=q></form>").is_none());
    }

    #[test]
    fn links_resolve_against_the_page() {
        let base = "https://a.test:8443/x/y/page?q=1";
        for (link, want) in [
            ("", "https://a.test:8443/x/y/page?q=1"),
            ("/Login?r=%2F", "https://a.test:8443/Login?r=%2F"),
            ("?z=2", "https://a.test:8443/x/y/page?z=2"),
            ("next", "https://a.test:8443/x/y/next"),
            ("../up?a=b#frag", "https://a.test:8443/x/up?a=b"),
            ("//b.test/p", "https://b.test/p"),
            ("http://c.test/p", "http://c.test/p"),
        ] {
            assert_eq!(resolve(base, link).unwrap(), want, "{link}");
        }
    }

    #[test]
    fn cookies_stay_with_the_site_that_set_them() {
        let url = |u: &str| Url::parse(u).unwrap();
        let site = url("https://app.example.test/");
        let mut jar = CookieJar::default();
        jar.store(&site, "a=1; path=/; samesite=strict; httponly");
        jar.store(&site, "b=2; Domain=.example.test; Secure");
        jar.store(&site, "evil=1; Domain=other.test");
        assert_eq!(jar.header(&site).as_deref(), Some("a=1; b=2"));
        assert_eq!(jar.header(&url("https://api.example.test/")).as_deref(), Some("b=2"));
        assert_eq!(jar.header(&url("http://api.example.test/")), None, "secure cookies need https");
        assert_eq!(jar.header(&url("https://notexample.test/")), None);
        assert_eq!(jar.header(&url("https://other.test/")), None);

        jar.store(&site, "a=3");
        jar.store(&site, "b=; Domain=example.test; expires=Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(jar.header(&site).as_deref(), Some("a=3"));
    }
}
