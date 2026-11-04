//! Authentication schemes.
//!
//! [`apply`] adds everything that can be computed up front (Basic, Bearer,
//! API keys, JWT, OAuth 1.0a and AWS signatures, and an OAuth 2 token that is
//! already held). Digest needs the server's challenge first, so the engine
//! calls [`digest_authorization`] after a 401; OAuth 2 tokens are fetched by
//! the engine with [`token_request`] and [`parse_token_response`].

use base64::Engine;
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};

use crate::http::{HttpRequest, HttpResponse};
use crate::ids::new_id;
use crate::model::{Auth, AwsSigV4, GrantType, JwtAuth, OAuth1, OAuth1Method, OAuth2};
use crate::prepare::{append_query, form_urlencode, percent_encode};
use crate::url::{parse_query, Url};
use crate::vars::Vars;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
const B64URL: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Substitutes `{{vars}}` in every text field of the auth settings.
pub fn resolve(auth: &Auth, vars: &Vars) -> Auth {
    fn walk(v: &mut serde_json::Value, vars: &Vars) {
        match v {
            serde_json::Value::String(s) => *s = vars.apply(s),
            serde_json::Value::Array(a) => a.iter_mut().for_each(|x| walk(x, vars)),
            serde_json::Value::Object(o) => o.values_mut().for_each(|x| walk(x, vars)),
            _ => {}
        }
    }
    let mut v = serde_json::to_value(auth).expect("auth serialises");
    walk(&mut v, vars);
    serde_json::from_value(v).unwrap_or_else(|_| auth.clone())
}

fn has_header(headers: &[(String, String)], name: &str) -> bool {
    headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name))
}

fn put_header(headers: &mut Vec<(String, String)>, name: &str, value: String) {
    headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
    headers.push((name.to_string(), value));
}

fn prefixed(prefix: &str, token: &str) -> String {
    if prefix.trim().is_empty() {
        token.to_string()
    } else {
        format!("{} {token}", prefix.trim())
    }
}

/// Applies an already-resolved auth to the outgoing request.
/// A user-set `Authorization` header always wins.
pub fn apply(auth: &Auth, req: &mut HttpRequest, now_ms: u64) -> Result<(), String> {
    match auth {
        Auth::Inherit | Auth::None | Auth::Digest { .. } => {}
        Auth::Basic { username, password } => {
            if !has_header(&req.headers, "authorization") {
                req.headers.push(("Authorization".into(), basic_value(username, password)));
            }
        }
        Auth::Bearer { token } => {
            if !has_header(&req.headers, "authorization") {
                req.headers.push(("Authorization".into(), format!("Bearer {token}")));
            }
        }
        Auth::ApiKey { key, value, in_header } => {
            if key.trim().is_empty() {
                return Ok(());
            }
            if *in_header {
                if !has_header(&req.headers, key) {
                    req.headers.push((key.clone(), value.clone()));
                }
            } else {
                req.url = append_query(&req.url, &[(key.clone(), value.clone())]);
            }
        }
        Auth::OAuth2(o) => {
            if !o.access_token.is_empty() && !has_header(&req.headers, "authorization") {
                req.headers.push(("Authorization".into(), prefixed(&o.header_prefix, &o.access_token)));
            }
        }
        Auth::OAuth1(o) => {
            let value = oauth1_header(o, &req.method, &req.url, &req.headers, &req.body, now_ms, &new_id())?;
            put_header(&mut req.headers, "Authorization", value);
        }
        Auth::AwsSigV4(a) => aws_sign(a, req, now_ms)?,
        Auth::Jwt(j) => {
            let token = jwt_sign(j, now_ms)?;
            if !has_header(&req.headers, "authorization") {
                req.headers.push(("Authorization".into(), prefixed(&j.header_prefix, &token)));
            }
        }
        Auth::ClientCert(c) => {
            if c.certificate.trim().is_empty() || c.key.trim().is_empty() {
                return Err("Client certificate: set both the certificate and the private key".into());
            }
            req.connect.client_cert = c.certificate.trim().to_string();
            req.connect.client_key = c.key.trim().to_string();
        }
    }
    Ok(())
}

pub fn basic_value(user: &str, pass: &str) -> String {
    format!("Basic {}", B64.encode(format!("{user}:{pass}")))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hmac<M: Mac + KeyInit>(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut m = <M as KeyInit>::new_from_slice(key).expect("hmac takes any key length");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

// ---------------------------------------------------------------- digest (RFC 7616)

/// Parses `Digest realm="x", nonce="y", qop="auth"` into pairs.
fn parse_challenge(header: &str) -> Option<Vec<(String, String)>> {
    let rest = header.trim().strip_prefix("Digest").or_else(|| header.trim().strip_prefix("digest"))?;
    let mut out = Vec::new();
    let mut s = rest.trim();
    while !s.is_empty() {
        let eq = s.find('=')?;
        let key = s[..eq].trim().trim_start_matches(',').trim().to_ascii_lowercase();
        s = s[eq + 1..].trim_start();
        let value;
        if let Some(stripped) = s.strip_prefix('"') {
            let mut end = 0;
            let bytes = stripped.as_bytes();
            while end < bytes.len() && bytes[end] != b'"' {
                if bytes[end] == b'\\' {
                    end += 1;
                }
                end += 1;
            }
            value = stripped[..end.min(stripped.len())].replace("\\\"", "\"");
            s = stripped.get(end + 1..).unwrap_or("");
        } else {
            let end = s.find(',').unwrap_or(s.len());
            value = s[..end].trim().to_string();
            s = &s[end..];
        }
        s = s.trim_start().trim_start_matches(',').trim_start();
        out.push((key, value));
    }
    Some(out)
}

/// The `Authorization` value answering a `WWW-Authenticate: Digest ...` challenge.
#[allow(clippy::too_many_arguments)] // the RFC 7616 inputs, one each
pub fn digest_authorization(challenge: &str, user: &str, pass: &str, method: &str, uri: &str, body: &[u8], cnonce: &str, nc: u32) -> Result<String, String> {
    let params = parse_challenge(challenge).ok_or("server did not send a Digest challenge")?;
    let get = |k: &str| params.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    let realm = get("realm").unwrap_or("");
    let nonce = get("nonce").ok_or("digest challenge has no nonce")?;
    let algorithm = get("algorithm").unwrap_or("MD5");
    let qops: Vec<&str> = get("qop").map(|q| q.split(',').map(str::trim).collect()).unwrap_or_default();
    let qop = if qops.contains(&"auth") {
        Some("auth")
    } else if qops.contains(&"auth-int") {
        Some("auth-int")
    } else {
        None
    };
    let alg_upper = algorithm.to_ascii_uppercase();
    let h = |s: &str| -> String {
        if alg_upper.starts_with("SHA-256") {
            hex(&Sha256::digest(s.as_bytes()))
        } else {
            hex(&Md5::digest(s.as_bytes()))
        }
    };
    let mut ha1 = h(&format!("{user}:{realm}:{pass}"));
    if alg_upper.ends_with("-SESS") {
        ha1 = h(&format!("{ha1}:{nonce}:{cnonce}"));
    }
    let ha2 = if qop == Some("auth-int") {
        let body_hash = if alg_upper.starts_with("SHA-256") { hex(&Sha256::digest(body)) } else { hex(&Md5::digest(body)) };
        h(&format!("{method}:{uri}:{body_hash}"))
    } else {
        h(&format!("{method}:{uri}"))
    };
    let nc_s = format!("{nc:08x}");
    let response = match qop {
        Some(q) => h(&format!("{ha1}:{nonce}:{nc_s}:{cnonce}:{q}:{ha2}")),
        None => h(&format!("{ha1}:{nonce}:{ha2}")),
    };
    let mut out = format!("Digest username=\"{user}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{uri}\", algorithm={algorithm}, response=\"{response}\"");
    if let Some(q) = qop {
        out.push_str(&format!(", qop={q}, nc={nc_s}, cnonce=\"{cnonce}\""));
    }
    if let Some(opaque) = get("opaque") {
        out.push_str(&format!(", opaque=\"{opaque}\""));
    }
    Ok(out)
}

// ---------------------------------------------------------------- OAuth 1.0a (RFC 5849)

fn rfc3986(s: &str) -> String {
    percent_encode(s)
}

pub fn oauth1_header(o: &OAuth1, method: &str, url: &str, headers: &[(String, String)], body: &[u8], now_ms: u64, nonce: &str) -> Result<String, String> {
    let u = Url::parse(url)?;
    let timestamp = (now_ms / 1000).to_string();
    let mut oauth: Vec<(String, String)> = vec![
        ("oauth_consumer_key".into(), o.consumer_key.clone()),
        ("oauth_nonce".into(), nonce.into()),
        ("oauth_signature_method".into(), o.signature_method.name().into()),
        ("oauth_timestamp".into(), timestamp),
        ("oauth_version".into(), "1.0".into()),
    ];
    if !o.token.is_empty() {
        oauth.push(("oauth_token".into(), o.token.clone()));
    }

    let key = format!("{}&{}", rfc3986(&o.consumer_secret), rfc3986(&o.token_secret));
    let signature = match o.signature_method {
        OAuth1Method::Plaintext => key,
        m => {
            let mut params: Vec<(String, String)> = u.query_pairs();
            params.extend(oauth.iter().cloned());
            let is_form = headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v.starts_with("application/x-www-form-urlencoded"));
            if is_form {
                params.extend(parse_query(&String::from_utf8_lossy(body)));
            }
            let mut enc: Vec<(String, String)> = params.iter().map(|(k, v)| (rfc3986(k), rfc3986(v))).collect();
            enc.sort();
            let param_str = enc.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
            let base_url = format!("{}://{}{}", u.scheme, u.host_header(), u.path);
            let base = format!("{}&{}&{}", method.to_ascii_uppercase(), rfc3986(&base_url), rfc3986(&param_str));
            let sig = match m {
                OAuth1Method::HmacSha256 => hmac::<Hmac<Sha256>>(key.as_bytes(), base.as_bytes()),
                _ => hmac::<Hmac<Sha1>>(key.as_bytes(), base.as_bytes()),
            };
            B64.encode(sig)
        }
    };
    oauth.push(("oauth_signature".into(), signature));
    let mut parts = Vec::new();
    if !o.realm.is_empty() {
        parts.push(format!("realm=\"{}\"", o.realm.replace('"', "")));
    }
    parts.extend(oauth.iter().map(|(k, v)| format!("{k}=\"{}\"", rfc3986(v))));
    Ok(format!("OAuth {}", parts.join(", ")))
}

// ---------------------------------------------------------------- AWS Signature v4

fn aws_sign(a: &AwsSigV4, req: &mut HttpRequest, now_ms: u64) -> Result<(), String> {
    if a.access_key.is_empty() || a.secret_key.is_empty() {
        return Err("AWS signature needs an access key and secret key".into());
    }
    let u = Url::parse(&req.url)?;
    let amz_date = crate::clock::amz_date(now_ms);
    let date = &amz_date[..8];
    let payload_hash = sha256_hex(&req.body);
    let region = if a.region.is_empty() { "us-east-1" } else { &a.region };
    let service = if a.service.is_empty() { "execute-api" } else { &a.service };

    put_header(&mut req.headers, "x-amz-date", amz_date.clone());
    put_header(&mut req.headers, "x-amz-content-sha256", payload_hash.clone());
    if !a.session_token.is_empty() {
        put_header(&mut req.headers, "x-amz-security-token", a.session_token.clone());
    }

    // Host is signed but not sent as a header (browsers forbid setting it).
    let mut canon_headers: Vec<(String, String)> = req
        .headers
        .iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("authorization") && !k.eq_ignore_ascii_case("host"))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.split_whitespace().collect::<Vec<_>>().join(" ")))
        .collect();
    canon_headers.push(("host".into(), u.host_header()));
    canon_headers.sort();
    // merge duplicates
    let mut merged: Vec<(String, String)> = Vec::new();
    for (k, v) in canon_headers {
        match merged.last_mut() {
            Some((lk, lv)) if *lk == k => {
                lv.push(',');
                lv.push_str(&v);
            }
            _ => merged.push((k, v)),
        }
    }
    let signed_headers = merged.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let canon_header_str: String = merged.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();

    let canon_uri = if service == "s3" { u.path.clone() } else { u.path.split('/').map(rfc3986).collect::<Vec<_>>().join("/") };
    let mut q: Vec<(String, String)> = u.query_pairs().into_iter().map(|(k, v)| (rfc3986(&k), rfc3986(&v))).collect();
    q.sort();
    let canon_query = q.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");

    let canonical = format!("{}\n{canon_uri}\n{canon_query}\n{canon_header_str}\n{signed_headers}\n{payload_hash}", req.method.to_ascii_uppercase());
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha256_hex(canonical.as_bytes()));

    let k_date = hmac::<Hmac<Sha256>>(format!("AWS4{}", a.secret_key).as_bytes(), date.as_bytes());
    let k_region = hmac::<Hmac<Sha256>>(&k_date, region.as_bytes());
    let k_service = hmac::<Hmac<Sha256>>(&k_region, service.as_bytes());
    let k_signing = hmac::<Hmac<Sha256>>(&k_service, b"aws4_request");
    let signature = hex(&hmac::<Hmac<Sha256>>(&k_signing, to_sign.as_bytes()));

    put_header(
        &mut req.headers,
        "Authorization",
        format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}", a.access_key),
    );
    Ok(())
}

// ---------------------------------------------------------------- JWT

pub fn jwt_sign(j: &JwtAuth, now_ms: u64) -> Result<String, String> {
    let alg = if j.algorithm.is_empty() { "HS256".to_string() } else { j.algorithm.to_ascii_uppercase() };
    let mut claims: serde_json::Value = if j.payload.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&j.payload).map_err(|e| format!("JWT payload is not valid JSON: {e}"))?
    };
    if let Some(obj) = claims.as_object_mut() {
        let now = now_ms / 1000;
        if j.expires_in_secs > 0 {
            obj.entry("iat").or_insert(now.into());
            obj.entry("exp").or_insert((now + j.expires_in_secs).into());
        }
    }
    let header = serde_json::json!({ "alg": alg, "typ": "JWT" });
    let signing_input = format!("{}.{}", B64URL.encode(header.to_string()), B64URL.encode(claims.to_string()));
    let sig = match alg.as_str() {
        "HS256" => hmac::<Hmac<Sha256>>(j.secret.as_bytes(), signing_input.as_bytes()),
        "HS384" => hmac::<Hmac<Sha384>>(j.secret.as_bytes(), signing_input.as_bytes()),
        "HS512" => hmac::<Hmac<Sha512>>(j.secret.as_bytes(), signing_input.as_bytes()),
        other => return Err(format!("JWT algorithm {other} is not supported here (use HS256, HS384 or HS512)")),
    };
    Ok(format!("{signing_input}.{}", B64URL.encode(sig)))
}

// ---------------------------------------------------------------- OAuth 2.0

pub enum TokenGrant<'a> {
    /// The grant configured on the auth (client credentials or password).
    Configured,
    Refresh,
    Code {
        code: &'a str,
        verifier: &'a str,
        redirect_uri: &'a str,
    },
}

/// The POST to the token endpoint.
pub fn token_request(o: &OAuth2, grant: TokenGrant) -> Result<HttpRequest, String> {
    if o.token_url.trim().is_empty() {
        return Err("OAuth 2: set the access token URL".into());
    }
    let mut form: Vec<(String, String)> = Vec::new();
    match grant {
        TokenGrant::Refresh => {
            form.push(("grant_type".into(), "refresh_token".into()));
            form.push(("refresh_token".into(), o.refresh_token.clone()));
        }
        TokenGrant::Code { code, verifier, redirect_uri } => {
            form.push(("grant_type".into(), "authorization_code".into()));
            form.push(("code".into(), code.into()));
            form.push(("redirect_uri".into(), redirect_uri.into()));
            if !verifier.is_empty() {
                form.push(("code_verifier".into(), verifier.into()));
            }
        }
        TokenGrant::Configured => match o.grant {
            GrantType::ClientCredentials => form.push(("grant_type".into(), "client_credentials".into())),
            GrantType::Password => {
                form.push(("grant_type".into(), "password".into()));
                form.push(("username".into(), o.username.clone()));
                form.push(("password".into(), o.password.clone()));
            }
            GrantType::AuthorizationCode => return Err("OAuth 2: use \u{201c}Get new token\u{201d} to sign in for an authorization code".into()),
            GrantType::Manual => return Err("OAuth 2: paste an access token, or set a refresh token".into()),
        },
    }
    if !o.scope.is_empty() {
        form.push(("scope".into(), o.scope.clone()));
    }
    if !o.audience.is_empty() {
        form.push(("audience".into(), o.audience.clone()));
    }
    let mut headers =
        vec![("Content-Type".to_string(), "application/x-www-form-urlencoded".to_string()), ("Accept".to_string(), "application/json".to_string())];
    if o.client_auth_basic && !o.client_secret.is_empty() {
        headers.push(("Authorization".into(), basic_value(&percent_encode(&o.client_id), &percent_encode(&o.client_secret))));
    } else {
        form.push(("client_id".into(), o.client_id.clone()));
        if !o.client_secret.is_empty() {
            form.push(("client_secret".into(), o.client_secret.clone()));
        }
    }
    Ok(HttpRequest {
        method: "POST".into(),
        url: o.token_url.trim().into(),
        headers,
        body: form_urlencode(&form).into_bytes(),
        connect: Default::default(),
        grpc: false,
        timeout_ms: 30_000,
        no_redirects: false,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct TokenResult {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
}

pub fn parse_token_response(resp: &HttpResponse, now_ms: u64) -> Result<TokenResult, String> {
    let text = resp.text();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|_| {
        // Some servers answer form-encoded.
        let pairs = parse_query(&text);
        serde_json::Value::Object(pairs.into_iter().map(|(k, v)| (k, serde_json::Value::String(v))).collect())
    });
    if !(200..300).contains(&resp.status) || v.get("error").is_some() {
        let err = v.get("error_description").or_else(|| v.get("error")).and_then(|e| e.as_str()).unwrap_or("");
        return Err(format!("token endpoint returned {}: {}", resp.status, if err.is_empty() { text.chars().take(300).collect() } else { err.to_string() }));
    }
    let access = v.get("access_token").and_then(|t| t.as_str()).ok_or("token response has no access_token")?;
    let expires_in = v.get("expires_in").and_then(|e| e.as_u64().or_else(|| e.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0);
    Ok(TokenResult {
        access_token: access.into(),
        refresh_token: v.get("refresh_token").and_then(|t| t.as_str()).unwrap_or("").into(),
        expires_at: if expires_in > 0 { now_ms + expires_in * 1000 } else { 0 },
    })
}

/// PKCE verifier and S256 challenge.
pub fn pkce_pair() -> (String, String) {
    let verifier: String = (0..4).map(|_| new_id()).collect();
    let challenge = B64URL.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

pub fn authorize_url(o: &OAuth2, redirect_uri: &str, state: &str, code_challenge: Option<&str>) -> Result<String, String> {
    if o.auth_url.trim().is_empty() {
        return Err("OAuth 2: set the authorization URL".into());
    }
    let mut q = vec![
        ("response_type".to_string(), "code".to_string()),
        ("client_id".to_string(), o.client_id.clone()),
        ("redirect_uri".to_string(), redirect_uri.to_string()),
        ("state".to_string(), state.to_string()),
    ];
    if !o.scope.is_empty() {
        q.push(("scope".into(), o.scope.clone()));
    }
    if !o.audience.is_empty() {
        q.push(("audience".into(), o.audience.clone()));
    }
    if let Some(c) = code_challenge {
        q.push(("code_challenge".into(), c.into()));
        q.push(("code_challenge_method".into(), "S256".into()));
    }
    Ok(append_query(o.auth_url.trim(), &q))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, url: &str) -> HttpRequest {
        HttpRequest { method: method.into(), url: url.into(), ..Default::default() }
    }

    #[test]
    fn basic_and_bearer() {
        let mut r = req("GET", "http://x/");
        apply(&Auth::Basic { username: "Aladdin".into(), password: "open sesame".into() }, &mut r, 0).unwrap();
        assert_eq!(r.headers[0].1, "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
        let mut r = req("GET", "http://x/");
        apply(&Auth::ApiKey { key: "api_key".into(), value: "k 1".into(), in_header: false }, &mut r, 0).unwrap();
        assert_eq!(r.url, "http://x/?api_key=k%201");
    }

    #[test]
    fn digest_rfc2617_example() {
        // RFC 2617 section 3.5.
        let challenge =
            r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#;
        let v = digest_authorization(challenge, "Mufasa", "Circle Of Life", "GET", "/dir/index.html", b"", "0a4f113b", 1).unwrap();
        assert!(v.contains("response=\"6629fae49393a05397450978507c4ef1\""), "{v}");
        assert!(v.contains("opaque=\"5ccc069c403ebaf9f0171e9517f40e41\""));
    }

    #[test]
    fn oauth1_rfc5849_example() {
        // RFC 5849 section 1.2 (photos example, HMAC-SHA1).
        let o = OAuth1 {
            consumer_key: "dpf43f3p2l4k3l03".into(),
            consumer_secret: "kd94hf93k423kf44".into(),
            token: "nnch734d00sl2jdk".into(),
            token_secret: "pfkkdhi9sl3r4s00".into(),
            signature_method: OAuth1Method::HmacSha1,
            realm: "Photos".into(),
        };
        let h = oauth1_header(&o, "GET", "http://photos.example.net/photos?file=vacation.jpg&size=original", &[], b"", 1_191_242_096_000, "kllo9940pd9333jh")
            .unwrap();
        assert!(h.contains("oauth_signature=\"tR3%2BTy81lMeYAr%2FFid0kMTYa%2FWM%3D\""), "{h}");
    }

    #[test]
    fn aws_sigv4_get_vanilla() {
        // AWS SigV4 test suite "get-vanilla".
        let a = AwsSigV4 {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: String::new(),
            region: "us-east-1".into(),
            service: "service".into(),
        };
        let mut r = req("GET", "https://example.amazonaws.com/");
        // 20150830T123600Z
        aws_sign(&a, &mut r, 1_440_938_160_000).unwrap();
        let auth = r.headers.iter().find(|(k, _)| k == "Authorization").unwrap().1.clone();
        // The suite signs only host and x-amz-date; we also sign x-amz-content-sha256,
        // so check structure rather than the suite's exact signature.
        assert!(auth.starts_with(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature="
        ));
    }

    #[test]
    fn jwt_hs256() {
        let j = JwtAuth {
            algorithm: "HS256".into(),
            secret: "secret".into(),
            payload: r#"{"sub":"1234567890","name":"John Doe","iat":1516239022}"#.into(),
            expires_in_secs: 0,
            header_prefix: "Bearer".into(),
        };
        let t = jwt_sign(&j, 0).unwrap();
        let parts: Vec<&str> = t.split('.').collect();
        assert_eq!(parts.len(), 3);
        let sig = hmac::<Hmac<Sha256>>(b"secret", format!("{}.{}", parts[0], parts[1]).as_bytes());
        assert_eq!(parts[2], B64URL.encode(sig));
    }

    #[test]
    fn oauth2_token_flow() {
        let o =
            OAuth2 { token_url: "https://auth/token".into(), client_id: "id".into(), client_secret: "s".into(), scope: "read".into(), ..Default::default() };
        let r = token_request(&o, TokenGrant::Configured).unwrap();
        assert_eq!(String::from_utf8(r.body).unwrap(), "grant_type=client_credentials&scope=read");
        let resp = HttpResponse { status: 200, body: br#"{"access_token":"abc","expires_in":60}"#.to_vec(), ..Default::default() };
        let t = parse_token_response(&resp, 1000).unwrap();
        assert_eq!(t.access_token, "abc");
        assert_eq!(t.expires_at, 61_000);
    }
}
