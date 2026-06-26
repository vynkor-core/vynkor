use veyron::events::store::EventStore;
use veyron::proto::veyron::Event;

fn tmp_store(tag: &str) -> EventStore {
    let dir = std::env::temp_dir().join(format!("veyron_store_test_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
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
