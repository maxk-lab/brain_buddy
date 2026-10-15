#![cfg(feature = "live")]
//! Black-box conformance for CLI device authorization (CAP-HTTP-030..033).
//!
//! Every `cap_http_NNN_*` test proves its row against `BB_CONFORMANCE_BASE_URL`.
//! The server must run with `BRAIN_BUDDY_CLI_VERIFICATION_ORIGIN` equal to
//! `BB_CONFORMANCE_CLI_ORIGIN` (default `http://127.0.0.1:5536`).
//!
//! `start` shares a per-IP budget of 10 calls per 60 s, so `pool()` mints the
//! grants once per run and the tests consume them. Run against a freshly
//! started server, or wait 60 s between runs.

use bb_conformance::{Config, TestControl, case};
use reqwest::header::HeaderMap;
use serde_json::{Value, json};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex as AsyncMutex, OnceCell};

const PASSWORD: &str = "Conformance-pass-1!";
const COOKIE_NAME: &str = "brainbuddy_session";
const DEFAULT_ORIGIN: &str = "http://127.0.0.1:5536";
const FOREIGN_ORIGIN: &str = "http://evil.example";
const ALPHABET: &str = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
/// The server's poll interval is 5 s; past this margin a poll is no longer slowed.
const POLL_WINDOW: Duration = Duration::from_millis(5_200);

/// Serializes tests so the shared report file and the start budget stay coherent.
static GATE: AsyncMutex<()> = AsyncMutex::const_new(());
static POOL: OnceCell<Pool> = OnceCell::const_new();

/// One HTTP reply, kept whole so tests can read headers and the error envelope.
#[derive(Clone)]
struct Resp {
    status: u16,
    headers: HeaderMap,
    body: String,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    fn code(&self) -> String {
        self.json()["detail"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)?.to_str().ok()
    }
}

/// Raw HTTP so tests can send exact content types, sizes and cookies.
struct Http {
    base: String,
    origin: String,
    http: reqwest::Client,
}

impl Http {
    fn new() -> Self {
        let cfg = Config::from_env().expect("BB_CONFORMANCE_BASE_URL is set");
        let origin = std::env::var("BB_CONFORMANCE_CLI_ORIGIN")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_ORIGIN.into());
        Self {
            base: cfg.base_url.trim_end_matches('/').to_string(),
            origin,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("http client"),
        }
    }

    async fn raw(
        &self,
        path: &str,
        content_type: &str,
        cookie: Option<&str>,
        origin: Option<&str>,
        body: Vec<u8>,
    ) -> Resp {
        let mut req = self
            .http
            .post(format!("{}{path}", self.base))
            .header("Content-Type", content_type)
            .body(body);
        if let Some(cookie) = cookie {
            req = req.header("Cookie", cookie);
        }
        if let Some(origin) = origin {
            req = req.header("Origin", origin);
        }
        let resp = req.send().await.expect("request sent");
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let body = resp.text().await.expect("body read");
        Resp {
            status,
            headers,
            body,
        }
    }

    async fn json(
        &self,
        path: &str,
        cookie: Option<&str>,
        origin: Option<&str>,
        body: &Value,
    ) -> Resp {
        let bytes = serde_json::to_vec(body).expect("json body");
        self.raw(path, "application/json", cookie, origin, bytes)
            .await
    }

    /// Browser-side call: the session cookie plus the CLI verification origin.
    async fn browser(&self, path: &str, cookie: Option<&str>, body: &Value) -> Resp {
        self.json(path, cookie, Some(&self.origin), body).await
    }

    async fn request_code(&self, cookie: Option<&str>, user_code: &str) -> Resp {
        self.browser(
            "/api/auth/device/request",
            cookie,
            &json!({ "user_code": user_code }),
        )
        .await
    }

    async fn decide(
        &self,
        cookie: Option<&str>,
        user_code: &str,
        decision: &str,
        expected_owner: &str,
    ) -> Resp {
        self.browser(
            "/api/auth/device/decision",
            cookie,
            &json!({
                "user_code": user_code,
                "decision": decision,
                "expected_owner": expected_owner,
            }),
        )
        .await
    }

    async fn token(&self, device_code: &str) -> Resp {
        self.json(
            "/api/auth/device/token",
            None,
            None,
            &json!({ "device_code": device_code }),
        )
        .await
    }
}

/// A signed-in synthetic owner with its session cookie.
struct Owner {
    id: String,
    email: String,
    cookie: String,
}

/// A pending grant minted by `start`, with the time the reply arrived.
struct Grant {
    device_code: String,
    user_code: String,
    received: Instant,
}

/// The run's grants and the replies that prove the start contract.
struct Pool {
    grants: Mutex<Vec<Grant>>,
    first_start: Resp,
    rejected: Resp,
}

fn unique() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{t:x}{:x}", N.fetch_add(1, Ordering::Relaxed))
}

/// Enables the `cli_auth` flag for everyone. Idempotent; never switched off,
/// because the other tests share the server.
async fn setup() -> (Http, TestControl) {
    let cfg = Config::from_env().expect("BB_CONFORMANCE_BASE_URL is set");
    let control = TestControl::new(&cfg).expect("test control client");
    control
        .flag("cli_auth", "on", &[])
        .await
        .expect("enable cli_auth");
    (Http::new(), control)
}

async fn owner(http: &Http, control: &TestControl) -> Owner {
    let invites = control.invites(1).await.expect("invite minted");
    let code = invites["codes"][0]
        .as_str()
        .expect("invite code")
        .to_string();
    let email = format!("cli-{}@example.com", unique());
    let body = json!({ "email": email, "password": PASSWORD, "invite_code": code });
    let reply = http.json("/api/auth/signup", None, None, &body).await;
    assert_eq!(reply.status, 201, "signup answered: {}", reply.body);
    let cookie = reply
        .header("set-cookie")
        .and_then(|raw| raw.split(';').next())
        .expect("signup sets the session cookie")
        .to_string();
    let id = reply.json()["id"]
        .as_str()
        .expect("signup returns the account id")
        .to_string();
    Owner { id, email, cookie }
}

async fn pool() -> &'static Pool {
    POOL.get_or_init(|| async {
        let (http, _) = setup().await;
        let mut grants = Vec::new();
        let mut first_start = None;
        for _ in 0..10 {
            let reply = http
                .json("/api/auth/device/start", None, None, &json!({}))
                .await;
            assert_eq!(reply.status, 200, "start answered: {}", reply.body);
            let body = reply.json();
            grants.push(Grant {
                device_code: body["device_code"].as_str().expect("device_code").into(),
                user_code: body["user_code"].as_str().expect("user_code").into(),
                received: Instant::now(),
            });
            first_start.get_or_insert(reply);
        }
        // The eleventh call inside the same window must be refused.
        let rejected = http
            .json("/api/auth/device/start", None, None, &json!({}))
            .await;
        Pool {
            grants: Mutex::new(grants),
            first_start: first_start.expect("first start reply"),
            rejected,
        }
    })
    .await
}

fn take(pool: &Pool) -> Grant {
    pool.grants
        .lock()
        .expect("grant pool lock")
        .pop()
        .expect("a pool grant is left")
}

async fn wait_for_poll_window(grant: &Grant) {
    let until = tokio::time::Instant::from_std(grant.received + POLL_WINDOW);
    tokio::time::sleep_until(until).await;
}

fn device_code_shape(s: &str) -> bool {
    s.len() == 43
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn user_code_shape(s: &str) -> bool {
    s.len() == 9
        && s.bytes().enumerate().all(|(i, b)| {
            if i == 4 {
                b == b'-'
            } else {
                ALPHABET.as_bytes().contains(&b)
            }
        })
}

fn set_cookie_values(reply: &Resp) -> Vec<String> {
    reply
        .headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn cap_http_030_start_issues_device_grant() {
    let _gate = GATE.lock().await;
    case(
        &["CAP-HTTP-030"],
        "cap_http_030_start_issues_device_grant",
        async {
            let pool = pool().await;
            let reply = &pool.first_start;
            assert_eq!(reply.status, 200);
            let body = reply.json();
            let device = body["device_code"].as_str().expect("device_code");
            let user = body["user_code"].as_str().expect("user_code");
            assert!(
                device_code_shape(device),
                "device_code is 43 url-safe chars"
            );
            assert!(user_code_shape(user), "user_code is XXXX-XXXX");
            assert_eq!(body["expires_in"], 600);
            assert_eq!(body["interval"], 5);
            assert_eq!(body["protocol_version"], 1);
            let uri = body["verification_uri"].as_str().expect("verification_uri");
            assert!(uri.ends_with("/cli/authorize"));
            assert_eq!(
                body["verification_uri_complete"].as_str(),
                Some(format!("{uri}#user_code={user}").as_str())
            );
            assert_eq!(reply.header("cache-control"), Some("no-store"));
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_030_start_rejects_bad_bodies_before_admission() {
    let _gate = GATE.lock().await;
    case(
        &["CAP-HTTP-030"],
        "cap_http_030_start_rejects_bad_bodies",
        async {
            let (http, _) = setup().await;
            let oversized = format!("{{\"pad\":\"{}\"}}", "a".repeat(1024));
            let probes: [(&str, &str, Vec<u8>, u16); 4] = [
                ("wrong content type", "text/plain", b"{}".to_vec(), 415),
                (
                    "oversized body",
                    "application/json",
                    oversized.into_bytes(),
                    413,
                ),
                ("invalid JSON", "application/json", b"{".to_vec(), 422),
                (
                    "unknown field",
                    "application/json",
                    br#"{"unexpected":true}"#.to_vec(),
                    422,
                ),
            ];
            for (name, content_type, body, status) in probes {
                let reply = http
                    .raw("/api/auth/device/start", content_type, None, None, body)
                    .await;
                assert_eq!(reply.status, status, "{name}: {}", reply.body);
                assert_eq!(reply.code(), "invalid_request", "{name}");
            }
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_030_start_rate_limit_per_ip() {
    let _gate = GATE.lock().await;
    case(
        &["CAP-HTTP-030"],
        "cap_http_030_start_rate_limit_per_ip",
        async {
            let rejected = &pool().await.rejected;
            assert_eq!(rejected.status, 429, "{}", rejected.body);
            assert_eq!(rejected.code(), "rate_limited");
            assert_eq!(rejected.header("retry-after"), Some("60"));
            assert_eq!(rejected.header("cache-control"), Some("no-store"));
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_031_request_looks_up_pending_grant() {
    let _gate = GATE.lock().await;
    case(
        &["CAP-HTTP-031"],
        "cap_http_031_request_looks_up_pending_grant",
        async {
            let (http, control) = setup().await;
            let owner = owner(&http, &control).await;
            let grant = take(pool().await);
            let reply = http
                .request_code(Some(&owner.cookie), &grant.user_code)
                .await;
            assert_eq!(reply.status, 200, "{}", reply.body);
            let body = reply.json();
            assert_eq!(body["user_code"].as_str(), Some(grant.user_code.as_str()));
            assert_eq!(body["client_name"], "BrainBuddy CLI");
            assert_eq!(body["state"], "pending");
            assert!(body["created_at"].is_string());
            assert!(body["expires_at"].is_string());
            assert_eq!(body["account"]["id"].as_str(), Some(owner.id.as_str()));
            assert_eq!(
                body["account"]["email"].as_str(),
                Some(owner.email.as_str())
            );
            // The dashless form names the same grant.
            let bare = grant.user_code.replace('-', "");
            let again = http.request_code(Some(&owner.cookie), &bare).await;
            assert_eq!(again.status, 200, "{}", again.body);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_031_request_refuses_bad_origin_session_and_body() {
    let _gate = GATE.lock().await;
    case(
        &["CAP-HTTP-031"],
        "cap_http_031_request_refuses_bad_origin_session_and_body",
        async {
            let (http, control) = setup().await;
            let owner = owner(&http, &control).await;
            let grant = take(pool().await);
            let path = "/api/auth/device/request";

            let foreign = http
                .json(
                    path,
                    Some(&owner.cookie),
                    Some(FOREIGN_ORIGIN),
                    &json!({ "user_code": grant.user_code }),
                )
                .await;
            assert_eq!(foreign.status, 403, "{}", foreign.body);
            assert_eq!(foreign.code(), "invalid_origin");

            let no_session = http.request_code(None, &grant.user_code).await;
            assert_eq!(no_session.status, 404, "{}", no_session.body);
            assert_eq!(no_session.code(), "cli_auth_unavailable");

            let unknown = http.request_code(Some(&owner.cookie), "ZZZZ-ZZZZ").await;
            assert_eq!(unknown.status, 404, "{}", unknown.body);
            assert_eq!(unknown.code(), "cli_auth_unavailable");

            let text = http
                .raw(
                    path,
                    "text/plain",
                    Some(&owner.cookie),
                    Some(&http.origin),
                    b"{}".to_vec(),
                )
                .await;
            assert_eq!(text.status, 415, "{}", text.body);

            let malformed = http
                .browser(path, Some(&owner.cookie), &json!({ "user_code": "ABCD" }))
                .await;
            assert_eq!(malformed.status, 422, "{}", malformed.body);
            assert_eq!(malformed.code(), "invalid_request");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_032_decision_approves_then_conflicts() {
    let _gate = GATE.lock().await;
    case(
        &["CAP-HTTP-032"],
        "cap_http_032_decision_approves_then_conflicts",
        async {
            let (http, control) = setup().await;
            let owner = owner(&http, &control).await;
            let grant = take(pool().await);
            let cookie = Some(owner.cookie.as_str());

            let approve = http
                .decide(cookie, &grant.user_code, "approve", &owner.id)
                .await;
            assert_eq!(approve.status, 200, "{}", approve.body);
            assert_eq!(approve.json()["state"], "approved");

            let repeat = http
                .decide(cookie, &grant.user_code, "approve", &owner.id)
                .await;
            assert_eq!(repeat.status, 200, "repeat approve is idempotent");

            let flip = http
                .decide(cookie, &grant.user_code, "deny", &owner.id)
                .await;
            assert_eq!(flip.status, 409, "{}", flip.body);
            assert_eq!(flip.code(), "decision_conflict");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_032_decision_hides_foreign_owner_like_missing_code() {
    let _gate = GATE.lock().await;
    case(
        &["CAP-HTTP-032"],
        "cap_http_032_decision_hides_foreign_owner",
        async {
            let (http, control) = setup().await;
            let alice = owner(&http, &control).await;
            let bob = owner(&http, &control).await;
            let grant = take(pool().await);
            let bob_cookie = Some(bob.cookie.as_str());

            // Bob names Alice as the expected owner of a code only he is signed into.
            let foreign = http
                .decide(bob_cookie, &grant.user_code, "approve", &alice.id)
                .await;
            let missing = http
                .decide(bob_cookie, "ZZZZ-ZZZZ", "approve", &bob.id)
                .await;
            for reply in [&foreign, &missing] {
                assert_eq!(reply.status, 404, "{}", reply.body);
                assert_eq!(reply.code(), "cli_auth_unavailable");
            }
            assert_eq!(foreign.json()["message"], missing.json()["message"]);

            // The refused attempt changed nothing: the grant is still pending.
            let lookup = http
                .request_code(Some(&alice.cookie), &grant.user_code)
                .await;
            assert_eq!(lookup.json()["state"], "pending");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_032_decision_denies_and_refuses_bad_input() {
    let _gate = GATE.lock().await;
    case(
        &["CAP-HTTP-032"],
        "cap_http_032_decision_denies_and_refuses_bad_input",
        async {
            let (http, control) = setup().await;
            let owner = owner(&http, &control).await;
            let grant = take(pool().await);
            let cookie = Some(owner.cookie.as_str());
            let path = "/api/auth/device/decision";

            let foreign = http
                .json(
                    path,
                    cookie,
                    Some(FOREIGN_ORIGIN),
                    &json!({
                        "user_code": grant.user_code,
                        "decision": "approve",
                        "expected_owner": owner.id,
                    }),
                )
                .await;
            assert_eq!(foreign.status, 403, "{}", foreign.body);
            assert_eq!(foreign.code(), "invalid_origin");

            let bad = http
                .browser(
                    path,
                    cookie,
                    &json!({
                        "user_code": grant.user_code,
                        "decision": "maybe",
                        "expected_owner": owner.id,
                    }),
                )
                .await;
            assert_eq!(bad.status, 422, "{}", bad.body);

            let deny = http
                .decide(cookie, &grant.user_code, "deny", &owner.id)
                .await;
            assert_eq!(deny.status, 200, "{}", deny.body);
            assert_eq!(deny.json()["state"], "denied");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_033_token_exchanges_approved_grant_once() {
    let _gate = GATE.lock().await;
    case(
        &["CAP-HTTP-033"],
        "cap_http_033_token_exchanges_approved_grant_once",
        async {
            let (http, control) = setup().await;
            let owner = owner(&http, &control).await;
            let grant = take(pool().await);
            let approve = http
                .decide(Some(&owner.cookie), &grant.user_code, "approve", &owner.id)
                .await;
            assert_eq!(approve.status, 200, "{}", approve.body);

            wait_for_poll_window(&grant).await;
            let reply = http.token(&grant.device_code).await;
            assert_eq!(reply.status, 200, "{}", reply.body);
            let body = reply.json();
            assert_eq!(body["account"]["id"].as_str(), Some(owner.id.as_str()));
            assert_eq!(
                body["account"]["email"].as_str(),
                Some(owner.email.as_str())
            );
            assert_eq!(body["credential_type"], "session_cookie");
            assert_eq!(body["cookie_name"], COOKIE_NAME);
            assert!(body["expires_at"].is_string());
            assert_eq!(reply.header("cache-control"), Some("no-store"));
            let prefix = format!("{COOKIE_NAME}=");
            let cookies = set_cookie_values(&reply);
            assert!(
                cookies
                    .iter()
                    .any(|c| c.starts_with(&prefix) && c.contains("HttpOnly")),
                "token sets the session cookie: {cookies:?}"
            );

            let again = http.token(&grant.device_code).await;
            assert_eq!(again.status, 409, "{}", again.body);
            assert_eq!(again.code(), "authorization_consumed");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_033_token_pending_then_slow_down() {
    let _gate = GATE.lock().await;
    case(
        &["CAP-HTTP-033"],
        "cap_http_033_token_pending_then_slow_down",
        async {
            let (http, _) = setup().await;
            let grant = take(pool().await);

            wait_for_poll_window(&grant).await;
            let pending = http.token(&grant.device_code).await;
            assert_eq!(pending.status, 400, "{}", pending.body);
            assert_eq!(pending.code(), "authorization_pending");
            assert_eq!(pending.header("retry-after"), Some("5"));

            let hurried = http.token(&grant.device_code).await;
            assert_eq!(hurried.status, 400, "{}", hurried.body);
            assert_eq!(hurried.code(), "slow_down");
            assert_eq!(hurried.header("retry-after"), Some("10"));
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_033_token_denied_unknown_and_malformed() {
    let _gate = GATE.lock().await;
    case(
        &["CAP-HTTP-033"],
        "cap_http_033_token_denied_unknown_and_malformed",
        async {
            let (http, control) = setup().await;
            let owner = owner(&http, &control).await;
            let grant = take(pool().await);
            let deny = http
                .decide(Some(&owner.cookie), &grant.user_code, "deny", &owner.id)
                .await;
            assert_eq!(deny.status, 200, "{}", deny.body);

            let denied = http.token(&grant.device_code).await;
            assert_eq!(denied.status, 403, "{}", denied.body);
            assert_eq!(denied.code(), "authorization_denied");

            let unknown = "A".repeat(43);
            let missing = http.token(&unknown).await;
            assert_eq!(missing.status, 400, "{}", missing.body);
            assert_eq!(missing.code(), "invalid_device_code");

            let short = http
                .json(
                    "/api/auth/device/token",
                    None,
                    None,
                    &json!({ "device_code": "short" }),
                )
                .await;
            assert_eq!(short.status, 422, "{}", short.body);

            let text = http
                .raw(
                    "/api/auth/device/token",
                    "text/plain",
                    None,
                    None,
                    b"{}".to_vec(),
                )
                .await;
            assert_eq!(text.status, 415, "{}", text.body);
        },
    )
    .await;
}
