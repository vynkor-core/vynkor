use vynkor::auth::device_store::DeviceStore;
use vynkor::auth::grants::{Grant, Grants, Preset};

const MASTER: &str = "master-secret-master-secret-master!!";

fn store() -> (tempfile::TempDir, DeviceStore) {
    let dir = tempfile::tempdir().unwrap();
    let s = DeviceStore::new(dir.path(), MASTER);
    (dir, s)
}

fn set(gs: &[Grant]) -> Grants {
    gs.iter().copied().collect()
}

fn rewrite_rows(dir: &tempfile::TempDir, f: impl FnOnce(&mut serde_json::Value)) {
    let path = dir.path().join("devices.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    f(&mut v);
    std::fs::write(&path, v.to_string()).unwrap();
}

#[test]
fn legacy_record_without_grants_is_user() {
    let (dir, s) = store();
    s.issue("d1", "phone", 60, None).unwrap();
    // strip the grants field, as an older build would have written it
    rewrite_rows(&dir, |v| {
        v[0].as_object_mut().unwrap().remove("grants");
    });
    assert_eq!(s.grants("d1").unwrap().unwrap(), Preset::User.grants());
    assert_eq!(
        Preset::User.grants(),
        set(&[
            Grant::PluginsView,
            Grant::PluginsLogs,
            Grant::PluginsControl
        ])
    );
}

#[test]
fn new_device_defaults_to_user() {
    let (_d, s) = store();
    s.issue("d1", "phone", 60, None).unwrap();
    assert_eq!(s.grants("d1").unwrap().unwrap(), Preset::User.grants());
}

#[test]
fn set_grants_round_trips_and_survives_reissue() {
    let (_d, s) = store();
    s.issue("d1", "phone", 60, None).unwrap();
    let g = set(&[Grant::PluginsView, Grant::DevicesManage]);
    assert!(s.set_grants("d1", &g).unwrap());
    assert_eq!(s.grants("d1").unwrap().unwrap(), g);
    s.issue("d1", "phone", 60, None).unwrap();
    assert_eq!(s.grants("d1").unwrap().unwrap(), g);
    s.issue("d1", "phone", 60, Some(&Preset::Guest.grants()))
        .unwrap();
    assert!(s.grants("d1").unwrap().unwrap().is_empty());
}

#[test]
fn unknown_device_has_no_grants_and_set_returns_false() {
    let (_d, s) = store();
    assert!(s.grants("ghost").unwrap().is_none());
    assert!(!s.set_grants("ghost", &Grants::new()).unwrap());
}

#[test]
fn unknown_grant_string_is_ignored_not_fatal() {
    let (dir, s) = store();
    s.issue("d1", "phone", 60, None).unwrap();
    rewrite_rows(&dir, |v| {
        v[0]["grants"] = serde_json::json!(["plugins.view", "plugins.foo"]);
    });
    assert_eq!(s.grants("d1").unwrap().unwrap(), set(&[Grant::PluginsView]));
    assert_eq!(s.list().unwrap().len(), 1);
}

#[test]
fn preset_parse_and_contents() {
    assert_eq!("guest".parse::<Preset>().unwrap().grants(), Grants::new());
    assert_eq!("owner".parse::<Preset>().unwrap().grants().len(), 5);
    assert_eq!(
        "user".parse::<Preset>().unwrap().grants(),
        set(&[
            Grant::PluginsView,
            Grant::PluginsLogs,
            Grant::PluginsControl
        ])
    );
    assert!("admin".parse::<Preset>().is_err());
    for g in Grant::ALL {
        assert_eq!(g.as_str().parse::<Grant>().unwrap(), g);
    }
    assert_eq!(Grant::PluginsInstall.as_str(), "plugins.install");
    assert!("plugins.foo".parse::<Grant>().is_err());
}
