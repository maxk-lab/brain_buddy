#![cfg(feature = "live")]
//! Black-box conformance for title completion and the Review navigator
//! (CAP-HTTP-110..112, CAP-HTTP-157..160; 027-FR-001, 027-FR-011, 027-FR-008).
//!
//! Each test names its capability id (`cap_http_<nnn>_*`) and is recorded in
//! the CAP-keyed run report. They encode the frozen Python server's behavior
//! and need `BRAIN_BUDDY_ENV=test` plus `BRAIN_BUDDY_REVIEW_NAVIGATOR_PROVIDER=
//! deterministic` on the server under test (title completion is deterministic
//! in the test environment by default).
//!
//! Not covered through HTTP, because test control cannot script these two
//! providers (its `ai` kind drives validation only): the provider call ledger
//! for title completion and the navigator, cost caps, and the discard of a
//! result that returns after consent is revoked.

use bb_conformance::{Client, Config, Owner, Reply, TestControl, case};
use reqwest::Method;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

const TITLE_PROVIDER: &str = "deterministic";
const NAVIGATOR_PROVIDER: &str = "openai";
const CONSENT_TEXT_VERSION: u64 = 1;

struct Env {
    ctl: TestControl,
    anon: Client,
}

async fn env() -> Env {
    let cfg = Config::from_env().expect("BB_CONFORMANCE_BASE_URL");
    let ctl = TestControl::new(&cfg).expect("test control");
    let anon = Client::new(&cfg).expect("client");
    // Both exposure flags are idempotent global switches (mode "on").
    for key in ["task_title_autocomplete", "weekly_review"] {
        ctl.flag(key, "on", &[]).await.expect("flag");
    }
    Env { ctl, anon }
}

fn unique() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{t:x}{:x}", N.fetch_add(1, Ordering::Relaxed))
}

/// A new synthetic owner (an `example.com` address) from the harness factory.
async fn owner(env: &Env) -> Owner {
    env.ctl.new_owner(&env.anon).await.expect("synthetic owner")
}

async fn call(c: &Client, method: Method, path: &str, body: Option<Value>) -> Reply {
    let mut c = c.clone();
    c.send(method, &format!("/api{path}"), body.as_ref())
        .await
        .expect("request")
}

async fn keyed(c: &Client, method: Method, path: &str, key: &str, body: Option<Value>) -> Reply {
    let mut c = c.clone().with_header("Idempotency-Key", key);
    c.send(method, &format!("/api{path}"), body.as_ref())
        .await
        .expect("request")
}

fn json_of(reply: &Reply) -> Value {
    reply
        .json()
        .unwrap_or_else(|e| panic!("json body {:?}: {e}", reply.body))
}

fn reason(reply: &Reply) -> String {
    json_of(reply)["detail"]["reason"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn title_body(draft: &str) -> Value {
    json!({
        "draft": draft,
        "consent": { "external_processing_allowed": true, "provider": TITLE_PROVIDER }
    })
}

fn nav_consent(allowed: bool) -> Value {
    json!({ "external_processing_allowed": allowed, "provider": NAVIGATOR_PROVIDER })
}

fn nav_request(title: &str) -> Value {
    json!({ "kind": "first_step", "consent": nav_consent(true), "task": { "title": title } })
}

async fn grant(c: &Client) -> Reply {
    let body =
        json!({ "provider": NAVIGATOR_PROVIDER, "consent_text_version": CONSENT_TEXT_VERSION });
    keyed(
        c,
        Method::POST,
        "/review/navigator/consent",
        &format!("grant-{}", unique()),
        Some(body),
    )
    .await
}

async fn revoke(c: &Client) -> Reply {
    keyed(
        c,
        Method::DELETE,
        "/review/navigator/consent",
        &format!("revoke-{}", unique()),
        None,
    )
    .await
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36 && s.matches('-').count() == 4
}

// ---------------------------------------------------------------- CAP-HTTP-110

#[tokio::test]
async fn cap_http_110_reports_the_provider_category_to_a_session_only() {
    case(
        &["CAP-HTTP-110"],
        "title completion provider category",
        async {
            let env = env().await;
            let anon = call(
                &env.anon,
                Method::GET,
                "/tasks/title-completion-provider",
                None,
            )
            .await;
            assert_eq!(anon.status, 401);
            let o = owner(&env).await;
            let got = call(
                &o.client,
                Method::GET,
                "/tasks/title-completion-provider",
                None,
            )
            .await;
            assert_eq!(got.status, 200, "{}", got.body);
            assert_eq!(json_of(&got), json!({ "provider": TITLE_PROVIDER }));
        },
    )
    .await;
}

// ---------------------------------------------------------------- CAP-HTTP-111

#[tokio::test]
async fn cap_http_111_returns_three_candidates_and_a_request_id() {
    case(&["CAP-HTTP-111"], "title completion candidates", async {
        let env = env().await;
        let o = owner(&env).await;
        let reply = call(
            &o.client,
            Method::POST,
            "/tasks/title-completions",
            Some(title_body("Write the quarterly report")),
        )
        .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        let body = json_of(&reply);
        assert!(is_uuid(body["request_id"].as_str().unwrap_or("")));
        assert_eq!(body["candidates"].as_array().map(Vec::len), Some(3));
    })
    .await;
}

#[tokio::test]
async fn cap_http_111_requires_a_session() {
    case(
        &["CAP-HTTP-111"],
        "title completion needs a session",
        async {
            let env = env().await;
            let reply = call(
                &env.anon,
                Method::POST,
                "/tasks/title-completions",
                Some(title_body("Write the quarterly report")),
            )
            .await;
            assert_eq!(reply.status, 401);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_111_consent_mismatch_is_400() {
    case(
        &["CAP-HTTP-111"],
        "title completion consent mismatch",
        async {
            let env = env().await;
            let o = owner(&env).await;
            let declined = json!({
                "draft": "Write the quarterly report",
                "consent": { "external_processing_allowed": false, "provider": TITLE_PROVIDER }
            });
            let other = json!({
                "draft": "Write the quarterly report",
                "consent": { "external_processing_allowed": true, "provider": "openai" }
            });
            for body in [declined, other] {
                let reply = call(
                    &o.client,
                    Method::POST,
                    "/tasks/title-completions",
                    Some(body),
                )
                .await;
                assert_eq!(reply.status, 400, "{}", reply.body);
                assert_eq!(
                    json_of(&reply)["message"],
                    "Valid provider consent is required"
                );
            }
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_111_rejects_a_draft_that_is_blank_or_too_short() {
    case(
        &["CAP-HTTP-111"],
        "title completion draft validation",
        async {
            let env = env().await;
            let o = owner(&env).await;
            let blank = call(
                &o.client,
                Method::POST,
                "/tasks/title-completions",
                Some(title_body("   ")),
            )
            .await;
            assert_eq!(blank.status, 422, "{}", blank.body);
            let short = call(
                &o.client,
                Method::POST,
                "/tasks/title-completions",
                Some(title_body("Write")),
            )
            .await;
            assert_eq!(short.status, 400, "{}", short.body);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_111_foreign_project_is_indistinguishable_from_missing() {
    case(
        &["CAP-HTTP-111"],
        "title completion project owner scope",
        async {
            let env = env().await;
            let (a, b) = (owner(&env).await, owner(&env).await);
            let made = keyed(
                &a.client,
                Method::POST,
                "/projects",
                &format!("project-{}", unique()),
                Some(json!({ "name": "Owner A private project" })),
            )
            .await;
            assert_eq!(made.status, 201, "{}", made.body);
            let id = json_of(&made)["id"]
                .as_str()
                .expect("project id")
                .to_string();
            let ask = |project: &str| {
                let mut body = title_body("Plan");
                body["project_id"] = json!(project);
                body
            };
            let own = call(
                &a.client,
                Method::POST,
                "/tasks/title-completions",
                Some(ask(&id)),
            )
            .await;
            assert_eq!(own.status, 200, "{}", own.body);
            let foreign = call(
                &b.client,
                Method::POST,
                "/tasks/title-completions",
                Some(ask(&id)),
            )
            .await;
            let missing = call(
                &b.client,
                Method::POST,
                "/tasks/title-completions",
                Some(ask("project_missing")),
            )
            .await;
            assert_eq!((foreign.status, missing.status), (404, 404));
            let shape = |r: &Reply, id: &str| {
                let body = json_of(r);
                (
                    body["message"].as_str().unwrap_or("").replace(id, "ID"),
                    body["detail"]["resource"].clone(),
                )
            };
            assert_eq!(shape(&foreign, &id), shape(&missing, "project_missing"));
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_111_limits_an_owner_to_20_requests_per_minute() {
    case(&["CAP-HTTP-111"], "title completion rate limit", async {
        let env = env().await;
        let o = owner(&env).await;
        let other = owner(&env).await;
        for n in 1..=20 {
            let reply = call(
                &o.client,
                Method::POST,
                "/tasks/title-completions",
                Some(title_body("Write the quarterly report")),
            )
            .await;
            assert_eq!(reply.status, 200, "request {n}: {}", reply.body);
        }
        let limited = call(
            &o.client,
            Method::POST,
            "/tasks/title-completions",
            Some(title_body("Write the quarterly report")),
        )
        .await;
        assert_eq!(limited.status, 429, "{}", limited.body);
        assert_eq!(limited.header("retry-after"), Some("60"));
        assert!(limited.header("x-correlation-id").is_some());
        // The allowance is per owner.
        let fresh = call(
            &other.client,
            Method::POST,
            "/tasks/title-completions",
            Some(title_body("Write the quarterly report")),
        )
        .await;
        assert_eq!(fresh.status, 200, "{}", fresh.body);
    })
    .await;
}

// ---------------------------------------------------------------- CAP-HTTP-112

#[tokio::test]
async fn cap_http_112_acknowledges_an_acceptance_with_204() {
    case(&["CAP-HTTP-112"], "title completion acceptance", async {
        let env = env().await;
        let o = owner(&env).await;
        let done = call(
            &o.client,
            Method::POST,
            "/tasks/title-completions",
            Some(title_body("Write the quarterly report")),
        )
        .await;
        let request_id = json_of(&done)["request_id"]
            .as_str()
            .expect("request id")
            .to_string();
        let accepted = call(
            &o.client,
            Method::POST,
            "/tasks/title-completions/accepted",
            Some(json!({ "request_id": request_id, "rank": 2 })),
        )
        .await;
        assert_eq!(accepted.status, 204, "{}", accepted.body);
        assert!(accepted.body.is_empty());
        let anon = call(
            &env.anon,
            Method::POST,
            "/tasks/title-completions/accepted",
            Some(json!({ "request_id": request_id, "rank": 2 })),
        )
        .await;
        assert_eq!(anon.status, 401);
    })
    .await;
}

#[tokio::test]
async fn cap_http_112_request_id_must_be_a_uuid() {
    case(
        &["CAP-HTTP-112"],
        "title completion acceptance request id",
        async {
            let env = env().await;
            let o = owner(&env).await;
            let bad = "x".repeat(36);
            let reply = call(
                &o.client,
                Method::POST,
                "/tasks/title-completions/accepted",
                Some(json!({ "request_id": bad, "rank": 1 })),
            )
            .await;
            assert_eq!(reply.status, 422, "{}", reply.body);
            assert_eq!(json_of(&reply)["message"], "request_id must be a UUID");
            let rank = call(
                &o.client,
                Method::POST,
                "/tasks/title-completions/accepted",
                Some(json!({ "request_id": "7f9f5b1e-6b5a-4e0e-9c1a-1b2c3d4e5f60", "rank": 4 })),
            )
            .await;
            assert_eq!(rank.status, 422, "{}", rank.body);
        },
    )
    .await;
}

// ---------------------------------------------------------------- CAP-HTTP-157

#[tokio::test]
async fn cap_http_157_status_shows_provider_consent_and_availability() {
    case(&["CAP-HTTP-157"], "navigator status", async {
        let env = env().await;
        let anon = call(&env.anon, Method::GET, "/review/navigator", None).await;
        assert_eq!(anon.status, 401);
        let o = owner(&env).await;
        let before = call(&o.client, Method::GET, "/review/navigator", None).await;
        assert_eq!(before.status, 200, "{}", before.body);
        assert_eq!(
            json_of(&before),
            json!({
                "provider": NAVIGATOR_PROVIDER, "consent": null, "consent_current": false,
                "consent_text_version": CONSENT_TEXT_VERSION, "available": true
            })
        );
        assert_eq!(grant(&o.client).await.status, 200);
        let after = json_of(&call(&o.client, Method::GET, "/review/navigator", None).await);
        assert_eq!(after["consent_current"], true);
        assert!(after["consent"]["revoked_at"].is_null());
    })
    .await;
}

#[tokio::test]
async fn cap_http_157_consent_belongs_to_one_owner() {
    case(&["CAP-HTTP-157"], "navigator status owner scope", async {
        let env = env().await;
        let (a, b) = (owner(&env).await, owner(&env).await);
        assert_eq!(grant(&a.client).await.status, 200);
        let seen_by_b = json_of(&call(&b.client, Method::GET, "/review/navigator", None).await);
        assert!(seen_by_b["consent"].is_null());
        assert_eq!(seen_by_b["consent_current"], false);
    })
    .await;
}

// ---------------------------------------------------------------- CAP-HTTP-158

#[tokio::test]
async fn cap_http_158_grant_requires_a_key_and_replays_it() {
    case(&["CAP-HTTP-158"], "navigator consent grant idempotency", async {
        let env = env().await;
        let o = owner(&env).await;
        let body = json!({ "provider": NAVIGATOR_PROVIDER, "consent_text_version": CONSENT_TEXT_VERSION });
        let missing = call(&o.client, Method::POST, "/review/navigator/consent", Some(body.clone())).await;
        assert_eq!(missing.status, 400, "{}", missing.body);
        assert_eq!(json_of(&missing)["message"], "Idempotency-Key header is required.");
        let key = format!("grant-{}", unique());
        let first = keyed(&o.client, Method::POST, "/review/navigator/consent", &key, Some(body.clone())).await;
        assert_eq!(first.status, 200, "{}", first.body);
        assert_eq!(json_of(&first)["consent_current"], true);
        // A revoke in between does not let the replayed key re-grant.
        assert_eq!(revoke(&o.client).await.status, 204);
        let replay = keyed(&o.client, Method::POST, "/review/navigator/consent", &key, Some(body)).await;
        assert_eq!(replay.status, 200);
        assert_eq!(json_of(&replay), json_of(&first));
        let now = json_of(&call(&o.client, Method::GET, "/review/navigator", None).await);
        assert_eq!(now["consent_current"], false);
    })
    .await;
}

#[tokio::test]
async fn cap_http_158_grant_refuses_another_provider_or_text_version() {
    case(&["CAP-HTTP-158"], "navigator consent grant mismatch", async {
        let env = env().await;
        let o = owner(&env).await;
        let other = json!({ "provider": "other", "consent_text_version": CONSENT_TEXT_VERSION });
        let wrong = keyed(&o.client, Method::POST, "/review/navigator/consent", &format!("k-{}", unique()), Some(other)).await;
        assert_eq!((wrong.status, reason(&wrong).as_str()), (400, "provider_mismatch"));
        let old = json!({ "provider": NAVIGATOR_PROVIDER, "consent_text_version": CONSENT_TEXT_VERSION + 8 });
        let stale = keyed(&o.client, Method::POST, "/review/navigator/consent", &format!("k-{}", unique()), Some(old)).await;
        assert_eq!((stale.status, reason(&stale).as_str()), (400, "consent_text_outdated"));
        let anon = call(&env.anon, Method::POST, "/review/navigator/consent", None).await;
        assert_eq!(anon.status, 401);
    })
    .await;
}

// ---------------------------------------------------------------- CAP-HTTP-159

#[tokio::test]
async fn cap_http_159_revoke_is_204_keyed_and_ends_the_grant() {
    case(&["CAP-HTTP-159"], "navigator consent revoke", async {
        let env = env().await;
        let o = owner(&env).await;
        assert_eq!(grant(&o.client).await.status, 200);
        let missing = call(&o.client, Method::DELETE, "/review/navigator/consent", None).await;
        assert_eq!(missing.status, 400, "{}", missing.body);
        let key = format!("revoke-{}", unique());
        let first = keyed(
            &o.client,
            Method::DELETE,
            "/review/navigator/consent",
            &key,
            None,
        )
        .await;
        assert_eq!(first.status, 204, "{}", first.body);
        let replay = keyed(
            &o.client,
            Method::DELETE,
            "/review/navigator/consent",
            &key,
            None,
        )
        .await;
        assert_eq!(replay.status, 204);
        let status = json_of(&call(&o.client, Method::GET, "/review/navigator", None).await);
        assert_eq!(status["consent_current"], false);
        assert!(status["consent"]["revoked_at"].is_string());
        let anon = keyed(
            &env.anon,
            Method::DELETE,
            "/review/navigator/consent",
            "k",
            None,
        )
        .await;
        assert_eq!(anon.status, 401);
    })
    .await;
}

#[tokio::test]
async fn cap_http_159_revoke_touches_only_the_calling_owner() {
    case(
        &["CAP-HTTP-159"],
        "navigator consent revoke owner scope",
        async {
            let env = env().await;
            let (a, b) = (owner(&env).await, owner(&env).await);
            assert_eq!(grant(&a.client).await.status, 200);
            assert_eq!(grant(&b.client).await.status, 200);
            assert_eq!(revoke(&a.client).await.status, 204);
            let kept = json_of(&call(&b.client, Method::GET, "/review/navigator", None).await);
            assert_eq!(kept["consent_current"], true);
        },
    )
    .await;
}

// ---------------------------------------------------------------- CAP-HTTP-160

#[tokio::test]
async fn cap_http_160_suggests_grounded_proposals_after_consent() {
    case(&["CAP-HTTP-160"], "navigator suggestions", async {
        let env = env().await;
        let anon = call(
            &env.anon,
            Method::POST,
            "/review/navigator/suggestions",
            Some(nav_request("Write the report")),
        )
        .await;
        assert_eq!(anon.status, 401);
        let o = owner(&env).await;
        assert_eq!(grant(&o.client).await.status, 200);
        let reply = call(
            &o.client,
            Method::POST,
            "/review/navigator/suggestions",
            Some(nav_request("Write the report")),
        )
        .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        let body = json_of(&reply);
        assert_eq!(body["provider"], NAVIGATOR_PROVIDER);
        assert_eq!(body["notes_truncated"], false);
        assert!(is_uuid(body["request_id"].as_str().unwrap_or("")));
        let proposals = body["proposals"].as_array().expect("proposals");
        assert!((1..=3).contains(&proposals.len()));
        assert!(body["clarifying_question"].is_null());
    })
    .await;
}

#[tokio::test]
async fn cap_http_160_without_current_consent_is_400() {
    case(&["CAP-HTTP-160"], "navigator suggestions need consent", async {
        let env = env().await;
        let o = owner(&env).await;
        let never = call(&o.client, Method::POST, "/review/navigator/suggestions", Some(nav_request("Write the report"))).await;
        assert_eq!((never.status, reason(&never).as_str()), (400, "navigator_consent_required"));
        assert_eq!(grant(&o.client).await.status, 200);
        let declined = json!({ "kind": "first_step", "consent": nav_consent(false), "task": { "title": "Write the report" } });
        let echo = call(&o.client, Method::POST, "/review/navigator/suggestions", Some(declined)).await;
        assert_eq!((echo.status, reason(&echo).as_str()), (400, "navigator_consent_required"));
        assert_eq!(revoke(&o.client).await.status, 204);
        let revoked = call(&o.client, Method::POST, "/review/navigator/suggestions", Some(nav_request("Write the report"))).await;
        assert_eq!((revoked.status, reason(&revoked).as_str()), (400, "navigator_consent_required"));
    })
    .await;
}

#[tokio::test]
async fn cap_http_160_another_owners_consent_does_not_count() {
    case(
        &["CAP-HTTP-160"],
        "navigator suggestions owner scope",
        async {
            let env = env().await;
            let (a, b) = (owner(&env).await, owner(&env).await);
            assert_eq!(grant(&a.client).await.status, 200);
            let reply = call(
                &b.client,
                Method::POST,
                "/review/navigator/suggestions",
                Some(nav_request("Write the report")),
            )
            .await;
            assert_eq!(
                (reply.status, reason(&reply).as_str()),
                (400, "navigator_consent_required")
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_160_provider_outcomes_map_to_fixed_errors() {
    case(&["CAP-HTTP-160"], "navigator provider outcomes", async {
        let env = env().await;
        let o = owner(&env).await;
        assert_eq!(grant(&o.client).await.status, 200);
        let ask = |title: &str| {
            call(
                &o.client,
                Method::POST,
                "/review/navigator/suggestions",
                Some(nav_request(title)),
            )
        };
        let question = ask("Plan navigator-test:question").await;
        assert_eq!(question.status, 200, "{}", question.body);
        let body = json_of(&question);
        assert!(body["proposals"].is_null());
        assert!(body["clarifying_question"].is_string());
        for (marker, status, why) in [
            ("navigator-test:timeout", 503, "navigator_timeout"),
            (
                "navigator-test:provider_error",
                503,
                "navigator_provider_error",
            ),
            (
                "navigator-test:malformed",
                503,
                "navigator_malformed_output",
            ),
        ] {
            let reply = ask(&format!("Plan {marker}")).await;
            assert_eq!(
                (reply.status, reason(&reply).as_str()),
                (status, why),
                "{marker}"
            );
            assert!(
                !reply.body.contains(marker),
                "an error never echoes request content"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn cap_http_160_limits_an_owner_to_20_suggestions_per_window() {
    case(&["CAP-HTTP-160"], "navigator rate limit", async {
        let env = env().await;
        let o = owner(&env).await;
        assert_eq!(grant(&o.client).await.status, 200);
        for n in 1..=20 {
            let reply = call(
                &o.client,
                Method::POST,
                "/review/navigator/suggestions",
                Some(nav_request("Write the report")),
            )
            .await;
            assert_eq!(reply.status, 200, "request {n}: {}", reply.body);
        }
        let limited = call(
            &o.client,
            Method::POST,
            "/review/navigator/suggestions",
            Some(nav_request("Write the report")),
        )
        .await;
        assert_eq!(
            (limited.status, reason(&limited).as_str()),
            (429, "navigator_rate_limited")
        );
        let wait: u64 = limited
            .header("retry-after")
            .and_then(|v| v.parse().ok())
            .expect("Retry-After");
        assert!((1..=600).contains(&wait), "Retry-After {wait}");
        assert!(limited.header("x-correlation-id").is_some());
        let fresh = owner(&env).await;
        assert_eq!(grant(&fresh.client).await.status, 200);
        let other = call(
            &fresh.client,
            Method::POST,
            "/review/navigator/suggestions",
            Some(nav_request("Write the report")),
        )
        .await;
        assert_eq!(other.status, 200, "{}", other.body);
    })
    .await;
}

#[tokio::test]
async fn cap_http_160_rejects_a_request_whose_shape_does_not_match_its_kind() {
    case(&["CAP-HTTP-160"], "navigator request shape", async {
        let env = env().await;
        let o = owner(&env).await;
        let bad = json!({ "kind": "first_step", "consent": nav_consent(true) });
        let reply = call(
            &o.client,
            Method::POST,
            "/review/navigator/suggestions",
            Some(bad),
        )
        .await;
        assert_eq!(reply.status, 422, "{}", reply.body);
    })
    .await;
}
