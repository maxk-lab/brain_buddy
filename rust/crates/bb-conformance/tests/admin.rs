#![cfg(feature = "live")]
//! Black-box conformance for operator administration (spec 027): the `port`
//! rows CAP-HTTP-060..070 and the operator denial audit CAP-MW-020 in
//! `specs/027-rust-server/contracts/capability-inventory.md`.
//!
//! Each test encodes what the Python server does today and must pass against
//! it before PR-152 and PR-153 port the routes. Seeding goes only through the
//! test control API.
//!
//! Launch requirements (the server, not this file, owns them):
//! - `BRAIN_BUDDY_ADMIN_OPERATOR_EMAILS` lists `OPERATOR_EMAIL`;
//! - `BRAIN_BUDDY_ENV=test` with `BRAIN_BUDDY_TEST_CONTROL_TOKEN` set;
//! - `BB_CONFORMANCE_SERVER_LOG` names the server's captured log (CAP-MW-020).

use bb_conformance::{Client, Config, Owner, Reply, TestControl, case};
use reqwest::Method;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

/// Operator allow-listed on the server (see the module docs).
const OPERATOR_EMAIL: &str = "conformance-operator@example.com";
const PASSWORD: &str = "Conformance-pass-1!";
/// An id that names no account on any server.
const MISSING_ID: &str = "user_conformance_missing";
/// Managed flags from `MANAGED_FLAGS` (feature_flag.py); each test owns one.
const MANAGED: [&str; 7] = [
    "voice_brain_dump",
    "external_agent_relay",
    "task_title_autocomplete",
    "crt_canvas",
    "task_mcp",
    "weekly_review",
    "rust_core_sync",
];
const ACCOUNT_KEYS: [&str; 4] = ["id", "email", "display_name", "deletion_requested"];

/// `case` merges into the per-server run report with an unlocked
/// read-modify-write, so cases in this binary run one at a time. Each one
/// still owns its flags and accounts; this only orders the report writes.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn serial<F: Future<Output = ()> + Send + 'static>(caps: &[&str], title: &str, body: F) {
    let _turn = SERIAL.lock().await;
    case(caps, title, body).await;
}

fn cfg() -> Config {
    Config::from_env().expect("BB_CONFORMANCE_BASE_URL is set")
}

fn unique(prefix: &str) -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock is after the epoch")
        .as_nanos();
    format!("{prefix}{nanos:x}{}", N.fetch_add(1, Ordering::Relaxed))
}

fn anon(cfg: &Config) -> Client {
    Client::new(cfg).expect("client")
}

async fn call(c: &mut Client, method: Method, path: &str, body: Option<Value>) -> Reply {
    c.send(method, path, body.as_ref()).await.expect("request")
}

fn status_is(r: &Reply, want: u16) {
    assert_eq!(r.status, want, "unexpected status, body: {}", r.body);
}

fn message(r: &Reply) -> String {
    r.json().expect("JSON error body")["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn key_set(v: &Value) -> BTreeSet<&str> {
    v.as_object()
        .expect("JSON object")
        .keys()
        .map(String::as_str)
        .collect()
}

fn accounts(r: &Reply) -> Vec<Value> {
    r.json().expect("JSON")["accounts"]
        .as_array()
        .expect("accounts array")
        .clone()
}

fn flag_entry(body: &Value, name: &str) -> Value {
    body["flags"]
        .as_array()
        .expect("flags array")
        .iter()
        .find(|f| f["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("flag {name} is not listed"))
}

/// The test control API takes `selected`, not the admin route's `selected_users`.
fn ctl_mode(mode: &str) -> &str {
    if mode == "selected_users" {
        "selected"
    } else {
        mode
    }
}

/// Seeds the allow-listed operator through test control, then signs in.
async fn operator(cfg: &Config, ctl: &TestControl) -> Client {
    ctl.admin(OPERATOR_EMAIL, PASSWORD)
        .await
        .expect("seed operator");
    let mut c = anon(cfg);
    let r = c
        .post(
            "/api/auth/login",
            &json!({"email": OPERATOR_EMAIL, "password": PASSWORD}),
        )
        .await
        .expect("login");
    status_is(&r, 200);
    c
}

/// A synthetic member signed up through an invite minted by test control.
/// Like `TestControl::new_owner`, with an `example.com` address the server's
/// email validator accepts.
async fn new_member(cfg: &Config, ctl: &TestControl) -> Owner {
    let invites = ctl.invites(1).await.expect("mint invite");
    let code = invites["codes"][0]
        .as_str()
        .expect("invite code")
        .to_string();
    let email = format!("{}@example.com", unique("member"));
    let mut c = anon(cfg);
    let body = json!({"email": email, "password": PASSWORD, "invite_code": code});
    let r = c.post("/api/auth/signup", &body).await.expect("signup");
    status_is(&r, 201);
    Owner { email, client: c }
}

/// One operator session and one non-operator member, both synthetic.
struct World {
    cfg: Config,
    ctl: TestControl,
    op: Client,
    member: Owner,
}

impl World {
    async fn new() -> Self {
        let cfg = cfg();
        let ctl = TestControl::new(&cfg).expect("test control client");
        let op = operator(&cfg, &ctl).await;
        let member = new_member(&cfg, &ctl).await;
        Self {
            cfg,
            ctl,
            op,
            member,
        }
    }
}

async fn who(c: &mut Client) -> Value {
    let r = call(c, Method::GET, "/api/auth/me", None).await;
    status_is(&r, 200);
    r.json().expect("me JSON")
}

async fn create_account(op: &mut Client) -> Value {
    let email = format!("{}@example.com", unique("acct"));
    let r = call(
        op,
        Method::POST,
        "/api/admin/accounts",
        Some(json!({"email": email, "display_name": "Conformance", "password": PASSWORD})),
    )
    .await;
    status_is(&r, 201);
    r.json().expect("account JSON")
}

#[tokio::test]
async fn cap_http_060_list_accounts_is_operator_only() {
    serial(
        &["CAP-HTTP-060"],
        "cap_http_060 list accounts is operator only",
        async move {
            let mut w = World::new().await;
            let listed = call(&mut w.op, Method::GET, "/api/admin/accounts", None).await;
            status_is(&listed, 200);
            let rows = accounts(&listed);
            for row in &rows {
                assert_eq!(key_set(row), BTreeSet::from(ACCOUNT_KEYS));
            }
            let emails: Vec<&str> = rows.iter().filter_map(|r| r["email"].as_str()).collect();
            assert!(emails.contains(&OPERATOR_EMAIL));
            assert!(emails.contains(&w.member.email.as_str()));

            let denied = call(
                &mut w.member.client,
                Method::GET,
                "/api/admin/accounts",
                None,
            )
            .await;
            status_is(&denied, 403);
            assert_eq!(message(&denied), "Not authorized.");

            let unauth = call(&mut anon(&w.cfg), Method::GET, "/api/admin/accounts", None).await;
            status_is(&unauth, 401);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_061_create_account_answers_201_and_is_listed() {
    serial(
        &["CAP-HTTP-061"],
        "cap_http_061 create account answers 201 and is listed",
        async move {
            let mut w = World::new().await;
            let created = create_account(&mut w.op).await;
            assert_eq!(key_set(&created), BTreeSet::from(ACCOUNT_KEYS));
            assert_eq!(created["deletion_requested"], false);
            let id = created["id"].as_str().expect("id").to_string();

            let listed = call(&mut w.op, Method::GET, "/api/admin/accounts", None).await;
            assert!(accounts(&listed).iter().any(|r| r["id"] == id.as_str()));

            let email = format!("{}@example.com", unique("denied"));
            let denied = call(
                &mut w.member.client,
                Method::POST,
                "/api/admin/accounts",
                Some(json!({"email": email, "password": PASSWORD})),
            )
            .await;
            status_is(&denied, 403);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_062_update_account_renames_and_guards_operator_and_missing() {
    serial(
        &["CAP-HTTP-062"],
        "cap_http_062 update account renames and guards operator and missing",
        async move {
            let mut w = World::new().await;
            let created = create_account(&mut w.op).await;
            let id = created["id"].as_str().expect("id").to_string();
            let renamed = format!("{}@example.com", unique("renamed"));

            let updated = call(
                &mut w.op,
                Method::PUT,
                &format!("/api/admin/accounts/{id}"),
                Some(json!({"email": renamed, "display_name": "Renamed"})),
            )
            .await;
            status_is(&updated, 200);
            let body = updated.json().expect("JSON");
            assert_eq!(key_set(&body), BTreeSet::from(ACCOUNT_KEYS));
            assert_eq!(body["email"], renamed.as_str());
            assert_eq!(body["display_name"], "Renamed");

            // The operator may not move their own address away: that would
            // strip the allow-listed identity. An unchanged email is accepted.
            let own = who(&mut w.op).await;
            let own_id = own["id"].as_str().expect("operator id").to_string();
            let lost = format!("{}@example.com", unique("authority-lost"));
            let self_update = call(
                &mut w.op,
                Method::PUT,
                &format!("/api/admin/accounts/{own_id}"),
                Some(json!({"email": lost, "display_name": null})),
            )
            .await;
            status_is(&self_update, 403);
            let still = who(&mut w.op).await;
            assert_eq!(still["email"], OPERATOR_EMAIL);

            let missing = call(
                &mut w.op,
                Method::PUT,
                &format!("/api/admin/accounts/{MISSING_ID}"),
                Some(json!({"email": renamed, "display_name": null})),
            )
            .await;
            status_is(&missing, 404);

            let member_id = who(&mut w.member.client).await["id"]
                .as_str()
                .expect("member id")
                .to_string();
            let denied = call(
                &mut w.member.client,
                Method::PUT,
                &format!("/api/admin/accounts/{member_id}"),
                Some(json!({"email": renamed, "display_name": null})),
            )
            .await;
            status_is(&denied, 403);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_063_delete_account_answers_200_and_unlists() {
    serial(
        &["CAP-HTTP-063"],
        "cap_http_063 delete account answers 200 and unlists",
        async move {
            let mut w = World::new().await;
            let created = create_account(&mut w.op).await;
            let id = created["id"].as_str().expect("id").to_string();

            let deleted = call(
                &mut w.op,
                Method::DELETE,
                &format!("/api/admin/accounts/{id}"),
                None,
            )
            .await;
            status_is(&deleted, 200);
            assert_eq!(
                deleted.json().expect("JSON"),
                json!({"account_id": id, "deleted": true})
            );
            let listed = call(&mut w.op, Method::GET, "/api/admin/accounts", None).await;
            assert!(!accounts(&listed).iter().any(|r| r["id"] == id.as_str()));

            let missing = call(
                &mut w.op,
                Method::DELETE,
                &format!("/api/admin/accounts/{MISSING_ID}"),
                None,
            )
            .await;
            status_is(&missing, 404);

            let denied = call(
                &mut w.member.client,
                Method::DELETE,
                &format!("/api/admin/accounts/{id}"),
                None,
            )
            .await;
            status_is(&denied, 403);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_064_status_is_the_operator_capability_probe() {
    serial(
        &["CAP-HTTP-064"],
        "cap_http_064 status is the operator capability probe",
        async move {
            let mut w = World::new().await;
            let probe = call(&mut w.op, Method::GET, "/api/admin/status", None).await;
            status_is(&probe, 200);
            assert_eq!(probe.json().expect("JSON"), json!({"is_operator": true}));

            let denied = call(&mut w.member.client, Method::GET, "/api/admin/status", None).await;
            status_is(&denied, 403);
            assert!(denied.json().expect("JSON").get("is_operator").is_none());

            let unauth = call(&mut anon(&w.cfg), Method::GET, "/api/admin/status", None).await;
            status_is(&unauth, 401);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_065_lookup_is_exact_match_or_404() {
    serial(
        &["CAP-HTTP-065"],
        "cap_http_065 lookup is exact match or 404",
        async move {
            let mut w = World::new().await;
            let member = who(&mut w.member.client).await;
            let member_id = member["id"].as_str().expect("member id").to_string();
            let email = w.member.email.clone();

            let by_id = call(
                &mut w.op,
                Method::POST,
                "/api/admin/accounts/lookup",
                Some(json!({"account_id": member_id})),
            )
            .await;
            status_is(&by_id, 200);
            let body = by_id.json().expect("JSON");
            assert_eq!(key_set(&body), BTreeSet::from(ACCOUNT_KEYS));
            assert_eq!(body["id"], member_id.as_str());
            assert_eq!(body["email"], email.as_str());

            let by_email = call(
                &mut w.op,
                Method::POST,
                "/api/admin/accounts/lookup",
                Some(json!({"email": email})),
            )
            .await;
            status_is(&by_email, 200);
            assert_eq!(by_email.json().expect("JSON")["id"], member_id.as_str());

            for body in [json!({}), json!({"account_id": member_id, "email": email})] {
                let r = call(
                    &mut w.op,
                    Method::POST,
                    "/api/admin/accounts/lookup",
                    Some(body),
                )
                .await;
                status_is(&r, 422);
            }

            let missing = call(
                &mut w.op,
                Method::POST,
                "/api/admin/accounts/lookup",
                Some(json!({"account_id": MISSING_ID})),
            )
            .await;
            status_is(&missing, 404);
            assert_eq!(message(&missing), "No account found.");

            let denied = call(
                &mut w.member.client,
                Method::POST,
                "/api/admin/accounts/lookup",
                Some(json!({"email": email})),
            )
            .await;
            status_is(&denied, 403);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_066_revoke_sessions_signs_the_account_out_and_is_idempotent() {
    serial(
        &["CAP-HTTP-066"],
        "cap_http_066 revoke sessions signs the account out and is idempotent",
        async move {
            let mut w = World::new().await;
            let member_id = who(&mut w.member.client).await["id"]
                .as_str()
                .expect("member id")
                .to_string();
            let path = format!("/api/admin/accounts/{member_id}/revoke-sessions");

            let first = call(&mut w.op, Method::POST, &path, None).await;
            status_is(&first, 200);
            assert!(first.json().expect("JSON")["revoked_count"].as_u64() >= Some(1));

            let second = call(&mut w.op, Method::POST, &path, None).await;
            status_is(&second, 200);
            assert_eq!(second.json().expect("JSON")["revoked_count"], 0);

            let me = call(&mut w.member.client, Method::GET, "/api/auth/me", None).await;
            status_is(&me, 401);

            let missing = call(
                &mut w.op,
                Method::POST,
                &format!("/api/admin/accounts/{MISSING_ID}/revoke-sessions"),
                None,
            )
            .await;
            status_is(&missing, 404);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_067_feature_flags_lists_every_managed_flag() {
    serial(
        &["CAP-HTTP-067"],
        "cap_http_067 feature flags lists every managed flag",
        async move {
            let mut w = World::new().await;
            let listed = call(&mut w.op, Method::GET, "/api/admin/feature-flags", None).await;
            status_is(&listed, 200);
            let body = listed.json().expect("JSON");
            assert_eq!(body["degraded"], false);
            let flags = body["flags"].as_array().expect("flags array");
            let names: BTreeSet<&str> = flags.iter().filter_map(|f| f["name"].as_str()).collect();
            for name in MANAGED {
                assert!(names.contains(name), "managed flag {name} is not listed");
            }
            for flag in flags {
                assert!(matches!(
                    flag["mode"].as_str(),
                    Some("off" | "on" | "selected_users")
                ));
                assert!(flag["selected_users"].is_array());
            }

            let denied = call(
                &mut w.member.client,
                Method::GET,
                "/api/admin/feature-flags",
                None,
            )
            .await;
            status_is(&denied, 403);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_068_set_flag_mode_round_trips_and_refuses_bad_input() {
    serial(
        &["CAP-HTTP-068"],
        "cap_http_068 set flag mode round-trips and refuses bad input",
        async move {
            let mut w = World::new().await;
            let flag = "crt_canvas";
            let before = call(&mut w.op, Method::GET, "/api/admin/feature-flags", None).await;
            let initial = flag_entry(&before.json().expect("JSON"), flag)["mode"]
                .as_str()
                .expect("mode")
                .to_string();
            let path = format!("/api/admin/feature-flags/{flag}/mode");

            for mode in ["on", "off"] {
                let set = call(&mut w.op, Method::PUT, &path, Some(json!({"mode": mode}))).await;
                status_is(&set, 200);
                let entry = flag_entry(&set.json().expect("JSON"), flag);
                assert_eq!(entry["mode"], mode);
            }

            let malformed = call(
                &mut w.op,
                Method::PUT,
                &path,
                Some(json!({"mode": "sideways"})),
            )
            .await;
            status_is(&malformed, 422);

            let unmanaged = call(
                &mut w.op,
                Method::PUT,
                "/api/admin/feature-flags/not_a_managed_flag/mode",
                Some(json!({"mode": "on"})),
            )
            .await;
            status_is(&unmanaged, 400);

            let denied = call(
                &mut w.member.client,
                Method::PUT,
                &path,
                Some(json!({"mode": "on"})),
            )
            .await;
            status_is(&denied, 403);

            w.ctl
                .flag(flag, ctl_mode(&initial), &[])
                .await
                .expect("restore flag mode");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_069_add_selected_user_is_exact_match_and_repeatable() {
    serial(
        &["CAP-HTTP-069"],
        "cap_http_069 add selected user is exact match and repeatable",
        async move {
            let mut w = World::new().await;
            let flag = "task_mcp";
            let path = format!("/api/admin/feature-flags/{flag}/selected-users");
            let member_id = who(&mut w.member.client).await["id"]
                .as_str()
                .expect("member id")
                .to_string();
            // A cohort only changes in selected_users mode (the server refuses
            // otherwise), so put the flag there through test control first.
            let before = call(&mut w.op, Method::GET, "/api/admin/feature-flags", None).await;
            let initial = flag_entry(&before.json().expect("JSON"), flag)["mode"]
                .as_str()
                .expect("mode")
                .to_string();
            w.ctl
                .flag(flag, "selected", &[])
                .await
                .expect("select cohort mode");

            let added = call(
                &mut w.op,
                Method::POST,
                &path,
                Some(json!({"email": w.member.email})),
            )
            .await;
            status_is(&added, 200);
            let cohort = flag_entry(&added.json().expect("JSON"), flag)["selected_users"].clone();
            assert_eq!(cohort.as_array().map(Vec::len), Some(1));
            assert_eq!(cohort[0]["account_id"], member_id.as_str());
            assert_eq!(cohort[0]["email"], w.member.email.as_str());

            let repeat = call(
                &mut w.op,
                Method::POST,
                &path,
                Some(json!({"account_id": member_id})),
            )
            .await;
            status_is(&repeat, 200);
            let again = flag_entry(&repeat.json().expect("JSON"), flag)["selected_users"].clone();
            assert_eq!(again, cohort);

            let missing = call(
                &mut w.op,
                Method::POST,
                &path,
                Some(json!({"account_id": MISSING_ID})),
            )
            .await;
            status_is(&missing, 404);
            assert_eq!(message(&missing), "No account found.");

            let denied = call(
                &mut w.member.client,
                Method::POST,
                &path,
                Some(json!({"account_id": member_id})),
            )
            .await;
            status_is(&denied, 403);

            let cleanup = call(
                &mut w.op,
                Method::DELETE,
                &format!("{path}/{member_id}"),
                None,
            )
            .await;
            status_is(&cleanup, 200);

            w.ctl
                .flag(flag, ctl_mode(&initial), &[])
                .await
                .expect("restore flag mode");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_070_remove_selected_user_is_idempotent_by_id() {
    serial(
        &["CAP-HTTP-070"],
        "cap_http_070 remove selected user is idempotent by id",
        async move {
            let mut w = World::new().await;
            let flag = "task_title_autocomplete";
            let member_id = who(&mut w.member.client).await["id"]
                .as_str()
                .expect("member id")
                .to_string();
            let before = call(&mut w.op, Method::GET, "/api/admin/feature-flags", None).await;
            let initial = flag_entry(&before.json().expect("JSON"), flag)["mode"]
                .as_str()
                .expect("mode")
                .to_string();
            w.ctl
                .flag(flag, "selected", &[w.member.email.as_str()])
                .await
                .expect("seed cohort through test control");
            let path = format!("/api/admin/feature-flags/{flag}/selected-users/{member_id}");

            let removed = call(&mut w.op, Method::DELETE, &path, None).await;
            status_is(&removed, 200);
            let after = flag_entry(&removed.json().expect("JSON"), flag)["selected_users"].clone();
            assert!(
                !after
                    .as_array()
                    .expect("array")
                    .iter()
                    .any(|u| u["account_id"] == member_id.as_str())
            );

            let repeat = call(&mut w.op, Method::DELETE, &path, None).await;
            status_is(&repeat, 200);
            assert_eq!(
                flag_entry(&repeat.json().expect("JSON"), flag)["selected_users"],
                after
            );

            let unknown = call(
                &mut w.op,
                Method::DELETE,
                &format!("/api/admin/feature-flags/{flag}/selected-users/{MISSING_ID}"),
                None,
            )
            .await;
            status_is(&unknown, 200);

            let denied = call(&mut w.member.client, Method::DELETE, &path, None).await;
            status_is(&denied, 403);

            w.ctl
                .flag(flag, ctl_mode(&initial), &[])
                .await
                .expect("restore flag mode");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_mw_020_operator_denials_are_logged_by_route_template() {
    serial(
        &["CAP-MW-020"],
        "cap_mw_020 operator denials are logged by route template",
        async move {
            let log_path = std::env::var_os("BB_CONFORMANCE_SERVER_LOG")
                .expect("BB_CONFORMANCE_SERVER_LOG names the server log");
            let mut w = World::new().await;
            let member_id = who(&mut w.member.client).await["id"]
                .as_str()
                .expect("member id")
                .to_string();

            // The caller-supplied target id is raw input and must never reach
            // an operator denial record; only the route template may.
            let target = unique("target");
            let data_cid = unique("mw020data");
            let mut member = w
                .member
                .client
                .clone()
                .with_header("X-Correlation-ID", &data_cid);
            let denied = call(
                &mut member,
                Method::POST,
                &format!("/api/admin/accounts/{target}/revoke-sessions"),
                None,
            )
            .await;
            status_is(&denied, 403);
            assert_eq!(denied.header("x-correlation-id"), Some(data_cid.as_str()));

            let probe_cid = unique("mw020probe");
            let mut anonymous = anon(&w.cfg).with_header("X-Correlation-ID", &probe_cid);
            let probe = call(&mut anonymous, Method::GET, "/api/admin/status", None).await;
            status_is(&probe, 401);

            let log = std::fs::read_to_string(&log_path).expect("read server log");
            let data_line = log
                .lines()
                .find(|l| l.contains(&data_cid) && l.contains("Admin access denied"))
                .expect("403 denial logged with its correlation id");
            assert!(data_line.contains("event=admin_data_route"));
            assert!(data_line.contains("route=/api/admin/accounts/{account_id}/revoke-sessions"));
            assert!(data_line.contains("outcome=not_an_operator"));
            assert!(data_line.contains(&format!("operator={member_id}")));

            let probe_line = log
                .lines()
                .find(|l| l.contains(&probe_cid) && l.contains("Admin access denied"))
                .expect("401 denial logged with its correlation id");
            assert!(probe_line.contains("event=admin_capability_probe"));
            assert!(probe_line.contains("route=/api/admin/status"));
            assert!(probe_line.contains("outcome=no_valid_session"));

            let admin_denials = log.lines().filter(|l| l.contains("Admin access denied"));
            for line in admin_denials {
                assert!(
                    !line.contains(&target),
                    "raw target id reached a denial record"
                );
                assert!(
                    !line.contains(&w.member.email),
                    "caller email reached a denial record"
                );
            }
        },
    )
    .await;
}
