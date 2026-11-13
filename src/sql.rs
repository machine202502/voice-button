//! Every SQL statement, embedded at build time from `sql/`.
//!
//! The files under `sql/` are the source of truth; `include_str!` keeps a copy
//! in the executable, so no `sql/` folder has to ship next to it. A query file
//! names its statements with `-- name:` markers and may use `{{placeholders}}`
//! for structural fragments. Values always stay bound parameters.
//!
//! A missing name, a duplicate name or an unknown placeholder is a programming
//! error, so the loader panics with the exact name instead of returning an
//! empty string.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Each query file and its embedded text. The stem is the prefix of every
/// statement name in the file: `telegram/chat_page`.
const QUERY_FILES: &[(&str, &str)] = &[
    ("store", include_str!("../sql/queries/store.sql")),
    ("telegram", include_str!("../sql/queries/telegram.sql")),
    ("history", include_str!("../sql/queries/history.sql")),
    ("site_db", include_str!("../sql/queries/site_db.sql")),
    ("site_api", include_str!("../sql/queries/site_api.sql")),
    ("audio", include_str!("../sql/queries/audio.sql")),
    ("session", include_str!("../sql/queries/session.sql")),
    ("hotkey", include_str!("../sql/queries/hotkey.sql")),
    ("site_network", include_str!("../sql/queries/site_network.sql")),
    ("site_tls", include_str!("../sql/queries/site_tls.sql")),
    ("site_hub", include_str!("../sql/queries/site_hub.sql")),
    ("ui", include_str!("../sql/queries/ui.sql")),
];

/// Migrations in application order, named by their file stem.
const MIGRATION_FILES: &[(&str, &str)] = &[(
    "001_telegram_added_at",
    include_str!("../sql/migrations/001_telegram_added_at.sql"),
)];

const SCHEMA: &str = include_str!("../sql/schema.sql");

/// Split one embedded file into `(qualified name, raw block)` pairs.
///
/// The raw block starts at the `-- name:` marker line and runs to the next
/// marker (or the end of the file). Only comments and blank lines may come
/// before the first marker; this is checked by the tests.
fn parse(file: &'static str, body: &'static str) -> Vec<(&'static str, &'static str)> {
    let mut statements = Vec::new();
    let mut current: Option<(&'static str, usize)> = None;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let Some(rest) = line.trim_start().strip_prefix("-- name:") else {
            continue;
        };
        if let Some((name, begin)) = current.take() {
            statements.push((name, &body[begin..start]));
        }
        let short = rest.trim();
        if short.is_empty() {
            panic!("empty SQL statement name in {file}");
        }
        let name: &'static str = Box::leak(format!("{file}/{short}").into_boxed_str());
        current = Some((name, start));
    }
    if let Some((name, begin)) = current {
        statements.push((name, &body[begin..]));
    }
    statements
}

fn statements() -> &'static HashMap<&'static str, &'static str> {
    static STATEMENTS: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    STATEMENTS.get_or_init(|| {
        let mut all = HashMap::new();
        for (file, body) in QUERY_FILES {
            for (name, sql) in parse(file, body) {
                if all.insert(name, sql).is_some() {
                    panic!("duplicate SQL statement {name:?}");
                }
            }
        }
        all
    })
}

/// The statement exactly as embedded, marker comment included.
///
/// Panics, naming the statement, when `name` is not declared in `sql/queries/`.
pub fn raw(name: &str) -> &'static str {
    *statements()
        .get(name)
        .unwrap_or_else(|| panic!("unknown SQL statement {name:?}"))
}

/// The same statement, trimmed and without its `-- name:` marker line.
pub fn query(name: &str) -> &'static str {
    let raw = raw(name);
    let body = match raw.find('\n') {
        Some(end) => &raw[end + 1..],
        None => "",
    };
    body.trim()
}

/// Replace `{{var}}` with the value supplied for it.
///
/// Only structural fragments may be templated (a WHERE clause, a table name);
/// values must stay bound parameters. Panics naming any placeholder that is
/// left over, including one misspelled in the statement.
pub fn render(name: &str, vars: &[(&str, &str)]) -> String {
    let mut rendered = query(name).to_string();
    for (key, value) in vars {
        rendered = rendered.replace(&format!("{{{{{key}}}}}"), value);
    }
    if let Some(start) = rendered.find("{{") {
        let rest = &rendered[start + 2..];
        let leftover = match rest.find("}}") {
            Some(end) => &rest[..end],
            None => rest,
        };
        panic!(
            "unknown placeholder {{{{{leftover}}}}} in SQL statement {name:?}"
        );
    }
    rendered
}

/// The whole DDL, exactly as `sql/schema.sql` holds it.
pub fn schema() -> &'static str {
    SCHEMA
}

/// Migrations as `(name, sql)`, sorted by name.
pub fn migrations() -> &'static [(&'static str, &'static str)] {
    static MIGRATIONS: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    MIGRATIONS
        .get_or_init(|| {
            let mut migrations = MIGRATION_FILES.to_vec();
            migrations.sort_by(|left, right| left.0.cmp(right.0));
            migrations
        })
        .as_slice()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Every placeholder a statement is allowed to use. A typo in `sql/` is
    /// caught here instead of panicking on a user's machine.
    const KNOWN_PLACEHOLDERS: &[&str] = &["filter", "key_column", "marks", "range", "table"];

    fn panic_message(run: impl FnOnce() + std::panic::UnwindSafe) -> String {
        let error = std::panic::catch_unwind(run).expect_err("the call must panic");
        error
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| error.downcast_ref::<&str>().map(|text| text.to_string()))
            .unwrap_or_default()
    }

    fn placeholder_names(sql: &str) -> Vec<&str> {
        let mut names = Vec::new();
        let mut rest = sql;
        while let Some(start) = rest.find("{{") {
            rest = &rest[start + 2..];
            let end = rest.find("}}").expect("a placeholder must be closed");
            names.push(&rest[..end]);
            rest = &rest[end + 2..];
        }
        names
    }

    #[test]
    fn every_statement_has_a_name() {
        for (file, body) in QUERY_FILES {
            let preamble = match body.find("-- name:") {
                Some(index) => &body[..index],
                None => body,
            };
            for line in preamble.lines() {
                let line = line.trim();
                assert!(
                    line.is_empty() || line.starts_with("--"),
                    "SQL in {file} is outside a named statement: {line:?}"
                );
            }
        }
        for name in statements().keys() {
            let (file, short) = name.split_once('/').expect("names are file-qualified");
            assert!(!file.is_empty(), "statement {name:?} has no file");
            assert!(!short.is_empty(), "statement {name:?} has no name");
        }
    }

    #[test]
    fn statement_names_are_unique() {
        let mut seen = BTreeSet::new();
        for (file, body) in QUERY_FILES {
            for (name, _) in parse(file, body) {
                assert!(seen.insert(name), "duplicate SQL statement {name:?}");
            }
        }
        assert_eq!(seen.len(), statements().len());
    }

    #[test]
    fn every_placeholder_is_known() {
        let mut used = BTreeSet::new();
        for name in statements().keys() {
            let sql = query(name);
            assert!(!sql.is_empty(), "{name} is empty");
            for placeholder in placeholder_names(sql) {
                assert!(
                    KNOWN_PLACEHOLDERS.contains(&placeholder),
                    "unknown placeholder {{{{{placeholder}}}}} in {name}"
                );
                used.insert(placeholder);
            }
        }
        for placeholder in KNOWN_PLACEHOLDERS {
            assert!(
                used.contains(placeholder),
                "placeholder {{{{{placeholder}}}}} is listed but never used"
            );
        }
    }

    #[test]
    fn render_substitutes_and_rejects_unknown_placeholders() {
        let rendered = render(
            "telegram/job_write",
            &[("table", "telegram_queue"), ("key_column", "position")],
        );
        assert!(rendered.contains("INSERT OR REPLACE INTO telegram_queue(position,"));
        assert!(!rendered.contains("{{"));

        let rendered = render("history/page_rows", &[("filter", " WHERE h.source IN (?)")]);
        assert!(rendered.contains("FROM history h WHERE h.source IN (?)"));
        assert!(!rendered.contains("{{filter}}"));

        let message = panic_message(|| {
            render("history/page_rows", &[]);
        });
        assert!(
            message.contains("{{filter}}") && message.contains("history/page_rows"),
            "panic must name the placeholder and the statement: {message}"
        );

        let message = panic_message(|| {
            raw("history/no_such_statement");
        });
        assert!(
            message.contains("history/no_such_statement"),
            "panic must name the missing statement: {message}"
        );
    }

    #[test]
    fn statements_are_loaded() {
        let count = statements().len();
        assert!(count > 0, "no SQL statements were loaded");
    }

    #[test]
    fn schema_and_migrations_are_present_and_sorted() {
        assert!(!schema().trim().is_empty(), "schema.sql is empty");
        let migrations = migrations();
        assert!(!migrations.is_empty(), "no migrations were loaded");
        assert!(migrations
            .iter()
            .all(|(name, sql)| !name.is_empty() && !sql.trim().is_empty()));
        assert!(migrations
            .windows(2)
            .all(|pair| pair[0].0 < pair[1].0), "migrations are not sorted");
    }
}
