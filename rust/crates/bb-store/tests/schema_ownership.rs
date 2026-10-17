//! Schema ownership (027 PR-21, T021; research R3, data-model.md "Rules").
//!
//! Scans every string literal in `src/<module>/` and fails when a module names a schema it
//! does not own, however the SQL reaches sqlx (`sqlx::query`, `use sqlx::query`, `raw_sql`,
//! `QueryBuilder`, a `const SQL`, `query_file!`). The `control` schema literal therefore
//! appears only inside `src/control/`; other modules reach it through the Rust call
//! `control::mark_applied`, the one allowed cross-module store call. The migration/skeleton
//! tests at the bottom need a live PostgreSQL and run only with `--features live`.

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

/// The `tasks` owner group's sub-namespaces, each a directory of its own.
const TASKS_SUBMODULES: [&str; 8] = [
    "items",
    "organize",
    "review",
    "sync",
    "devices",
    "transfers",
    "snapshots",
    "jobs",
];

fn all_schemas() -> Vec<&'static str> {
    OWNERS.iter().flat_map(|(_, s)| s.iter().copied()).collect()
}

/// End (exclusive) of the plain string literal whose opening quote is at `open`.
fn plain_end(bytes: &[u8], open: usize) -> usize {
    let mut j = open + 1;
    while j < bytes.len() && bytes[j] != b'"' {
        j += if bytes[j] == b'\\' { 2 } else { 1 };
    }
    j
}

/// Every string literal (plain, byte and raw) in `src`, skipping comments and char
/// literals. Returns the literal bodies, unescaped only as far as quotes are concerned.
fn literals(src: &str) -> Vec<String> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let prev_ident = i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if src[i..].starts_with("//") {
            i += src[i..].find('\n').unwrap_or(src.len() - i);
        } else if src[i..].starts_with("/*") {
            let mut depth = 0;
            while i < bytes.len() {
                if src[i..].starts_with("/*") {
                    depth += 1;
                    i += 2;
                } else if src[i..].starts_with("*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if (b == b'r' || (b == b'b' && bytes.get(i + 1) == Some(&b'r'))) && !prev_ident {
            let mut j = i + if b == b'b' { 2 } else { 1 };
            let hashes_start = j;
            while bytes.get(j) == Some(&b'#') {
                j += 1;
            }
            if bytes.get(j) == Some(&b'"') {
                let terminator = format!("\"{}", "#".repeat(j - hashes_start));
                let body = j + 1;
                let close = src[body..]
                    .find(&terminator)
                    .map_or(src.len(), |p| body + p);
                out.push(src[body..close].to_string());
                i = close + terminator.len();
            } else {
                i += 1;
            }
        } else if b == b'"' {
            let close = plain_end(bytes, i);
            out.push(src[i + 1..close.min(src.len())].to_string());
            i = close + 1;
        } else if b == b'\'' {
            // A char literal ('"', '\'', 'x') is skipped whole; a lifetime is just a tick.
            if bytes.get(i + 1) == Some(&b'\\') {
                i += 2 + src[i + 2..].find('\'').unwrap_or(0) + 1;
            } else if bytes.get(i + 2) == Some(&b'\'') {
                i += 3;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Schemas referenced as `schema.relation` (or `schema.{placeholder}`) in `text`, with SQL
/// identifier quoting and Rust escapes removed.
fn schemas_in(text: &str) -> Vec<String> {
    let lower: String = text
        .to_lowercase()
        .chars()
        .filter(|c| *c != '"' && *c != '\\')
        .collect();
    let mut out = Vec::new();
    for token in lower.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.')) {
        if let Some((schema, rel)) = token.split_once('.')
            && all_schemas().contains(&schema)
            && !out.iter().any(|s| s == schema)
            && (!rel.is_empty() || lower.contains(&format!("{schema}.{{")))
        {
            out.push(schema.to_string());
        }
    }
    out
}

/// Schema references in one source file: every literal, plus the content of any `.sql` file
/// a literal points at (`query_file!`, `include_str!`), resolved next to the source file.
fn schema_refs(src: &str, dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for lit in literals(src) {
        found.extend(schemas_in(&lit));
        if lit.ends_with(".sql") {
            let text = [
                dir.join(&lit),
                Path::new(env!("CARGO_MANIFEST_DIR")).join(&lit),
            ]
            .iter()
            .find_map(|p| fs::read_to_string(p).ok());
            match text {
                Some(text) => found.extend(schemas_in(&text)),
                None => found.push(format!("unresolved-sql-file:{lit}")),
            }
        }
    }
    found
}

/// Violations in one source file of `module`: a literal naming a foreign schema, and Rust
/// paths into another module's store code (except `control::mark_applied`).
fn violations(module: &str, src: &str, dir: &Path) -> Vec<String> {
    let owned = OWNERS
        .iter()
        .find(|(m, _)| *m == module)
        .map(|(_, s)| *s)
        .expect("known module");
    let mut out = Vec::new();
    for schema in schema_refs(src, dir) {
        if !owned.contains(&schema.as_str()) {
            out.push(format!("{module}: literal touches schema `{schema}`"));
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

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn module_sources(module: &str) -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    let dir = src_dir().join(module);
    assert!(dir.is_dir(), "module `{module}` must be a directory");
    rust_files(&dir, &mut files);
    files
        .into_iter()
        .map(|p| {
            let text = fs::read_to_string(&p).expect("read source");
            (p, text)
        })
        .collect()
}

#[test]
fn store_027_fr_002_every_module_is_a_directory_with_a_mod_file() {
    allure_test!(
        EPIC,
        FEATURE,
        "027-FR-002 module schema ownership",
        "Every store namespace is a directory so slices never share a file",
        |t| {
            t.step(
                "each module and tasks sub-namespace has <name>/mod.rs",
                || {
                    for (module, _) in OWNERS {
                        let dir = src_dir().join(module);
                        assert!(dir.join("mod.rs").is_file(), "src/{module}/mod.rs missing");
                        assert!(
                            !src_dir().join(format!("{module}.rs")).exists(),
                            "src/{module}.rs must not exist beside src/{module}/"
                        );
                    }
                    for sub in TASKS_SUBMODULES {
                        let dir = src_dir().join("tasks").join(sub);
                        assert!(
                            dir.join("mod.rs").is_file(),
                            "src/tasks/{sub}/mod.rs missing"
                        );
                    }
                },
            );
        }
    );
}

#[test]
fn store_027_fr_002_every_module_touches_only_its_own_schemas() {
    allure_test!(
        EPIC,
        FEATURE,
        "027-FR-002 module schema ownership",
        "Every bb-store module names only its own schemas",
        |t| {
            let mut control_refs = 0;
            t.step("scan every module's sources", || {
                for (module, _) in OWNERS {
                    let sources = module_sources(module);
                    assert!(!sources.is_empty(), "no sources for module {module}");
                    for (path, text) in sources {
                        let dir = path.parent().expect("parent");
                        if module == "control" {
                            control_refs += schema_refs(&text, dir).len();
                        }
                        let bad = violations(module, &text, dir);
                        assert!(bad.is_empty(), "{}: {bad:?}", path.display());
                    }
                }
            });
            t.step("the scanner really saw the control query", || {
                assert!(
                    control_refs >= 1,
                    "literal scanner found no schema reference"
                );
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
            let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
            let bad = |module: &str, src: &str| violations(module, src, dir).len();
            t.step("every way of reaching sqlx is caught", || {
                let plain = r#"let _ = sqlx::query("SELECT 1 FROM tasks.tasks");"#;
                assert_eq!(bad("identity", plain), 1);
                let imported = r#"use sqlx::query; query("DELETE FROM crt.trees");"#;
                assert_eq!(bad("identity", imported), 1);
                let raw_fn = r##"sqlx::raw_sql(r#"SELECT 1 FROM Control.control_applied"#)"##;
                assert_eq!(bad("crt", raw_fn), 1);
                let builder = r#"QueryBuilder::new("INSERT INTO voice.operations ")"#;
                assert_eq!(bad("agents", builder), 1);
                let konst = "const SQL: &str = \"SELECT 1 FROM agents.runs\";\nsqlx::query(SQL);";
                assert_eq!(bad("voice", konst), 1);
                let mac = r#"sqlx::query!("DELETE FROM voice.operations WHERE owner_id = $1", id)"#;
                assert_eq!(bad("agents", mac), 1);
                let quoted = r#"query("SELECT 1 FROM \"crt\".\"trees\"")"#;
                assert_eq!(bad("flags", quoted), 1);
                let formatted = r#"query(&format!("SELECT 1 FROM crt.{table}"))"#;
                assert_eq!(bad("flags", formatted), 1);
            });
            t.step(
                "the control schema literal is only allowed in control",
                || {
                    let sql =
                        r#"sqlx::query("INSERT INTO control.control_applied (seq) VALUES (1)")"#;
                    assert_eq!(bad("crt", sql), 1);
                    assert_eq!(bad("control", sql), 0);
                },
            );
            t.step("comments and prose are not queries", || {
                assert_eq!(bad("crt", "// reads crt.trees\n/* identity.users */"), 0);
                assert_eq!(bad("crt", r#"let c = '"'; let m = "no tasks. done";"#), 0);
            });
            t.step("a query_file! target is scanned too", || {
                let missing = r#"sqlx::query_file!("no/such/query.sql")"#;
                assert_eq!(bad("crt", missing), 1);
            });
            t.step("the Tasks owner group may span tasks, sync and jobs", || {
                let sql = r#"sqlx::query("INSERT INTO sync.change_records SELECT * FROM jobs.jobs JOIN tasks.tasks USING (id)")"#;
                assert_eq!(bad("tasks", sql), 0);
                assert_eq!(bad("tasks", r#"sqlx::query("SELECT 1 FROM crt.trees")"#), 1);
            });
            t.step(
                "only control::mark_applied is an allowed cross-module call",
                || {
                    let ok = "bb_store::control::mark_applied(&mut tx, 1).await?;";
                    assert_eq!(bad("crt", ok), 0);
                    assert_eq!(bad("crt", "crate::control::mark_applied(tx, 1)"), 0);
                    assert_eq!(bad("crt", "crate::control::other(tx)"), 1);
                    assert_eq!(bad("crt", "use crate::identity::lookup;"), 1);
                    assert_eq!(bad("crt", "use crate::identity_x::a;"), 0);
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
