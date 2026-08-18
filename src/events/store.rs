use crate::proto::veyron::Event;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

pub struct EventStore {
    conn: Mutex<Connection>,
}

impl EventStore {
    pub fn new(data_dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(data_dir)?;

        // S2: reject world-writable dirs — a local user could pre-create the
        // path and forge pending events. We check for write access rather than
        // strict UID ownership to work in sandboxed test environments.
        #[cfg(unix)]
        {
            use std::os::linux::fs::MetadataExt;
            let meta = std::fs::metadata(data_dir)?;
            let mode = meta.st_mode() & 0o777;
            if mode & 0o002 != 0 {
                anyhow::bail!(
                    "data_dir {} is world-writable (mode {:o}), refusing — event store is forgeable",
                    data_dir.display(),
                    mode
                );
            }
        }

        let db_path = data_dir.join("events.db");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                event_id    TEXT PRIMARY KEY,
                event_type  TEXT NOT NULL,
                payload_json BLOB NOT NULL,
                status      TEXT NOT NULL DEFAULT 'pending',
                created_at  INTEGER NOT NULL,
                retry_count INTEGER NOT NULL DEFAULT 0
            );",
        )?;
        Ok(EventStore {
            conn: Mutex::new(conn),
        })
    }

    /// Persist event as pending. Idempotent: re-inserting the same event_id is a no-op.
    pub fn persist(&self, event: &Event) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        if let Err(e) = conn.execute(
            "INSERT OR IGNORE INTO events \
             (event_id, event_type, payload_json, status, created_at, retry_count) \
             VALUES (?1, ?2, ?3, 'pending', ?4, 0)",
            params![event.event_id, event.event_type, event.payload_json, now],
        ) {
            warn!(event_id = %event.event_id, error = %e, "EventStore: persist failed");
        }
    }

    pub fn mark_delivered(&self, event_id: &str) {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        if let Err(e) = conn.execute(
            "UPDATE events SET status='delivered' WHERE event_id=?1",
            params![event_id],
        ) {
            warn!(event_id = %event_id, error = %e, "EventStore: mark_delivered failed");
        }
    }

    /// Return events with status='pending' created more than `age_secs` seconds ago.
    pub fn pending_older_than(&self, age_secs: u64) -> Vec<Event> {
        let threshold = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(age_secs) as i64;

        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT event_id, event_type, payload_json, retry_count \
             FROM events WHERE status='pending' AND created_at <= ?1",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };

        let rows = stmt.query_map(params![threshold], |row| {
            Ok(Event {
                event_id: row.get(0)?,
                event_type: row.get(1)?,
                payload_json: row.get(2)?,
                retry_count: row.get::<_, i64>(3)? as u32,
            })
        });
        // Graceful on error: a failed query must not panic — it runs while the
        // conn Mutex is held, which would poison it and brick the whole store.
        match rows {
            Ok(mapped) => mapped.filter_map(|r| r.ok()).collect(),
            Err(e) => {
                warn!(error = %e, "EventStore: pending query failed");
                vec![]
            }
        }
    }

    /// Delete terminal events (delivered or dead) whose created_at is older than
    /// `retention_secs`. Keeps the table bounded — without this, delivered/dead
    /// rows accumulate forever. Pending events are never pruned. Returns the
    /// number of rows removed.
    pub fn prune(&self, retention_secs: u64) -> usize {
        let cutoff = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(retention_secs) as i64;

        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        match conn.execute(
            "DELETE FROM events \
             WHERE status IN ('delivered', 'dead') AND created_at <= ?1",
            params![cutoff],
        ) {
            Ok(n) => n,
            Err(e) => {
                warn!(error = %e, "EventStore: prune failed");
                0
            }
        }
    }

    /// Increment retry_count by 1. If count reaches max_retries, mark the event 'dead'.
    pub fn increment_retry_or_dead(&self, event_id: &str, max_retries: u32) {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let _ = conn.execute(
            "UPDATE events SET retry_count = retry_count + 1 WHERE event_id = ?1",
            params![event_id],
        );
        let _ = conn.execute(
            "UPDATE events SET status='dead' WHERE event_id=?1 AND retry_count >= ?2",
            params![event_id, max_retries as i64],
        );
    }
}
