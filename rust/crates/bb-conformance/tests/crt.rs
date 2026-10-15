#![cfg(feature = "live")]
//! Black-box conformance for the CRT canvas routes, the legacy tree routes,
//! CRT correlation strictness and CRT idempotency keys (027-FR-001, 027-FR-013,
//! 027-FR-008, 027-SC-003; T113 / PR-112).
//!
//! Every test encodes today's Python behavior and seeds only through the test
//! control API. `CAP-HTTP-108` (node validation consent refusal) is a `change`
//! row owned by PR-121 and is deliberately not covered here.

use bb_conformance::{Client, Config, Owner, Reply, TestControl, case};
use reqwest::Method;
use serde_json::{Value, json};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

const PASSWORD: &str = "Conformance-pass-1!";

/// Serializes the cases: each `case` call rewrites the shared run report, and
/// concurrent read-merge-write cycles would drop entries.
static GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn gated<F>(caps: &[&str], title: &str, body: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let _gate = GATE.lock().await;
    case(caps, title, body).await;
}

fn config() -> Config {
    Config::from_env().expect("BB_CONFORMANCE_BASE_URL is set")
}

fn unique() -> u128 {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    nanos ^ (u128::from(N.fetch_add(1, Ordering::Relaxed)) << 96)
}

/// A canonical lowercase UUID, as the CRT `Idempotency-Key` header requires.
fn idem() -> String {
    let v = unique();
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        v >> 96,
        (v >> 80) & 0xffff,
        (v >> 68) & 0xfff,
        (v >> 56) & 0xfff,
        v & 0xffff_ffff_ffff
    )
}

fn with_key(key: &str) -> Vec<(&'static str, String)> {
    vec![("Idempotency-Key", key.to_string())]
}

/// Turns the CRT exposure flag on for every owner (idempotent).
async fn enable_crt(cfg: &Config) {
    TestControl::new(cfg)
        .expect("test control")
        .flag("crt_canvas", "on", &[])
        .await
        .expect("crt_canvas flag");
}

/// A synthetic owner signed in through the public signup route. The shared
/// owner factory mints `@conformance.invalid` addresses, which the Python
/// signup rejects as reserved, so this file mints its own.
async fn owner(cfg: &Config) -> Owner {
    let minted = TestControl::new(cfg)
        .expect("test control")
        .invites(1)
        .await
        .expect("invite");
    let code = minted["codes"][0]
        .as_str()
        .expect("invite code")
        .to_string();
    let email = format!("crt-{:x}@conformance.example.com", unique());
    let mut client = Client::new(cfg).expect("client");
    let reply = client
        .post(
            "/api/auth/signup",
            &json!({"email": email, "password": PASSWORD, "invite_code": code}),
        )
        .await
        .expect("signup");
    assert_eq!(reply.status, 201, "synthetic signup");
    Owner { email, client }
}

/// One request on the owner's session. Headers ride on this request only.
async fn send(
    client: &Client,
    method: Method,
    path: &str,
    body: Option<Value>,
    headers: &[(&str, String)],
) -> Reply {
    let mut req = client.clone();
    for (name, value) in headers {
        req = req.with_header(name, value);
    }
    req.send(method, path, body.as_ref())
        .await
        .expect("request reaches the server")
}

fn body_of(reply: &Reply) -> Value {
    reply
        .json()
        .unwrap_or_else(|e| panic!("json body ({e}): {}", reply.body))
}

fn str_field(v: &Value, key: &str) -> String {
    v[key]
        .as_str()
        .unwrap_or_else(|| panic!("string field `{key}` in {v}"))
        .to_string()
}

fn tree_body(name: &str, expected: Option<u64>) -> Value {
    let mut v = json!({
        "name": name,
        "schema_version": 1,
        "metadata": {
            "version": 1,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        },
        "nodes": [],
        "relations": []
    });
    if let Some(revision) = expected {
        v["expected_revision"] = json!(revision);
    }
    v
}

/// Legacy PUT body: the metadata's `updated_at` must match the stored tree or
/// the save is refused with 409 (no `expected_revision` on this route).
fn legacy_body(name: &str, metadata: &Value) -> Value {
    json!({
        "name": name,
        "schema_version": 1,
        "metadata": metadata,
        "nodes": [],
        "relations": []
    })
}

fn node_body(label: &str, kind: &str) -> Value {
    json!({"label": label, "type": kind, "position": {"x": 1.0, "y": 2.0}})
}

/// A foreign owner probing a tree id that is not theirs must see the same 404
/// as a missing id (027-FR-008): status, resource and no extra detail. Every
/// probe carries a fresh `Idempotency-Key` so CRT writes reach the owner check.
async fn assert_hidden(other: &Client, method: Method, path: &str, body: Option<Value>) {
    let reply = send(other, method, path, body, &with_key(&idem())).await;
    assert_eq!(reply.status, 404, "foreign or missing id answers 404");
    let parsed = body_of(&reply);
    assert_eq!(parsed["detail"]["resource"], "Tree", "{parsed}");
    assert_eq!(
        parsed["detail"].as_object().map(|o| o.len()),
        Some(2),
        "detail carries only resource and id: {parsed}"
    );
}

/// Creates a CRT tree through the command path and returns its id.
async fn crt_tree(client: &Client, name: &str) -> String {
    let made = send(
        client,
        Method::POST,
        "/api/crt/trees",
        Some(tree_body(name, None)),
        &with_key(&idem()),
    )
    .await;
    assert_eq!(made.status, 201, "CRT create");
    str_field(&body_of(&made), "id")
}

/// Creates a legacy tree and returns its id.
async fn legacy_tree(client: &Client, name: &str) -> String {
    let made = send(
        client,
        Method::POST,
        "/api/trees",
        Some(json!({"name": name})),
        &[],
    )
    .await;
    assert_eq!(made.status, 201, "legacy create");
    str_field(&body_of(&made), "id")
}

async fn legacy_node(client: &Client, tree: &str, label: &str, kind: &str) -> String {
    let made = send(
        client,
        Method::POST,
        &format!("/api/trees/{tree}/nodes"),
        Some(node_body(label, kind)),
        &[],
    )
    .await;
    assert_eq!(made.status, 201, "node create");
    str_field(&body_of(&made), "id")
}

async fn legacy_relation(client: &Client, tree: &str, source: &str, target: &str) -> String {
    let made = send(
        client,
        Method::POST,
        &format!("/api/trees/{tree}/relations"),
        Some(json!({"source_node_id": source, "target_node_id": target, "kind": "why"})),
        &[],
    )
    .await;
    assert_eq!(made.status, 201, "relation create");
    str_field(&body_of(&made), "id")
}

async fn legacy_version(client: &Client, tree: &str, label: &str) -> String {
    let made = send(
        client,
        Method::POST,
        &format!("/api/trees/{tree}/versions"),
        Some(json!({"label": label})),
        &[],
    )
    .await;
    assert_eq!(made.status, 201, "version create");
    str_field(&body_of(&made), "id")
}

// ---------------------------------------------------------------- CRT routes

#[tokio::test]
async fn cap_http_080_crt_exposure_probe() {
    gated(
        &["CAP-HTTP-080"],
        "cap_http_080_crt_exposure_probe",
        async {
            let cfg = config();
            enable_crt(&cfg).await;
            let o = owner(&cfg).await;
            let reply = send(&o.client, Method::GET, "/api/crt/exposure", None, &[]).await;
            assert_eq!(reply.status, 204);
            assert!(reply.body.is_empty(), "204 carries no body");
            let anon = Client::new(&cfg).expect("client");
            let anon_reply = send(&anon, Method::GET, "/api/crt/exposure", None, &[]).await;
            assert_eq!(anon_reply.status, 401, "no session, no probe");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_081_crt_list_is_owner_scoped() {
    gated(
        &["CAP-HTTP-081"],
        "cap_http_081_crt_list_is_owner_scoped",
        async {
            let cfg = config();
            enable_crt(&cfg).await;
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = crt_tree(&a.client, "Listed").await;
            let listed = send(&a.client, Method::GET, "/api/crt/trees", None, &[]).await;
            assert_eq!(listed.status, 200);
            let items = body_of(&listed);
            let mine = items
                .as_array()
                .expect("array")
                .iter()
                .find(|i| i["id"].as_str() == Some(tid.as_str()))
                .expect("own tree listed");
            assert_eq!(mine["name"], "Listed");
            assert!(mine["owner_id"].is_string());
            let other = send(&b.client, Method::GET, "/api/crt/trees", None, &[]).await;
            assert_eq!(other.status, 200);
            let foreign = body_of(&other);
            assert!(
                !foreign
                    .as_array()
                    .expect("array")
                    .iter()
                    .any(|i| i["id"].as_str() == Some(tid.as_str())),
                "another owner's tree is not listed"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_082_crt_create_replays_on_same_key() {
    gated(
        &["CAP-HTTP-082"],
        "cap_http_082_crt_create_replays_on_same_key",
        async {
            let cfg = config();
            enable_crt(&cfg).await;
            let o = owner(&cfg).await;
            let key = idem();
            let first = send(
                &o.client,
                Method::POST,
                "/api/crt/trees",
                Some(tree_body("Once", None)),
                &with_key(&key),
            )
            .await;
            assert_eq!(first.status, 201);
            let replay = send(
                &o.client,
                Method::POST,
                "/api/crt/trees",
                Some(tree_body("Once", None)),
                &with_key(&key),
            )
            .await;
            assert_eq!(replay.status, 201, "replay answers the stored 201");
            assert_eq!(
                str_field(&body_of(&first), "id"),
                str_field(&body_of(&replay), "id"),
                "replay returns the same tree"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_083_crt_import_creates_a_copy() {
    gated(
        &["CAP-HTTP-083"],
        "cap_http_083_crt_import_creates_a_copy",
        async {
            let cfg = config();
            enable_crt(&cfg).await;
            let o = owner(&cfg).await;
            let tid = crt_tree(&o.client, "Source").await;
            let exported = send(
                &o.client,
                Method::POST,
                &format!("/api/crt/trees/{tid}/export"),
                None,
                &[],
            )
            .await;
            assert_eq!(exported.status, 200);
            let tree = body_of(&exported)["tree"].clone();
            let imported = send(
                &o.client,
                Method::POST,
                "/api/crt/trees/import",
                Some(json!({"tree": tree})),
                &with_key(&idem()),
            )
            .await;
            assert_eq!(imported.status, 201);
            let copy = body_of(&imported);
            assert_ne!(str_field(&copy, "id"), tid, "import mints a new id");
            assert_eq!(copy["name"], "Source");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_084_crt_get_is_owner_scoped() {
    gated(
        &["CAP-HTTP-084"],
        "cap_http_084_crt_get_is_owner_scoped",
        async {
            let cfg = config();
            enable_crt(&cfg).await;
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = crt_tree(&a.client, "Readable").await;
            let got = send(
                &a.client,
                Method::GET,
                &format!("/api/crt/trees/{tid}"),
                None,
                &[],
            )
            .await;
            assert_eq!(got.status, 200);
            assert_eq!(str_field(&body_of(&got), "id"), tid);
            assert_hidden(
                &b.client,
                Method::GET,
                &format!("/api/crt/trees/{tid}"),
                None,
            )
            .await;
            assert_hidden(
                &b.client,
                Method::GET,
                "/api/crt/trees/tree_missing0000",
                None,
            )
            .await;
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_085_crt_update_checks_expected_revision() {
    gated(
        &["CAP-HTTP-085"],
        "cap_http_085_crt_update_checks_expected_revision",
        async {
            let cfg = config();
            enable_crt(&cfg).await;
            let o = owner(&cfg).await;
            let tid = crt_tree(&o.client, "Before").await;
            let path = format!("/api/crt/trees/{tid}");
            let key = idem();
            let updated = send(
                &o.client,
                Method::PUT,
                &path,
                Some(tree_body("After", Some(1))),
                &with_key(&key),
            )
            .await;
            assert_eq!(updated.status, 200);
            let body = body_of(&updated);
            assert_eq!(body["name"], "After");
            assert_eq!(body["revision"], 2);
            let replay = send(
                &o.client,
                Method::PUT,
                &path,
                Some(tree_body("After", Some(1))),
                &with_key(&key),
            )
            .await;
            assert_eq!(replay.status, 200, "same key replays the stored result");
            assert_eq!(
                body_of(&replay)["revision"],
                2,
                "replay does not bump again"
            );
            let stale = send(
                &o.client,
                Method::PUT,
                &path,
                Some(tree_body("Stale", Some(1))),
                &with_key(&idem()),
            )
            .await;
            assert_eq!(stale.status, 409, "stale revision is refused");
            assert_eq!(body_of(&stale)["detail"]["reason"], "stale_revision");
            let missing = send(
                &o.client,
                Method::PUT,
                &path,
                Some(tree_body("Unversioned", None)),
                &with_key(&idem()),
            )
            .await;
            assert_eq!(missing.status, 400);
            assert_eq!(
                body_of(&missing)["detail"]["reason"],
                "expected_revision_required"
            );
            assert_hidden(
                &owner(&cfg).await.client,
                Method::PUT,
                &path,
                Some(tree_body("Intruder", Some(2))),
            )
            .await;
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_086_crt_delete_checks_expected_revision() {
    gated(
        &["CAP-HTTP-086"],
        "cap_http_086_crt_delete_checks_expected_revision",
        async {
            let cfg = config();
            enable_crt(&cfg).await;
            let o = owner(&cfg).await;
            let tid = crt_tree(&o.client, "Doomed").await;
            let path = format!("/api/crt/trees/{tid}");
            assert_hidden(
                &owner(&cfg).await.client,
                Method::DELETE,
                &format!("{path}?expected_revision=1"),
                None,
            )
            .await;
            let key = idem();
            let stale = send(
                &o.client,
                Method::DELETE,
                &format!("{path}?expected_revision=5"),
                None,
                &with_key(&key),
            )
            .await;
            assert_eq!(stale.status, 409, "wrong revision is refused");
            let deleted = send(
                &o.client,
                Method::DELETE,
                &format!("{path}?expected_revision=1"),
                None,
                &with_key(&key),
            )
            .await;
            assert_eq!(deleted.status, 204);
            let replay = send(
                &o.client,
                Method::DELETE,
                &format!("{path}?expected_revision=1"),
                None,
                &with_key(&key),
            )
            .await;
            assert_eq!(replay.status, 204, "same key replays the tombstone");
            let gone = send(&o.client, Method::GET, &path, None, &[]).await;
            assert_eq!(gone.status, 404);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_087_crt_export_is_owner_scoped() {
    gated(
        &["CAP-HTTP-087"],
        "cap_http_087_crt_export_is_owner_scoped",
        async {
            let cfg = config();
            enable_crt(&cfg).await;
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = crt_tree(&a.client, "Exported").await;
            let path = format!("/api/crt/trees/{tid}/export");
            let exported = send(&a.client, Method::POST, &path, None, &[]).await;
            assert_eq!(exported.status, 200);
            let tree = body_of(&exported)["tree"].clone();
            assert_eq!(str_field(&tree, "id"), tid);
            assert_eq!(tree["name"], "Exported");
            assert_hidden(&b.client, Method::POST, &path, None).await;
        },
    )
    .await;
}

// ------------------------------------------------------------- legacy routes

#[tokio::test]
async fn cap_http_090_legacy_create_tree() {
    gated(
        &["CAP-HTTP-090"],
        "cap_http_090_legacy_create_tree",
        async {
            let cfg = config();
            let o = owner(&cfg).await;
            let made = send(
                &o.client,
                Method::POST,
                "/api/trees",
                Some(json!({"name": "Legacy"})),
                &[],
            )
            .await;
            assert_eq!(made.status, 201);
            let tree = body_of(&made);
            assert!(tree["id"].as_str().is_some_and(|id| !id.is_empty()));
            assert_eq!(tree["name"], "Legacy");
            assert_eq!(tree["revision"], 1);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_091_legacy_list_is_owner_scoped() {
    gated(
        &["CAP-HTTP-091"],
        "cap_http_091_legacy_list_is_owner_scoped",
        async {
            let cfg = config();
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = legacy_tree(&a.client, "Mine").await;
            let listed = send(&a.client, Method::GET, "/api/trees", None, &[]).await;
            assert_eq!(listed.status, 200);
            let items = body_of(&listed);
            let item = items
                .as_array()
                .expect("array")
                .iter()
                .find(|i| i["id"].as_str() == Some(tid.as_str()))
                .expect("own tree listed");
            for key in ["id", "name", "updated_at", "owner_id"] {
                assert!(item.get(key).is_some(), "list item carries `{key}`");
            }
            let other = send(&b.client, Method::GET, "/api/trees", None, &[]).await;
            assert!(
                !body_of(&other)
                    .as_array()
                    .expect("array")
                    .iter()
                    .any(|i| i["id"].as_str() == Some(tid.as_str())),
                "another owner's tree is not listed"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_092_legacy_get_is_owner_scoped() {
    gated(
        &["CAP-HTTP-092"],
        "cap_http_092_legacy_get_is_owner_scoped",
        async {
            let cfg = config();
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = legacy_tree(&a.client, "Readable").await;
            let path = format!("/api/trees/{tid}");
            let got = send(&a.client, Method::GET, &path, None, &[]).await;
            assert_eq!(got.status, 200);
            assert_eq!(str_field(&body_of(&got), "id"), tid);
            assert_hidden(&b.client, Method::GET, &path, None).await;
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_093_legacy_update_replaces_the_tree() {
    gated(
        &["CAP-HTTP-093"],
        "cap_http_093_legacy_update_replaces_the_tree",
        async {
            let cfg = config();
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = legacy_tree(&a.client, "Old").await;
            let path = format!("/api/trees/{tid}");
            let current = body_of(&send(&a.client, Method::GET, &path, None, &[]).await);
            let fresh = legacy_body("New", &current["metadata"]);
            let updated = send(&a.client, Method::PUT, &path, Some(fresh), &[]).await;
            assert_eq!(updated.status, 200);
            assert_eq!(body_of(&updated)["name"], "New");
            let stale = send(
                &a.client,
                Method::PUT,
                &path,
                Some(legacy_body("Stale", &current["metadata"])),
                &[],
            )
            .await;
            assert_eq!(stale.status, 409, "metadata from before the save is stale");
            assert_hidden(
                &b.client,
                Method::PUT,
                &path,
                Some(legacy_body("Intruder", &current["metadata"])),
            )
            .await;
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_094_legacy_delete_tree() {
    gated(
        &["CAP-HTTP-094"],
        "cap_http_094_legacy_delete_tree",
        async {
            let cfg = config();
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = legacy_tree(&a.client, "Gone").await;
            let path = format!("/api/trees/{tid}");
            assert_hidden(&b.client, Method::DELETE, &path, None).await;
            let deleted = send(&a.client, Method::DELETE, &path, None, &[]).await;
            assert_eq!(deleted.status, 204);
            let after = send(&a.client, Method::GET, &path, None, &[]).await;
            assert_eq!(after.status, 404);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_095_legacy_import_creates_a_copy() {
    gated(
        &["CAP-HTTP-095"],
        "cap_http_095_legacy_import_creates_a_copy",
        async {
            let cfg = config();
            let o = owner(&cfg).await;
            let tid = legacy_tree(&o.client, "Carried").await;
            let exported = send(
                &o.client,
                Method::POST,
                &format!("/api/trees/{tid}/export"),
                None,
                &[],
            )
            .await;
            assert_eq!(exported.status, 200);
            let tree = body_of(&exported)["tree"].clone();
            let imported = send(
                &o.client,
                Method::POST,
                "/api/trees/import",
                Some(json!({"tree": tree})),
                &[],
            )
            .await;
            assert_eq!(imported.status, 201);
            let copy = body_of(&imported);
            assert_ne!(str_field(&copy, "id"), tid);
            assert_eq!(copy["name"], "Carried");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_096_legacy_export_is_owner_scoped() {
    gated(
        &["CAP-HTTP-096"],
        "cap_http_096_legacy_export_is_owner_scoped",
        async {
            let cfg = config();
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = legacy_tree(&a.client, "Exported").await;
            let path = format!("/api/trees/{tid}/export");
            let exported = send(&a.client, Method::POST, &path, None, &[]).await;
            assert_eq!(exported.status, 200);
            assert_eq!(str_field(&body_of(&exported)["tree"], "id"), tid);
            assert_hidden(&b.client, Method::POST, &path, None).await;
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_097_ai_feedback_requires_consent() {
    gated(
        &["CAP-HTTP-097"],
        "cap_http_097_ai_feedback_requires_consent",
        async {
            let cfg = config();
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = legacy_tree(&a.client, "Reviewed").await;
            let path = format!("/api/trees/{tid}/ai-feedback");
            let refused = send(
                &a.client,
                Method::POST,
                &path,
                Some(json!({"consent": false})),
                &[],
            )
            .await;
            assert_eq!(refused.status, 400, "no consent, no feedback");
            let granted = send(
                &a.client,
                Method::POST,
                &path,
                Some(json!({"consent": true})),
                &[],
            )
            .await;
            assert_eq!(granted.status, 200);
            let feedback = body_of(&granted);
            assert_eq!(feedback["status"], "success", "deterministic local summary");
            assert!(feedback["summary"].is_string());
            assert!(feedback["recommendations"].is_array());
            assert_hidden(
                &b.client,
                Method::POST,
                &path,
                Some(json!({"consent": true})),
            )
            .await;
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_098_legacy_create_node() {
    gated(
        &["CAP-HTTP-098"],
        "cap_http_098_legacy_create_node",
        async {
            let cfg = config();
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = legacy_tree(&a.client, "Nodes").await;
            let path = format!("/api/trees/{tid}/nodes");
            let made = send(
                &a.client,
                Method::POST,
                &path,
                Some(node_body("Cause", "parent")),
                &[],
            )
            .await;
            assert_eq!(made.status, 201);
            let node = body_of(&made);
            assert_eq!(node["label"], "Cause");
            assert_eq!(node["type"], "parent");
            assert_eq!(node["relation_counts"]["up_count"], 0);
            assert_hidden(
                &b.client,
                Method::POST,
                &path,
                Some(node_body("Intruder", "child")),
            )
            .await;
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_099_legacy_patch_node() {
    gated(&["CAP-HTTP-099"], "cap_http_099_legacy_patch_node", async {
        let cfg = config();
        let a = owner(&cfg).await;
        let b = owner(&cfg).await;
        let tid = legacy_tree(&a.client, "Patch").await;
        let nid = legacy_node(&a.client, &tid, "Before", "parent").await;
        let path = format!("/api/trees/{tid}/nodes/{nid}");
        let patched = send(
            &a.client,
            Method::PATCH,
            &path,
            Some(json!({"label": "After"})),
            &[],
        )
        .await;
        assert_eq!(patched.status, 200);
        assert_eq!(body_of(&patched)["label"], "After");
        assert_hidden(
            &b.client,
            Method::PATCH,
            &path,
            Some(json!({"label": "Intruder"})),
        )
        .await;
    })
    .await;
}

#[tokio::test]
async fn cap_http_100_legacy_delete_node_cascades() {
    gated(
        &["CAP-HTTP-100"],
        "cap_http_100_legacy_delete_node_cascades",
        async {
            let cfg = config();
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = legacy_tree(&a.client, "Cascade").await;
            let cause = legacy_node(&a.client, &tid, "Cause", "parent").await;
            let effect = legacy_node(&a.client, &tid, "Effect", "child").await;
            legacy_relation(&a.client, &tid, &cause, &effect).await;
            assert_hidden(
                &b.client,
                Method::DELETE,
                &format!("/api/trees/{tid}/nodes/{effect}"),
                None,
            )
            .await;
            let deleted = send(
                &a.client,
                Method::DELETE,
                &format!("/api/trees/{tid}/nodes/{cause}?cascade=true"),
                None,
                &[],
            )
            .await;
            assert_eq!(deleted.status, 204);
            let tree = body_of(
                &send(
                    &a.client,
                    Method::GET,
                    &format!("/api/trees/{tid}"),
                    None,
                    &[],
                )
                .await,
            );
            assert_eq!(
                tree["relations"].as_array().map(Vec::len),
                Some(0),
                "cascade drops links"
            );
            assert_eq!(
                tree["nodes"].as_array().map(Vec::len),
                Some(1),
                "only the effect remains"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_101_legacy_create_relation() {
    gated(
        &["CAP-HTTP-101"],
        "cap_http_101_legacy_create_relation",
        async {
            let cfg = config();
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = legacy_tree(&a.client, "Links").await;
            let cause = legacy_node(&a.client, &tid, "Cause", "parent").await;
            let effect = legacy_node(&a.client, &tid, "Effect", "child").await;
            let path = format!("/api/trees/{tid}/relations");
            let made = send(
                &a.client,
                Method::POST,
                &path,
                Some(json!({"source_node_id": cause, "target_node_id": effect, "kind": "why"})),
                &[],
            )
            .await;
            assert_eq!(made.status, 201);
            let relation = body_of(&made);
            assert_eq!(relation["source_node_id"], cause);
            assert_eq!(relation["target_node_id"], effect);
            assert_eq!(relation["kind"], "why");
            assert_hidden(
                &b.client,
                Method::POST,
                &path,
                Some(json!({"source_node_id": cause, "target_node_id": effect})),
            )
            .await;
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_102_legacy_patch_relation() {
    gated(
        &["CAP-HTTP-102"],
        "cap_http_102_legacy_patch_relation",
        async {
            let cfg = config();
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = legacy_tree(&a.client, "Patch links").await;
            let cause = legacy_node(&a.client, &tid, "Cause", "parent").await;
            let effect = legacy_node(&a.client, &tid, "Effect", "child").await;
            let rid = legacy_relation(&a.client, &tid, &cause, &effect).await;
            let path = format!("/api/trees/{tid}/relations/{rid}");
            let patched = send(
                &a.client,
                Method::PATCH,
                &path,
                Some(json!({"kind": "why"})),
                &[],
            )
            .await;
            assert_eq!(patched.status, 200);
            assert_eq!(str_field(&body_of(&patched), "id"), rid);
            assert_hidden(
                &b.client,
                Method::PATCH,
                &path,
                Some(json!({"kind": "why"})),
            )
            .await;
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_103_legacy_delete_relation() {
    gated(
        &["CAP-HTTP-103"],
        "cap_http_103_legacy_delete_relation",
        async {
            let cfg = config();
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = legacy_tree(&a.client, "Unlink").await;
            let cause = legacy_node(&a.client, &tid, "Cause", "parent").await;
            let effect = legacy_node(&a.client, &tid, "Effect", "child").await;
            let rid = legacy_relation(&a.client, &tid, &cause, &effect).await;
            let path = format!("/api/trees/{tid}/relations/{rid}");
            assert_hidden(&b.client, Method::DELETE, &path, None).await;
            let deleted = send(&a.client, Method::DELETE, &path, None, &[]).await;
            assert_eq!(deleted.status, 204);
            let again = send(&a.client, Method::DELETE, &path, None, &[]).await;
            assert_eq!(again.status, 404, "a deleted relation is gone");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_104_legacy_create_version() {
    gated(
        &["CAP-HTTP-104"],
        "cap_http_104_legacy_create_version",
        async {
            let cfg = config();
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = legacy_tree(&a.client, "Snapshots").await;
            let path = format!("/api/trees/{tid}/versions");
            let made = send(
                &a.client,
                Method::POST,
                &path,
                Some(json!({"label": "v1"})),
                &[],
            )
            .await;
            assert_eq!(made.status, 201);
            let version = body_of(&made);
            assert!(version["id"].as_str().is_some_and(|id| !id.is_empty()));
            assert_eq!(version["label"], "v1");
            assert!(version["diff_summary"].is_object());
            assert_hidden(
                &b.client,
                Method::POST,
                &path,
                Some(json!({"label": "intruder"})),
            )
            .await;
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_105_legacy_list_versions() {
    gated(
        &["CAP-HTTP-105"],
        "cap_http_105_legacy_list_versions",
        async {
            let cfg = config();
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = legacy_tree(&a.client, "Listed versions").await;
            let vid = legacy_version(&a.client, &tid, "first").await;
            let path = format!("/api/trees/{tid}/versions");
            let listed = send(&a.client, Method::GET, &path, None, &[]).await;
            assert_eq!(listed.status, 200);
            let items = body_of(&listed);
            assert!(
                items
                    .as_array()
                    .expect("array")
                    .iter()
                    .any(|v| v["id"].as_str() == Some(vid.as_str())),
                "the snapshot is listed"
            );
            assert_hidden(&b.client, Method::GET, &path, None).await;
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_106_legacy_restore_version() {
    gated(
        &["CAP-HTTP-106"],
        "cap_http_106_legacy_restore_version",
        async {
            let cfg = config();
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = legacy_tree(&a.client, "Restorable").await;
            let vid = legacy_version(&a.client, &tid, "keep").await;
            let path = format!("/api/trees/{tid}/versions/{vid}/restore");
            let restored = send(&a.client, Method::POST, &path, None, &[]).await;
            assert_eq!(restored.status, 200);
            assert_eq!(str_field(&body_of(&restored), "id"), tid);
            assert_hidden(&b.client, Method::POST, &path, None).await;
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_107_legacy_delete_version() {
    gated(
        &["CAP-HTTP-107"],
        "cap_http_107_legacy_delete_version",
        async {
            let cfg = config();
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = legacy_tree(&a.client, "Prunable").await;
            let vid = legacy_version(&a.client, &tid, "drop").await;
            let path = format!("/api/trees/{tid}/versions/{vid}");
            assert_hidden(&b.client, Method::DELETE, &path, None).await;
            let deleted = send(&a.client, Method::DELETE, &path, None, &[]).await;
            assert_eq!(deleted.status, 204);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_109_legacy_validation_history() {
    gated(
        &["CAP-HTTP-109"],
        "cap_http_109_legacy_validation_history",
        async {
            let cfg = config();
            let a = owner(&cfg).await;
            let b = owner(&cfg).await;
            let tid = legacy_tree(&a.client, "Checked").await;
            let nid = legacy_node(&a.client, &tid, "Claim", "parent").await;
            let path = format!("/api/trees/{tid}/nodes/{nid}/validation-history");
            let history = send(&a.client, Method::GET, &path, None, &[]).await;
            assert_eq!(history.status, 200);
            assert_eq!(body_of(&history)["items"], json!([]), "no validations yet");
            assert_hidden(&b.client, Method::GET, &path, None).await;
        },
    )
    .await;
}

// ------------------------------------------- CRT Idempotency-Key (CAP-MW-016)

#[tokio::test]
async fn cap_mw_016_crt_writes_need_a_canonical_uuid_key() {
    gated(
        &["CAP-MW-016"],
        "cap_mw_016_crt_writes_need_a_canonical_uuid_key",
        async {
            let cfg = config();
            enable_crt(&cfg).await;
            let o = owner(&cfg).await;
            let missing = send(
                &o.client,
                Method::POST,
                "/api/crt/trees",
                Some(tree_body("NoKey", None)),
                &[],
            )
            .await;
            assert_eq!(missing.status, 400);
            assert_eq!(
                body_of(&missing)["detail"]["reason"],
                "missing_idempotency_key"
            );
            let uppercase = idem().to_uppercase();
            for bad in ["not-a-uuid", uppercase.as_str()] {
                let refused = send(
                    &o.client,
                    Method::POST,
                    "/api/crt/trees",
                    Some(tree_body("BadKey", None)),
                    &with_key(bad),
                )
                .await;
                assert_eq!(refused.status, 400, "rejects key {bad}");
                assert_eq!(
                    body_of(&refused)["detail"]["reason"],
                    "invalid_idempotency_key"
                );
            }
            let tid = crt_tree(&o.client, "Keyed").await;
            let no_key_delete = send(
                &o.client,
                Method::DELETE,
                &format!("/api/crt/trees/{tid}?expected_revision=1"),
                None,
                &[],
            )
            .await;
            assert_eq!(no_key_delete.status, 400);
            assert_eq!(
                body_of(&no_key_delete)["detail"]["reason"],
                "missing_idempotency_key"
            );
        },
    )
    .await;
}

// --------------------------------------------- CRT correlation (CAP-MW-018)

#[tokio::test]
async fn cap_mw_018_crt_correlation_headers_must_be_uuids() {
    gated(
        &["CAP-MW-018"],
        "cap_mw_018_crt_correlation_headers_must_be_uuids",
        async {
            let cfg = config();
            enable_crt(&cfg).await;
            let o = owner(&cfg).await;
            let probe = |name: &'static str, value: &str| vec![(name, value.to_string())];
            let bad_correlation = send(
                &o.client,
                Method::GET,
                "/api/crt/exposure",
                None,
                &probe("X-Correlation-ID", "not-a-uuid"),
            )
            .await;
            assert_eq!(bad_correlation.status, 400);
            assert_eq!(
                body_of(&bad_correlation)["detail"]["reason"],
                "invalid_correlation_id"
            );
            let upper_request = send(
                &o.client,
                Method::POST,
                "/api/crt/trees",
                Some(tree_body("Correlated", None)),
                &[
                    ("Idempotency-Key", idem()),
                    (
                        "X-Request-ID",
                        "AAAAAAAA-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_string(),
                    ),
                ],
            )
            .await;
            assert_eq!(upper_request.status, 400, "uppercase is not canonical");
            assert_eq!(
                body_of(&upper_request)["detail"]["reason"],
                "invalid_correlation_id"
            );
            let canonical = send(
                &o.client,
                Method::GET,
                "/api/crt/exposure",
                None,
                &probe("X-Correlation-ID", &idem()),
            )
            .await;
            assert_eq!(canonical.status, 204, "a canonical UUID passes");
        },
    )
    .await;
}
