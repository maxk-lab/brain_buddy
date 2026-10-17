#![cfg(feature = "live")]
//! Black-box conformance for the external-agent relay, CAP-HTTP-170..185
//! (027-FR-001, 027-FR-014, 027-FR-008, 027-SC-003).
//!
//! Every test encodes today's Python behavior and is named `cap_http_<nnn>_*`.
//! Owners and the rollout flag come from the test control API only. The agent
//! under test is a reference peer (the Compose `hermes-a2a` fixture, which is
//! the unmodified Hermes A2A plugin behind a scripted brain):
//!
//! * `BB_CONFORMANCE_AGENT_URL`: the peer's address *as the server under test
//!   reaches it* (default `http://127.0.0.1:9900/`, the Compose port mapping);
//! * `BB_CONFORMANCE_AGENT_TOKEN`: the peer's bearer token (default is the
//!   Compose fixture's).
//!
//! The server must allow private destinations, as the Compose stack does with
//! `BRAIN_BUDDY_AGENT_ALLOW_PRIVATE_DESTINATIONS=1`.
//!
//! Not covered black-box, and why: the success `204` of CAP-HTTP-185 and its
//! per-run 30/min and global 600/min push limits. A verified push needs the
//! per-run token, which the server mints and never shows, and 600 requests
//! would disturb every other test; PR-143 covers them in-process. The streamed
//! (undeclared length) body cap is tested here over a raw chunked request.
//!
//! The synthetic owners sign up under `conformance.example.com`, the domain the
//! shared `TestControl::new_owner` uses: the server's email validation refuses
//! reserved names such as `.invalid`.

use bb_conformance::{Client, Config, Owner, Reply, TestControl, case};
use reqwest::Method;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::Mutex;

const PASSWORD: &str = "Conformance-pass-1!";
const FLAG: &str = "external_agent_relay";
const ABSENT_CONNECTION: &str = "agentconn_doesnotexist0";
const ABSENT_RUN: &str = "agentrun_doesnotexist0";
const ABSENT_TASK: &str = "task_doesnotexist0";
/// A prompt the scripted peer answers with a question, so the run blocks on the
/// owner (`needs_user`) instead of completing: replies and cancels need that.
const ASKING_TITLE: &str = "ask me which list this goes on";

/// The rollout flag is process-wide server state, so tests that change it, and
/// the owners that depend on it, run one at a time.
static LANE: Mutex<()> = Mutex::const_new(());

struct Ctx {
    control: TestControl,
    client: Client,
}

fn ctx() -> Ctx {
    let cfg = Config::from_env().expect("conformance configuration");
    Ctx {
        control: TestControl::new(&cfg).expect("test control"),
        client: Client::new(&cfg).expect("client"),
    }
}

fn agent_url() -> String {
    std::env::var("BB_CONFORMANCE_AGENT_URL").unwrap_or_else(|_| "http://127.0.0.1:9900/".into())
}

fn agent_token() -> String {
    std::env::var("BB_CONFORMANCE_AGENT_TOKEN").unwrap_or_else(|_| "hermes-e2e-bearer-token".into())
}

fn unique() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{t:x}{:x}", N.fetch_add(1, Ordering::Relaxed))
}

async fn set_flag(c: &Ctx, mode: &str) {
    c.control.flag(FLAG, mode, &[]).await.expect("set flag");
}

async fn owner(c: &Ctx) -> Owner {
    let invites = c.control.invites(1).await.expect("invite");
    let code = invites["codes"][0].as_str().expect("invite code");
    let email = format!("owner-{}@conformance.example.com", unique());
    let mut client = c.client.fresh();
    let body = json!({"email": email, "password": PASSWORD, "invite_code": code});
    let reply = client
        .post("/api/auth/signup", &body)
        .await
        .expect("signup");
    assert_eq!(reply.status, 201, "synthetic signup: {}", reply.body);
    Owner { email, client }
}

async fn send(
    client: &Client,
    method: Method,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> Reply {
    let mut c = client.clone();
    if let Some(key) = key {
        c = c.with_header("Idempotency-Key", key);
    }
    c.send(method, path, body.as_ref()).await.expect("request")
}

async fn get(o: &Owner, path: &str) -> Reply {
    send(&o.client, Method::GET, path, None, None).await
}

async fn post(o: &Owner, path: &str, body: Value) -> Reply {
    send(&o.client, Method::POST, path, Some(body), None).await
}

/// A mutation with a fresh `Idempotency-Key`.
async fn keyed(o: &Owner, method: Method, path: &str, body: Option<Value>) -> Reply {
    send(
        &o.client,
        method,
        path,
        body,
        Some(&format!("idem-{}", unique())),
    )
    .await
}

fn json_of(r: &Reply) -> Value {
    r.json()
        .unwrap_or_else(|_| panic!("JSON body, got: {}", r.body))
}

/// Asserts the status and returns the JSON body.
fn expect(r: &Reply, status: u16) -> Value {
    assert_eq!(r.status, status, "unexpected status, body: {}", r.body);
    if r.body.is_empty() {
        Value::Null
    } else {
        json_of(r)
    }
}

fn message(r: &Reply) -> String {
    json_of(r)["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// A foreign id must read exactly like a missing one (027-FR-008): the same
/// status, the same message once the id is masked, the same detail resource.
fn assert_indistinguishable(foreign: &Reply, foreign_id: &str, missing: &Reply, missing_id: &str) {
    assert_eq!(foreign.status, 404, "foreign: {}", foreign.body);
    assert_eq!(missing.status, 404, "missing: {}", missing.body);
    let (f, m) = (json_of(foreign), json_of(missing));
    assert_eq!(
        f["message"]
            .as_str()
            .unwrap_or_default()
            .replace(foreign_id, "ID"),
        m["message"]
            .as_str()
            .unwrap_or_default()
            .replace(missing_id, "ID"),
    );
    assert_eq!(f["detail"]["resource"], m["detail"]["resource"]);
}

fn connection_body(name: &str) -> Value {
    json!({
        "name": name,
        "agent_address": agent_url(),
        "auth_scheme": "bearer",
        "credential": agent_token(),
        "current_password": PASSWORD,
    })
}

async fn create_connection(o: &Owner) -> Value {
    let name = format!("Agent {}", unique());
    let r = keyed(
        o,
        Method::POST,
        "/api/agent-connections",
        Some(connection_body(&name)),
    )
    .await;
    expect(&r, 201)
}

/// A created connection that has passed its test and can take a hand-off.
async fn ready_connection(o: &Owner) -> Value {
    let created = create_connection(o).await;
    let id = created["id"].as_str().expect("connection id").to_string();
    let tested = expect(
        &post(o, &format!("/api/agent-connections/{id}/test"), json!({})).await,
        200,
    );
    assert_eq!(
        tested["status"], "ready",
        "the reference peer must be reachable: {tested}"
    );
    tested
}

async fn create_task(o: &Owner, title: &str) -> String {
    let r = keyed(o, Method::POST, "/api/tasks", Some(json!({"title": title}))).await;
    expect(&r, 201)["id"].as_str().expect("task id").to_string()
}

async fn preview(o: &Owner, task_id: &str, connection_id: &str) -> Value {
    let path = format!("/api/tasks/{task_id}/agent-runs/preview");
    expect(
        &post(o, &path, json!({"connection_id": connection_id})).await,
        200,
    )
}

fn confirm_body(connection_id: &str, manifest: &Value) -> Value {
    json!({
        "connection_id": connection_id,
        "manifest_token": manifest["token"],
        "acknowledge_duplicate_risk": true,
    })
}

struct Handoff {
    connection_id: String,
    task_id: String,
    run: Value,
}

impl Handoff {
    fn run_id(&self) -> &str {
        self.run["id"].as_str().expect("run id")
    }
}

/// Connection, task and one dispatched run, left blocked on the owner.
async fn blocked_handoff(o: &Owner) -> Handoff {
    let connection_id = ready_connection(o).await["id"]
        .as_str()
        .expect("id")
        .to_string();
    let task_id = create_task(o, ASKING_TITLE).await;
    let manifest = preview(o, &task_id, &connection_id).await;
    let path = format!("/api/tasks/{task_id}/agent-runs");
    let body = confirm_body(&connection_id, &manifest);
    let run = expect(&keyed(o, Method::POST, &path, Some(body)).await, 201);
    let id = run["id"].as_str().expect("run id").to_string();
    let run = settle(o, &id, |r| r["reported_state"] == "blocked").await;
    Handoff {
        connection_id,
        task_id,
        run,
    }
}

async fn settle(o: &Owner, run_id: &str, done: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..80 {
        let run = expect(&get(o, &format!("/api/agent-runs/{run_id}")).await, 200);
        if done(&run) {
            return run;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("run {run_id} did not settle");
}

#[tokio::test]
async fn cap_http_170_list_agent_connections_is_owner_scoped_and_session_only() {
    case(
        &["CAP-HTTP-170"],
        "027-FR-001 listing agent connections returns only the owner's",
        async {
            let _lane = LANE.lock().await;
            let c = ctx();
            set_flag(&c, "on").await;
            let (a, b) = (owner(&c).await, owner(&c).await);

            let anonymous = send(
                &c.client.fresh(),
                Method::GET,
                "/api/agent-connections",
                None,
                None,
            )
            .await;
            assert_eq!(
                expect(&anonymous, 401)["message"],
                "Authentication required."
            );

            assert_eq!(
                expect(&get(&a, "/api/agent-connections").await, 200),
                json!([])
            );
            let created = create_connection(&a).await;
            let listed = expect(&get(&a, "/api/agent-connections").await, 200);
            assert_eq!(listed.as_array().map(Vec::len), Some(1));
            assert_eq!(listed[0]["id"], created["id"]);
            assert!(
                !get(&a, "/api/agent-connections")
                    .await
                    .body
                    .contains(&agent_token())
            );

            // 027-FR-008: a second owner sees none of it.
            assert_eq!(
                expect(&get(&b, "/api/agent-connections").await, 200),
                json!([])
            );

            // Listing is not gated by the rollout flag.
            set_flag(&c, "off").await;
            let off = expect(&get(&a, "/api/agent-connections").await, 200);
            set_flag(&c, "on").await;
            assert_eq!(off[0]["id"], created["id"]);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_171_create_agent_connection_needs_key_password_and_returns_no_secret() {
    case(
        &["CAP-HTTP-171"],
        "027-FR-001 creating an agent connection",
        async {
            let _lane = LANE.lock().await;
            let c = ctx();
            set_flag(&c, "on").await;
            let a = owner(&c).await;
            let path = "/api/agent-connections";

            let anonymous = send(
                &c.client.fresh(),
                Method::POST,
                path,
                Some(connection_body("x")),
                Some("k"),
            )
            .await;
            expect(&anonymous, 401);

            let no_key = post(&a, path, connection_body("No key")).await;
            assert_eq!(
                expect(&no_key, 400)["message"],
                "Idempotency-Key header is required."
            );

            let mut no_password = connection_body("No password");
            no_password
                .as_object_mut()
                .expect("object")
                .remove("current_password");
            expect(&keyed(&a, Method::POST, path, Some(no_password)).await, 422);

            let mut wrong = connection_body("Wrong password");
            wrong["current_password"] = json!("not-the-password");
            let refused = keyed(&a, Method::POST, path, Some(wrong)).await;
            assert_eq!(
                expect(&refused, 403)["message"],
                "Current password is incorrect."
            );

            // The cloud metadata address is refused before the credential leaves,
            // and the credential is not echoed back.
            let mut metadata = connection_body("Metadata");
            metadata["agent_address"] = json!("http://169.254.169.254/latest/meta-data/");
            metadata["credential"] = json!("must-not-leave");
            let refused = keyed(&a, Method::POST, path, Some(metadata)).await;
            expect(&refused, 400);
            assert!(!refused.body.contains("must-not-leave"));

            let key = format!("idem-{}", unique());
            let body = connection_body("First");
            let created = send(
                &a.client,
                Method::POST,
                path,
                Some(body.clone()),
                Some(&key),
            )
            .await;
            let first = expect(&created, 201);
            assert_eq!(first["status"], "untested");
            assert_eq!(first["ready_for_handoff"], false);
            assert!(
                first["id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("agentconn_"))
            );
            assert!(
                !created.body.contains(&agent_token()),
                "the 201 carries no secret"
            );
            assert!(first.get("credential").is_none());

            let replay = send(&a.client, Method::POST, path, Some(body), Some(&key)).await;
            assert_eq!(
                expect(&replay, 201)["id"],
                first["id"],
                "replay is one connection"
            );

            let reused = send(
                &a.client,
                Method::POST,
                path,
                Some(connection_body("Other")),
                Some(&key),
            )
            .await;
            assert_eq!(
                expect(&reused, 409)["message"],
                "This Idempotency-Key was already used for a different request."
            );

            set_flag(&c, "off").await;
            let gated = keyed(&a, Method::POST, path, Some(connection_body("Gated"))).await;
            set_flag(&c, "on").await;
            assert_eq!(
                expect(&gated, 404)["message"],
                "External agent relay is not available."
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_172_get_agent_connection_hides_foreign_ids() {
    case(
        &["CAP-HTTP-172"],
        "027-FR-008 reading one agent connection",
        async {
            let _lane = LANE.lock().await;
            let c = ctx();
            set_flag(&c, "on").await;
            let (a, b) = (owner(&c).await, owner(&c).await);
            let created = create_connection(&a).await;
            let id = created["id"].as_str().expect("id");

            let own = expect(&get(&a, &format!("/api/agent-connections/{id}")).await, 200);
            assert_eq!(own["id"], created["id"]);
            assert_eq!(own["agent_address"], agent_url());
            assert_eq!(own["revision"], 1);

            let foreign = get(&b, &format!("/api/agent-connections/{id}")).await;
            let missing = get(&b, &format!("/api/agent-connections/{ABSENT_CONNECTION}")).await;
            assert_indistinguishable(&foreign, id, &missing, ABSENT_CONNECTION);

            let anonymous = send(
                &c.client.fresh(),
                Method::GET,
                &format!("/api/agent-connections/{id}"),
                None,
                None,
            )
            .await;
            expect(&anonymous, 401);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_173_update_agent_connection_checks_revision_and_owner() {
    case(
        &["CAP-HTTP-173"],
        "027-FR-001 updating an agent connection",
        async {
            let _lane = LANE.lock().await;
            let c = ctx();
            set_flag(&c, "on").await;
            let (a, b) = (owner(&c).await, owner(&c).await);
            let created = create_connection(&a).await;
            let id = created["id"].as_str().expect("id");
            let path = format!("/api/agent-connections/{id}");

            let no_key = send(
                &a.client,
                Method::PUT,
                &path,
                Some(json!({"name": "N", "expected_revision": 1})),
                None,
            )
            .await;
            assert_eq!(
                expect(&no_key, 400)["message"],
                "Idempotency-Key header is required."
            );

            let empty = keyed(
                &a,
                Method::PUT,
                &path,
                Some(json!({"expected_revision": 1})),
            )
            .await;
            expect(&empty, 422);

            let renamed = keyed(
                &a,
                Method::PUT,
                &path,
                Some(json!({"name": "Renamed", "expected_revision": 1})),
            )
            .await;
            let updated = expect(&renamed, 200);
            assert_eq!(updated["name"], "Renamed");
            assert_eq!(updated["revision"], 2);

            let stale = keyed(
                &a,
                Method::PUT,
                &path,
                Some(json!({"name": "Again", "expected_revision": 1})),
            )
            .await;
            assert_eq!(
                expect(&stale, 409)["message"],
                "This agent changed elsewhere; reload and try again."
            );

            let foreign = keyed(
                &b,
                Method::PUT,
                &path,
                Some(json!({"name": "Mine", "expected_revision": 1})),
            )
            .await;
            let absent_path = format!("/api/agent-connections/{ABSENT_CONNECTION}");
            let missing = keyed(
                &b,
                Method::PUT,
                &absent_path,
                Some(json!({"name": "Mine", "expected_revision": 1})),
            )
            .await;
            assert_indistinguishable(&foreign, id, &missing, ABSENT_CONNECTION);

            set_flag(&c, "off").await;
            let gated = keyed(
                &a,
                Method::PUT,
                &path,
                Some(json!({"name": "Gated", "expected_revision": 2})),
            )
            .await;
            set_flag(&c, "on").await;
            assert_eq!(
                expect(&gated, 404)["message"],
                "External agent relay is not available."
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_174_test_agent_connection_discovers_the_card_and_reports_credentials() {
    case(
        &["CAP-HTTP-174"],
        "027-FR-001 testing an agent connection",
        async {
            let _lane = LANE.lock().await;
            let c = ctx();
            set_flag(&c, "on").await;
            let (a, b) = (owner(&c).await, owner(&c).await);

            let tested = ready_connection(&a).await;
            let id = tested["id"].as_str().expect("id").to_string();
            assert_eq!(tested["ready_for_handoff"], true);
            assert_eq!(
                tested["controls_offered"],
                json!({"reply": true, "cancel": true})
            );
            assert!(
                tested["card"]["name"].is_string(),
                "the card is read: {tested}"
            );
            assert!(tested["guarantee_tier"].is_string());
            assert!(tested["last_tested_at"].is_string());

            // A wrong credential is reported as a status, never echoed.
            let mut wrong = connection_body("Wrong credential");
            wrong["credential"] = json!("not-the-fixture-token");
            let made = expect(
                &keyed(&a, Method::POST, "/api/agent-connections", Some(wrong)).await,
                201,
            );
            let wrong_id = made["id"].as_str().expect("id");
            let result = post(
                &a,
                &format!("/api/agent-connections/{wrong_id}/test"),
                json!({}),
            )
            .await;
            assert_eq!(expect(&result, 200)["status"], "invalid_credentials");
            assert!(!result.body.contains("not-the-fixture-token"));

            let foreign = post(&b, &format!("/api/agent-connections/{id}/test"), json!({})).await;
            let missing = post(
                &b,
                &format!("/api/agent-connections/{ABSENT_CONNECTION}/test"),
                json!({}),
            )
            .await;
            assert_indistinguishable(&foreign, &id, &missing, ABSENT_CONNECTION);

            set_flag(&c, "off").await;
            let gated = post(&a, &format!("/api/agent-connections/{id}/test"), json!({})).await;
            set_flag(&c, "on").await;
            assert_eq!(
                expect(&gated, 404)["message"],
                "External agent relay is not available."
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_175_rotate_agent_credential_needs_password_and_revision() {
    case(&["CAP-HTTP-175"], "027-FR-001 rotating an agent credential", async {
        let _lane = LANE.lock().await;
        let c = ctx();
        set_flag(&c, "on").await;
        let (a, b) = (owner(&c).await, owner(&c).await);
        let tested = ready_connection(&a).await;
        let id = tested["id"].as_str().expect("id").to_string();
        let revision = tested["revision"].as_u64().expect("revision");
        let path = format!("/api/agent-connections/{id}/credential");
        let rotate = |password: &str, revision: u64| {
            json!({"credential": agent_token(), "current_password": password, "expected_revision": revision})
        };

        let no_key = post(&a, &path, rotate(PASSWORD, revision)).await;
        assert_eq!(expect(&no_key, 400)["message"], "Idempotency-Key header is required.");

        let wrong = keyed(&a, Method::POST, &path, Some(rotate("not-the-password", revision))).await;
        assert_eq!(expect(&wrong, 403)["message"], "Current password is incorrect.");

        let stale = keyed(&a, Method::POST, &path, Some(rotate(PASSWORD, 1))).await;
        expect(&stale, 409);

        let foreign = keyed(&b, Method::POST, &path, Some(rotate(PASSWORD, revision))).await;
        let absent = format!("/api/agent-connections/{ABSENT_CONNECTION}/credential");
        let missing = keyed(&b, Method::POST, &absent, Some(rotate(PASSWORD, revision))).await;
        assert_indistinguishable(&foreign, &id, &missing, ABSENT_CONNECTION);

        let rotated = keyed(&a, Method::POST, &path, Some(rotate(PASSWORD, revision))).await;
        assert!(!rotated.body.contains(&agent_token()), "the new secret is not echoed");
        let rotated = expect(&rotated, 200);
        assert_eq!(rotated["revision"], revision + 1);
        assert_eq!(rotated["status"], "untested", "a new credential must be tested again");

        set_flag(&c, "off").await;
        let gated = keyed(&a, Method::POST, &path, Some(rotate(PASSWORD, revision + 1))).await;
        set_flag(&c, "on").await;
        assert_eq!(expect(&gated, 404)["message"], "External agent relay is not available.");
    })
    .await;
}

#[tokio::test]
async fn cap_http_176_disconnect_agent_connection_stays_available_with_the_flag_off() {
    case(&["CAP-HTTP-176"], "027-FR-001 disconnecting an agent connection", async {
        let _lane = LANE.lock().await;
        let c = ctx();
        set_flag(&c, "on").await;
        let (a, b) = (owner(&c).await, owner(&c).await);
        let tested = ready_connection(&a).await;
        let id = tested["id"].as_str().expect("id").to_string();
        let revision = tested["revision"].as_u64().expect("revision");
        let path = format!("/api/agent-connections/{id}/disconnect");
        let body = |revision: u64| json!({"current_password": PASSWORD, "expected_revision": revision});

        let no_key = post(&a, &path, body(revision)).await;
        assert_eq!(expect(&no_key, 400)["message"], "Idempotency-Key header is required.");
        expect(&keyed(&a, Method::POST, &path, Some(json!({"expected_revision": revision}))).await, 422);
        let wrong = json!({"current_password": "not-the-password", "expected_revision": revision});
        expect(&keyed(&a, Method::POST, &path, Some(wrong)).await, 403);
        expect(&keyed(&a, Method::POST, &path, Some(body(revision + 5))).await, 409);

        let foreign = keyed(&b, Method::POST, &path, Some(body(revision))).await;
        let absent = format!("/api/agent-connections/{ABSENT_CONNECTION}/disconnect");
        let missing = keyed(&b, Method::POST, &absent, Some(body(revision))).await;
        assert_indistinguishable(&foreign, &id, &missing, ABSENT_CONNECTION);

        // Not gated: an owner can always cut a connection off.
        set_flag(&c, "off").await;
        let done = keyed(&a, Method::POST, &path, Some(body(revision))).await;
        set_flag(&c, "on").await;
        let done = expect(&done, 200);
        assert_eq!(done["status"], "disconnected");
        assert_eq!(done["disconnect_reason"], "owner");
        assert_eq!(done["ready_for_handoff"], false);
    })
    .await;
}

#[tokio::test]
async fn cap_http_177_preview_handoff_builds_the_reviewed_manifest() {
    case(
        &["CAP-HTTP-177"],
        "027-FR-001 previewing a hand-off",
        async {
            let _lane = LANE.lock().await;
            let c = ctx();
            set_flag(&c, "on").await;
            let (a, b) = (owner(&c).await, owner(&c).await);
            let connection_id = ready_connection(&a).await["id"]
                .as_str()
                .expect("id")
                .to_string();
            let task_id = create_task(&a, "Hand this one over").await;
            let path = format!("/api/tasks/{task_id}/agent-runs/preview");

            let manifest = preview(&a, &task_id, &connection_id).await;
            let token = manifest["token"].as_str().expect("token");
            assert!(token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(
                manifest["run_id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("agentrun_"))
            );
            assert_eq!(manifest["task_id"], task_id.as_str());
            assert_eq!(manifest["connection_id"], connection_id.as_str());
            assert_eq!(manifest["title"], "Hand this one over");
            assert_eq!(manifest["destination_interface"], agent_url());
            assert_eq!(
                manifest["acknowledgement_required"], true,
                "best-effort peer"
            );
            assert!(manifest["external_copy_notice"].is_string());

            // A foreign owner's connection and task read as absent.
            let foreign = post(&b, &path, json!({"connection_id": connection_id})).await;
            assert_eq!(foreign.status, 404, "{}", foreign.body);
            let b_conn = ready_connection(&b).await["id"]
                .as_str()
                .expect("id")
                .to_string();
            let foreign_task = post(&b, &path, json!({"connection_id": b_conn})).await;
            let missing_task = post(
                &b,
                &format!("/api/tasks/{ABSENT_TASK}/agent-runs/preview"),
                json!({"connection_id": b_conn}),
            )
            .await;
            assert_indistinguishable(&foreign_task, &task_id, &missing_task, ABSENT_TASK);

            // A connection that was never tested cannot take a hand-off.
            let untested = create_connection(&a).await["id"]
                .as_str()
                .expect("id")
                .to_string();
            let early = post(&a, &path, json!({"connection_id": untested})).await;
            assert_eq!(
                expect(&early, 400)["detail"]["reason"],
                "connection_not_ready"
            );

            set_flag(&c, "off").await;
            let gated = post(&a, &path, json!({"connection_id": connection_id})).await;
            set_flag(&c, "on").await;
            assert_eq!(
                expect(&gated, 404)["message"],
                "External agent relay is not available."
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_178_dispatch_agent_run_is_confirmed_once_per_key() {
    case(
        &["CAP-HTTP-178"],
        "027-FR-001 dispatching an agent run",
        async {
            let _lane = LANE.lock().await;
            let c = ctx();
            set_flag(&c, "on").await;
            let (a, b) = (owner(&c).await, owner(&c).await);
            let connection_id = ready_connection(&a).await["id"]
                .as_str()
                .expect("id")
                .to_string();
            let task_id = create_task(&a, ASKING_TITLE).await;
            let manifest = preview(&a, &task_id, &connection_id).await;
            let path = format!("/api/tasks/{task_id}/agent-runs");
            let body = confirm_body(&connection_id, &manifest);

            let no_key = post(&a, &path, body.clone()).await;
            assert_eq!(
                expect(&no_key, 400)["message"],
                "Idempotency-Key header is required."
            );

            let mut forged = body.clone();
            forged["manifest_token"] = json!("0".repeat(64));
            let refused = keyed(&a, Method::POST, &path, Some(forged)).await;
            assert_eq!(
                expect(&refused, 400)["detail"]["reason"],
                "manifest_not_reserved"
            );

            let mut unacknowledged = body.clone();
            unacknowledged["acknowledge_duplicate_risk"] = json!(false);
            let refused = keyed(&a, Method::POST, &path, Some(unacknowledged)).await;
            assert_eq!(
                expect(&refused, 400)["detail"]["reason"],
                "duplicate_risk_acknowledgement_required"
            );

            let foreign = keyed(&b, Method::POST, &path, Some(body.clone())).await;
            let absent_path = format!("/api/tasks/{ABSENT_TASK}/agent-runs");
            let absent = keyed(&b, Method::POST, &absent_path, Some(body.clone())).await;
            assert_indistinguishable(&foreign, &task_id, &absent, ABSENT_TASK);

            let key = format!("idem-{}", unique());
            let first = send(
                &a.client,
                Method::POST,
                &path,
                Some(body.clone()),
                Some(&key),
            )
            .await;
            let run = expect(&first, 201);
            assert_eq!(
                run["id"], manifest["run_id"],
                "the reserved run is the one dispatched"
            );
            assert_eq!(run["task_id"], task_id.as_str());
            assert_eq!(run["dispatch_state"], "sent");

            let replay = send(
                &a.client,
                Method::POST,
                &path,
                Some(body.clone()),
                Some(&key),
            )
            .await;
            assert_eq!(expect(&replay, 201)["id"], run["id"], "replay is one run");
            let runs = expect(&get(&a, &path).await, 200);
            assert_eq!(runs.as_array().map(Vec::len), Some(1));

            set_flag(&c, "off").await;
            let other = create_task(&a, "After the flag turned off").await;
            let gated = keyed(
                &a,
                Method::POST,
                &format!("/api/tasks/{other}/agent-runs"),
                Some(body),
            )
            .await;
            set_flag(&c, "on").await;
            assert_eq!(
                expect(&gated, 404)["message"],
                "External agent relay is not available."
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_179_list_agent_runs_for_a_task_is_owner_scoped() {
    case(
        &["CAP-HTTP-179"],
        "027-FR-008 listing the runs of a task",
        async {
            let _lane = LANE.lock().await;
            let c = ctx();
            set_flag(&c, "on").await;
            let (a, b) = (owner(&c).await, owner(&c).await);
            let handoff = blocked_handoff(&a).await;
            let path = format!("/api/tasks/{}/agent-runs", handoff.task_id);

            let runs = expect(&get(&a, &path).await, 200);
            assert_eq!(runs.as_array().map(Vec::len), Some(1));
            assert_eq!(runs[0]["id"], handoff.run["id"]);
            assert_eq!(runs[0]["connection_id"], handoff.connection_id.as_str());

            let foreign = expect(&get(&b, &path).await, 200);
            let missing = expect(
                &get(&b, &format!("/api/tasks/{ABSENT_TASK}/agent-runs")).await,
                200,
            );
            assert_eq!(foreign, json!([]));
            assert_eq!(foreign, missing, "a foreign task reads like a missing one");

            let anonymous = send(&c.client.fresh(), Method::GET, &path, None, None).await;
            expect(&anonymous, 401);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_180_agent_run_summaries_are_sparse_latest_per_task() {
    case(&["CAP-HTTP-180"], "027-FR-008 latest run per task", async {
        let _lane = LANE.lock().await;
        let c = ctx();
        set_flag(&c, "on").await;
        let (a, b) = (owner(&c).await, owner(&c).await);
        let handoff = blocked_handoff(&a).await;
        let idle = create_task(&a, "Never handed over").await;
        let path = format!(
            "/api/agent-run-summaries?task_id={}&task_id={idle}",
            handoff.task_id
        );

        let summaries = expect(&get(&a, &path).await, 200);
        let map = summaries.as_object().expect("a map keyed by task id");
        assert_eq!(
            map.len(),
            1,
            "a task with no hand-off is absent: {summaries}"
        );
        let summary = &summaries[handoff.task_id.as_str()];
        assert_eq!(summary["id"], handoff.run["id"]);
        assert_eq!(summary["agent_name"], handoff.run["agent_name"]);
        assert_eq!(summary["needs_user"], true);
        assert_eq!(summary["primary_state_label"], "Needs you");

        assert_eq!(
            expect(&get(&a, "/api/agent-run-summaries").await, 200),
            json!({})
        );
        assert_eq!(
            expect(&get(&b, &path).await, 200),
            json!({}),
            "foreign tasks are absent"
        );
        let anonymous = send(&c.client.fresh(), Method::GET, &path, None, None).await;
        expect(&anonymous, 401);
    })
    .await;
}

#[tokio::test]
async fn cap_http_181_get_agent_run_hides_foreign_ids() {
    case(
        &["CAP-HTTP-181"],
        "027-FR-008 reading one agent run",
        async {
            let _lane = LANE.lock().await;
            let c = ctx();
            set_flag(&c, "on").await;
            let (a, b) = (owner(&c).await, owner(&c).await);
            let handoff = blocked_handoff(&a).await;
            let id = handoff.run_id();

            let run = expect(&get(&a, &format!("/api/agent-runs/{id}")).await, 200);
            assert_eq!(run["id"], id);
            assert_eq!(run["task_id"], handoff.task_id.as_str());
            assert_eq!(run["dispatch_state"], "sent");
            assert_eq!(run["reported_state"], "blocked");
            assert_eq!(
                run["correlation_id"], id,
                "the run keeps its own correlation id"
            );
            assert!(run["revision"].as_u64().is_some());
            assert!(run["events"].is_array());

            let foreign = get(&b, &format!("/api/agent-runs/{id}")).await;
            let missing = get(&b, &format!("/api/agent-runs/{ABSENT_RUN}")).await;
            assert_indistinguishable(&foreign, id, &missing, ABSENT_RUN);

            let anonymous = send(
                &c.client.fresh(),
                Method::GET,
                &format!("/api/agent-runs/{id}"),
                None,
                None,
            )
            .await;
            expect(&anonymous, 401);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_182_reply_to_agent_run_answers_a_blocked_run_even_with_the_flag_off() {
    case(&["CAP-HTTP-182"], "027-FR-001 replying to an agent run", async {
        let _lane = LANE.lock().await;
        let c = ctx();
        set_flag(&c, "on").await;
        let (a, b) = (owner(&c).await, owner(&c).await);
        let handoff = blocked_handoff(&a).await;
        let id = handoff.run_id().to_string();
        let revision = handoff.run["revision"].as_u64().expect("revision");
        let path = format!("/api/agent-runs/{id}/reply");
        let reply = |revision: u64| json!({"message": "The reply goes on the Next list", "expected_revision": revision});

        let no_key = post(&a, &path, reply(revision)).await;
        assert_eq!(expect(&no_key, 400)["message"], "Idempotency-Key header is required.");

        let stale = keyed(&a, Method::POST, &path, Some(reply(1))).await;
        assert_eq!(
            expect(&stale, 409)["message"],
            "This run changed elsewhere; reload and try again."
        );

        let foreign = keyed(&b, Method::POST, &path, Some(reply(revision))).await;
        let absent = format!("/api/agent-runs/{ABSENT_RUN}/reply");
        let missing = keyed(&b, Method::POST, &absent, Some(reply(revision))).await;
        assert_indistinguishable(&foreign, &id, &missing, ABSENT_RUN);

        // Not gated: answering a question on a run that already exists.
        set_flag(&c, "off").await;
        let answered = keyed(&a, Method::POST, &path, Some(reply(revision))).await;
        set_flag(&c, "on").await;
        expect(&answered, 200);
        let run = settle(&a, &id, |r| r["reported_state"] == "completed").await;
        assert_eq!(run["needs_user"], false);
        assert_eq!(run["primary_state_label"], "Agent reported complete");
    })
    .await;
}

#[tokio::test]
async fn cap_http_183_check_delivery_looks_the_run_up_without_the_flag() {
    case(
        &["CAP-HTTP-183"],
        "027-FR-001 checking delivery of an agent run",
        async {
            let _lane = LANE.lock().await;
            let c = ctx();
            set_flag(&c, "on").await;
            let (a, b) = (owner(&c).await, owner(&c).await);
            let handoff = blocked_handoff(&a).await;
            let id = handoff.run_id().to_string();
            let path = format!("/api/agent-runs/{id}/check-delivery");

            let no_key = post(&a, &path, json!({})).await;
            assert_eq!(
                expect(&no_key, 400)["message"],
                "Idempotency-Key header is required."
            );

            let foreign = keyed(&b, Method::POST, &path, Some(json!({}))).await;
            let absent = format!("/api/agent-runs/{ABSENT_RUN}/check-delivery");
            let missing = keyed(&b, Method::POST, &absent, Some(json!({}))).await;
            assert_indistinguishable(&foreign, &id, &missing, ABSENT_RUN);

            let checked = expect(&keyed(&a, Method::POST, &path, Some(json!({}))).await, 200);
            assert_eq!(checked["id"], id.as_str());
            assert_eq!(checked["dispatch_state"], "sent");
            assert_eq!(checked["reported_state"], "blocked");

            // The lookup is not gated; a run already delivered stays checkable.
            set_flag(&c, "off").await;
            let off = keyed(&a, Method::POST, &path, Some(json!({}))).await;
            set_flag(&c, "on").await;
            assert_eq!(expect(&off, 200)["dispatch_state"], "sent");
            let one = expect(
                &get(&a, &format!("/api/tasks/{}/agent-runs", handoff.task_id)).await,
                200,
            );
            assert_eq!(
                one.as_array().map(Vec::len),
                Some(1),
                "checking never sends a second copy"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_184_cancel_agent_run_sends_cancel_and_closes_the_run() {
    case(
        &["CAP-HTTP-184"],
        "027-FR-001 cancelling an agent run",
        async {
            let _lane = LANE.lock().await;
            let c = ctx();
            set_flag(&c, "on").await;
            let (a, b) = (owner(&c).await, owner(&c).await);
            let handoff = blocked_handoff(&a).await;
            let id = handoff.run_id().to_string();
            let path = format!("/api/agent-runs/{id}/cancel");

            let no_key = send(&a.client, Method::POST, &path, None, None).await;
            assert_eq!(
                expect(&no_key, 400)["message"],
                "Idempotency-Key header is required."
            );

            let foreign = keyed(&b, Method::POST, &path, None).await;
            let missing = keyed(
                &b,
                Method::POST,
                &format!("/api/agent-runs/{ABSENT_RUN}/cancel"),
                None,
            )
            .await;
            assert_indistinguishable(&foreign, &id, &missing, ABSENT_RUN);

            // Not gated: cancelling is always available to the owner.
            set_flag(&c, "off").await;
            let cancelled = keyed(&a, Method::POST, &path, None).await;
            set_flag(&c, "on").await;
            let cancelled = expect(&cancelled, 200);
            assert_eq!(cancelled["reported_state"], "cancelled");
            assert_eq!(cancelled["cancel_outcome"], "accepted");

            let again = keyed(&a, Method::POST, &path, None).await;
            let again = expect(&again, 400);
            assert_eq!(again["message"], "This run has already finished.");
            assert_eq!(again["detail"]["reason"], "run_terminal");
        },
    )
    .await;
}

/// A raw HTTP/1.1 push with `Transfer-Encoding: chunked` and no `Content-Length`,
/// whose body (a JSON string) totals `total` bytes. The shared client always
/// declares a length, so this speaks to the socket directly. Returns the
/// status and the raw response text.
async fn chunked_push(cfg: &Config, run: &str, token: &str, total: usize) -> (u16, String) {
    use std::io::{Read, Write};
    let authority = cfg
        .base_url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    let epoch = cfg
        .epoch
        .map(|e| format!("X-Client-Epoch: {e}\r\n"))
        .unwrap_or_default();
    let head = format!(
        "POST /api/a2a/push/{run}/{token} HTTP/1.1\r\nHost: {authority}\r\n{epoch}\
         Content-Type: application/json\r\nTransfer-Encoding: chunked\r\n\
         Connection: close\r\n\r\n"
    );
    // A JSON string of `total` bytes: two quotes around `total - 2` letters.
    let mut payload = vec![b'"'];
    payload.extend(std::iter::repeat_n(b'x', total - 2));
    payload.push(b'"');
    tokio::task::spawn_blocking(move || {
        let mut stream = std::net::TcpStream::connect(&authority).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .expect("read timeout");
        stream.write_all(head.as_bytes()).expect("send head");
        // The server may answer and close before the last chunk: ignore write errors.
        for chunk in payload.chunks(1_000) {
            let frame = format!("{:x}\r\n", chunk.len());
            let sent = stream
                .write_all(frame.as_bytes())
                .and_then(|()| stream.write_all(chunk))
                .and_then(|()| stream.write_all(b"\r\n"));
            if sent.is_err() {
                break;
            }
        }
        let _ = stream.write_all(b"0\r\n\r\n");
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        let text = String::from_utf8_lossy(&raw).into_owned();
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, text)
    })
    .await
    .expect("raw push task")
}

#[tokio::test]
async fn cap_http_185_push_callback_refuses_identically_and_caps_the_body() {
    case(
        &["CAP-HTTP-185"],
        "027-FR-008 inbound A2A push notification",
        async {
            let _lane = LANE.lock().await;
            let c = ctx();
            set_flag(&c, "on").await;
            let a = owner(&c).await;
            let handoff = blocked_handoff(&a).await;
            let id = handoff.run_id().to_string();

            // No session: the caller is the agent. Each refusal is the same answer.
            let anonymous = c.client.fresh();
            let push = |run: String, token: &'static str, bytes: usize| {
                let client = anonymous.clone();
                async move {
                    let path = format!("/api/a2a/push/{run}/{token}");
                    let body = Value::String("x".repeat(bytes));
                    send(&client, Method::POST, &path, Some(body), None).await
                }
            };
            let unknown = push(ABSENT_RUN.into(), "forged-token", 0).await;
            // A real run that is still open (blocked on the owner), forged token.
            let forged = push(id.clone(), "forged-token", 0).await;
            let cancel = keyed(
                &a,
                Method::POST,
                &format!("/api/agent-runs/{id}/cancel"),
                None,
            )
            .await;
            expect(&cancel, 200);
            // The run is now closed (cancelled): the closed-run case.
            let closed = push(id.clone(), "another-forged-token", 0).await;
            for refusal in [&unknown, &forged, &closed] {
                assert_eq!(refusal.status, 403, "{}", refusal.body);
                assert_eq!(message(refusal), "Notification rejected.");
                assert_eq!(json_of(refusal)["detail"], Value::Null);
                assert_eq!(
                    refusal.header("retry-after"),
                    None,
                    "no distinguishing header"
                );
                assert!(refusal.header("x-correlation-id").is_some());
            }
            assert_eq!(
                unknown.header("content-type"),
                forged.header("content-type")
            );
            assert_eq!(forged.header("content-type"), closed.header("content-type"));

            // The cap is checked before anything is looked up: 64 000 bytes pass
            // on to the (refusing) lookup, 64 001 are rejected outright, for a real
            // run and for an unknown one alike.
            let overhead = serde_json::to_string(&Value::String(String::new()))
                .expect("json")
                .len();
            let at_cap = push(id.clone(), "forged-token", 64_000 - overhead).await;
            assert_eq!(at_cap.status, 403, "{}", at_cap.body);
            for run in [id.clone(), ABSENT_RUN.to_string()] {
                let over = push(run, "forged-token", 64_001 - overhead).await;
                assert_eq!(over.status, 413, "{}", over.body);
                assert_eq!(message(&over), "Notification payload is too large.");
            }

            // No declared length: the cap is enforced while the chunks stream in.
            let cfg = Config::from_env().expect("conformance configuration");
            for run in [id.clone(), ABSENT_RUN.to_string()] {
                let (status, body) = chunked_push(&cfg, &run, "forged-token", 64_001).await;
                assert_eq!(status, 413, "streamed over the cap: {body}");
                assert!(
                    body.contains("Notification payload is too large."),
                    "streamed over the cap: {body}"
                );
            }
        },
    )
    .await;
}
