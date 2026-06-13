use std::{
    io::Read,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    body::{Body, Bytes},
    extract::{ConnectInfo, Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::state::{AppState, AuthState};

const AUTH_COOKIE: &str = "blogger_session";
const PIN_TTL: Duration = Duration::from_secs(120);

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

fn fill_random(bytes: &mut [u8]) {
    match std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(bytes)) {
        Ok(()) => {}
        Err(_) => {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            for (i, byte) in bytes.iter_mut().enumerate() {
                *byte = ((nanos >> ((i % 16) * 8)) as u8) ^ (std::process::id() as u8) ^ (i as u8);
            }
        }
    }
}

fn random_hex<const N: usize>() -> String {
    let mut bytes = [0_u8; N];
    fill_random(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn generate_pin() -> String {
    let mut bytes = [0_u8; 4];
    fill_random(&mut bytes);
    let value = u32::from_ne_bytes(bytes) % 1_000_000;
    format!("{value:06}")
}

pub fn format_pin(pin: &str) -> String {
    pin.as_bytes()
        .chunks(2)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn new_auth_state() -> AuthState {
    AuthState {
        pin: generate_pin(),
        pin_expires_at: Instant::now() + PIN_TTL,
        session_token: random_hex::<32>(),
    }
}

pub fn pin_seconds_remaining(auth: &AuthState) -> u64 {
    auth.pin_expires_at
        .saturating_duration_since(Instant::now())
        .as_secs()
}

fn cookie_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    let value = headers.get(header::COOKIE)?.to_str().ok()?;
    value.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == AUTH_COOKIE).then_some(value)
    })
}

fn bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ")
}

fn is_localhost_addr(addr: SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_loopback(),
    }
}

fn has_session(headers: &axum::http::HeaderMap, session_token: &str) -> bool {
    cookie_token(headers).is_some_and(|token| token_eq(token, session_token))
        || bearer_token(headers).is_some_and(|token| token_eq(token, session_token))
}

fn pin_form(message: &str, status: StatusCode) -> Response {
    let html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Blogger PIN</title>
  <style>
    :root {{ color-scheme: light dark; font-family: system-ui, sans-serif; }}
    body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; background: #111827; color: #f9fafb; }}
    main {{ width: min(24rem, calc(100vw - 2rem)); }}
    label {{ display: block; margin-bottom: .5rem; font-weight: 650; }}
    input {{ box-sizing: border-box; width: 100%; padding: .75rem; font-size: 1.25rem; letter-spacing: .2rem; }}
    button {{ margin-top: .75rem; width: 100%; padding: .75rem; font: inherit; font-weight: 650; cursor: pointer; }}
    p {{ color: #d1d5db; line-height: 1.5; }}
    strong {{ color: #fff; font-size: 1.1rem; }}
    .status {{ min-height: 1.5rem; }}
  </style>
</head>
<body>
  <main>
    <h1>Blogger</h1>
    <p class="status" aria-live="polite">{message}</p>
    <p>The terminal PIN is shown in bold as <strong>12 34 56</strong>.</p>
    <form method="post" action="/auth/pin">
      <label for="pin">PIN</label>
      <input id="pin" name="pin" inputmode="numeric" autocomplete="one-time-code" maxlength="8" placeholder="12 34 56" autofocus required>
      <button type="submit">Continue</button>
    </form>
  </main>
</body>
</html>"#
    );

    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

fn pin_prompt(expired: bool) -> Response {
    if expired {
        pin_form(
            "The startup PIN has expired. Restart Blogger to generate a new PIN.",
            StatusCode::FORBIDDEN,
        )
    } else {
        pin_form(
            "Enter the PIN shown in the Blogger terminal.",
            StatusCode::OK,
        )
    }
}

fn unauthorized(request: &Request<Body>, expired: bool) -> Response {
    if request.method() == axum::http::Method::GET {
        return pin_prompt(expired);
    }

    (
        StatusCode::UNAUTHORIZED,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "Unauthorized. Open the Blogger UI in a browser and enter the startup PIN.",
    )
        .into_response()
}

fn parse_pin(body: &[u8]) -> Option<String> {
    std::str::from_utf8(body).ok()?.split('&').find_map(|part| {
        let (name, value) = part.split_once('=')?;
        (name == "pin").then(|| {
            value
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect::<String>()
        })
    })
}

fn session_cookie(session_token: &str) -> String {
    format!("{AUTH_COOKIE}={session_token}; HttpOnly; SameSite=Lax; Path=/; Max-Age=2592000")
}

fn pin_accepted(session_token: &str) -> Response {
    let html = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Blogger PIN Accepted</title>
  <meta http-equiv="refresh" content="1; url=/">
  <style>
    :root { color-scheme: light dark; font-family: system-ui, sans-serif; }
    body { margin: 0; min-height: 100vh; display: grid; place-items: center; background: #111827; color: #f9fafb; }
    main { width: min(24rem, calc(100vw - 2rem)); }
    p { color: #d1d5db; line-height: 1.5; }
    a { color: #93c5fd; }
  </style>
  <script>setTimeout(() => location.replace("/"), 250);</script>
</head>
<body>
  <main>
    <h1>PIN accepted</h1>
    <p>Your browser is now authorized. Opening Blogger...</p>
    <p><a href="/">Continue</a></p>
  </main>
</body>
</html>"#;

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::SET_COOKIE, session_cookie(session_token).as_str()),
        ],
        html,
    )
        .into_response()
}

pub async fn submit_pin(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    let pin_expired = pin_seconds_remaining(&state.auth) == 0;
    let pin_is_valid =
        !pin_expired && parse_pin(&body).is_some_and(|pin| token_eq(&pin, &state.auth.pin));

    if pin_expired {
        eprintln!("remote PIN attempt rejected: startup PIN expired");
        return pin_prompt(true);
    }

    if !pin_is_valid {
        eprintln!("remote PIN attempt rejected: incorrect PIN");
        return pin_form(
            "That PIN did not match. Check the terminal and try again.",
            StatusCode::UNAUTHORIZED,
        );
    }

    println!("remote PIN accepted; session cookie issued");
    pin_accepted(&state.auth.session_token)
}

pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if path == "/auth/pin" || path == "/api/health" {
        return next.run(request).await;
    }

    let is_localhost = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|ConnectInfo(addr)| is_localhost_addr(*addr));

    if is_localhost || has_session(request.headers(), &state.auth.session_token) {
        return next.run(request).await;
    }

    unauthorized(&request, pin_seconds_remaining(&state.auth) == 0)
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

    use super::{format_pin, is_localhost_addr, parse_pin, token_eq};

    #[test]
    fn parses_pin_form_body() {
        assert_eq!(parse_pin(b"pin=123456"), Some("123456".to_string()));
        assert_eq!(parse_pin(b"pin=12+34+56"), Some("123456".to_string()));
    }

    #[test]
    fn formats_pin_for_display() {
        assert_eq!(format_pin("123456"), "12 34 56");
    }

    #[test]
    fn compares_tokens() {
        assert!(token_eq("abc123", "abc123"));
        assert!(!token_eq("abc123", "abc124"));
        assert!(!token_eq("abc123", "abc1234"));
    }

    #[test]
    fn detects_loopback_addresses() {
        assert!(is_localhost_addr(SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            1
        ))));
        assert!(is_localhost_addr(SocketAddr::from((
            Ipv6Addr::LOCALHOST,
            1
        ))));
        assert!(!is_localhost_addr(SocketAddr::from(([192, 168, 1, 10], 1))));
    }
}
