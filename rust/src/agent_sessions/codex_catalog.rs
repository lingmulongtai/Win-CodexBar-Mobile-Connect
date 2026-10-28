//! Best-effort discovery of recently updated, older Codex session files.
//! The catalog is read-only and supplies paths, never activity evidence.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};

use crate::codex_workspaces::thread_names::{
    configured_sqlite_home, newest_state_database, sqlite_home,
};

pub(super) fn recent_rollouts(root: &Path, now: DateTime<Utc>) -> Vec<PathBuf> {
    let Some(home) = root.parent() else {
        return Vec::new();
    };
    let configured = configured_sqlite_home(home);
    let env = std::env::var("CODEX_SQLITE_HOME").ok();
    let dir = sqlite_home(home, configured.as_deref(), env.as_deref(), None);
    recent_rollouts_from_database(root, &newest_state_database(&dir), now)
}

fn recent_rollouts_from_database(root: &Path, database: &Path, now: DateTime<Utc>) -> Vec<PathBuf> {
    let Ok(canonical_root) = root.canonicalize() else {
        return Vec::new();
    };
    let Ok(conn) = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return Vec::new();
    };
    if conn.busy_timeout(Duration::from_millis(100)).is_err()
        || conn.execute_batch("PRAGMA query_only = ON").is_err()
    {
        return Vec::new();
    }
    // The index timestamp can lag the file; it narrows the candidate set and
    // is never used to decide whether a turn is still running.
    let threshold = (now - chrono::Duration::days(2)).timestamp_millis();
    let mut statement = match conn.prepare("SELECT rollout_path FROM threads WHERE updated_at_ms >= ?1 ORDER BY updated_at_ms DESC LIMIT 128") {
        Ok(statement) => statement,
        Err(_) => return Vec::new(),
    };
    let Ok(rows) = statement.query_map([threshold], |row| row.get::<_, String>(0)) else {
        return Vec::new();
    };
    rows.flatten()
        .filter_map(|raw| {
            if raw.len() > 32_768 {
                return None;
            }
            let raw_path = PathBuf::from(raw);
            if !raw_path.is_absolute() || !raw_path.starts_with(root) {
                return None;
            }
            let path = raw_path.canonicalize().ok()?;
            if !path.starts_with(&canonical_root)
                || path.extension().and_then(|s| s.to_str()) != Some("jsonl")
                || !path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("rollout-"))
            {
                return None;
            }
            let modified = DateTime::<Utc>::from(path.metadata().ok()?.modified().ok()?);
            let age = now.signed_duration_since(modified);
            // The file may change while the scan is in progress.
            (age >= chrono::Duration::seconds(-5)
                && age <= chrono::Duration::seconds(crate::discord_presence::MAX_OPEN_TURN_SECONDS))
            .then_some(path)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_only_discovers_recent_session_files_inside_the_session_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        std::fs::create_dir_all(root.join("old")).unwrap();
        let allowed = root.join("old").join("rollout-old.jsonl");
        let outside = temp.path().join("rollout-outside.jsonl");
        let wrong_name = root.join("old").join("other.jsonl");
        for path in [&allowed, &outside, &wrong_name] {
            std::fs::write(path, "{}\n").unwrap();
        }
        let database = temp.path().join("state.sqlite");
        let conn = Connection::open(&database).unwrap();
        conn.execute(
            "CREATE TABLE threads (rollout_path TEXT, updated_at_ms INTEGER)",
            [],
        )
        .unwrap();
        let now = Utc::now();
        for path in [&allowed, &outside, &wrong_name] {
            conn.execute(
                "INSERT INTO threads VALUES (?1, ?2)",
                rusqlite::params![path.to_string_lossy(), now.timestamp_millis()],
            )
            .unwrap();
        }
        let traversal = root.join("..").join("rollout-outside.jsonl");
        conn.execute(
            "INSERT INTO threads VALUES (?1, ?2)",
            rusqlite::params![traversal.to_string_lossy(), now.timestamp_millis()],
        )
        .unwrap();
        drop(conn);
        assert_eq!(
            recent_rollouts_from_database(&root, &database, now),
            vec![allowed.canonicalize().unwrap()]
        );
        assert!(
            recent_rollouts_from_database(&root, &temp.path().join("missing.sqlite"), now)
                .is_empty()
        );
        assert!(!temp.path().join("missing.sqlite").exists());
    }
}
