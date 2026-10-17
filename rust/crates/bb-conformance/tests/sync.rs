#![cfg(feature = "live")]
//! Sync v1 conformance (027-FR-001, 027-FR-006, 027-FR-008, 027-SC-003): devices,
//! commands, receipt lookup, capabilities, delta, transfers, snapshots, hints.
//!
//! One test per inventory row, named `cap_<kind>_<nnn>_*`, plus two-owner probes
//! (a foreign id reads exactly like a missing one). Seeding goes through the test
//! control API only. Every test passes against the frozen Python server; the
//! `rust_core_sync` flag-off branches describe Python only (inventory #3: the
//! Rust server is always on), so they return early when
//! `BB_CONFORMANCE_SERVER=rust`.

use bb_conformance::{Client, Config, Owner, Reply, TestControl, case};
use reqwest::Method;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;

const FLAG: &str = "rust_core_sync";
const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;
const PASSWORD: &str = "Conformance-pass-1!";

/// The feature flag is process-global server state, so the tests take turns.
static TURN: Mutex<()> = Mutex::const_new(());

fn rust_server() -> bool {
    Config::from_env().is_ok_and(|c| c.server == "rust")
}

fn uuid() -> String {
    static N: AtomicU64 = AtomicU64::new(1);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = u128::from(N.fetch_add(1, Ordering::Relaxed));
    let tail = (t ^ (n << 96)) & 0xffff_ffff_ffff_ffff_ffff_ffff;
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (t >> 64) & 0xffff_ffff,
        (t >> 48) & 0xffff,
        (t >> 36) & 0xfff,
        (t >> 24) & 0xfff,
        tail & 0xffff_ffff_ffff
    )
}

/// One synthetic owner with the sync flag set, a registered device and the
/// other helpers the cases share.
struct World {
    cfg: Config,
    ctl: TestControl,
    owner: Owner,
    id: String,
}

impl World {
    async fn new(flag_on: bool) -> Self {
        let cfg = Config::from_env().expect("config");
        let ctl = TestControl::new(&cfg).expect("test control");
        let mode = if flag_on { "on" } else { "off" };
        ctl.flag(FLAG, mode, &[]).await.expect("set flag");
        let base = Client::new(&cfg).expect("client");
        let owner = signed_up(&ctl, &base).await;
        let mut w = Self {
            cfg,
            ctl,
            owner,
            id: String::new(),
        };
        w.id = w.get("/api/auth/me").await.json().expect("me")["id"]
            .as_str()
            .expect("owner id")
            .to_string();
        w
    }

    async fn flag(&self, on: bool) {
        let mode = if on { "on" } else { "off" };
        self.ctl.flag(FLAG, mode, &[]).await.expect("set flag");
    }

    /// A second, independent owner (the "foreign" side of a two-owner probe).
    async fn other(&self) -> (Owner, String) {
        let base = Client::new(&self.cfg).expect("client");
        let mut owner = signed_up(&self.ctl, &base).await;
        let me = owner
            .client
            .send(Method::GET, "/api/auth/me", None)
            .await
            .expect("me");
        let id = me.json().expect("me json")["id"]
            .as_str()
            .expect("id")
            .into();
        (owner, id)
    }

    fn anonymous(&self) -> Client {
        Client::new(&self.cfg).expect("client")
    }

    async fn get(&mut self, path: &str) -> Reply {
        get(&mut self.owner.client, path).await
    }

    async fn post(&mut self, path: &str, body: &Value) -> Reply {
        self.owner.client.post(path, body).await.expect("post")
    }

    async fn register(&mut self) -> Reply {
        let body = device(&self.id, "epoch-1");
        self.post("/api/sync/v1/devices", &body).await
    }

    /// Registers the device and submits one `project.create`; returns its id.
    async fn create_project(&mut self, name: &str) -> (String, Reply) {
        let command_id = uuid();
        let body = project_command(&self.id, &command_id, name);
        let reply = self.post("/api/sync/v1/commands", &body).await;
        (command_id, reply)
    }

    async fn ready(&mut self) {
        assert_eq!(self.register().await.status, 200, "device registration");
    }

    async fn snapshot(&mut self) -> Value {
        let body = json!({ "scope_id": self.id, "projection_schema_version": 1 });
        let reply = self.post("/api/sync/v1/snapshots", &body).await;
        assert_eq!(reply.status, 200, "snapshot: {}", reply.body);
        reply.json().expect("snapshot json")
    }
}

async fn get(client: &mut Client, path: &str) -> Reply {
    client.send(Method::GET, path, None).await.expect("get")
}

/// A new synthetic owner (an `example.com` address, signed up through an
/// invite from the test control API; the password is `PASSWORD`).
async fn signed_up(ctl: &TestControl, base: &Client) -> Owner {
    ctl.new_owner(base).await.expect("synthetic owner")
}

/// Device ids are server-global (another owner's id reads as unknown), so a
/// scope's device id is derived from the scope.
fn device_of(scope: &str) -> String {
    format!("dev-{scope}")
}

fn device(scope: &str, epoch: &str) -> Value {
    json!({
        "scope_id": scope, "device_id": device_of(scope),
        "device_epoch": epoch, "protocol_version": 1
    })
}

fn project_command(scope: &str, command_id: &str, name: &str) -> Value {
    json!({
        "protocol_version": 1, "command_id": command_id, "scope_id": scope,
        "device_id": device_of(scope), "device_epoch": "epoch-1", "local_sequence": "1",
        "type": "project.create", "command_version": 1,
        "entity_id": format!("project_{}", uuid()),
        "preconditions": [], "depends_on": [],
        "issued_at": "2026-10-08T10:00:00Z", "payload": { "name": name }
    })
}

/// An `ErrorBody` refusal (CAP-MW-011): shape, status, code, `no-store` and
/// the correlation id echoed in the header.
fn refused(reply: &Reply, status: u16, code: &str) {
    assert_eq!(reply.status, status, "status of {}", reply.body);
    let body = reply.json().expect("error body json");
    let object = body.as_object().expect("object");
    assert!(object.len() == 2 && object.contains_key("error"), "{body}");
    assert_eq!(body["error"]["code"], code, "{body}");
    for key in ["retryable", "message", "details"] {
        assert!(body["error"].get(key).is_some(), "error.{key} in {body}");
    }
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    let correlation = reply
        .header("x-correlation-id")
        .expect("correlation header");
    assert_eq!(body["correlation_id"], correlation);
}

/// A foreign or unknown scope: 404 `RESOURCE_NOT_FOUND`, with no echo of the id.
fn not_found(reply: &Reply, leaked: &str) {
    refused(reply, 404, "RESOURCE_NOT_FOUND");
    assert!(
        !reply.body.contains(leaked),
        "scope id echoed: {}",
        reply.body
    );
}

fn without_correlation(reply: &Reply) -> Value {
    let mut body = reply.json().expect("json");
    body.as_object_mut()
        .expect("object")
        .remove("correlation_id");
    body
}

fn scope_keys_present(body: &Value) {
    for key in [
        "scope_id",
        "server_generation",
        "server_now",
        "correlation_id",
    ] {
        assert!(body.get(key).is_some(), "{key} missing in {body}");
    }
}

// ---------------------------------------------------------------- CAP-HTTP-190

#[tokio::test]
async fn cap_http_190_registers_a_device_idempotently() {
    case(
        &["CAP-HTTP-190"],
        "POST /sync/v1/devices registers and replays",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            let first = w.register().await;
            assert_eq!(first.status, 200, "{}", first.body);
            let body = first.json().expect("json");
            scope_keys_present(&body);
            assert_eq!(body["scope_id"], w.id.as_str());
            assert_eq!(body["device_id"], device_of(&w.id).as_str());
            assert_eq!(body["device_epoch"], "epoch-1");
            assert_eq!(body["epoch_status"], "active");
            let again = w.register().await.json().expect("json");
            for key in [
                "device_id",
                "device_epoch",
                "epoch_status",
                "server_generation",
            ] {
                assert_eq!(again[key], body[key], "{key}");
            }
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_190_refuses_malformed_registrations() {
    case(
        &["CAP-HTTP-190", "CAP-MW-011"],
        "device registration validation",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            let good = device(&w.id, "epoch-1");
            let mut extra = good.clone();
            extra["extra"] = json!(1);
            let mut missing = good.clone();
            missing.as_object_mut().expect("object").remove("device_id");
            let mut empty = good.clone();
            empty["device_epoch"] = json!("");
            let mut stringy = good.clone();
            stringy["protocol_version"] = json!("1");
            let mut future = good.clone();
            future["protocol_version"] = json!(2);
            let cases = [
                (extra, 422, "INVALID_REQUEST"),
                (missing, 422, "INVALID_REQUEST"),
                (empty, 422, "INVALID_REQUEST"),
                (stringy, 422, "INVALID_REQUEST"),
                (future, 426, "UPGRADE_REQUIRED"),
                (json!([]), 400, "INVALID_REQUEST"),
            ];
            for (body, status, code) in cases {
                let reply = w.post("/api/sync/v1/devices", &body).await;
                refused(&reply, status, code);
            }
            let future = w
                .post(
                    "/api/sync/v1/devices",
                    &json!({ "protocol_version": 2, "scope_id": w.id,
                "device_id": "d", "device_epoch": "e" }),
                )
                .await;
            assert_eq!(
                future.json().expect("json")["error"]["details"]["reason"],
                "protocol_version"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_190_caps_the_body_at_64_kib() {
    case(&["CAP-HTTP-190"], "device registration body cap", async {
        let _turn = TURN.lock().await;
        let mut w = World::new(true).await;
        let mut big = device(&w.id, "epoch-1");
        big["device_id"] = json!("x".repeat(64 * KIB + 1));
        let reply = w.post("/api/sync/v1/devices", &big).await;
        refused(&reply, 413, "COMMAND_TOO_LARGE");
    })
    .await;
}

#[tokio::test]
async fn cap_http_190_is_owner_scoped_and_needs_a_session() {
    case(
        &["CAP-HTTP-190", "CAP-MW-021"],
        "device registration admission",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            let (_other, foreign) = w.other().await;
            let mut anon = w.anonymous();
            let reply = anon
                .post("/api/sync/v1/devices", &device(&w.id, "epoch-1"))
                .await
                .expect("post");
            refused(&reply, 401, "AUTH_REQUIRED");
            let foreign_reply = w
                .post("/api/sync/v1/devices", &device(&foreign, "epoch-1"))
                .await;
            not_found(&foreign_reply, &foreign);
            let unknown = w
                .post("/api/sync/v1/devices", &device("scope-unknown", "epoch-1"))
                .await;
            not_found(&unknown, "scope-unknown");
            assert_eq!(
                without_correlation(&foreign_reply),
                without_correlation(&unknown)
            );
            // A device id bound to this owner reads as unknown to another owner.
            w.ready().await;
            let (mut other, other_id) = w.other().await;
            let mut taken = device(&other_id, "epoch-1");
            taken["device_id"] = json!(device_of(&w.id));
            let reply = other
                .client
                .post("/api/sync/v1/devices", &taken)
                .await
                .expect("post");
            not_found(&reply, &w.id);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_190_flag_off_denies_registration() {
    case(
        &["CAP-HTTP-190"],
        "registration is gated by rust_core_sync",
        async {
            if rust_server() {
                return;
            }
            let _turn = TURN.lock().await;
            let mut w = World::new(false).await;
            refused(&w.register().await, 403, "ACCESS_DENIED");
        },
    )
    .await;
}

// ---------------------------------------------------------------- CAP-HTTP-191

#[tokio::test]
async fn cap_http_191_executes_a_command_and_replays_its_receipt() {
    case(
        &["CAP-HTTP-191"],
        "POST /sync/v1/commands accepts and replays",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            w.ready().await;
            let command_id = uuid();
            let body = project_command(&w.id, &command_id, "Sync project");
            let first = w.post("/api/sync/v1/commands", &body).await;
            assert_eq!(first.status, 200, "{}", first.body);
            let receipt = first.json().expect("json");
            scope_keys_present(&receipt);
            assert_eq!(receipt["command_id"], command_id.as_str());
            assert_eq!(receipt["outcome"], "accepted");
            assert_eq!(receipt["has_changes"], true);
            assert!(receipt["commit_seq"].is_string(), "{receipt}");
            assert!(receipt["error"].is_null());
            let replay = w.post("/api/sync/v1/commands", &body).await;
            assert_eq!(replay.status, 200);
            let again = replay.json().expect("json");
            for key in [
                "command_id",
                "outcome",
                "commit_seq",
                "result_versions",
                "id_bindings",
            ] {
                assert_eq!(again[key], receipt[key], "{key}");
            }
            let mut reused = body.clone();
            reused["payload"]["name"] = json!("A different project");
            let clash = w.post("/api/sync/v1/commands", &reused).await;
            refused(&clash, 409, "IDEMPOTENCY_KEY_REUSED");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_191_answers_a_replay_before_the_flag_check() {
    case(&["CAP-HTTP-191"], "replay precedes the flag check", async {
        if rust_server() {
            return;
        }
        let _turn = TURN.lock().await;
        let mut w = World::new(true).await;
        w.ready().await;
        let command_id = uuid();
        let body = project_command(&w.id, &command_id, "Replayed");
        assert_eq!(w.post("/api/sync/v1/commands", &body).await.status, 200);
        w.flag(false).await;
        let replay = w.post("/api/sync/v1/commands", &body).await;
        assert_eq!(replay.status, 200, "{}", replay.body);
        assert_eq!(replay.json().expect("json")["outcome"], "accepted");
        let fresh = project_command(&w.id, &uuid(), "Unseen");
        refused(
            &w.post("/api/sync/v1/commands", &fresh).await,
            403,
            "ACCESS_DENIED",
        );
    })
    .await;
}

#[tokio::test]
async fn cap_http_191_requires_an_upgrade_for_unsupported_commands() {
    case(
        &["CAP-HTTP-191"],
        "unsupported type or version is UPGRADE_REQUIRED",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            w.ready().await;
            let mut future = project_command(&w.id, &uuid(), "Future");
            future["command_version"] = json!(99);
            let reply = w.post("/api/sync/v1/commands", &future).await;
            refused(&reply, 426, "UPGRADE_REQUIRED");
            assert!(reply.json().expect("json")["error"]["details"]["reason"].is_string());
            let mut unknown = project_command(&w.id, &uuid(), "Unknown type");
            unknown["type"] = json!("project.explode");
            refused(
                &w.post("/api/sync/v1/commands", &unknown).await,
                426,
                "UPGRADE_REQUIRED",
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_191_caps_command_and_body_sizes() {
    case(
        &["CAP-HTTP-191"],
        "command 4 MiB cap and 16 MiB body cap",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            w.ready().await;
            let mut command = project_command(&w.id, &uuid(), "Large");
            command["payload"]["note"] = json!("x".repeat(4 * MIB + KIB));
            refused(
                &w.post("/api/sync/v1/commands", &command).await,
                413,
                "COMMAND_TOO_LARGE",
            );
            let mut body = project_command(&w.id, &uuid(), "Huge");
            body["payload"]["note"] = json!("x".repeat(16 * MIB + KIB));
            refused(
                &w.post("/api/sync/v1/commands", &body).await,
                413,
                "COMMAND_TOO_LARGE",
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_191_refuses_bad_bodies_and_foreign_scopes() {
    case(
        &["CAP-HTTP-191", "CAP-MW-021"],
        "command body and scope admission",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            w.ready().await;
            let (_other, foreign) = w.other().await;
            refused(
                &w.post("/api/sync/v1/commands", &json!([])).await,
                400,
                "INVALID_REQUEST",
            );
            let mut anon = w.anonymous();
            let body = project_command(&w.id, &uuid(), "Anonymous");
            let unauth = anon
                .post("/api/sync/v1/commands", &body)
                .await
                .expect("post");
            refused(&unauth, 401, "AUTH_REQUIRED");
            let foreign_reply = w
                .post(
                    "/api/sync/v1/commands",
                    &project_command(&foreign, &uuid(), "Foreign"),
                )
                .await;
            not_found(&foreign_reply, &foreign);
            let unknown_epoch = {
                let mut c = project_command(&w.id, &uuid(), "Unregistered");
                c["device_epoch"] = json!("epoch-never-registered");
                w.post("/api/sync/v1/commands", &c).await
            };
            refused(&unknown_epoch, 404, "RESOURCE_NOT_FOUND");
        },
    )
    .await;
}

// ---------------------------------------------------------------- CAP-HTTP-192

#[tokio::test]
async fn cap_http_192_looks_up_receipts_per_owner() {
    case(
        &["CAP-HTTP-192"],
        "GET /sync/v1/commands/{id} receipt lookup",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            w.ready().await;
            let (command_id, sent) = w.create_project("Looked up").await;
            assert_eq!(sent.status, 200, "{}", sent.body);
            let path = format!("/api/sync/v1/commands/{command_id}?scope_id={}", w.id);
            let found = w.get(&path).await;
            assert_eq!(found.status, 200);
            let body = found.json().expect("json");
            scope_keys_present(&body);
            assert_eq!(body["status"], "terminal");
            assert_eq!(body["receipt"]["command_id"], command_id.as_str());
            assert_eq!(body["receipt"]["outcome"], "accepted");
            let unknown = format!("/api/sync/v1/commands/{}?scope_id={}", uuid(), w.id);
            let missing = w.get(&unknown).await.json().expect("json");
            assert_eq!(missing["status"], "not_found");
            // Two owners: another owner's id reads exactly like a missing command.
            let (mut other, other_id) = w.other().await;
            let theirs = format!("/api/sync/v1/commands/{command_id}?scope_id={other_id}");
            let probe = get(&mut other.client, &theirs).await.json().expect("json");
            assert_eq!(probe["status"], "not_found");
            let wrong_scope = format!("/api/sync/v1/commands/{command_id}?scope_id={other_id}");
            not_found(&w.get(&wrong_scope).await, &other_id);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_192_needs_a_session_and_a_scope() {
    case(&["CAP-HTTP-192"], "receipt lookup admission", async {
        let _turn = TURN.lock().await;
        let mut w = World::new(true).await;
        let path = format!("/api/sync/v1/commands/{}?scope_id={}", uuid(), w.id);
        refused(&get(&mut w.anonymous(), &path).await, 401, "AUTH_REQUIRED");
        let no_scope = format!("/api/sync/v1/commands/{}", uuid());
        refused(&w.get(&no_scope).await, 422, "INVALID_REQUEST");
        let bad_id = format!("/api/sync/v1/commands/not-a-uuid?scope_id={}", w.id);
        refused(&w.get(&bad_id).await, 422, "INVALID_REQUEST");
    })
    .await;
}

#[tokio::test]
async fn cap_http_192_has_no_flag_check() {
    case(
        &["CAP-HTTP-192"],
        "receipt lookup survives flag off",
        async {
            if rust_server() {
                return;
            }
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            w.ready().await;
            let (command_id, _) = w.create_project("Recoverable").await;
            w.flag(false).await;
            let path = format!("/api/sync/v1/commands/{command_id}?scope_id={}", w.id);
            let found = w.get(&path).await;
            assert_eq!(found.status, 200);
            assert_eq!(found.json().expect("json")["status"], "terminal");
        },
    )
    .await;
}

// ---------------------------------------------------------------- CAP-HTTP-193

#[tokio::test]
async fn cap_http_193_reports_capabilities() {
    case(&["CAP-HTTP-193"], "GET /sync/v1/capabilities", async {
        let _turn = TURN.lock().await;
        let mut w = World::new(true).await;
        let reply = w
            .get(&format!("/api/sync/v1/capabilities?scope_id={}", w.id))
            .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        let body = reply.json().expect("json");
        scope_keys_present(&body);
        assert_eq!(body["protocol_versions"], json!([1]));
        assert_eq!(body["scope_enabled"], true);
        assert_eq!(body["rule_version"], 1);
        assert_eq!(body["projection_schema_version"], 1);
        assert_eq!(body["limits"]["command_bytes"], 4 * MIB);
        assert_eq!(body["limits"]["stable_replay_bytes"], 16 * MIB);
        assert_eq!(body["recovery"]["snapshot_supported"], true);
        let types = body["command_versions"].as_array().expect("array");
        assert_eq!(types.len(), 30);
        let create = types
            .iter()
            .find(|t| t["type"] == "project.create")
            .expect("project.create");
        assert_eq!(create["supported_versions"], json!([1]));
    })
    .await;
}

#[tokio::test]
async fn cap_http_193_is_owner_scoped_and_survives_flag_off() {
    case(
        &["CAP-HTTP-193", "CAP-MW-021"],
        "capabilities admission",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            let (_other, foreign) = w.other().await;
            let path = format!("/api/sync/v1/capabilities?scope_id={}", w.id);
            refused(&get(&mut w.anonymous(), &path).await, 401, "AUTH_REQUIRED");
            let theirs = w
                .get(&format!("/api/sync/v1/capabilities?scope_id={foreign}"))
                .await;
            not_found(&theirs, &foreign);
            let nobody = w
                .get("/api/sync/v1/capabilities?scope_id=scope-unknown")
                .await;
            assert_eq!(without_correlation(&theirs), without_correlation(&nobody));
            refused(
                &w.get("/api/sync/v1/capabilities").await,
                422,
                "INVALID_REQUEST",
            );
            if !rust_server() {
                w.flag(false).await;
                let off = w.get(&path).await;
                assert_eq!(off.status, 200);
                assert_eq!(off.json().expect("json")["scope_enabled"], false);
            }
        },
    )
    .await;
}

// ---------------------------------------------------------------- CAP-HTTP-194

#[tokio::test]
async fn cap_http_194_serves_changes_after_a_cursor() {
    case(
        &["CAP-HTTP-194"],
        "GET /sync/v1/changes delta feed",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            w.ready().await;
            let cursor = w.snapshot().await["cursor"]
                .as_str()
                .expect("cursor")
                .to_string();
            let (command_id, sent) = w.create_project("Delta project").await;
            assert_eq!(sent.status, 200, "{}", sent.body);
            let path = format!(
                "/api/sync/v1/changes?scope_id={}&cursor={cursor}&limit=10",
                w.id
            );
            let page = w.get(&path).await;
            assert_eq!(page.status, 200, "{}", page.body);
            let body = page.json().expect("json");
            scope_keys_present(&body);
            assert_eq!(body["from_cursor"], cursor.as_str());
            assert_eq!(body["has_more"], false);
            let transactions = body["transactions"].as_array().expect("transactions");
            assert_eq!(transactions.len(), 1, "{body}");
            assert_eq!(transactions[0]["source_command_id"], command_id.as_str());
            assert!(
                transactions[0]["changes"]
                    .as_array()
                    .is_some_and(|c| !c.is_empty())
            );
            assert_eq!(body["high_watermark"], transactions[0]["commit_seq"]);
            let next = body["next_cursor"].as_str().expect("next cursor");
            let empty = format!("/api/sync/v1/changes?scope_id={}&cursor={next}", w.id);
            let tail = w.get(&empty).await.json().expect("json");
            assert_eq!(tail["transactions"], json!([]));
            assert_eq!(tail["has_more"], false);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_194_refuses_bad_cursors_and_foreign_owners() {
    case(
        &["CAP-HTTP-194", "CAP-MW-021"],
        "delta admission and cursor rules",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            let cursor = w.snapshot().await["cursor"]
                .as_str()
                .expect("cursor")
                .to_string();
            let ok = format!("/api/sync/v1/changes?scope_id={}&cursor={cursor}", w.id);
            refused(&get(&mut w.anonymous(), &ok).await, 401, "AUTH_REQUIRED");
            let garbage = format!("/api/sync/v1/changes?scope_id={}&cursor=not-a-cursor", w.id);
            refused(&w.get(&garbage).await, 422, "INVALID_REQUEST");
            let zero = format!("{ok}&limit=0");
            refused(&w.get(&zero).await, 422, "INVALID_REQUEST");
            let no_cursor = format!("/api/sync/v1/changes?scope_id={}", w.id);
            refused(&w.get(&no_cursor).await, 422, "INVALID_REQUEST");
            let (mut other, other_id) = w.other().await;
            let foreign_scope = format!("/api/sync/v1/changes?scope_id={other_id}&cursor={cursor}");
            not_found(&w.get(&foreign_scope).await, &other_id);
            // A cursor never authorizes: another owner's cursor is a reset, not data.
            let borrowed = format!("/api/sync/v1/changes?scope_id={other_id}&cursor={cursor}");
            let reply = get(&mut other.client, &borrowed).await;
            refused(&reply, 409, "RESET_REQUIRED");
            assert_eq!(
                reply.json().expect("json")["error"]["details"]["reset_reason"],
                "feed_changed"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_194_is_gated_by_the_flag() {
    case(
        &["CAP-HTTP-194"],
        "delta feed is gated by rust_core_sync",
        async {
            if rust_server() {
                return;
            }
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            let cursor = w.snapshot().await["cursor"]
                .as_str()
                .expect("cursor")
                .to_string();
            w.flag(false).await;
            let path = format!("/api/sync/v1/changes?scope_id={}&cursor={cursor}", w.id);
            refused(&w.get(&path).await, 403, "ACCESS_DENIED");
        },
    )
    .await;
}

// ---------------------------------------------------------------- CAP-HTTP-195

#[tokio::test]
async fn cap_http_195_unknown_and_foreign_transfers_read_the_same() {
    case(
        &["CAP-HTTP-195", "CAP-MW-021"],
        "GET /sync/v1/transactions/{id}",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            let transfer = uuid();
            // A syntactically valid page token for a transfer that does not exist.
            let token = "eyJ2IjoxLCJ0IjoiLSIsImkiOjB9";
            let path = format!("/api/sync/v1/transactions/{transfer}?page_token={token}");
            let mine = w.get(&path).await;
            refused(&mine, 404, "RESOURCE_NOT_FOUND");
            let (mut other, _) = w.other().await;
            let theirs = get(&mut other.client, &path).await;
            assert_eq!(without_correlation(&mine), without_correlation(&theirs));
            refused(&get(&mut w.anonymous(), &path).await, 401, "AUTH_REQUIRED");
            let no_token = format!("/api/sync/v1/transactions/{transfer}");
            refused(&w.get(&no_token).await, 422, "INVALID_REQUEST");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_195_is_gated_by_the_flag() {
    case(
        &["CAP-HTTP-195"],
        "transfer pages are gated by rust_core_sync",
        async {
            if rust_server() {
                return;
            }
            let _turn = TURN.lock().await;
            let mut w = World::new(false).await;
            let path = format!("/api/sync/v1/transactions/{}?page_token=x", uuid());
            refused(&w.get(&path).await, 403, "ACCESS_DENIED");
        },
    )
    .await;
}

// ---------------------------------------------------------------- CAP-HTTP-196

#[tokio::test]
async fn cap_http_196_creates_a_snapshot() {
    case(&["CAP-HTTP-196"], "POST /sync/v1/snapshots", async {
        let _turn = TURN.lock().await;
        let mut w = World::new(true).await;
        w.ready().await;
        assert_eq!(w.create_project("In the snapshot").await.1.status, 200);
        let snap = w.snapshot().await;
        scope_keys_present(&snap);
        for key in [
            "snapshot_id",
            "watermark",
            "cursor",
            "sha256",
            "expires_at",
            "first_page_token",
        ] {
            assert!(snap[key].is_string(), "{key} in {snap}");
        }
        for key in ["page_count", "record_count", "total_bytes"] {
            assert!(snap[key].is_u64(), "{key} in {snap}");
        }
        assert!(snap["record_count"].as_u64().unwrap_or(0) >= 1);
    })
    .await;
}

#[tokio::test]
async fn cap_http_196_validates_and_admits() {
    case(
        &["CAP-HTTP-196", "CAP-MW-021"],
        "snapshot request admission",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            let (_other, foreign) = w.other().await;
            let good = json!({ "scope_id": w.id, "projection_schema_version": 1 });
            refused(
                &w.anonymous()
                    .post("/api/sync/v1/snapshots", &good)
                    .await
                    .expect("post"),
                401,
                "AUTH_REQUIRED",
            );
            let extra = json!({ "scope_id": w.id, "projection_schema_version": 1, "x": 1 });
            refused(
                &w.post("/api/sync/v1/snapshots", &extra).await,
                422,
                "INVALID_REQUEST",
            );
            let future = json!({ "scope_id": w.id, "projection_schema_version": 2 });
            let reply = w.post("/api/sync/v1/snapshots", &future).await;
            refused(&reply, 426, "UPGRADE_REQUIRED");
            let theirs = json!({ "scope_id": foreign, "projection_schema_version": 1 });
            not_found(&w.post("/api/sync/v1/snapshots", &theirs).await, &foreign);
            let big =
                json!({ "scope_id": "x".repeat(64 * KIB + 1), "projection_schema_version": 1 });
            refused(
                &w.post("/api/sync/v1/snapshots", &big).await,
                413,
                "COMMAND_TOO_LARGE",
            );
            if !rust_server() {
                w.flag(false).await;
                refused(
                    &w.post("/api/sync/v1/snapshots", &good).await,
                    403,
                    "ACCESS_DENIED",
                );
            }
        },
    )
    .await;
}

// ---------------------------------------------------------------- CAP-HTTP-197

#[tokio::test]
async fn cap_http_197_serves_snapshot_pages_per_owner() {
    case(
        &["CAP-HTTP-197", "CAP-MW-021"],
        "GET /sync/v1/snapshots/{id}",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            w.ready().await;
            assert_eq!(w.create_project("Paged").await.1.status, 200);
            let snap = w.snapshot().await;
            let (id, token) = (
                snap["snapshot_id"].as_str().expect("id"),
                snap["first_page_token"].as_str().expect("token"),
            );
            let path = format!(
                "/api/sync/v1/snapshots/{id}?scope_id={}&page_token={token}",
                w.id
            );
            let page = w.get(&path).await;
            assert_eq!(page.status, 200, "{}", page.body);
            let body = page.json().expect("json");
            scope_keys_present(&body);
            assert_eq!(body["snapshot_id"], id);
            assert_eq!(body["page_index"], 0);
            assert_eq!(body["page_sha256"].as_str().map(str::len), Some(64));
            assert!(
                body["payload_base64"]
                    .as_str()
                    .is_some_and(|p| !p.is_empty())
            );
            assert_eq!(
                body["has_more"],
                snap["page_count"].as_u64().unwrap_or(0) > 1
            );
            refused(&get(&mut w.anonymous(), &path).await, 401, "AUTH_REQUIRED");
            // Two owners: another owner's snapshot id reads like an unknown one.
            let (mut other, other_id) = w.other().await;
            let theirs =
                format!("/api/sync/v1/snapshots/{id}?scope_id={other_id}&page_token={token}");
            let foreign = get(&mut other.client, &theirs).await;
            refused(&foreign, 404, "RESOURCE_NOT_FOUND");
            let unknown = format!(
                "/api/sync/v1/snapshots/{}?scope_id={other_id}&page_token={token}",
                uuid()
            );
            let nobody = get(&mut other.client, &unknown).await;
            assert_eq!(without_correlation(&foreign), without_correlation(&nobody));
            let cross_scope =
                format!("/api/sync/v1/snapshots/{id}?scope_id={other_id}&page_token={token}");
            not_found(&w.get(&cross_scope).await, &other_id);
            let bad_token = format!(
                "/api/sync/v1/snapshots/{id}?scope_id={}&page_token=garbage",
                w.id
            );
            refused(&w.get(&bad_token).await, 422, "INVALID_REQUEST");
            let no_token = format!("/api/sync/v1/snapshots/{id}?scope_id={}", w.id);
            refused(&w.get(&no_token).await, 422, "INVALID_REQUEST");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_197_is_gated_by_the_flag() {
    case(
        &["CAP-HTTP-197"],
        "snapshot pages are gated by rust_core_sync",
        async {
            if rust_server() {
                return;
            }
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            let snap = w.snapshot().await;
            w.flag(false).await;
            let path = format!(
                "/api/sync/v1/snapshots/{}?scope_id={}&page_token={}",
                snap["snapshot_id"].as_str().expect("id"),
                w.id,
                snap["first_page_token"].as_str().expect("token")
            );
            refused(&w.get(&path).await, 403, "ACCESS_DENIED");
        },
    )
    .await;
}

// ---------------------------------------------------------------- CAP-HTTP-198

/// Signs in with a raw client so the SSE body can be read chunk by chunk.
async fn stream_client(w: &World) -> (reqwest::Client, String) {
    let http = reqwest::Client::new();
    let base = w.cfg.base_url.trim_end_matches('/').to_string();
    let login = json!({ "email": w.owner.email, "password": PASSWORD });
    let reply = http
        .post(format!("{base}/api/auth/login"))
        .json(&login)
        .send()
        .await
        .expect("login");
    assert!(reply.status().is_success(), "login {}", reply.status());
    let cookie = reply
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .collect::<Vec<_>>()
        .join("; ");
    (http, cookie)
}

async fn next_chunk(resp: &mut reqwest::Response, secs: u64) -> Option<String> {
    let chunk = tokio::time::timeout(Duration::from_secs(secs), resp.chunk())
        .await
        .expect("stream stalled")
        .expect("stream read")?;
    Some(String::from_utf8_lossy(&chunk).into_owned())
}

#[tokio::test]
async fn cap_http_198_streams_scope_changed_hints() {
    case(
        &["CAP-HTTP-198"],
        "GET /sync/v1/hints server-sent events",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            w.ready().await;
            let (http, cookie) = stream_client(&w).await;
            let url = format!(
                "{}/api/sync/v1/hints?scope_id={}",
                w.cfg.base_url.trim_end_matches('/'),
                w.id
            );
            // `Last-Event-ID` is ignored: it neither grants nor positions anything.
            let mut resp = http
                .get(url)
                .header("Cookie", cookie)
                .header("Last-Event-ID", "999")
                .send()
                .await
                .expect("hints");
            assert_eq!(resp.status().as_u16(), 200);
            let headers = resp.headers().clone();
            let text = |name: &str| {
                headers
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string()
            };
            assert!(text("content-type").starts_with("text/event-stream"));
            assert_eq!(text("cache-control"), "no-store");
            assert_eq!(text("x-accel-buffering"), "no");
            let first = next_chunk(&mut resp, 10).await.expect("initial event");
            assert!(first.starts_with("event: scope_changed\ndata: "), "{first}");
            let data = first
                .lines()
                .nth(1)
                .and_then(|l| l.strip_prefix("data: "))
                .expect("data");
            let event: Value = serde_json::from_str(data).expect("event json");
            assert_eq!(event["scope_id"], w.id.as_str());
            for key in [
                "server_generation",
                "feed_generation",
                "server_now",
                "correlation_id",
            ] {
                assert!(event.get(key).is_some(), "{key} in {event}");
            }
            // A committed change wakes the stream with a further event.
            assert_eq!(w.create_project("Wakes the stream").await.1.status, 200);
            let second = next_chunk(&mut resp, 10).await.expect("second event");
            assert!(second.starts_with("event: scope_changed"), "{second}");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_198_refuses_foreign_unknown_and_anonymous_streams() {
    case(&["CAP-HTTP-198", "CAP-MW-021"], "hints admission", async {
        let _turn = TURN.lock().await;
        let mut w = World::new(true).await;
        let (_other, foreign) = w.other().await;
        let theirs = w
            .get(&format!("/api/sync/v1/hints?scope_id={foreign}"))
            .await;
        not_found(&theirs, &foreign);
        let nobody = w.get("/api/sync/v1/hints?scope_id=scope-unknown").await;
        assert_eq!(without_correlation(&theirs), without_correlation(&nobody));
        let own = format!("/api/sync/v1/hints?scope_id={}", w.id);
        refused(&get(&mut w.anonymous(), &own).await, 401, "AUTH_REQUIRED");
        refused(&w.get("/api/sync/v1/hints").await, 422, "INVALID_REQUEST");
    })
    .await;
}

#[tokio::test]
async fn cap_http_198_rechecks_the_flag_during_the_stream() {
    case(
        &["CAP-HTTP-198"],
        "hints are gated by rust_core_sync, also mid-stream",
        async {
            if rust_server() {
                return;
            }
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            w.ready().await;
            let (http, cookie) = stream_client(&w).await;
            let url = format!(
                "{}/api/sync/v1/hints?scope_id={}",
                w.cfg.base_url.trim_end_matches('/'),
                w.id
            );
            let mut resp = http
                .get(&url)
                .header("Cookie", &cookie)
                .send()
                .await
                .expect("hints");
            assert_eq!(resp.status().as_u16(), 200);
            assert!(next_chunk(&mut resp, 10).await.is_some());
            w.flag(false).await;
            // The poller re-admits every 0.25 s; the stream then ends.
            loop {
                if next_chunk(&mut resp, 10).await.is_none() {
                    break;
                }
            }
            let denied = http
                .get(&url)
                .header("Cookie", &cookie)
                .send()
                .await
                .expect("hints");
            assert_eq!(denied.status().as_u16(), 403);
        },
    )
    .await;
}

// ------------------------------------------------------------------ CAP-MW-011

#[tokio::test]
async fn cap_mw_011_renders_every_refusal_as_an_error_body() {
    case(
        &["CAP-MW-011"],
        "sync ErrorBody with no-store and correlation id",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            let ok = format!("/api/sync/v1/capabilities?scope_id={}", w.id);
            let success = w.get(&ok).await;
            assert_eq!(success.status, 200);
            assert!(success.header("x-correlation-id").is_some());
            refused(&get(&mut w.anonymous(), &ok).await, 401, "AUTH_REQUIRED");
            refused(
                &w.get("/api/sync/v1/capabilities?scope_id=nobody").await,
                404,
                "RESOURCE_NOT_FOUND",
            );
            refused(
                &w.post("/api/sync/v1/devices", &json!([])).await,
                400,
                "INVALID_REQUEST",
            );
            refused(
                &w.get("/api/sync/v1/capabilities").await,
                422,
                "INVALID_REQUEST",
            );
            let mut future = device(&w.id, "e");
            future["protocol_version"] = json!(2);
            refused(
                &w.post("/api/sync/v1/devices", &future).await,
                426,
                "UPGRADE_REQUIRED",
            );
            let mut big = device(&w.id, "e");
            big["device_id"] = json!("x".repeat(64 * KIB + 1));
            refused(
                &w.post("/api/sync/v1/devices", &big).await,
                413,
                "COMMAND_TOO_LARGE",
            );
            // The correlation id the caller sends is the one the refusal carries.
            let mut tagged = w
                .owner
                .client
                .clone()
                .with_header("X-Correlation-ID", "conformance-corr-1");
            let reply = get(&mut tagged, "/api/sync/v1/capabilities?scope_id=nobody").await;
            refused(&reply, 404, "RESOURCE_NOT_FOUND");
        },
    )
    .await;
}

// ------------------------------------------------------------------ CAP-MW-021

#[tokio::test]
async fn cap_mw_021_scope_must_equal_the_owner_on_every_route() {
    case(
        &["CAP-MW-021"],
        "sync admission binds the scope to the owner",
        async {
            let _turn = TURN.lock().await;
            let mut w = World::new(true).await;
            w.ready().await;
            let (_other, foreign) = w.other().await;
            let cursor = w.snapshot().await["cursor"]
                .as_str()
                .expect("cursor")
                .to_string();
            let gets = [
                format!("/api/sync/v1/commands/{}?scope_id={foreign}", uuid()),
                format!("/api/sync/v1/capabilities?scope_id={foreign}"),
                format!("/api/sync/v1/changes?scope_id={foreign}&cursor={cursor}"),
                format!(
                    "/api/sync/v1/snapshots/{}?scope_id={foreign}&page_token=x",
                    uuid()
                ),
                format!("/api/sync/v1/hints?scope_id={foreign}"),
            ];
            for path in gets {
                not_found(&w.get(&path).await, &foreign);
            }
            let posts = [
                ("/api/sync/v1/devices", device(&foreign, "epoch-1")),
                (
                    "/api/sync/v1/snapshots",
                    json!({ "scope_id": foreign, "projection_schema_version": 1 }),
                ),
                (
                    "/api/sync/v1/commands",
                    project_command(&foreign, &uuid(), "Foreign"),
                ),
            ];
            for (path, body) in posts {
                not_found(&w.post(path, &body).await, &foreign);
            }
            // The caller's own scope is admitted on the same routes.
            let own = format!("/api/sync/v1/capabilities?scope_id={}", w.id);
            assert_eq!(w.get(&own).await.status, 200);
        },
    )
    .await;
}
