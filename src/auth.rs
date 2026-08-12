use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::Next,
    response::{Html, IntoResponse, Response},
};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;

use crate::state::AppState;

const AUTH_COOKIE: &str = "blogger_session";
const SESSION_LIFETIME_SECS: u64 = 30 * 24 * 60 * 60;
const MAX_LOGIN_FAILURES: u8 = 5;
const LOGIN_BLOCK_DURATION: Duration = Duration::from_secs(60);

struct LoginLimiter {
    failures: u8,
    blocked_until: Option<Instant>,
}

static LOGIN_LIMITER: Mutex<LoginLimiter> = Mutex::new(LoginLimiter {
    failures: 0,
    blocked_until: None,
});

fn token_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut diff = left.len() ^ right.len();
    for i in 0..left.len().max(right.len()) {
        let a = left.get(i).copied().unwrap_or(0);
        let b = right.get(i).copied().unwrap_or(0);
        diff |= usize::from(a ^ b);
    }
    diff == 0
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn session_signature(secret: &[u8; 32], expiry: u64) -> String {
    let message = format!("v1.{expiry}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts a 32-byte key");
    mac.update(message.as_bytes());
    hex_encode(&mac.finalize().into_bytes())
}

fn sign_session(secret: &[u8; 32], expiry: u64) -> String {
    format!("v1.{expiry}.{}", session_signature(secret, expiry))
}

fn verify_session(value: &str, secret: &[u8; 32], now: u64) -> bool {
    let mut parts = value.split('.');
    let Some(version) = parts.next() else {
        return false;
    };
    let Some(expiry_text) = parts.next() else {
        return false;
    };
    let Some(signature) = parts.next() else {
        return false;
    };
    if parts.next().is_some() || version != "v1" {
        return false;
    }

    let Ok(expiry) = expiry_text.parse::<u64>() else {
        return false;
    };
    if expiry <= now {
        return false;
    }

    token_eq(signature, &session_signature(secret, expiry))
}

fn cookie_value(headers: &HeaderMap) -> Option<&str> {
    headers.get_all(header::COOKIE).iter().find_map(|value| {
        value.to_str().ok()?.split(';').find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            (name == AUTH_COOKIE).then_some(value)
        })
    })
}

fn session_cookie(secret: &[u8; 32], now: u64) -> String {
    let expiry = now.saturating_add(SESSION_LIFETIME_SECS);
    let value = sign_session(secret, expiry);
    format!(
        "{AUTH_COOKIE}={value}; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age={SESSION_LIFETIME_SECS}"
    )
}

fn logout_cookie() -> String {
    format!("{AUTH_COOKIE}=; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=0")
}

fn decode_form_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                let high = (bytes[index + 1] as char).to_digit(16)?;
                let low = (bytes[index + 2] as char).to_digit(16)?;
                decoded.push(((high << 4) | low) as u8);
                index += 2;
            }
            b'%' => return None,
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8(decoded).ok()
}

fn form_password(body: &[u8]) -> Option<String> {
    let body = std::str::from_utf8(body).ok()?;
    body.split('&').find_map(|part| {
        let (name, value) = part.split_once('=').unwrap_or((part, ""));
        (decode_form_component(name).as_deref() == Some("password"))
            .then(|| decode_form_component(value))
            .flatten()
    })
}

fn json_password(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()?
        .get("password")?
        .as_str()
        .map(str::to_owned)
}

fn request_password(headers: &HeaderMap, body: &[u8]) -> Option<String> {
    let content_type = headers
        .get(header::CONTENT_TYPE)?
        .to_str()
        .ok()?
        .split(';')
        .next()?
        .trim();
    match content_type {
        "application/x-www-form-urlencoded" => form_password(body),
        "application/json" => json_password(body),
        _ => None,
    }
}

fn login_allowed(password: Option<&str>, expected: &str) -> bool {
    let now = Instant::now();
    let mut limiter = LOGIN_LIMITER.lock().unwrap_or_else(|e| e.into_inner());

    if let Some(blocked_until) = limiter.blocked_until {
        if now < blocked_until {
            return false;
        }
        limiter.failures = 0;
        limiter.blocked_until = None;
    }

    if password.is_some_and(|password| token_eq(password, expected)) {
        limiter.failures = 0;
        limiter.blocked_until = None;
        return true;
    }

    limiter.failures = limiter.failures.saturating_add(1);
    if limiter.failures >= MAX_LOGIN_FAILURES {
        limiter.blocked_until = Some(now + LOGIN_BLOCK_DURATION);
    }
    false
}

fn incorrect_password() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({
            "error": "Incorrect password"
        })),
    )
        .into_response()
}

pub async fn login(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let password = request_password(&headers, &body);
    if !login_allowed(password.as_deref(), &state.config.password) {
        return incorrect_password();
    }

    (
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            session_cookie(&state.config.session_secret, unix_now()),
        )],
    )
        .into_response()
}

pub async fn logout() -> Response {
    (
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, logout_cookie())],
    )
        .into_response()
}

fn login_page() -> Response {
    const PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Blogger Login</title>
  <style>
    :root { color-scheme: dark; font-family: system-ui, sans-serif; }
    body { margin: 0; min-height: 100vh; display: grid; place-items: center; background: #111827; color: #f9fafb; }
    main { width: min(24rem, calc(100vw - 2rem)); }
    label { display: block; margin-bottom: .5rem; font-weight: 650; }
    input { box-sizing: border-box; width: 100%; padding: .75rem; font: inherit; background: #1f2937; color: inherit; border: 1px solid #4b5563; border-radius: .35rem; }
    button { margin-top: .75rem; width: 100%; padding: .75rem; font: inherit; font-weight: 650; cursor: pointer; }
    p { min-height: 1.5rem; color: #fca5a5; }
  </style>
</head>
<body>
  <main>
    <h1>Blogger</h1>
    <form method="post" action="/auth/login">
      <label for="password">Password</label>
      <input id="password" name="password" type="password" autocomplete="current-password" autofocus required>
      <button type="submit">Continue</button>
      <p id="status" aria-live="polite"></p>
    </form>
  </main>
  <script>
    document.querySelector("form").addEventListener("submit", async event => {
      event.preventDefault();
      const response = await fetch("/auth/login", {
        method: "POST",
        headers: { "Content-Type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams(new FormData(event.currentTarget))
      });
      if (response.status === 204) location.replace("/");
      else document.querySelector("#status").textContent = "Incorrect password";
    });
  </script>
</body>
</html>"##;

    (StatusCode::UNAUTHORIZED, Html(PAGE)).into_response()
}

fn unauthorized(method: &Method) -> Response {
    if method == Method::GET || method == Method::HEAD {
        return login_page();
    }

    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({ "error": "unauthorized" })),
    )
        .into_response()
}

pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if matches!(path, "/api/health" | "/api/ready" | "/auth/login") {
        return next.run(request).await;
    }

    let authenticated = cookie_value(request.headers())
        .is_some_and(|value| verify_session(value, &state.config.session_secret, unix_now()));
    if authenticated {
        next.run(request).await
    } else {
        unauthorized(request.method())
    }
}

#[cfg(test)]
mod tests {
    use super::{SESSION_LIFETIME_SECS, sign_session, token_eq, verify_session};

    const SECRET: [u8; 32] = [0x5a; 32];

    #[test]
    fn compares_tokens() {
        assert!(token_eq("abc123", "abc123"));
        assert!(!token_eq("abc123", "abc124"));
        assert!(!token_eq("abc123", "abc1234"));
    }

    #[test]
    fn session_round_trip_and_expiry() {
        let now = 1_700_000_000;
        let expiry = now + SESSION_LIFETIME_SECS;
        let session = sign_session(&SECRET, expiry);

        assert!(verify_session(&session, &SECRET, now));
        assert!(verify_session(&session, &SECRET, expiry - 1));
        assert!(!verify_session(&session, &SECRET, expiry));
        assert!(!verify_session(&session, &SECRET, expiry + 1));
    }

    #[test]
    fn rejects_tampered_sessions() {
        let now = 1_700_000_000;
        let session = sign_session(&SECRET, now + SESSION_LIFETIME_SECS);
        let mut tampered_signature = session.clone();
        let last = tampered_signature.pop().expect("session has a signature");
        tampered_signature.push(if last == '0' { '1' } else { '0' });

        assert!(!verify_session(&tampered_signature, &SECRET, now));
        assert!(!verify_session(
            &session.replace("v1.", "v2."),
            &SECRET,
            now
        ));
        assert!(!verify_session(&session, &[0xa5; 32], now));
    }
}
