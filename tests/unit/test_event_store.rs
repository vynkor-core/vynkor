use veyron::events::store::EventStore;
use veyron::proto::veyron::Event;

fn tmp_store(tag: &str) -> EventStore {
    // S2: use a private 0o700 dir — the ownership check rejects world-writable /tmp
    let dir = std::env::temp_dir().join(format!("veyron_store_test_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // set 0o700 so the ownership check passes
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    EventStore::new(&dir).expect("EventStore::new must succeed")
}

fn ev(id: &str) -> Event {
    Event {
        event_id: id.to_string(),
        event_type: "t".to_string(),
        payload_json: b"{}".to_vec(),
        retry_count: 0,
    }
}

// ── Unit tests: persist / mark_delivered / pending_older_than ────────────────

#[test]
fn persist_creates_pending_entry() {
    let store = tmp_store("persist_creates");
    store.persist(&ev("x"));
    let ids: Vec<String> = store
        .pending_older_than(0)
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert_eq!(ids, vec!["x".to_string()]);
}

#[test]
fn persist_is_idempotent() {
    let store = tmp_store("persist_idempotent");
    store.persist(&ev("dup"));
    store.persist(&ev("dup")); // second call must be a no-op
    let rows = store.pending_older_than(0);
    assert_eq!(
        rows.len(),
        1,
        "duplicate persist must not create a second row"
    );
}

#[test]
fn mark_delivered_removes_from_pending() {
    let store = tmp_store("mark_delivered");
    store.persist(&ev("y"));
    store.mark_delivered("y");
    let rows = store.pending_older_than(0);
    assert!(
        rows.is_empty(),
        "delivered event must not appear in pending"
    );
}

#[test]
fn pending_older_than_ignores_fresh_events() {
    let store = tmp_store("fresh");
    store.persist(&ev("fresh"));
    // age threshold of 1 hour — just-created event must not appear
    let rows = store.pending_older_than(3600);
    assert!(
        rows.is_empty(),
        "fresh event must not appear before age threshold"
    );
}

// ── Unit tests: retry / dead ─────────────────────────────────────────────────

#[test]
fn increment_retry_increments_count_below_max() {
    let store = tmp_store("retry_count");
    store.persist(&ev("r"));
    store.increment_retry_or_dead("r", 5); // max=5, count becomes 1 → still pending
    let rows = store.pending_older_than(0);
    assert_eq!(rows.len(), 1, "event below max_retries must remain pending");
    assert_eq!(
        rows[0].retry_count, 1,
        "retry_count must be 1 after one increment"
    );
}

#[test]
fn increment_retry_marks_dead_at_max() {
    let store = tmp_store("retry_dead");
    store.persist(&ev("d"));
    store.increment_retry_or_dead("d", 1); // max=1, count becomes 1 → dead
    let rows = store.pending_older_than(0);
    assert!(
        rows.is_empty(),
        "event at max_retries must become dead (not pending)"
    );
}

#[test]
fn prune_removes_terminal_events_keeps_pending() {
    let store = tmp_store("prune");

    // delivered
    store.persist(&ev("a"));
    store.mark_delivered("a");
    // dead: one retry with max_retries=1 marks it dead
    store.persist(&ev("c"));
    store.increment_retry_or_dead("c", 1);
    // pending
    store.persist(&ev("b"));

    // retention 0: every terminal event up to now is eligible
    let removed = store.prune(0);
    assert_eq!(removed, 2, "delivered + dead must be pruned");

    // pending event survives
    let ids: Vec<String> = store
        .pending_older_than(0)
        .into_iter()
        .map(|e| e.event_id)
        .collect();
    assert_eq!(ids, vec!["b".to_string()], "only pending must remain");
}

#[test]
fn prune_respects_retention_window() {
    let store = tmp_store("retain");
    store.persist(&ev("a"));
    store.mark_delivered("a");

    // generous retention: the just-created terminal event is too new to prune
    let removed = store.prune(3600);
    assert_eq!(removed, 0, "recent terminal event must be retained");
}

#[cfg(unix)]
#[test]
fn new_rejects_world_writable_dir() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!(
        "veyron_store_test_world_writable_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();

    match EventStore::new(&dir) {
        Err(e) => {
            let err = e.to_string();
            assert!(
                err.contains("world-writable"),
                "error must mention world-writable: {err}"
            );
        }
        Ok(_) => panic!("EventStore::new must reject world-writable dir (AUDIT S2)"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}
