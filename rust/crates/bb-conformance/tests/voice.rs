#![cfg(feature = "live")]
//! Black-box conformance for voice brain dump operations: CAP-HTTP-113..120
//! (spec 027, T114). Every test encodes the behavior of today's Python server.
//!
//! Run: `BB_CONFORMANCE_BASE_URL=http://localhost:8000 \
//!       BRAIN_BUDDY_TEST_CONTROL_TOKEN=<token> cargo test -p bb-conformance --test voice`.
//!
//! All seeding goes through the test control API (invites, the `voice_brain_dump`
//! flag, scripted and held speech-to-text, sweeps). The flag and the provider
//! script are server-wide, so the tests run one at a time under `SERIAL`.

use bb_conformance::{Client, Config, Reply, TestControl, case};
use reqwest::Method;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const FLAG: &str = "voice_brain_dump";
const AUDIO_TYPE: &str = "audio/x-brain-buddy-test-text";
const SPOKEN: &str = "Pay VAT. Send invoice.";
const NOT_AVAILABLE: &str = "Voice brain dump is not available.";
const MISSING: &str = "brain_dump_missing000000";
const OPS: &str = "/brain-dump-operations";

static SERIAL: Mutex<()> = Mutex::const_new(());

// ---------------------------------------------------------------- plumbing

fn uniq() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{t:x}{:x}", N.fetch_add(1, Ordering::Relaxed))
}

fn key(tag: &str) -> String {
    format!("{tag}-{}", uniq())
}

/// SHA-256 as lowercase hex (the server checks `X-Content-SHA256` and the seal
/// manifest with it).
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (slot, add) in h.iter_mut().zip(v) {
            *slot = slot.wrapping_add(add);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

/// The seal manifest hash: SHA-256 of the sorted-key compact JSON of the chunk list.
fn manifest_hash(chunks: &[&[u8]]) -> String {
    let items: Vec<String> = chunks
        .iter()
        .enumerate()
        .map(|(n, c)| {
            format!(
                r#"{{"chunk_number":{n},"sha256":"{}","size_bytes":{}}}"#,
                sha256_hex(c),
                c.len()
            )
        })
        .collect();
    sha256_hex(format!("[{}]", items.join(",")).as_bytes())
}

/// Asserts the status and returns the JSON body (`Null` when it has none).
fn ok(r: &Reply, status: u16) -> Value {
    assert_eq!(r.status, status, "unexpected status, body: {}", r.body);
    r.json().unwrap_or(Value::Null)
}

fn message(r: &Reply) -> String {
    r.json()
        .ok()
        .and_then(|v| v["message"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Status plus error body without the per-request reference id and with the
/// resource id masked, so two refusals can be compared.
fn refusal(r: &Reply, id: &str) -> (u16, String) {
    let body = r.json().unwrap_or(Value::Null);
    let masked = json!({ "message": body["message"], "detail": body["detail"] })
        .to_string()
        .replace(id, "<id>");
    (r.status, masked)
}

fn consent() -> Value {
    json!({ "microphone": true, "external_processing_allowed": true, "provider": "openai" })
}

fn titles(op: &Value) -> Vec<String> {
    op["proposals"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["title"].as_str().map(str::to_owned))
        .collect()
}

fn id_of(op: &Value) -> String {
    op["id"].as_str().expect("operation id").to_owned()
}

fn rev_of(op: &Value) -> i64 {
    op["revision"].as_i64().expect("operation revision")
}

fn actions(op: &Value) -> Vec<String> {
    op["available_recovery_actions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a.as_str().map(str::to_owned))
        .collect()
}

struct World {
    cfg: Config,
    ctl: Arc<TestControl>,
    anon: Client,
}

impl World {
    /// Takes the suite lock, enables the voice flag and clears the call ledger.
    async fn new() -> (tokio::sync::MutexGuard<'static, ()>, Self) {
        let guard = SERIAL.lock().await;
        let cfg = Config::from_env().expect("conformance environment");
        let ctl = Arc::new(TestControl::new(&cfg).expect("test control"));
        let anon = Client::new(&cfg).expect("client");
        let w = Self { cfg, ctl, anon };
        w.voice(true).await;
        (guard, w)
    }

    async fn voice(&self, on: bool) {
        let mode = if on { "on" } else { "off" };
        self.ctl.flag(FLAG, mode, &[]).await.expect("set flag");
    }

    /// Scripts speech-to-text (and resets its call ledger).
    async fn script(&self, script: Value, hold: bool) {
        let ctl = &self.ctl;
        ctl.script_provider("stt", script, hold)
            .await
            .expect("script stt");
    }

    async fn speak(&self) {
        self.script(json!([{ "text": SPOKEN }]), false).await;
    }

    async fn stt_calls(&self) -> u64 {
        let calls = self.ctl.provider_calls("stt").await.expect("ledger");
        calls["count"].as_u64().expect("call count")
    }

    /// A new synthetic owner on `example.com`. The test control owner factory is
    /// not used because `Member` also needs the raw session cookie.
    async fn owner(&self) -> Member {
        let invites = self.ctl.invites(1).await.expect("invite");
        let code = invites["codes"][0].as_str().expect("invite code");
        let email = format!("voice-{}@conformance.example.com", uniq());
        let mut api = self.anon.fresh();
        let body = json!({
            "email": email, "password": "Conformance-pass-1!", "invite_code": code
        });
        let reply = api.post("/api/auth/signup", &body).await.expect("signup");
        assert!(reply.status < 300, "signup answered {}", reply.body);
        let cookie = reply
            .headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(|v| v.split(';').next())
            .find(|pair| pair.split_once('=').is_some_and(|(_, val)| !val.is_empty()))
            .expect("session cookie")
            .to_owned();
        Member {
            email,
            api,
            cookie,
            base: self.cfg.base_url.trim_end_matches('/').to_owned(),
            epoch: self.cfg.epoch,
            http: reqwest::Client::new(),
        }
    }

    async fn anonymous(
        &self,
        method: Method,
        path: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> Reply {
        let mut c = self.anon.fresh();
        if let Some(k) = key {
            c = c.with_header("Idempotency-Key", k);
        }
        c.send(method, &format!("/api{path}"), body.as_ref())
            .await
            .expect("request")
    }

    /// Polls (sweeping the voice runner) until the operation reaches a status.
    async fn settle(&self, m: &Member, id: &str, want: &[&str]) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.ctl.sweep("voice").await.expect("voice sweep");
            let op = m.op(id).await;
            if want.contains(&op["status"].as_str().unwrap_or_default()) {
                return op;
            }
            assert!(Instant::now() < deadline, "never reached {want:?}: {op}");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Seals one chunk and returns the post-seal operation.
    async fn sealed(&self, m: &Member, text: &str) -> Value {
        let op = m.start().await;
        m.seal_one(&id_of(&op), text).await
    }

    /// A sealed operation advanced to review (needs `speak()` first).
    async fn reviewable(&self, m: &Member) -> Value {
        let op = self.sealed(m, SPOKEN).await;
        self.settle(m, &id_of(&op), &["awaiting_confirmation"])
            .await
    }
}

struct Member {
    email: String,
    api: Client,
    cookie: String,
    base: String,
    epoch: Option<u32>,
    http: reqwest::Client,
}

impl Member {
    async fn call(
        &self,
        method: Method,
        path: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> Reply {
        let mut c = self.api.clone();
        if let Some(k) = key {
            c = c.with_header("Idempotency-Key", k);
        }
        c.send(method, &format!("/api{path}"), body.as_ref())
            .await
            .expect("request")
    }

    async fn start_with(&self, key: &str, consent: Value) -> Reply {
        let body = json!({ "consent": consent });
        self.call(Method::POST, OPS, Some(key), Some(body)).await
    }

    async fn start(&self) -> Value {
        ok(&self.start_with(&key("start"), consent()).await, 201)
    }

    async fn get(&self, id: &str) -> Reply {
        self.call(Method::GET, &format!("{OPS}/{id}"), None, None)
            .await
    }

    async fn op(&self, id: &str) -> Value {
        ok(&self.get(id).await, 200)
    }

    async fn transcript(&self, id: &str, key: &str, body: Value) -> Reply {
        let path = format!("{OPS}/{id}/transcript");
        self.call(Method::POST, &path, Some(key), Some(body)).await
    }

    async fn upload(
        &self,
        id: &str,
        n: i64,
        body: &[u8],
        content_type: Option<&str>,
        sha: Option<&str>,
    ) -> Reply {
        let url = format!("{}/api{OPS}/{id}/audio/{n}", self.base);
        let mut req = self
            .http
            .put(url)
            .header("Cookie", &self.cookie)
            .body(body.to_vec());
        if let Some(epoch) = self.epoch {
            req = req.header("X-Client-Epoch", epoch.to_string());
        }
        if let Some(ct) = content_type {
            req = req.header("Content-Type", ct);
        }
        if let Some(sha) = sha {
            req = req.header("X-Content-SHA256", sha);
        }
        let resp = req.send().await.expect("upload request");
        let (status, headers) = (resp.status().as_u16(), resp.headers().clone());
        let body = resp.text().await.expect("upload body");
        Reply {
            status,
            headers,
            body,
        }
    }

    /// Uploads `body` as chunk `n` with a correct hash and the fixture MIME type.
    async fn chunk(&self, id: &str, n: i64, body: &[u8]) -> Reply {
        self.upload(id, n, body, Some(AUDIO_TYPE), Some(&sha256_hex(body)))
            .await
    }

    async fn seal(&self, id: &str, key: &str, body: Value) -> Reply {
        let path = format!("{OPS}/{id}/seal");
        self.call(Method::POST, &path, Some(key), Some(body)).await
    }

    /// Uploads one chunk of `text` and seals it.
    async fn seal_one(&self, id: &str, text: &str) -> Value {
        let uploaded = ok(&self.chunk(id, 0, text.as_bytes()).await, 200);
        let body = json!({
            "expected_revision": rev_of(&uploaded),
            "expected_chunks": 1,
            "manifest_hash": manifest_hash(&[text.as_bytes()]),
        });
        ok(&self.seal(id, &key("seal"), body).await, 200)
    }

    async fn patch(&self, id: &str, pid: &str, key: Option<&str>, body: Value) -> Reply {
        let path = format!("{OPS}/{id}/proposals/{pid}");
        self.call(Method::PATCH, &path, key, Some(body)).await
    }

    /// Posts an action against the operation's current revision.
    async fn act(&self, id: &str, action: &str) -> Reply {
        let rev = rev_of(&self.op(id).await);
        self.act_at(id, action, &key(action), rev).await
    }

    async fn act_at(&self, id: &str, action: &str, key: &str, rev: i64) -> Reply {
        let path = format!("{OPS}/{id}/{action}");
        let body = json!({ "expected_revision": rev });
        self.call(Method::POST, &path, Some(key), Some(body)).await
    }

    /// The same request against another owner's id and a never-created id must
    /// be indistinguishable (027-FR-008).
    async fn foreign_is_missing(
        &self,
        foreign_id: &str,
        method: Method,
        suffix: &str,
        with_key: bool,
        body: Option<Value>,
    ) {
        let mut seen = Vec::new();
        for id in [foreign_id, MISSING] {
            let path = format!("{OPS}/{id}{suffix}");
            let k = with_key.then(|| key("probe"));
            let r = self
                .call(method.clone(), &path, k.as_deref(), body.clone())
                .await;
            assert_eq!(r.status, 404, "{method} {suffix} on {id}: {}", r.body);
            seen.push(refusal(&r, id));
        }
        assert_eq!(seen[0], seen[1], "{method} {suffix} reveals the foreign id");
    }

    async fn owner_task_titles(&self, w: &World) -> Vec<String> {
        let tasks = w.ctl.owner_tasks(&self.email).await.expect("owner tasks");
        tasks["tasks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["title"].as_str().map(str::to_owned))
            .collect()
    }
}

// ------------------------------------------------ CAP-HTTP-113 start

#[tokio::test]
async fn cap_http_113_start_returns_a_recording_operation() {
    case(
        &["CAP-HTTP-113"],
        "CAP-HTTP-113 start answers 201 with a recording operation (027-FR-001, 027-FR-012)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            let r = m.start_with(&key("start"), consent()).await;
            assert!(r.header("x-correlation-id").is_some());
            let op = ok(&r, 201);
            assert_eq!(op["kind"], "voice_brain_dump");
            assert_eq!(op["status"], "recording");
            assert_eq!(op["revision"], 1);
            assert_eq!(op["consent"]["microphone"], true);
            assert_eq!(op["consent"]["external_processing_allowed"], true);
            assert_eq!(op["consent"]["provider"], "openai");
            for list in [
                "segments",
                "proposals",
                "audio_chunks",
                "provider_runs",
                "action_receipts",
            ] {
                assert_eq!(op[list].as_array().map(Vec::len), Some(0), "{list}");
            }
            assert_eq!(op["raw_audio_present"], false);
            assert_eq!(op["committable"], false);
            assert!(op["working_artifacts_expires_at"].is_string());
            let me = ok(&m.call(Method::GET, "/auth/me", None, None).await, 200);
            assert_eq!(op["owner_id"], me["id"]);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_113_start_is_idempotent_and_needs_a_key_and_a_session() {
    case(
        &["CAP-HTTP-113"],
        "CAP-HTTP-113 start replays by key and refuses a reused key, no key, no session (027-FR-001)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            let k = key("start");
            let first = ok(&m.start_with(&k, consent()).await, 201);
            let again = ok(&m.start_with(&k, consent()).await, 201);
            assert_eq!(first["id"], again["id"]);
            let mut other = consent();
            other["vocabulary"] = json!(["VAT"]);
            assert_eq!(m.start_with(&k, other).await.status, 409);
            let body = json!({ "consent": consent() });
            let nokey = m.call(Method::POST, OPS, None, Some(body.clone())).await;
            assert_eq!(nokey.status, 400);
            assert_eq!(message(&nokey), "Idempotency-Key header is required.");
            let anon = w.anonymous(Method::POST, OPS, Some(&key("anon")), Some(body)).await;
            assert_eq!(anon.status, 401);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_113_start_refuses_without_consent_or_flag_and_calls_no_provider() {
    case(
        &["CAP-HTTP-113"],
        "CAP-HTTP-113 start refuses missing microphone consent and a closed flag without a provider call (027-FR-011)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            w.speak().await;
            let declined = m
                .start_with(&key("declined"), json!({ "microphone": false, "external_processing_allowed": true, "provider": "openai" }))
                .await;
            let no_consent = m.call(Method::POST, OPS, Some(&key("none")), Some(json!({}))).await;
            let calls = w.stt_calls().await;
            w.voice(false).await;
            let closed = m.start_with(&key("closed"), consent()).await;
            w.voice(true).await;
            assert_eq!(declined.status, 400);
            assert!(message(&declined).contains("Microphone consent is required"));
            assert_eq!(no_consent.status, 422);
            assert_eq!(calls, 0, "a refused start must not reach a provider");
            assert_eq!(closed.status, 404);
            assert_eq!(message(&closed), NOT_AVAILABLE);
        },
    )
    .await;
}

// ------------------------------------------------ CAP-HTTP-114 providers

#[tokio::test]
async fn cap_http_114_reports_the_configured_provider_category_per_role() {
    case(
        &["CAP-HTTP-114"],
        "CAP-HTTP-114 providers lists one category per pipeline role from configuration (027-FR-012)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            let r = m.call(Method::GET, "/brain-dump-providers", None, None).await;
            let body = ok(&r, 200);
            let mut roles: Vec<&str> = body.as_object().expect("object").keys().map(String::as_str).collect();
            roles.sort_unstable();
            assert_eq!(roles, ["accurate_stt", "reconciler"]);
            assert_eq!(body["accurate_stt"], "deterministic");
            assert!(body["reconciler"].is_null(), "a disabled role is reported as null");
            let anon = w.anonymous(Method::GET, "/brain-dump-providers", None, None).await;
            assert_eq!(anon.status, 401);
            w.voice(false).await;
            let closed = m.call(Method::GET, "/brain-dump-providers", None, None).await;
            w.voice(true).await;
            assert_eq!(closed.status, 404);
            assert_eq!(message(&closed), NOT_AVAILABLE);
        },
    )
    .await;
}

// ------------------------------------------------ CAP-HTTP-115 read

#[tokio::test]
async fn cap_http_115_reads_the_owners_operation_even_with_the_flag_closed() {
    case(
        &["CAP-HTTP-115"],
        "CAP-HTTP-115 read returns the operation and stays reachable when the flag is closed (027-FR-001, 027-FR-011)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            let started = m.start().await;
            let id = id_of(&started);
            let r = m.get(&id).await;
            assert!(r.header("x-correlation-id").is_some());
            assert_eq!(ok(&r, 200), started);
            let anon = w.anonymous(Method::GET, &format!("{OPS}/{id}"), None, None).await;
            assert_eq!(anon.status, 401);
            w.voice(false).await;
            let closed = m.get(&id).await;
            w.voice(true).await;
            assert_eq!(closed.status, 200, "privacy authority survives a closed flag");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_115_foreign_operation_is_indistinguishable_from_missing() {
    case(
        &["CAP-HTTP-115"],
        "CAP-HTTP-115 a foreign operation id answers exactly like a missing one (027-FR-008)",
        async {
            let (_g, w) = World::new().await;
            let (a, b) = (w.owner().await, w.owner().await);
            let id = id_of(&a.start().await);
            b.foreign_is_missing(&id, Method::GET, "", false, None)
                .await;
        },
    )
    .await;
}

// ------------------------------------------------ CAP-HTTP-116 transcript

#[tokio::test]
async fn cap_http_116_appends_segments_idempotently() {
    case(
        &["CAP-HTTP-116"],
        "CAP-HTTP-116 transcript appends a client segment, replays by key and refuses a reused key (027-FR-001)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            let id = id_of(&m.start().await);
            let k = key("transcript");
            let body = json!({ "segments": [{ "sequence": 1, "text": "hello world", "stability": "interim" }] });
            let first = ok(&m.transcript(&id, &k, body.clone()).await, 200);
            assert_eq!(first["status"], "recording");
            assert_eq!(first["revision"], 2);
            let seg = &first["segments"][0];
            assert_eq!(seg["text"], "hello world");
            assert_eq!(seg["stability"], "interim");
            assert_eq!(seg["provider_role"], "browser_preview");
            let replay = ok(&m.transcript(&id, &k, body).await, 200);
            assert_eq!(replay["revision"], 2);
            assert_eq!(replay["segments"].as_array().map(Vec::len), Some(1));
            let changed = json!({ "segments": [{ "sequence": 1, "text": "different" }] });
            assert_eq!(m.transcript(&id, &k, changed).await.status, 409);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_116_refuses_bad_requests_and_a_sealed_operation() {
    case(
        &["CAP-HTTP-116"],
        "CAP-HTTP-116 transcript refuses no key, bad bodies, a sealed operation and a closed flag (027-FR-001)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            let id = id_of(&m.start().await);
            let path = format!("{OPS}/{id}/transcript");
            let one = json!({ "segments": [{ "sequence": 1, "text": "a" }] });
            assert_eq!(m.call(Method::POST, &path, None, Some(one.clone())).await.status, 400);
            let empty = json!({ "segments": [] });
            assert_eq!(m.transcript(&id, &key("t"), empty).await.status, 422);
            let bad = json!({ "segments": [{ "sequence": 1, "text": "a", "stability": "maybe" }] });
            assert_eq!(m.transcript(&id, &key("t"), bad).await.status, 422);
            let zero = json!({ "segments": [{ "sequence": 0, "text": "a" }] });
            assert_eq!(m.transcript(&id, &key("t"), zero).await.status, 422);
            w.voice(false).await;
            let closed = m.transcript(&id, &key("t"), one.clone()).await;
            w.voice(true).await;
            assert_eq!((closed.status, message(&closed).as_str()), (404, NOT_AVAILABLE));
            w.speak().await;
            let sealed = m.seal_one(&id, SPOKEN).await;
            assert_eq!(sealed["status"], "accurate_transcribing");
            let late = m.transcript(&id, &key("t"), one).await;
            assert_eq!(late.status, 400);
            assert_eq!(message(&late), "Transcript can only be appended while recording or paused.");
            w.settle(&m, &id, &["awaiting_confirmation"]).await;
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_116_foreign_operation_is_indistinguishable_from_missing() {
    case(
        &["CAP-HTTP-116"],
        "CAP-HTTP-116 transcript on a foreign id answers like a missing id (027-FR-008)",
        async {
            let (_g, w) = World::new().await;
            let (a, b) = (w.owner().await, w.owner().await);
            let id = id_of(&a.start().await);
            let body = json!({ "segments": [{ "sequence": 1, "text": "a" }] });
            b.foreign_is_missing(&id, Method::POST, "/transcript", true, Some(body))
                .await;
        },
    )
    .await;
}

// ------------------------------------------------ CAP-HTTP-117 audio chunk

#[tokio::test]
async fn cap_http_117_stores_a_chunk_and_accepts_an_identical_resend() {
    case(
        &["CAP-HTTP-117"],
        "CAP-HTTP-117 audio chunk is stored, an identical resend is accepted and different bytes conflict (027-FR-012)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            let id = id_of(&m.start().await);
            let audio = b"first chunk of speech";
            let first = ok(&m.chunk(&id, 0, audio).await, 200);
            assert_eq!(first["raw_audio_present"], true);
            let stored = &first["audio_chunks"][0];
            assert_eq!(stored["chunk_number"], 0);
            assert_eq!(stored["sha256"], sha256_hex(audio));
            assert_eq!(stored["size_bytes"], audio.len());
            let again = ok(&m.chunk(&id, 0, audio).await, 200);
            assert_eq!(again["audio_chunks"].as_array().map(Vec::len), Some(1));
            let other = m.chunk(&id, 0, b"other bytes entirely").await;
            assert_eq!(other.status, 409);
            assert!(message(&other).starts_with("CHUNK_CONFLICT"));
            let wrong = m.upload(&id, 1, audio, Some(AUDIO_TYPE), Some(&sha256_hex(b"nope"))).await;
            assert_eq!(wrong.status, 409);
            assert!(message(&wrong).starts_with("CHUNK_CONFLICT"));
            let second = ok(&m.chunk(&id, 1, b"second chunk").await, 200);
            assert_eq!(second["audio_chunks"].as_array().map(Vec::len), Some(2));
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_117_enforces_header_size_and_order_rules() {
    case(
        &["CAP-HTTP-117"],
        "CAP-HTTP-117 audio chunk enforces hash header, MIME type, size cap and chunk order (027-FR-012)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            let id = id_of(&m.start().await);
            let audio = b"speech bytes";
            let sha = sha256_hex(audio);
            let refused = |r: &Reply, status: u16, code: &str| {
                assert_eq!(r.status, status, "{}", r.body);
                assert!(message(r).contains(code), "{} lacks {code}", message(r));
            };
            let nosha = m.upload(&id, 0, audio, Some(AUDIO_TYPE), None).await;
            refused(&nosha, 400, "X-Content-SHA256 header is required.");
            let notype = m.upload(&id, 0, audio, None, Some(&sha)).await;
            refused(&notype, 400, "AUDIO_CHUNK_MIME_TYPE_REQUIRED");
            let text = m.upload(&id, 0, audio, Some("text/plain"), Some(&sha)).await;
            refused(&text, 400, "AUDIO_CHUNK_MIME_TYPE_UNSUPPORTED");
            let cap: usize = std::env::var("BRAIN_BUDDY_VOICE_AUDIO_MAX_CHUNK_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10_485_760);
            let big = vec![b'x'; cap + 1];
            let large = m.chunk(&id, 0, &big).await;
            refused(&large, 400, "AUDIO_CHUNK_TOO_LARGE");
            refused(&m.chunk(&id, 3, audio).await, 400, "AUDIO_CHUNK_SEQUENCE_INVALID");
            refused(&m.chunk(&id, -1, audio).await, 400, "AUDIO_CHUNK_SEQUENCE_INVALID");
            let after = m.op(&id).await;
            assert_eq!(after["audio_chunks"].as_array().map(Vec::len), Some(0), "refused chunks leave nothing");
            w.voice(false).await;
            let closed = m.chunk(&id, 0, audio).await;
            w.voice(true).await;
            assert_eq!((closed.status, message(&closed).as_str()), (404, NOT_AVAILABLE));
            let anon = reqwest::Client::new()
                .put(format!("{}/api{OPS}/{id}/audio/0", m.base))
                .header("Content-Type", AUDIO_TYPE)
                .header("X-Content-SHA256", &sha)
                .body(audio.to_vec())
                .send()
                .await
                .expect("anonymous upload");
            assert_eq!(anon.status().as_u16(), 401);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_117_refuses_chunks_after_seal_and_for_foreign_operations() {
    case(
        &["CAP-HTTP-117"],
        "CAP-HTTP-117 audio chunk after seal is refused and a foreign id answers like a missing id (027-FR-008)",
        async {
            let (_g, w) = World::new().await;
            let (a, b) = (w.owner().await, w.owner().await);
            w.speak().await;
            let id = id_of(&a.start().await);
            a.seal_one(&id, SPOKEN).await;
            let late = a.chunk(&id, 1, b"late").await;
            assert_eq!(late.status, 400);
            assert_eq!(message(&late), "Audio chunks can only be uploaded while recording or paused.");
            w.settle(&a, &id, &["awaiting_confirmation"]).await;
            let sha = sha256_hex(b"x");
            let mut seen = Vec::new();
            for target in [id.as_str(), MISSING] {
                let r = b.upload(target, 0, b"x", Some(AUDIO_TYPE), Some(&sha)).await;
                assert_eq!(r.status, 404, "{}", r.body);
                seen.push(refusal(&r, target));
            }
            assert_eq!(seen[0], seen[1]);
        },
    )
    .await;
}

// ------------------------------------------------ CAP-HTTP-118 seal

#[tokio::test]
async fn cap_http_118_seal_starts_transcription_and_reaches_review() {
    case(
        &["CAP-HTTP-118"],
        "CAP-HTTP-118 seal freezes the manifest, starts the pipeline and ends in review with proposals (027-FR-001, 027-FR-012)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            w.speak().await;
            let id = id_of(&m.start().await);
            let sealed = m.seal_one(&id, SPOKEN).await;
            assert_eq!(sealed["status"], "accurate_transcribing");
            assert_eq!(sealed["sealed_manifest_hash"], manifest_hash(&[SPOKEN.as_bytes()]));
            assert_eq!(sealed["provider_runs"][0]["role"], "accurate_stt");
            let review = w.settle(&m, &id, &["awaiting_confirmation"]).await;
            assert_eq!(review["committable"], true);
            assert_eq!(review["reconciliation_quality"], "accurate");
            assert_eq!(titles(&review), ["Pay VAT", "Send invoice"]);
            let accurate = review["segments"]
                .as_array()
                .expect("segments")
                .iter()
                .any(|s| s["provider_role"] == "accurate" && s["text"] == SPOKEN);
            assert!(accurate, "the scripted accurate transcript is stored");
            let roles: Vec<&Value> = review["provider_runs"].as_array().expect("runs").iter().map(|r| &r["role"]).collect();
            assert_eq!(roles, ["accurate_stt", "reconciler"]);
            assert_eq!(w.stt_calls().await, 1);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_118_seal_replays_by_key_and_checks_the_manifest() {
    case(
        &["CAP-HTTP-118"],
        "CAP-HTTP-118 seal replays by key and refuses key, format, chunk set, manifest, revision and state errors (027-FR-001)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            w.speak().await;
            let id = id_of(&m.start().await);
            let audio = SPOKEN.as_bytes();
            let uploaded = ok(&m.chunk(&id, 0, audio).await, 200);
            let rev = rev_of(&uploaded);
            let good = manifest_hash(&[audio]);
            let seal = |expected_revision: i64, chunks: i64, hash: &str| {
                json!({ "expected_revision": expected_revision, "expected_chunks": chunks, "manifest_hash": hash })
            };
            let path = format!("{OPS}/{id}/seal");
            let nokey = m.call(Method::POST, &path, None, Some(seal(rev, 1, &good))).await;
            assert_eq!(nokey.status, 400);
            assert_eq!(m.seal(&id, &key("s"), seal(rev, 1, "abc")).await.status, 422);
            let missing = m.seal(&id, &key("s"), seal(rev, 3, &good)).await;
            assert_eq!(missing.status, 400);
            assert_eq!(missing.json().expect("json")["detail"]["missing_chunks"], json!([1, 2]));
            let extra = m.seal(&id, &key("s"), seal(rev, 0, &good)).await;
            assert_eq!(extra.status, 400);
            assert_eq!(extra.json().expect("json")["detail"]["unexpected_chunks"], json!([0]));
            let forged = m.seal(&id, &key("s"), seal(rev, 1, &"0".repeat(64))).await;
            assert_eq!(forged.status, 409);
            assert!(message(&forged).starts_with("MANIFEST_CONFLICT"));
            assert_eq!(m.seal(&id, &key("s"), seal(rev - 1, 1, &good)).await.status, 409);
            w.voice(false).await;
            let closed = m.seal(&id, &key("s"), seal(rev, 1, &good)).await;
            w.voice(true).await;
            assert_eq!((closed.status, message(&closed).as_str()), (404, NOT_AVAILABLE));
            assert_eq!(m.op(&id).await["status"], "recording", "refusals change nothing");

            let k = key("seal");
            let first = ok(&m.seal(&id, &k, seal(rev, 1, &good)).await, 200);
            let replay = ok(&m.seal(&id, &k, seal(rev, 1, &good)).await, 200);
            assert_eq!(rev_of(&first), rev_of(&replay));
            let twice = m.seal(&id, &key("s"), seal(rev_of(&first), 1, &good)).await;
            assert_eq!(twice.status, 400);
            assert_eq!(message(&twice), "Only an active brain dump can be sealed.");
            w.settle(&m, &id, &["awaiting_confirmation"]).await;
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_118_foreign_operation_is_indistinguishable_from_missing() {
    case(
        &["CAP-HTTP-118"],
        "CAP-HTTP-118 seal on a foreign id answers like a missing id (027-FR-008)",
        async {
            let (_g, w) = World::new().await;
            let (a, b) = (w.owner().await, w.owner().await);
            let id = id_of(&a.start().await);
            let body = json!({ "expected_revision": 1, "expected_chunks": 0, "manifest_hash": "0".repeat(64) });
            b.foreign_is_missing(&id, Method::POST, "/seal", true, Some(body)).await;
        },
    )
    .await;
}

// ------------------------------------------------ CAP-HTTP-119 proposals

#[tokio::test]
async fn cap_http_119_edits_and_deletes_a_proposal() {
    case(
        &["CAP-HTTP-119"],
        "CAP-HTTP-119 proposal edit locks the title, replays by key and a delete marks it deleted (027-FR-001)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            w.speak().await;
            let review = w.reviewable(&m).await;
            let id = id_of(&review);
            let pid = review["proposals"][0]["id"].as_str().expect("proposal").to_owned();
            let k = key("edit");
            let body = json!({ "title": "Renamed", "expected_revision": rev_of(&review) });
            let edited = ok(&m.patch(&id, &pid, Some(&k), body.clone()).await, 200);
            let first = &edited["proposals"][0];
            assert_eq!(first["title"], "Renamed");
            assert_eq!(first["user_edited"], true);
            assert_eq!(first["status"], "user_edited");
            assert_eq!(first["locked_fields"], json!(["title"]));
            let patch = edited["proposal_patches"].as_array().and_then(|p| p.last()).expect("patch");
            assert_eq!((patch["operation"].as_str(), patch["producer"].as_str()), (Some("update"), Some("user")));
            let replay = ok(&m.patch(&id, &pid, Some(&k), body).await, 200);
            assert_eq!(replay["proposals"][0]["title"], "Renamed");
            let other = json!({ "title": "Another", "expected_revision": rev_of(&review) });
            assert_eq!(m.patch(&id, &pid, Some(&k), other).await.status, 409);
            let now = m.op(&id).await;
            let delete = json!({ "deleted": true, "expected_revision": rev_of(&now) });
            let deleted = ok(&m.patch(&id, &pid, Some(&key("del")), delete).await, 200);
            assert_eq!(deleted["proposals"][0]["deleted"], true);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_119_refuses_bad_proposal_requests() {
    case(
        &["CAP-HTTP-119"],
        "CAP-HTTP-119 proposal edit refuses no key, stale revision, unknown proposal, bad body and a closed flag (027-FR-001)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            w.speak().await;
            let review = w.reviewable(&m).await;
            let id = id_of(&review);
            let rev = rev_of(&review);
            let pid = review["proposals"][0]["id"].as_str().expect("proposal").to_owned();
            let edit = json!({ "title": "x", "expected_revision": rev });
            assert_eq!(m.patch(&id, &pid, None, edit.clone()).await.status, 400);
            let stale = json!({ "title": "x", "expected_revision": rev + 50 });
            assert_eq!(m.patch(&id, &pid, Some(&key("p")), stale).await.status, 409);
            let unknown = m.patch(&id, "proposal_nope", Some(&key("p")), edit.clone()).await;
            assert_eq!(unknown.status, 404);
            assert!(message(&unknown).contains("Brain dump proposal"));
            let blank = json!({ "title": "", "expected_revision": rev });
            assert_eq!(m.patch(&id, &pid, Some(&key("p")), blank).await.status, 422);
            let resolve = json!({ "conflict_resolution": "merge", "expected_revision": rev });
            assert_eq!(m.patch(&id, &pid, Some(&key("p")), resolve).await.status, 422);
            w.voice(false).await;
            let closed = m.patch(&id, &pid, Some(&key("p")), edit).await;
            w.voice(true).await;
            assert_eq!((closed.status, message(&closed).as_str()), (404, NOT_AVAILABLE));
            assert_eq!(titles(&m.op(&id).await), ["Pay VAT", "Send invoice"], "refusals change nothing");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_119_foreign_operation_is_indistinguishable_from_missing() {
    case(
        &["CAP-HTTP-119"],
        "CAP-HTTP-119 proposal edit on a foreign id answers like a missing id (027-FR-008)",
        async {
            let (_g, w) = World::new().await;
            let (a, b) = (w.owner().await, w.owner().await);
            w.speak().await;
            let review = w.reviewable(&a).await;
            let pid = review["proposals"][0]["id"]
                .as_str()
                .expect("proposal")
                .to_owned();
            let body = json!({ "title": "stolen", "expected_revision": rev_of(&review) });
            let suffix = format!("/proposals/{pid}");
            b.foreign_is_missing(&id_of(&review), Method::PATCH, &suffix, true, Some(body))
                .await;
            assert_eq!(
                titles(&a.op(&id_of(&review)).await),
                ["Pay VAT", "Send invoice"]
            );
        },
    )
    .await;
}

// ------------------------------------------------ CAP-HTTP-120 actions

#[tokio::test]
async fn cap_http_120_pause_resume_finish_and_cancel_follow_the_state_machine() {
    case(
        &["CAP-HTTP-120"],
        "CAP-HTTP-120 pause, resume, finish and cancel move the operation and refuse impossible moves (027-FR-001)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            let id = id_of(&m.start().await);
            let resume = m.act(&id, "resume").await;
            assert_eq!((resume.status, message(&resume).as_str()), (400, "Only a paused brain dump can be resumed."));
            assert_eq!(ok(&m.act(&id, "pause").await, 200)["status"], "paused");
            let again = m.act(&id, "pause").await;
            assert_eq!((again.status, message(&again).as_str()), (400, "Only a recording brain dump can be paused."));
            assert_eq!(ok(&m.act(&id, "resume").await, 200)["status"], "recording");
            assert_eq!(ok(&m.act(&id, "finish").await, 200)["status"], "awaiting_confirmation");
            assert_eq!(ok(&m.act(&id, "cancel").await, 200)["status"], "cancelled");
            let nokey = m
                .call(Method::POST, &format!("{OPS}/{id}/pause"), None, Some(json!({ "expected_revision": 1 })))
                .await;
            assert_eq!(nokey.status, 400);
            let invalid = m.call(Method::POST, &format!("{OPS}/{id}/pause"), Some(&key("p")), Some(json!({}))).await;
            assert_eq!(invalid.status, 422);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_120_unknown_action_is_a_bad_request_and_a_session_is_required() {
    case(
        &["CAP-HTTP-120"],
        "CAP-HTTP-120 an unknown action answers 400 and no session answers 401 (027-FR-001)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            let id = id_of(&m.start().await);
            let r = m.act(&id, "explode").await;
            assert_eq!(r.status, 400);
            assert_eq!(message(&r), "Unsupported brain dump operation command.");
            let anon = w
                .anonymous(
                    Method::POST,
                    &format!("{OPS}/{id}/cancel"),
                    Some(&key("a")),
                    Some(json!({ "expected_revision": 1 })),
                )
                .await;
            assert_eq!(anon.status, 401);
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_120_commit_creates_tasks_once_and_replays_by_key() {
    case(
        &["CAP-HTTP-120"],
        "CAP-HTTP-120 commit turns proposals into tasks with receipts, replays by key and refuses a stale revision (027-FR-001, 027-FR-012)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            w.speak().await;
            let review = w.reviewable(&m).await;
            let id = id_of(&review);
            let rev = rev_of(&review);
            assert_eq!(m.act_at(&id, "commit", &key("c"), rev - 1).await.status, 409);
            let k = key("commit");
            let done = ok(&m.act_at(&id, "commit", &k, rev).await, 200);
            assert_eq!(done["status"], "completed");
            assert_eq!(done["committable"], false);
            assert_eq!(done["committed_task_ids"].as_array().map(Vec::len), Some(2));
            let receipts = done["action_receipts"].as_array().expect("receipts");
            assert_eq!(receipts.len(), 2);
            assert!(receipts.iter().all(|r| r["decision"] == "create_native_inbox_task"));
            let replay = ok(&m.act_at(&id, "commit", &k, rev).await, 200);
            assert_eq!(replay["committed_task_ids"], done["committed_task_ids"]);
            let mut tasks = m.owner_task_titles(&w).await;
            tasks.sort();
            assert_eq!(tasks, ["Pay VAT", "Send invoice"], "exactly one task per proposal");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_120_retry_recovers_a_retryable_provider_failure() {
    case(
        &["CAP-HTTP-120"],
        "CAP-HTTP-120 retry restarts a retryable failure and is refused otherwise (027-FR-001, 027-FR-012)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            w.script(json!([{ "error": "retryable" }, { "text": SPOKEN }]), false).await;
            let sealed = w.sealed(&m, SPOKEN).await;
            let id = id_of(&sealed);
            let failed = w.settle(&m, &id, &["retryable_error"]).await;
            assert_eq!(actions(&failed), ["retry", "cancel"]);
            let retried = ok(&m.act(&id, "retry").await, 200);
            assert_eq!(retried["status"], "accurate_transcribing");
            let runs = retried["provider_runs"].as_array().expect("runs");
            assert_eq!(runs.last().map(|r| r["status"].clone()), Some(json!("pending")));
            let review = w.settle(&m, &id, &["awaiting_confirmation"]).await;
            assert_eq!(titles(&review), ["Pay VAT", "Send invoice"]);
            let refused = m.act(&id, "retry").await;
            assert_eq!(refused.status, 400);
            assert_eq!(message(&refused), "Only a retryable brain dump can be retried.");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_120_preview_recovery_after_a_terminal_failure() {
    case(
        &["CAP-HTTP-120"],
        "CAP-HTTP-120 reconcile_preview recovers tasks from preview text after a terminal failure; review_provisional needs reviewable proposals (027-FR-001, 027-FR-012)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            w.script(json!([{ "error": "terminal" }]), false).await;
            let id = id_of(&m.start().await);
            let preview = json!({ "segments": [{ "sequence": 1, "text": "Call mum. Buy milk.", "stability": "stable" }] });
            ok(&m.transcript(&id, &key("t"), preview).await, 200);
            m.seal_one(&id, "Call mum. Buy milk.").await;
            let failed = w.settle(&m, &id, &["terminal_error"]).await;
            assert_eq!(actions(&failed), ["reconcile_preview", "cancel"]);
            assert_eq!(failed["provider_runs"][0]["status"], "terminal_error");
            let provisional = m.act(&id, "review_provisional").await;
            assert_eq!(provisional.status, 400);
            assert!(message(&provisional).starts_with("Only a terminal transcription or reconciliation failure"));
            let recovering = ok(&m.act(&id, "reconcile_preview").await, 200);
            assert_eq!(recovering["status"], "reconciling");
            let review = w.settle(&m, &id, &["awaiting_confirmation"]).await;
            assert_eq!(titles(&review), ["Call mum", "Buy milk"]);
            let spent = m.act(&id, "reconcile_preview").await;
            assert_eq!(spent.status, 400);
            assert!(message(&spent).starts_with("BRAIN_DUMP_PREVIEW_RECOVERY_UNAVAILABLE"));
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_120_wrong_state_actions_are_refused() {
    case(
        &["CAP-HTTP-120"],
        "CAP-HTTP-120 retry, review_provisional and reconcile_preview are refused on a healthy operation (027-FR-001)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            w.speak().await;
            let review = w.reviewable(&m).await;
            let id = id_of(&review);
            for action in ["retry", "review_provisional", "reconcile_preview", "pause", "resume"] {
                let r = m.act(&id, action).await;
                assert_eq!(r.status, 400, "{action}: {}", r.body);
            }
            assert_eq!(m.op(&id).await["status"], "awaiting_confirmation");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_120_privacy_actions_stay_reachable_with_the_flag_closed() {
    case(
        &["CAP-HTTP-120"],
        "CAP-HTTP-120 withdraw_consent, delete_raw_audio and cancel work with the flag closed; forward actions answer 404 (027-FR-011, 027-FR-012)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            w.speak().await;
            let review = w.reviewable(&m).await;
            let id = id_of(&review);
            assert_eq!(review["raw_audio_present"], true);
            w.voice(false).await;
            let mut gated = Vec::new();
            for action in ["commit", "retry", "review_provisional", "reconcile_preview", "pause", "resume", "finish"] {
                gated.push((action, m.act(&id, action).await));
            }
            let deleted = m.act(&id, "delete_raw_audio").await;
            let withdrawn = m.act(&id, "withdraw_consent").await;
            let cancelled = m.act(&id, "cancel").await;
            w.voice(true).await;
            for (action, r) in &gated {
                assert_eq!(r.status, 404, "{action}: {}", r.body);
                assert_eq!(message(r), NOT_AVAILABLE, "{action}");
            }
            let deleted = ok(&deleted, 200);
            assert_eq!(deleted["raw_audio_present"], false);
            assert_eq!(deleted["audio_chunks"].as_array().map(Vec::len), Some(0));
            assert_eq!(ok(&withdrawn, 200)["consent"]["external_processing_allowed"], false);
            assert_eq!(ok(&cancelled, 200)["status"], "cancelled");
            assert!(m.owner_task_titles(&w).await.is_empty(), "no task without a commit");
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_120_withdrawn_consent_discards_a_late_provider_result() {
    case(
        &["CAP-HTTP-120", "CAP-HTTP-118"],
        "CAP-HTTP-120 a transcript returning after consent is withdrawn is discarded, not stored or applied (027-FR-011, 027-SC-003)",
        async {
            let (_g, w) = World::new().await;
            let m = w.owner().await;
            w.script(json!([{ "text": SPOKEN }]), true).await;
            let id = id_of(&m.start().await);
            m.seal_one(&id, SPOKEN).await;
            let ctl = Arc::clone(&w.ctl);
            let sweep = tokio::spawn(async move { ctl.sweep("voice").await });
            let deadline = Instant::now() + Duration::from_secs(20);
            while w.stt_calls().await == 0 {
                assert!(Instant::now() < deadline, "the held provider was never called");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let withdrawn = m.act(&id, "withdraw_consent").await;
            let released = w.ctl.release("stt", 1).await.expect("release");
            sweep.await.expect("sweep task").expect("sweep");
            // Let the runner finish too; the outcome must stay discarded.
            tokio::time::sleep(Duration::from_millis(800)).await;
            let calls = w.stt_calls().await;
            w.script(json!([]), false).await;
            let op = m.op(&id).await;
            assert_eq!(released["released"], 1);
            let withdrawn = ok(&withdrawn, 200);
            assert_eq!(withdrawn["consent"]["external_processing_allowed"], false);
            assert_eq!(calls, 1, "exactly the one held call");
            assert_eq!(op["status"], "terminal_error");
            assert_eq!(op["provider_runs"][0]["error_code"], "CONSENT_WITHDRAWN");
            let stored = op["segments"].as_array().expect("segments");
            assert!(stored.iter().all(|s| s["provider_role"] != "accurate"), "late text stored");
            assert!(op["proposals"].as_array().expect("proposals").is_empty());
            assert_eq!(op["raw_audio_present"], false);
            assert!(m.owner_task_titles(&w).await.is_empty());
        },
    )
    .await;
}

#[tokio::test]
async fn cap_http_120_foreign_operation_is_indistinguishable_from_missing() {
    case(
        &["CAP-HTTP-120"],
        "CAP-HTTP-120 every action on a foreign id answers like a missing id (027-FR-008)",
        async {
            let (_g, w) = World::new().await;
            let (a, b) = (w.owner().await, w.owner().await);
            let id = id_of(&a.start().await);
            let body = json!({ "expected_revision": 1 });
            for action in [
                "commit",
                "retry",
                "review_provisional",
                "reconcile_preview",
                "withdraw_consent",
                "delete_raw_audio",
                "pause",
                "resume",
                "finish",
                "cancel",
            ] {
                let suffix = format!("/{action}");
                b.foreign_is_missing(&id, Method::POST, &suffix, true, Some(body.clone()))
                    .await;
            }
            assert_eq!(
                a.op(&id).await["status"],
                "recording",
                "the owner's operation is untouched"
            );
        },
    )
    .await;
}
