use serde_json::Value;

#[test]
fn ci_pins_actions_and_keeps_candidate_jobs_read_only() {
    let workflow: Value =
        serde_yaml::from_str(include_str!("../.github/workflows/ci.yml")).unwrap();
    assert_eq!(
        workflow["permissions"],
        serde_json::json!({"contents": "read"})
    );
    let jobs = workflow["jobs"].as_object().unwrap();
    for name in [
        "workflow", "check", "frontend", "platform", "msrv", "security",
    ] {
        assert!(jobs.contains_key(name), "missing CI job: {name}");
    }
    for (name, job) in jobs {
        assert!(
            job.get("permissions").is_none(),
            "unexpected job permissions: {name}"
        );
        assert!(job["timeout-minutes"].as_u64().is_some());
        for step in job["steps"].as_array().unwrap() {
            if let Some(action) = step["uses"].as_str() {
                let (_, revision) = action.split_once('@').expect("action revision");
                assert_eq!(revision.len(), 40, "unpinned action: {action}");
                assert!(revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
                if action.starts_with("actions/checkout@") {
                    assert_eq!(step["with"]["persist-credentials"], false);
                }
            }
        }
    }
}

#[test]
fn frontend_lock_is_complete_and_matches_the_private_package() {
    let package: Value = serde_json::from_str(include_str!("../clients/ink/package.json")).unwrap();
    let lock: Value =
        serde_json::from_str(include_str!("../clients/ink/package-lock.json")).unwrap();
    assert_eq!(lock["lockfileVersion"], 3);
    assert_eq!(lock["version"], package["version"]);
    let packages = lock["packages"].as_object().unwrap();
    for field in [
        "name",
        "version",
        "license",
        "dependencies",
        "devDependencies",
        "engines",
    ] {
        assert_eq!(
            packages[""][field], package[field],
            "lock metadata: {field}"
        );
    }
    for (name, entry) in packages {
        if name.is_empty() {
            continue;
        }
        assert!(entry["resolved"]
            .as_str()
            .unwrap()
            .starts_with("https://registry.npmjs.org/"));
        assert!(entry["integrity"].as_str().unwrap().starts_with("sha512-"));
    }
    assert!(!include_str!("../clients/ink/.gitignore")
        .lines()
        .any(|line| line == "package-lock.json"));
    let ci = include_str!("../.github/workflows/ci.yml");
    assert!(ci.contains("npm ci --ignore-scripts"));
    assert!(!ci.contains("npm ci ||"));
}

#[test]
fn dependency_updates_target_development_with_bounded_weekly_groups() {
    let config: Value = serde_yaml::from_str(include_str!("../.github/dependabot.yml")).unwrap();
    assert_eq!(config["version"], 2);
    let updates = config["updates"].as_array().unwrap();
    assert_eq!(updates.len(), 3);
    for update in updates {
        assert_eq!(update["target-branch"], "develop");
        assert_eq!(update["schedule"]["interval"], "weekly");
        assert!(update["open-pull-requests-limit"].as_u64().unwrap() <= 5);
        assert!(update["groups"].is_object());
    }
}
