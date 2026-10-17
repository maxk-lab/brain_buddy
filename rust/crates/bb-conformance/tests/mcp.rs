#![cfg(feature = "live")]
//! Black-box conformance for the task MCP server (CAP-HTTP-007, CAP-MCP-001..007).
//!
//! Today's behavior, run against the frozen Python server at `/api/mcp/` and
//! later against the Rust server at `/api/mcp/v2/` (the owner-approved `change`
//! of CAP-HTTP-007). The endpoint is chosen from the report name
//! (`BB_CONFORMANCE_SERVER`), or `BB_CONFORMANCE_MCP_PATH` when set. Owners,
//! sessions and the `task_mcp` flag come from signup and the test control API;
//! tasks are made only through the MCP tools under test. Needs the server to
//! run with `BRAIN_BUDDY_MCP_ENABLED=1` and a Host allow-list admitting the
//! base URL's host. The last test is Rust-only and a no-op against Python.

use bb_conformance::{Client, Config, Reply, TestControl, case};
use reqwest::Method;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{OnceCell, RwLock, RwLockReadGuard};

const LEGACY_PATH: &str = "/api/mcp/";
const V2_PATH: &str = "/api/mcp/v2/";
const REFUSAL: &str = "Brain Buddy refused this request: this MCP address is out of date. \
Connect to the new Brain Buddy MCP address (/api/mcp/v2/), then try again. \
Nothing in your account was changed. Reference ID: ";
const INSTRUCTIONS: &str = "Manage the authenticated user's BrainBuddy tasks. Find a task before \
deleting it; never invent IDs. delete_task cancels an open task and \
removes it from active lists. Use a unique idempotency_key per mutation \
and reuse it with identical arguments when retrying.";

/// Tests hold the read side with `task_mcp` ON; the flag test holds the write side.
static GATE: RwLock<()> = RwLock::const_new(());
static FLAG_ON: OnceCell<()> = OnceCell::const_new();

type R<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn cfg() -> Config {
    Config::from_env().expect("BB_CONFORMANCE_BASE_URL must be set")
}

fn mcp_path(cfg: &Config) -> String {
    std::env::var("BB_CONFORMANCE_MCP_PATH")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| {
            if cfg.server == "rust" {
                V2_PATH
            } else {
                LEGACY_PATH
            }
            .to_string()
        })
}

fn uniq() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{t:x}{:x}", N.fetch_add(1, Ordering::Relaxed))
}

/// Shared lock held for a test that needs the `task_mcp` flag ON.
async fn flag_on() -> RwLockReadGuard<'static, ()> {
    let guard = GATE.read().await;
    FLAG_ON
        .get_or_init(|| async {
            let ctl = TestControl::new(&cfg()).expect("control");
            ctl.flag("task_mcp", "on", &[]).await.expect("flag on");
        })
        .await;
    guard
}

/// A signed-in synthetic owner and the session token used as the MCP bearer.
struct Caller {
    email: String,
    token: String,
    web: Client,
}

impl Caller {
    async fn new() -> R<Self> {
        let cfg = cfg();
        let codes = TestControl::new(&cfg)?.invites(1).await?;
        let email = format!("mcp-{}@conformance.example.com", uniq());
        let mut web = Client::new(&cfg)?;
        let body = json!({
            "email": email, "password": "Conformance-pass-1!",
            "invite_code": codes["codes"][0].as_str().ok_or("no invite code")?
        });
        let reply = web.post("/api/auth/signup", &body).await?;
        assert!(reply.status < 300, "signup answered {}", reply.status);
        let token = reply
            .headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok()?.split(';').next()?.split_once('='))
            .map(|(_, v)| v.to_string())
            .find(|v| !v.is_empty())
            .ok_or("signup set no session cookie")?;
        Ok(Self { email, token, web })
    }
}

async fn post_to(
    path: &str,
    token: Option<&str>,
    extra: &[(&str, &str)],
    body: &Value,
) -> R<Reply> {
    let cfg = cfg();
    let mut c = Client::new(&cfg)?.with_header("Accept", "application/json, text/event-stream");
    if let Some(t) = token {
        c = c
            .with_header("Authorization", &format!("Bearer {t}"))
            .with_header("MCP-Protocol-Version", "2025-06-18");
    }
    for (k, v) in extra {
        c = c.with_header(k, v);
    }
    c.send(Method::POST, path, Some(body)).await
}

fn envelope(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
}

async fn rpc(token: Option<&str>, extra: &[(&str, &str)], method: &str, params: Value) -> R<Reply> {
    post_to(&mcp_path(&cfg()), token, extra, &envelope(method, params)).await
}

/// `tools/call` that must be answered 200; returns the JSON-RPC `result`.
async fn call(who: &Caller, tool: &str, args: Value) -> Value {
    let params = json!({ "name": tool, "arguments": args });
    let reply = rpc(Some(&who.token), &[], "tools/call", params)
        .await
        .expect("rpc");
    assert_eq!(reply.status, 200, "{tool}: {}", reply.body);
    reply.json().expect("json body")["result"].clone()
}

fn is_error(result: &Value) -> bool {
    result["isError"] == json!(true)
}

fn ok(result: &Value) -> Value {
    assert!(!is_error(result), "unexpected tool error: {result}");
    result["structuredContent"].clone()
}

fn text(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

async fn create(who: &Caller, title: &str, key: &str) -> Value {
    ok(&call(
        who,
        "create_task",
        json!({ "title": title, "idempotency_key": key }),
    )
    .await)
}

async fn items(who: &Caller, args: Value) -> Vec<Value> {
    let page = ok(&call(who, "list_tasks", args).await);
    page["items"].as_array().cloned().unwrap_or_default()
}

fn ids(tasks: &[Value]) -> Vec<String> {
    tasks
        .iter()
        .filter_map(|t| t["id"].as_str())
        .map(String::from)
        .collect()
}

#[tokio::test]
async fn cap_http_007_bearer_session_required_cookie_is_not_enough() {
    case(
        &["CAP-HTTP-007"],
        "cap_http_007_bearer_session_required_cookie_is_not_enough",
        async {
            let _g = flag_on().await;
            let who = Caller::new().await.expect("caller");
            let cookie = format!("brainbuddy_session={}", who.token);
            let attempts: [Vec<(&str, &str)>; 4] = [
                vec![],
                vec![("Authorization", "Bearer invalid")],
                vec![("Authorization", "Basic invalid")],
                vec![("Cookie", cookie.as_str())],
            ];
            for extra in &attempts {
                let reply = rpc(None, extra, "tools/list", json!({}))
                    .await
                    .expect("rpc");
                assert_eq!(reply.status, 401, "{extra:?}");
                let challenge = reply.header("www-authenticate").unwrap_or_default();
                assert!(challenge.starts_with("Bearer "), "{challenge}");
            }
            let valid = rpc(Some(&who.token), &[], "tools/list", json!({}))
                .await
                .expect("rpc");
            assert_eq!(valid.status, 200);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_007_logout_revokes_the_bearer_token() {
    case(
        &["CAP-HTTP-007"],
        "cap_http_007_logout_revokes_the_bearer_token",
        async {
            let _g = flag_on().await;
            let mut who = Caller::new().await.expect("caller");
            let before = rpc(Some(&who.token), &[], "tools/list", json!({}))
                .await
                .expect("rpc");
            assert_eq!(before.status, 200);
            let out = who
                .web
                .post("/api/auth/logout", &json!({}))
                .await
                .expect("logout");
            assert!(out.status < 300, "logout answered {}", out.status);
            let after = rpc(Some(&who.token), &[], "tools/list", json!({}))
                .await
                .expect("rpc");
            assert_eq!(after.status, 401);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_007_task_mcp_flag_scopes_the_audience() {
    case(
        &["CAP-HTTP-007"],
        "cap_http_007_task_mcp_flag_scopes_the_audience",
        async {
            let _g = GATE.write().await;
            let (member, outsider) = (
                Caller::new().await.expect("a"),
                Caller::new().await.expect("b"),
            );
            let ctl = TestControl::new(&cfg()).expect("control");
            let list = |who: &Caller| {
                let token = who.token.clone();
                async move {
                    rpc(Some(&token), &[], "tools/list", json!({}))
                        .await
                        .expect("rpc")
                        .status
                }
            };
            ctl.flag("task_mcp", "off", &[]).await.expect("off");
            assert_eq!(list(&member).await, 403, "OFF admits nobody");
            let create_args = json!({ "title": "Denied", "idempotency_key": "denied" });
            let params = json!({ "name": "create_task", "arguments": create_args });
            let denied = rpc(Some(&member.token), &[], "tools/call", params)
                .await
                .expect("rpc");
            assert_eq!(denied.status, 403);
            ctl.flag("task_mcp", "selected", &[])
                .await
                .expect("selected");
            assert_eq!(list(&member).await, 403, "empty cohort admits nobody");
            ctl.flag("task_mcp", "selected", &[member.email.as_str()])
                .await
                .expect("cohort");
            assert_eq!(list(&member).await, 200);
            assert_eq!(
                list(&outsider).await,
                403,
                "a valid session outside the cohort"
            );
            ctl.flag("task_mcp", "on", &[]).await.expect("on");
            assert_eq!(list(&outsider).await, 200);
            assert!(
                items(&outsider, json!({})).await.is_empty(),
                "denied call created nothing"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_007_stateless_json_responses_and_tool_catalog() {
    case(
        &["CAP-HTTP-007"],
        "cap_http_007_stateless_json_responses_and_tool_catalog",
        async {
            let _g = flag_on().await;
            let who = Caller::new().await.expect("caller");
            let reply = rpc(Some(&who.token), &[], "tools/list", json!({}))
                .await
                .expect("rpc");
            assert_eq!(reply.status, 200);
            assert!(
                reply
                    .header("content-type")
                    .unwrap_or_default()
                    .starts_with("application/json")
            );
            assert!(
                reply.header("mcp-session-id").is_none(),
                "stateless: no session id"
            );
            let tools = reply.json().expect("json")["result"]["tools"].clone();
            let mut names: Vec<&str> = tools
                .as_array()
                .expect("tools")
                .iter()
                .filter_map(|t| t["name"].as_str())
                .collect();
            names.sort_unstable();
            assert_eq!(
                names,
                ["create_task", "delete_task", "get_task", "list_tasks"]
            );
            let hint = |name: &str, key: &str| {
                let tool = tools
                    .as_array()
                    .expect("tools")
                    .iter()
                    .find(|t| t["name"] == name)
                    .cloned();
                tool.expect("tool")["annotations"][key].clone()
            };
            assert_eq!(hint("list_tasks", "readOnlyHint"), json!(true));
            assert_eq!(hint("get_task", "readOnlyHint"), json!(true));
            assert_eq!(hint("create_task", "idempotentHint"), json!(true));
            assert_eq!(hint("delete_task", "destructiveHint"), json!(true));
            let again = rpc(Some(&who.token), &[], "tools/list", json!({}))
                .await
                .expect("rpc");
            assert_eq!(again.status, 200, "repeat request on the same token");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_007_session_termination_and_bad_requests_are_refused() {
    case(
        &["CAP-HTTP-007"],
        "cap_http_007_session_termination_and_bad_requests_are_refused",
        async {
            let _g = flag_on().await;
            let who = Caller::new().await.expect("caller");
            let mut c = Client::new(&cfg())
                .expect("client")
                .with_header("Accept", "application/json, text/event-stream")
                .with_header("Authorization", &format!("Bearer {}", who.token));
            let del = c
                .send(Method::DELETE, &mcp_path(&cfg()), None)
                .await
                .expect("delete");
            assert_eq!(del.status, 405, "no session to terminate");
            let junk = c
                .send(
                    Method::POST,
                    &mcp_path(&cfg()),
                    Some(&json!("not an rpc call")),
                )
                .await;
            assert!(junk.expect("junk").status < 500);
            let unknown = rpc(Some(&who.token), &[], "no/such/method", json!({}))
                .await
                .expect("rpc");
            assert!(
                unknown.json().expect("json")["error"].is_object(),
                "{}",
                unknown.body
            );
        },
    )
    .await;
}

#[tokio::test]
async fn cap_mcp_001_list_tasks_filters_pagination_and_owner_scope_027_fr_008() {
    let title = "cap_mcp_001_list_tasks_filters_pagination_and_owner_scope 027-FR-008";
    case(&["CAP-MCP-001"], title, async {
        let _g = flag_on().await;
        let (who, other) = (Caller::new().await.expect("a"), Caller::new().await.expect("b"));
        let tag = uniq();
        for n in 0..3 {
            let args = json!({
                "title": format!("Поиск {tag} {n}"), "idempotency_key": format!("list-{tag}-{n}"),
                "state": "waiting", "waiting_for": "Supplier"
            });
            ok(&call(&who, "create_task", args).await);
        }
        let plain = create(&who, &format!("Plain {tag}"), &format!("plain-{tag}")).await;
        let page = ok(&call(&who, "list_tasks", json!({})).await);
        assert_eq!(page["items"].as_array().map(Vec::len), Some(4));
        assert_eq!(page["has_more"], json!(false));
        assert!(page["counts_by_state"].is_object(), "{page}");
        let args = json!({ "q": format!("Поиск {tag}"), "state": "waiting", "limit": 2 });
        let first = ok(&call(&who, "list_tasks", args.clone()).await);
        assert_eq!(first["has_more"], json!(true));
        let cursor = first["next_cursor"].as_str().expect("cursor").to_string();
        let args = json!({ "q": format!("Поиск {tag}"), "state": "waiting", "limit": 2, "cursor": cursor });
        let second = ok(&call(&who, "list_tasks", args).await);
        assert_eq!(second["has_more"], json!(false));
        let mut both = ids(first["items"].as_array().expect("items"));
        both.extend(ids(second["items"].as_array().expect("items")));
        both.sort();
        both.dedup();
        assert_eq!(both.len(), 3, "the search finds exactly the three waiting tasks");
        assert!(!both.contains(&plain["id"].as_str().unwrap_or_default().to_string()));
        assert!(is_error(&call(&who, "list_tasks", json!({ "limit": 201 })).await));
        assert!(is_error(&call(&who, "list_tasks", json!({ "limit": 0 })).await));
        let cancel = json!({
            "task_id": plain["id"], "expected_revision": 1, "idempotency_key": format!("del-{tag}")
        });
        ok(&call(&who, "delete_task", cancel).await);
        assert_eq!(items(&who, json!({})).await.len(), 3, "cancelled tasks leave the default list");
        let all = items(&who, json!({ "include_cancelled": true })).await;
        assert!(ids(&all).contains(&plain["id"].as_str().unwrap_or_default().to_string()));
        assert!(items(&other, json!({})).await.is_empty(), "another owner sees nothing");
    })
    .await;
}

#[tokio::test]
async fn cap_mcp_002_get_task_detail_and_foreign_id_matches_missing_027_fr_008() {
    let title = "cap_mcp_002_get_task_detail_and_foreign_id_matches_missing 027-FR-008";
    case(&["CAP-MCP-002"], title, async {
        let _g = flag_on().await;
        let (who, other) = (
            Caller::new().await.expect("a"),
            Caller::new().await.expect("b"),
        );
        let task = create(&who, "Купить молоко", &format!("get-{}", uniq())).await;
        let id = task["id"].as_str().expect("id").to_string();
        let read = ok(&call(&who, "get_task", json!({ "task_id": id })).await);
        assert_eq!(read, task, "detail equals the created task");
        assert_eq!(read["revision"], json!(1));
        assert!(read["subtasks"].is_array() && read["comments"].is_array());
        let missing = "task_does_not_exist";
        let foreign = call(&other, "get_task", json!({ "task_id": id })).await;
        let absent = call(&other, "get_task", json!({ "task_id": missing })).await;
        assert!(is_error(&foreign) && is_error(&absent));
        assert_eq!(
            text(&foreign).replace(&id, "<id>"),
            text(&absent).replace(missing, "<id>"),
            "a foreign id is indistinguishable from a missing one"
        );
        assert!(
            !text(&foreign).contains("Купить молоко"),
            "no foreign content leaks"
        );
    })
    .await;
}

#[tokio::test]
async fn cap_mcp_003_create_task_defaults_replay_validation_and_scope_027_fr_008() {
    let title = "cap_mcp_003_create_task_defaults_replay_validation_and_scope 027-FR-008";
    case(&["CAP-MCP-003"], title, async {
        let _g = flag_on().await;
        let (who, other) = (
            Caller::new().await.expect("a"),
            Caller::new().await.expect("b"),
        );
        let key = format!("create-{}", uniq());
        let args = json!({ "title": "Купить молоко", "idempotency_key": key });
        let task = ok(&call(&who, "create_task", args.clone()).await);
        assert_eq!(task["state"], json!("inbox"));
        assert!(task.get("priority").is_none(), "no priority field: {task}");
        assert_eq!(
            ok(&call(&who, "create_task", args).await),
            task,
            "identical retry replays"
        );
        let changed = json!({ "title": "Other title", "idempotency_key": key });
        assert!(
            is_error(&call(&who, "create_task", changed).await),
            "same key, other payload"
        );
        let full = json!({
            "title": "Full", "idempotency_key": format!("full-{}", uniq()), "details": "Details",
            "state": "waiting", "waiting_for": "Supplier", "due_date": "2026-12-01"
        });
        let full = ok(&call(&who, "create_task", full).await);
        assert_eq!(full["waiting_for"], json!("Supplier"));
        assert_eq!(full["due_date"], json!("2026-12-01"));
        let before = items(&who, json!({})).await.len();
        let bad = [
            json!({ "title": "", "idempotency_key": "invalid-a" }),
            json!({ "title": "Valid", "idempotency_key": "" }),
            json!({ "title": "Valid", "idempotency_key": "invalid-b", "state": "waiting" }),
            json!({ "title": "Valid", "idempotency_key": "invalid-c", "due_date": "not-a-date" }),
            json!({ "title": "Valid", "idempotency_key": "invalid-d", "project_id": "foreign" }),
            json!({ "title": "x".repeat(501), "idempotency_key": "invalid-e" }),
        ];
        for args in bad {
            assert!(
                is_error(&call(&who, "create_task", args.clone()).await),
                "{args}"
            );
        }
        assert_eq!(
            items(&who, json!({})).await.len(),
            before,
            "invalid calls create nothing"
        );
        let theirs = create(&other, "Купить молоко", &key).await;
        assert_ne!(theirs["id"], task["id"], "idempotency keys are per owner");
    })
    .await;
}

#[tokio::test]
async fn cap_mcp_004_delete_task_cancels_checks_revision_and_scope_027_fr_008() {
    let title = "cap_mcp_004_delete_task_cancels_checks_revision_and_scope 027-FR-008";
    case(&["CAP-MCP-004"], title, async {
        let _g = flag_on().await;
        let (who, other) = (Caller::new().await.expect("a"), Caller::new().await.expect("b"));
        let task = create(&who, "Удалить", &format!("del-create-{}", uniq())).await;
        let id = task["id"].as_str().expect("id").to_string();
        let stale = json!({ "task_id": id, "expected_revision": 2, "idempotency_key": format!("s-{}", uniq()) });
        assert!(is_error(&call(&who, "delete_task", stale).await), "stale revision fails");
        let still = ok(&call(&who, "get_task", json!({ "task_id": id })).await);
        assert_eq!((still["state"].clone(), still["revision"].clone()), (json!("inbox"), json!(1)));
        let theirs = json!({ "task_id": id, "expected_revision": 1, "idempotency_key": format!("o-{}", uniq()) });
        let foreign = call(&other, "delete_task", theirs).await;
        let missing = json!({ "task_id": "task_does_not_exist", "expected_revision": 1, "idempotency_key": format!("m-{}", uniq()) });
        let absent = call(&other, "delete_task", missing).await;
        assert!(is_error(&foreign) && is_error(&absent));
        assert_eq!(
            text(&foreign).replace(&id, "<id>"),
            text(&absent).replace("task_does_not_exist", "<id>"),
            "a foreign id is indistinguishable from a missing one"
        );
        let args = json!({ "task_id": id, "expected_revision": 1, "idempotency_key": format!("d-{}", uniq()) });
        let deleted = call(&who, "delete_task", args.clone()).await;
        assert_eq!(ok(&deleted)["state"], json!("cancelled"));
        assert_eq!(ok(&deleted)["revision"], json!(2));
        assert_eq!(call(&who, "delete_task", args).await, deleted, "identical retry replays");
        assert!(!ids(&items(&who, json!({})).await).contains(&id));
        assert!(ids(&items(&who, json!({ "include_cancelled": true })).await).contains(&id));
        let after = call(&other, "get_task", json!({ "task_id": id })).await;
        assert!(is_error(&after), "the foreign owner still cannot read it");
    })
    .await;
}

#[tokio::test]
async fn cap_mcp_005_initialize_returns_the_fixed_instructions() {
    case(
        &["CAP-MCP-005"],
        "cap_mcp_005_initialize_returns_the_fixed_instructions",
        async {
            let _g = flag_on().await;
            let who = Caller::new().await.expect("caller");
            let params = json!({
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": { "name": "conformance", "version": "1" }
            });
            let reply = rpc(Some(&who.token), &[], "initialize", params)
                .await
                .expect("rpc");
            assert_eq!(reply.status, 200, "{}", reply.body);
            let result = reply.json().expect("json")["result"].clone();
            assert_eq!(result["instructions"], json!(INSTRUCTIONS));
            assert_eq!(result["serverInfo"]["name"], json!("BrainBuddy Tasks"));
            assert!(result["capabilities"]["tools"].is_object());
        },
    )
    .await;
}

#[tokio::test]
async fn cap_mcp_006_host_allow_list_and_no_allowed_origins() {
    case(
        &["CAP-MCP-006"],
        "cap_mcp_006_host_allow_list_and_no_allowed_origins",
        async {
            let _g = flag_on().await;
            let who = Caller::new().await.expect("caller");
            let token = Some(who.token.as_str());
            let host = rpc(
                token,
                &[("Host", "attacker.example")],
                "tools/list",
                json!({}),
            )
            .await
            .expect("rpc");
            assert!(
                [403, 421].contains(&host.status),
                "foreign Host refused: {}",
                host.status
            );
            let origin = rpc(
                token,
                &[("Origin", "https://attacker.example")],
                "tools/list",
                json!({}),
            )
            .await
            .expect("rpc");
            assert!(
                [403, 421].contains(&origin.status),
                "any Origin refused: {}",
                origin.status
            );
            let own_origin = rpc(
                token,
                &[("Origin", cfg().base_url.as_str())],
                "tools/list",
                json!({}),
            )
            .await
            .expect("rpc");
            assert!(
                [403, 421].contains(&own_origin.status),
                "no origin is allowed"
            );
            let plain = rpc(token, &[], "tools/list", json!({})).await.expect("rpc");
            assert_eq!(plain.status, 200, "the base URL's own host is admitted");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_mcp_007_transport_is_live_from_startup_and_survives_many_requests() {
    let title = "cap_mcp_007_transport_is_live_from_startup_and_survives_many_requests";
    case(&["CAP-MCP-007"], title, async {
        let _g = flag_on().await;
        let who = Caller::new().await.expect("caller");
        for n in 0..3 {
            let reply = rpc(Some(&who.token), &[], "tools/list", json!({}))
                .await
                .expect("rpc");
            assert_eq!(reply.status, 200, "request {n}: {}", reply.body);
        }
        let task = create(&who, "Lifespan", &format!("life-{}", uniq())).await;
        let read = ok(&call(&who, "get_task", json!({ "task_id": task["id"] })).await);
        assert_eq!(
            read["id"], task["id"],
            "stateless calls need no earlier handshake"
        );
    })
    .await;
}

/// Rust-only `change` rows (027-FR-018): serve at `/api/mcp/v2/`; the old path
/// answers every call with the design.md refusal and runs no tool.
#[tokio::test]
async fn cap_http_007_rust_serves_v2_and_old_path_refuses_every_call() {
    if cfg().server != "rust" {
        return;
    }
    let title = "cap_http_007_rust_serves_v2_and_old_path_refuses_every_call";
    case(&["CAP-HTTP-007"], title, async {
        let _g = flag_on().await;
        let who = Caller::new().await.expect("caller");
        let token = Some(who.token.as_str());
        let live = post_to(V2_PATH, token, &[], &envelope("tools/list", json!({}))).await.expect("v2");
        assert_eq!(live.status, 200, "{}", live.body);
        for (method, params) in [
            ("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "c", "version": "1" } })),
            ("tools/list", json!({})),
        ] {
            let old = post_to(LEGACY_PATH, token, &[], &envelope(method, params)).await.expect("old");
            let message = old.json().expect("json")["error"]["message"].as_str().unwrap_or_default().to_string();
            assert!(message.starts_with(REFUSAL), "{method}: {message}");
        }
        let args = json!({ "title": "Never created", "idempotency_key": format!("old-{}", uniq()) });
        let params = json!({ "name": "create_task", "arguments": args });
        let old = post_to(LEGACY_PATH, token, &[], &envelope("tools/call", params)).await.expect("old");
        let result = old.json().expect("json")["result"].clone();
        assert!(is_error(&result) && text(&result).starts_with(REFUSAL), "{result}");
        let seen = post_to(V2_PATH, token, &[], &envelope("tools/call", json!({ "name": "list_tasks", "arguments": {} }))).await.expect("v2");
        let seen = seen.json().expect("json")["result"]["structuredContent"]["items"].clone();
        assert_eq!(seen, json!([]), "no tool ran on the old path");
    })
    .await;
}
