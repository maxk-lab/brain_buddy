#![cfg(feature = "live")]
//! Black-box conformance for account management (spec 027, T103).
//!
//! These tests encode today's Python behavior for CAP-HTTP-025, CAP-HTTP-026,
//! CAP-HTTP-050..055 and CAP-MW-017. They talk HTTP only, seed owners through
//! the test control API and run unchanged against the Rust server later.

use bb_conformance::{Client, Config, Owner, Reply, TestControl, case};
use reqwest::Method;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const PASSWORD: &str = "Conformance-pass-1!";
const WRONG_PASSWORD: &str = "Not-the-password-1!";
const EXPECTED_OWNER: &str = "X-BrainBuddy-Expected-Owner";

fn env() -> Config {
    Config::from_env().expect("BB_CONFORMANCE_BASE_URL and BRAIN_BUDDY_TEST_CONTROL_TOKEN")
}

/// Process-unique synthetic address on `example.com`, for addresses the tests
/// change an owner to.
fn synthetic_email(prefix: &str) -> String {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{t:x}{n:x}@example.com")
}

/// Seeds one synthetic owner through the test control API.
async fn seed() -> Owner {
    let cfg = env();
    let control = TestControl::new(&cfg).expect("control");
    let client = Client::new(&cfg).expect("client");
    control.new_owner(&client).await.expect("synthetic owner")
}

/// The signed-in account's id, read through `GET /api/account`.
async fn account_id(client: &mut Client) -> String {
    let reply = client
        .send(Method::GET, "/api/account", None)
        .await
        .expect("account");
    assert_eq!(reply.status, 200, "GET /api/account: {}", reply.body);
    reply.json().expect("json")["id"]
        .as_str()
        .expect("id")
        .to_string()
}

/// A second session for the same owner, through the password login route.
async fn login(owner: &Owner, email: &str, password: &str) -> Client {
    let mut c = owner.client.fresh();
    let reply = c
        .post(
            "/api/auth/login",
            &json!({ "email": email, "password": password }),
        )
        .await
        .expect("login");
    assert_eq!(reply.status, 200, "login: {}", reply.body);
    c
}

/// Reauthenticates with the password and returns a single-use recent proof.
async fn recent_proof(client: &mut Client, id: &str, action: &str) -> String {
    let reply = client
        .post(
            "/api/auth/confirm/password",
            &json!({
                "current_password": PASSWORD,
                "action": action,
                "expected_account_id": id
            }),
        )
        .await
        .expect("confirm");
    assert_eq!(reply.status, 200, "confirm {action}: {}", reply.body);
    reply.json().expect("json")["recent_proof"]
        .as_str()
        .expect("recent_proof")
        .to_string()
}

fn assert_status(reply: &Reply, want: u16, what: &str) {
    assert_eq!(reply.status, want, "{what}: {}", reply.body);
}

fn assert_zip_attachment(reply: &Reply) {
    assert_status(reply, 200, "export");
    assert_eq!(reply.header("content-type"), Some("application/zip"));
    let disposition = reply.header("content-disposition").unwrap_or_default();
    assert!(
        disposition.starts_with("attachment;"),
        "disposition {disposition}"
    );
    assert!(reply.body.starts_with("PK"), "not a ZIP archive");
    // Entry names are stored uncompressed in the ZIP headers.
    assert!(reply.body.contains("account.json"), "account entry");
    assert!(
        reply.body.contains("auth/connected-methods.json"),
        "connected methods entry"
    );
}

fn assert_cleared_session_cookie(reply: &Reply) {
    let cleared = reply
        .headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| v.starts_with("brainbuddy_session=") && v.contains("Max-Age=0"));
    assert!(cleared, "session cookie is not cleared");
}

#[tokio::test]
async fn cap_http_050_get_account_returns_owner_fields() {
    case(
        &["CAP-HTTP-050"],
        "GET /api/account returns the owner profile, completed count and no pending deletion",
        async {
            let mut owner = seed().await;
            let reply = owner
                .client
                .send(Method::GET, "/api/account", None)
                .await
                .expect("account");
            assert_status(&reply, 200, "GET /api/account");
            let body = reply.json().expect("json");
            assert_eq!(body["email"], json!(owner.email));
            assert_eq!(body["display_name"], Value::Null);
            assert_eq!(body["completed_task_count"], json!(0));
            assert_eq!(body["deletion_requested_at"], Value::Null);
            assert_eq!(body["purge_at"], Value::Null);
            assert!(body["created_at"].is_string(), "created_at");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_050_get_account_requires_session() {
    case(
        &["CAP-HTTP-050"],
        "GET /api/account without a session is 401",
        async {
            let cfg = env();
            let mut anon = Client::new(&cfg).expect("client");
            let reply = anon
                .send(Method::GET, "/api/account", None)
                .await
                .expect("account");
            assert_status(&reply, 401, "anonymous GET /api/account");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_051_profile_patch_strips_whitespace_and_clears_on_empty() {
    case(
        &["CAP-HTTP-051"],
        "PATCH /api/account/profile strips whitespace, clears on empty and bounds length",
        async {
            let mut owner = seed().await;
            let reply = owner
                .client
                .send(
                    Method::PATCH,
                    "/api/account/profile",
                    Some(&json!({ "display_name": "  Ada Lovelace  " })),
                )
                .await
                .expect("patch");
            assert_status(&reply, 200, "PATCH profile");
            assert_eq!(
                reply.json().expect("json")["display_name"],
                json!("Ada Lovelace")
            );

            let reply = owner
                .client
                .send(Method::GET, "/api/account", None)
                .await
                .expect("account");
            assert_eq!(
                reply.json().expect("json")["display_name"],
                json!("Ada Lovelace")
            );

            let reply = owner
                .client
                .send(
                    Method::PATCH,
                    "/api/account/profile",
                    Some(&json!({ "display_name": "   " })),
                )
                .await
                .expect("patch");
            assert_status(&reply, 200, "PATCH empty profile");
            assert_eq!(reply.json().expect("json")["display_name"], Value::Null);

            let too_long = "x".repeat(65);
            let reply = owner
                .client
                .send(
                    Method::PATCH,
                    "/api/account/profile",
                    Some(&json!({ "display_name": too_long })),
                )
                .await
                .expect("patch");
            assert_status(&reply, 422, "PATCH overlong profile");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_052_email_change_rechecks_password_and_keeps_only_caller_session() {
    case(
        &["CAP-HTTP-052"],
        "POST /api/account/email re-checks the password and revokes other sessions",
        async {
            let mut owner = seed().await;
            let old_email = owner.email.clone();
            let mut other = login(&owner, &old_email, PASSWORD).await;
            let new_email = synthetic_email("moved");

            let reply = owner
                .client
                .post(
                    "/api/account/email",
                    &json!({ "new_email": new_email, "current_password": WRONG_PASSWORD }),
                )
                .await
                .expect("email");
            assert!(
                reply.status >= 400,
                "wrong password accepted: {}",
                reply.status
            );
            let reply = owner
                .client
                .send(Method::GET, "/api/account", None)
                .await
                .expect("account");
            assert_eq!(reply.json().expect("json")["email"], json!(old_email));

            let reply = owner
                .client
                .post(
                    "/api/account/email",
                    &json!({ "new_email": new_email, "current_password": PASSWORD }),
                )
                .await
                .expect("email");
            assert_status(&reply, 200, "POST email");
            assert_eq!(reply.json().expect("json")["email"], json!(new_email));

            let reply = other
                .send(Method::GET, "/api/account", None)
                .await
                .expect("account");
            assert_status(&reply, 401, "other session after email change");

            let reply = owner
                .client
                .send(Method::GET, "/api/account", None)
                .await
                .expect("account");
            assert_status(&reply, 200, "caller session after email change");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_052_sensitive_limiter_allows_ten_attempts_per_ten_minutes() {
    case(
        &["CAP-HTTP-052"],
        "POST /api/account/email is limited to 10 attempts per 10 minutes per user",
        async {
            let mut owner = seed().await;
            let body = json!({ "new_email": synthetic_email("moved"), "current_password": WRONG_PASSWORD });
            for attempt in 0..10 {
                let reply = owner
                    .client
                    .post("/api/account/email", &body)
                    .await
                    .expect("email");
                assert!(
                    reply.status != 429,
                    "attempt {attempt} was limited early"
                );
            }
            let reply = owner
                .client
                .post("/api/account/email", &body)
                .await
                .expect("email");
            assert_status(&reply, 429, "eleventh attempt");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_053_password_change_keeps_caller_and_revokes_others() {
    case(
        &["CAP-HTTP-053"],
        "POST /api/account/password is 204, keeps the caller's session and revokes others",
        async {
            let mut owner = seed().await;
            let email = owner.email.clone();
            let mut other = login(&owner, &email, PASSWORD).await;
            let new_password = "Conformance-pass-2!";

            let reply = owner
                .client
                .post(
                    "/api/account/password",
                    &json!({ "current_password": WRONG_PASSWORD, "new_password": new_password }),
                )
                .await
                .expect("password");
            assert!(
                reply.status >= 400,
                "wrong password accepted: {}",
                reply.status
            );

            let reply = owner
                .client
                .post(
                    "/api/account/password",
                    &json!({ "current_password": PASSWORD, "new_password": new_password }),
                )
                .await
                .expect("password");
            assert_status(&reply, 204, "POST password");
            assert!(reply.body.is_empty(), "204 must not carry a body");

            let reply = owner
                .client
                .send(Method::GET, "/api/account", None)
                .await
                .expect("account");
            assert_status(&reply, 200, "caller session after password change");

            let reply = other
                .send(Method::GET, "/api/account", None)
                .await
                .expect("account");
            assert_status(&reply, 401, "other session after password change");

            let mut relogin = owner.client.fresh();
            let reply = relogin
                .post(
                    "/api/auth/login",
                    &json!({ "email": email, "password": new_password }),
                )
                .await
                .expect("login");
            assert_status(&reply, 200, "login with new password");

            let mut stale = owner.client.fresh();
            let reply = stale
                .post(
                    "/api/auth/login",
                    &json!({ "email": email, "password": PASSWORD }),
                )
                .await
                .expect("login");
            assert!(reply.status >= 400, "old password still accepted");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_054_account_export_is_a_zip_attachment() {
    case(
        &["CAP-HTTP-054"],
        "GET /api/account/export returns the owner's ZIP as an attachment",
        async {
            let mut owner = seed().await;
            let reply = owner
                .client
                .send(Method::GET, "/api/account/export", None)
                .await
                .expect("export");
            assert_zip_attachment(&reply);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_054_account_export_requires_session() {
    case(
        &["CAP-HTTP-054"],
        "GET /api/account/export without a session is 401",
        async {
            let cfg = env();
            let mut anon = Client::new(&cfg).expect("client");
            let reply = anon
                .send(Method::GET, "/api/account/export", None)
                .await
                .expect("export");
            assert_status(&reply, 401, "anonymous export");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_025_auth_export_needs_a_single_use_recent_proof() {
    case(
        &["CAP-HTTP-025"],
        "POST /api/account/auth-export streams the ZIP once per recent proof",
        async {
            let mut owner = seed().await;
            let id = account_id(&mut owner.client).await;
            let proof = recent_proof(&mut owner.client, &id, "export").await;
            let body = json!({ "recent_proof": proof, "expected_account_id": id });

            let reply = owner
                .client
                .post("/api/account/auth-export", &body)
                .await
                .expect("auth-export");
            assert_zip_attachment(&reply);

            let reply = owner
                .client
                .post("/api/account/auth-export", &body)
                .await
                .expect("auth-export replay");
            assert!(reply.status >= 400, "replayed proof accepted");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_025_auth_export_rejects_cross_site_requests() {
    case(
        &["CAP-HTTP-025"],
        "POST /api/account/auth-export refuses a foreign Origin",
        async {
            let mut owner = seed().await;
            let id = account_id(&mut owner.client).await;
            let proof = recent_proof(&mut owner.client, &id, "export").await;
            let mut foreign = owner
                .client
                .clone()
                .with_header("Origin", "https://attacker.invalid");
            let reply = foreign
                .post(
                    "/api/account/auth-export",
                    &json!({ "recent_proof": proof, "expected_account_id": id }),
                )
                .await
                .expect("auth-export");
            assert_status(&reply, 403, "cross-origin auth-export");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_026_auth_delete_schedules_purge_and_clears_session() {
    case(
        &["CAP-HTTP-026"],
        "POST /api/account/auth-delete answers 202, clears the cookie and ends the session",
        async {
            let mut owner = seed().await;
            let id = account_id(&mut owner.client).await;
            let proof = recent_proof(&mut owner.client, &id, "delete").await;
            let reply = owner
                .client
                .post(
                    "/api/account/auth-delete",
                    &json!({ "recent_proof": proof, "expected_account_id": id }),
                )
                .await
                .expect("auth-delete");
            assert_status(&reply, 202, "POST auth-delete");
            let body = reply.json().expect("json");
            assert!(
                body["deletion_requested_at"].is_string(),
                "deletion_requested_at"
            );
            assert!(body["purge_at"].is_string(), "purge_at");
            assert_cleared_session_cookie(&reply);

            let reply = owner
                .client
                .send(Method::GET, "/api/account", None)
                .await
                .expect("account");
            assert_status(&reply, 401, "session after auth-delete");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_055_account_delete_rechecks_password_then_schedules_purge() {
    case(
        &["CAP-HTTP-055"],
        "POST /api/account/delete re-checks the password, answers 202 and revokes sessions",
        async {
            let mut owner = seed().await;
            let email = owner.email.clone();
            let mut other = login(&owner, &email, PASSWORD).await;

            let reply = owner
                .client
                .post(
                    "/api/account/delete",
                    &json!({ "current_password": WRONG_PASSWORD }),
                )
                .await
                .expect("delete");
            assert!(
                reply.status >= 400,
                "wrong password accepted: {}",
                reply.status
            );

            let reply = owner
                .client
                .post(
                    "/api/account/delete",
                    &json!({ "current_password": PASSWORD }),
                )
                .await
                .expect("delete");
            assert_status(&reply, 202, "POST delete");
            let body = reply.json().expect("json");
            let requested = body["deletion_requested_at"].as_str().expect("requested");
            let purge = body["purge_at"].as_str().expect("purge");
            assert!(requested < purge, "purge_at must follow the request");
            assert_cleared_session_cookie(&reply);

            let reply = other
                .send(Method::GET, "/api/account", None)
                .await
                .expect("account");
            assert_status(&reply, 401, "other session after delete");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_mw_017_expected_owner_mismatch_is_404_like_a_missing_account() {
    case(
        &["CAP-MW-017"],
        "X-BrainBuddy-Expected-Owner with a foreign id is 404, indistinguishable from a missing id",
        async {
            let mut owner = seed().await;
            let mut foreign_owner = seed().await;
            let foreign_id = account_id(&mut foreign_owner.client).await;
            let own_id = account_id(&mut owner.client).await;

            let mut bound = owner.client.clone().with_header(EXPECTED_OWNER, &own_id);
            let reply = bound
                .send(Method::GET, "/api/account", None)
                .await
                .expect("account");
            assert_status(&reply, 200, "matching expected owner");

            let mut foreign = owner
                .client
                .clone()
                .with_header(EXPECTED_OWNER, &foreign_id);
            let reply = foreign
                .send(Method::GET, "/api/account", None)
                .await
                .expect("account");
            assert_status(&reply, 404, "foreign expected owner");
            let foreign_body = reply.body.clone();

            let mut missing = owner
                .client
                .clone()
                .with_header(EXPECTED_OWNER, "no-such-owner-bb027");
            let reply = missing
                .send(Method::GET, "/api/account", None)
                .await
                .expect("account");
            assert_status(&reply, 404, "missing expected owner");
            // Each reply carries its own reference_id, so compare the rest.
            let envelope = |body: &str| {
                let v: Value = serde_json::from_str(body).expect("json");
                (v["message"].clone(), v["detail"].clone())
            };
            assert_eq!(
                envelope(&reply.body),
                envelope(&foreign_body),
                "404 bodies differ"
            );

            let mut patch = owner
                .client
                .clone()
                .with_header(EXPECTED_OWNER, &foreign_id);
            let reply = patch
                .send(
                    Method::PATCH,
                    "/api/account/profile",
                    Some(&json!({ "display_name": "Intruder" })),
                )
                .await
                .expect("patch");
            assert_status(&reply, 404, "foreign expected owner on PATCH");

            let reply = owner
                .client
                .send(Method::GET, "/api/account", None)
                .await
                .expect("account");
            assert_eq!(reply.json().expect("json")["display_name"], Value::Null);
        },
    )
    .await;
}
