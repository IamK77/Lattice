use super::*;

#[test]
fn the_marked_row_is_the_one_requests_go_to_not_the_first_of_that_name() {
    let seat = |id: &str, base_url: &str| models::Entry {
        id: id.to_string(),
        adapter: "openai".to_string(),
        model: "gpt-5.6-terra".to_string(),
        base_url: base_url.to_string(),
        key_env: "K".to_string(),
        profile: None,
    };
    let mine = seat("mirror", "https://second.example");
    let view = from_entries(
        vec![seat("first", "https://first.example"), mine.clone()],
        &mine,
    );
    assert_eq!(view.now, Some(1), "the endpoint decides, not a shared name");
}

#[test]
fn rereading_keeps_the_running_target_and_observes_catalog_changes() {
    crate::terminal_host::test_support::isolated(|| {
        let path = models::path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let running = models::Entry {
            id: "running".into(),
            adapter: "scripted".into(),
            model: "fixture".into(),
            base_url: "https://running.example/v1".into(),
            key_env: "UNSET_FIXTURE_KEY".into(),
            profile: None,
        };
        let empty = load(&running);
        assert_eq!(empty.rows.len(), 1);
        assert_eq!(empty.now, Some(0));
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"models": {
                "spare": {"adapter":"openai", "model":"fixture",
                    "baseUrl":"https://spare.example/v1", "apiKeyEnv":"UNSET_FIXTURE_KEY"}
            }}))
            .unwrap(),
        )
        .unwrap();
        let changed = load(&running);
        assert_eq!(changed.rows.len(), 2);
        assert_eq!(
            changed.rows[changed.now.unwrap()].endpoint,
            "running.example/v1"
        );
        assert!(changed.rows.iter().any(|row| row.id == "spare"));
        assert!(changed.rows.iter().all(|row| !row.key_present));
        // Changes belong to a new read, not to an already handed-out snapshot.
        assert_eq!(empty.rows.len(), 1);
    });
}
