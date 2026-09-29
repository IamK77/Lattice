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
fn native_artifacts_use_read_only_hosts_and_non_cached_distribution_paths() {
    let workflow: Value =
        serde_yaml::from_str(include_str!("../.github/workflows/artifacts.yml")).unwrap();
    assert_eq!(
        workflow["permissions"],
        serde_json::json!({"contents": "read"})
    );
    let job = &workflow["jobs"]["build"];
    assert!(job.get("permissions").is_none());
    assert_eq!(job["strategy"]["fail-fast"], false);
    assert_eq!(
        job["strategy"]["matrix"]["include"],
        serde_json::json!([
            {"runner": "ubuntu-24.04", "target": "x86_64-unknown-linux-gnu"},
            {"runner": "macos-15", "target": "aarch64-apple-darwin"}
        ])
    );
    assert!(job["env"]["PREVIEW"]
        .as_str()
        .unwrap()
        .contains("workflow_dispatch"));
    let steps = job["steps"].as_array().unwrap();
    for step in steps {
        if let Some(action) = step["uses"].as_str() {
            let (_, revision) = action.split_once('@').expect("action revision");
            assert_eq!(revision.len(), 40, "unpinned action: {action}");
            assert!(revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
            if action.starts_with("actions/checkout@") {
                assert_eq!(step["with"]["persist-credentials"], false);
                assert_eq!(step["with"]["ref"], "${{ env.EXPECTED_COMMIT }}");
            }
        }
    }
    let upload = steps
        .iter()
        .find(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|name| name.starts_with("actions/upload-artifact@"))
        })
        .unwrap();
    assert_eq!(
        upload["with"]["path"],
        "${{ runner.temp }}/release-artifacts/*.tar.gz"
    );
    assert_eq!(upload["with"]["if-no-files-found"], "error");
    let recipe = steps
        .iter()
        .find_map(|step| {
            step["run"]
                .as_str()
                .filter(|script| script.contains("scripts/build_release.py"))
        })
        .unwrap();
    assert!(recipe.contains("$RUNNER_TEMP/release-artifacts"));
    assert!(recipe.contains("$EXPECTED_COMMIT"));
}

#[test]
fn publication_separates_preview_authority_and_retains_proof_before_mutating_releases() {
    let preparation: Value =
        serde_yaml::from_str(include_str!("../.github/workflows/prepare-release.yml")).unwrap();
    assert!(preparation["jobs"]["prepare"]["if"]
        .as_str()
        .unwrap()
        .contains("|| github.event_name == 'workflow_dispatch'"));
    let workflow: Value =
        serde_yaml::from_str(include_str!("../.github/workflows/release.yml")).unwrap();
    assert_eq!(workflow["permissions"], serde_json::json!({}));
    let jobs = &workflow["jobs"];
    assert_eq!(jobs["preview"]["permissions"]["contents"], "read");
    assert_eq!(jobs["publish"]["permissions"]["contents"], "write");
    assert_eq!(
        jobs["build"]["permissions"],
        serde_json::json!({"contents": "read"})
    );
    assert!(jobs["identity"]["if"]
        .as_str()
        .unwrap()
        .contains("RELEASE_AUTOMATION_ENABLED"));
    assert!(jobs["build"]["if"]
        .as_str()
        .unwrap()
        .contains("retained_id == ''"));
    for name in ["identity", "preview", "publish"] {
        let job = &jobs[name];
        assert_eq!(job["runs-on"], "ubuntu-24.04");
        for step in job["steps"].as_array().unwrap() {
            if let Some(action) = step["uses"].as_str() {
                let (_, revision) = action.split_once('@').unwrap();
                assert_eq!(revision.len(), 40);
                assert!(revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
                if action.starts_with("actions/checkout@") {
                    assert_eq!(step["with"]["persist-credentials"], false);
                }
            }
        }
    }
    let steps = jobs["publish"]["steps"].as_array().unwrap();
    let position = |needle: &str| {
        steps
            .iter()
            .position(|step| step["run"].as_str().is_some_and(|run| run.contains(needle)))
            .unwrap()
    };
    let retained = steps
        .iter()
        .position(|step| {
            step["with"]["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("sealed-release-"))
        })
        .unwrap();
    assert!(position("publish_release.py verify") < retained);
    assert!(retained < position("publish_release.py finalize"));
    assert_eq!(steps[retained]["with"]["overwrite"], false);
    assert!(steps
        .iter()
        .any(|step| step["with"]["artifact-ids"].as_str()
            == Some("${{ steps.retained.outputs.retained_id }}")));
    for name in ["preview", "publish"] {
        for step in jobs[name]["steps"].as_array().unwrap() {
            if let Some(run) = step["run"].as_str() {
                assert!(!run.contains("build_release.py") && !run.contains("cargo "));
            }
        }
    }
}

#[test]
fn stable_sync_waits_for_publication_and_has_no_signing_authority() {
    let release: Value =
        serde_yaml::from_str(include_str!("../.github/workflows/release.yml")).unwrap();
    let recovery: Value =
        serde_yaml::from_str(include_str!("../.github/workflows/sync-develop.yml")).unwrap();
    assert_eq!(
        release["jobs"]["sync"]["needs"],
        serde_json::json!(["identity", "publish"])
    );
    assert!(release["jobs"]["sync"]["if"]
        .as_str()
        .unwrap()
        .contains("preview == 'false'"));
    assert!(recovery["jobs"]["sync"]["if"]
        .as_str()
        .unwrap()
        .contains("refs/heads/main"));
    for workflow in [&release, &recovery] {
        let job = &workflow["jobs"]["sync"];
        assert_eq!(
            job["permissions"],
            serde_json::json!({"contents": "write", "pull-requests": "write"})
        );
        assert_eq!(job["concurrency"]["group"], "stable-history-sync");
        assert_eq!(job["runs-on"], "ubuntu-24.04");
        let checkout = job["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| {
                step["uses"]
                    .as_str()
                    .is_some_and(|action| action.starts_with("actions/checkout@"))
            })
            .unwrap();
        assert_eq!(checkout["with"]["fetch-depth"], 0);
        for step in job["steps"].as_array().unwrap() {
            if let Some(action) = step["uses"].as_str() {
                let (_, revision) = action.split_once('@').unwrap();
                assert_eq!(revision.len(), 40);
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
