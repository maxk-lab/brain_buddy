//! Schema ownership (027 PR-21, T021; research R3, data-model.md "Rules").
//!
//! Parses every `sqlx` query in `src/<module>` and fails when a module touches a schema it
//! does not own, or calls another module's store code. The one allowed exception is
//! `control::mark_applied`. The migration/skeleton tests at the bottom need a live
//! PostgreSQL and run only with `--features live`.

use bb_allure::allure_test;
use std::fs;
use std::path::{Path, PathBuf};

const EPIC: &str = "Rust server";
const FEATURE: &str = "Store";

/// Module -> the schemas it owns. The Tasks owner group owns `tasks`, `sync` and `jobs`.
const OWNERS: [(&str, &[&str]); 7] = [
    ("identity", &["identity"]),
    ("flags", &["flags"]),
    ("control", &["control"]),
    ("crt", &["crt"]),
    ("voice", &["voice"]),
    ("agents", &["agents"]),
    ("tasks", &["tasks", "sync", "jobs"]),
];

const QUERY_FNS: [&str; 8] = [
    "query!",
    "query_as!",
    "query_scalar!",
    "query(",
    "query_as::<",
    "query_as(",
    "query_scalar::<",
    "query_scalar(",
];

fn all_schemas() -> Vec<&'static str> {
    OWNERS.iter().flat_map(|(_, s)| s.iter().copied()).collect()
}

/// The first string literal (plain or raw) at or after byte `from`, and where it ends.
fn next_literal(src: &str, from: usize) -> Option<(String, usize)> {
    let quote = from + src[from..].find('"')?;
    let before = src[..quote].trim_end_matches('#');
    let hashes = quote - before.len();
    let raw = before.ends_with('r');
    let start = quote + 1;
    let close = if raw {
        let terminator = format!("\"{}", "#".repeat(hashes));
        start + src[start..].find(&terminator)?
    } else {
        let bytes = src.as_bytes();
        let mut j = start;
        while bytes[j] != b'"' {
            j += if bytes[j] == b'\\' { 2 } else { 1 };
        }
        j
    };
    Some((src[start..close].to_string(), close))
}

/// The SQL text of every `sqlx::query*` call in `src`.
fn queries(src: &str) -> Vec<String> {
    let mut found = Vec::new();
    for name in QUERY_FNS {
        let needle = format!("sqlx::{name}");
        let mut at = 0;
        while let Some(p) = src[at..].find(&needle) {
            let after = at + p + needle.len();
            if let Some((sql, end)) = next_literal(src, after) {
                found.push(sql);
                at = end;
            } else {
                break;
            }
        }
    }
    found
}

/// Schemas referenced as `schema.relation` in `sql`.
fn schemas_in(sql: &str) -> Vec<String> {
    let lower = sql.to_lowercase();
    let mut out = Vec::new();
    for token in lower.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.')) {
        if let Some((schema, rel)) = token.split_once('.')
            && !rel.is_empty()
            && all_schemas().contains(&schema)
            && !out.iter().any(|s| s == schema)
        {
            out.push(schema.to_string());
        }
    }
    out
}

/// Violations in one source file of `module`: foreign-schema SQL, and Rust paths into
/// another module's store code (except `control::mark_applied`).
fn violations(module: &str, src: &str) -> Vec<String> {
    let owned = OWNERS
        .iter()
        .find(|(m, _)| *m == module)
        .map(|(_, s)| *s)
        .expect("known module");
    let mut out = Vec::new();
    for sql in queries(src) {
        for schema in schemas_in(&sql) {
            if !owned.contains(&schema.as_str()) {
                out.push(format!("{module}: query touches schema `{schema}`: {sql}"));
            }
        }
    }
    for (other, _) in OWNERS.iter().filter(|(m, _)| *m != module) {
        for prefix in ["crate::", "bb_store::", "super::"] {
            let needle = format!("{prefix}{other}");
            for (p, _) in src.match_indices(&needle) {
                let tail = &src[p + needle.len()..];
                let ident_end = tail
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(tail.len());
                if ident_end != 0 {
                    continue; // a longer identifier, not this module
                }
                let allowed = *other == "control" && tail.starts_with("::mark_applied");
                if !allowed {
                    out.push(format!("{module}: uses module `{other}` ({prefix}{other})"));
                }
            }
        }
    }
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn module_sources(module: &str) -> Vec<(PathBuf, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let single = src.join(format!("{module}.rs"));
    if single.exists() {
        files.push(single);
    }
    let dir = src.join(module);
    if dir.is_dir() {
        rust_files(&dir, &mut files);
    }
    files
        .into_iter()
        .map(|p| {
            let text = fs::read_to_string(&p).expect("read source");
            (p, text)
        })
        .collect()
}

#[test]
fn store_027_fr_002_every_module_touches_only_its_own_schemas() {
    allure_test!(
        EPIC,
        FEATURE,
        "027-FR-002 module schema ownership",
        "Every bb-store module queries only its own schemas",
        |t| {
            let mut parsed = 0;
            t.step("scan every module's sources", || {
                for (module, _) in OWNERS {
                    let sources = module_sources(module);
                    assert!(!sources.is_empty(), "no sources for module {module}");
                    for (path, text) in sources {
                        parsed += queries(&text).len();
                        let bad = violations(module, &text);
                        assert!(bad.is_empty(), "{}: {bad:?}", path.display());
                    }
                }
            });
            t.step("the parser really saw the control query", || {
                assert!(parsed >= 1, "query parser found no sqlx queries at all");
            });
        }
    );
}

#[test]
fn store_027_fr_002_planted_cross_schema_query_is_rejected() {
    allure_test!(
        EPIC,
        FEATURE,
        "027-FR-002 module schema ownership",
        "A planted cross-schema query fails the ownership check",
        |t| {
            t.step("plain and raw literals, runtime and macro forms", || {
                let plain = r#"let _ = sqlx::query("SELECT 1 FROM tasks.tasks");"#;
                assert_eq!(violations("identity", plain).len(), 1);
                let raw =
                    "sqlx::query_as::<_, (i64,)>(r#\"SELECT seq FROM Control.control_applied\"#)";
                assert_eq!(violations("crt", raw).len(), 1);
                let mac = r#"sqlx::query!("DELETE FROM voice.operations WHERE owner_id = $1", id)"#;
                assert_eq!(violations("agents", mac).len(), 1);
            });
            t.step("the Tasks owner group may span tasks, sync and jobs", || {
                let sql = r#"sqlx::query("INSERT INTO sync.change_records SELECT * FROM jobs.jobs JOIN tasks.tasks USING (id)")"#;
                assert!(violations("tasks", sql).is_empty());
                assert_eq!(violations("tasks", r#"sqlx::query("SELECT 1 FROM crt.trees")"#).len(), 1);
            });
            t.step(
                "only control::mark_applied is an allowed cross-module call",
                || {
                    let ok = "bb_store::control::mark_applied(&mut tx, 1).await?;";
                    assert!(violations("crt", ok).is_empty());
                    assert!(violations("crt", "crate::control::mark_applied(tx, 1)").is_empty());
                    assert_eq!(violations("crt", "crate::control::other(tx)").len(), 1);
                    assert_eq!(violations("crt", "use crate::identity::lookup;").len(), 1);
                    assert!(violations("crt", "use crate::identity_x::a;").is_empty());
                },
            );
        }
    );
}

#[test]
fn store_027_fr_002_every_migration_schema_has_an_owner() {
    allure_test!(
        EPIC,
        FEATURE,
        "027-FR-002 module schema ownership",
        "Every schema a migration creates is owned by exactly one module",
        |t| {
            t.step("collect CREATE SCHEMA names and check ownership", || {
                let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
                let mut created = Vec::new();
                for entry in fs::read_dir(dir).expect("migrations dir") {
                    let text = fs::read_to_string(entry.expect("entry").path()).expect("read");
                    for line in text.lines() {
                        let up = line.trim().to_uppercase();
                        if let Some(rest) = up.strip_prefix("CREATE SCHEMA IF NOT EXISTS ") {
                            created.push(rest.trim_end_matches(';').to_lowercase());
                        }
                    }
                }
                assert!(!created.is_empty());
                for schema in &created {
                    let owners = OWNERS
                        .iter()
                        .filter(|(_, s)| s.contains(&schema.as_str()))
                        .count();
                    assert_eq!(owners, 1, "schema `{schema}` must have exactly one owner");
                }
            });
        }
    );
}

#[cfg(feature = "live")]
mod live {
    use super::{EPIC, FEATURE};
    use bb_allure::allure_test;
    use bb_store::tasks::UnitOfWork;
    use bb_store::{PgPool, connect, control, migrate};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }

    /// Creates a fresh database on the server named by `BB_STORE_TEST_DATABASE_URL`.
    async fn fresh_database(admin_url: &str, name: &str) -> PgPool {
        let admin = connect(admin_url, 1).expect("admin pool");
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {name}"
        )))
        .execute(&admin)
        .await
        .expect("drop");
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&admin)
            .await
            .expect("create");
        let base = admin_url.rsplit_once('/').expect("url with database").0;
        connect(&format!("{base}/{name}"), 2).expect("pool")
    }

    #[test]
    fn store_027_fr_008_migrations_run_and_unit_of_work_commits_control_row() {
        allure_test!(
            EPIC,
            FEATURE,
            "027-FR-002 027-FR-008 migrations and unit of work",
            "Migrations apply and mark_applied commits with the unit of work",
            |t| {
                let url = std::env::var("BB_STORE_TEST_DATABASE_URL").expect("database url");
                runtime().block_on(async {
                    let pool = fresh_database(&url, "bb_store_skeleton_test").await;
                    t.step("migrate::run applies every migration, twice", || ());
                    migrate::run(&pool).await.expect("migrate");
                    migrate::run(&pool).await.expect("migrate again");
                    let schemas: i64 = sqlx::query_scalar(
                        "SELECT count(*) FROM information_schema.schemata \
                         WHERE schema_name IN ('tasks','sync','jobs','control','crt','voice','agents')",
                    )
                    .fetch_one(&pool)
                    .await
                    .expect("schemas");
                    assert_eq!(schemas, 7);

                    t.step("mark_applied is idempotent and commits with the unit of work", || ());
                    let mut uow = UnitOfWork::begin(&pool).await.expect("begin");
                    control::mark_applied(uow.tx(), 7).await.expect("mark");
                    control::mark_applied(uow.tx(), 7).await.expect("mark again");
                    uow.commit().await.expect("commit");

                    t.step("a dropped unit of work rolls its control row back", || ());
                    let mut dropped = UnitOfWork::begin(&pool).await.expect("begin");
                    control::mark_applied(dropped.tx(), 8).await.expect("mark");
                    drop(dropped);

                    let seqs: Vec<i64> =
                        sqlx::query_scalar("SELECT seq FROM control.control_applied ORDER BY seq")
                            .fetch_all(&pool)
                            .await
                            .expect("seqs");
                    assert_eq!(seqs, vec![7]);
                });
            }
        );
    }
}
