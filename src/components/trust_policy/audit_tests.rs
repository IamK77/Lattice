use super::*;

#[test]
fn malformed_grants_are_preserved_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("grants.json");
    std::fs::write(&path, "not json").unwrap();
    let policy = TrustPolicy::from_config(Some(&json!({"grants": path})));
    assert!(policy
        .record_grant("k", "code", &json!({}), "test", "user")
        .is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "not json");
}

#[test]
fn concurrent_grants_keep_both_updates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("grants.json");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|key| {
            let barrier = barrier.clone();
            let policy = TrustPolicy::from_config(Some(&json!({"grants": path})));
            std::thread::spawn(move || {
                barrier.wait();
                policy
                    .record_grant(key, "code", &json!({}), "test", "user")
                    .unwrap();
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    let policy = TrustPolicy::from_config(Some(&json!({"grants": path})));
    assert!(policy.granted("a", &json!({})));
    assert!(policy.granted("b", &json!({})));
}
