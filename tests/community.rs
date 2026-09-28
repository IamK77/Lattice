use serde_json::Value;
use std::collections::HashSet;

#[test]
fn package_metadata_and_notices_agree_on_the_license() {
    let cargo = include_str!("../Cargo.toml");
    assert!(cargo.contains("license = \"Apache-2.0\""));
    assert!(cargo.contains("publish = false"));
    let package: Value = serde_json::from_str(include_str!("../clients/ink/package.json")).unwrap();
    assert_eq!(package["license"], "Apache-2.0");
    assert_eq!(package["private"], true);
    let license = include_str!("../LICENSE");
    assert!(license.contains("Version 2.0, January 2004"));
    assert!(license.contains("END OF TERMS AND CONDITIONS"));
    assert!(include_str!("../NOTICE").contains("assets/earth_mask.bin"));
    assert!(include_str!("../assets/README.md").contains("Keep this attribution with the asset."));
}

#[test]
fn issue_forms_collect_reproductions_and_route_secrets_privately() {
    for (text, expected_label) in [
        (
            include_str!("../.github/ISSUE_TEMPLATE/bug_report.yml"),
            "bug",
        ),
        (
            include_str!("../.github/ISSUE_TEMPLATE/feature_request.yml"),
            "enhancement",
        ),
    ] {
        let form: Value = serde_yaml::from_str(text).unwrap();
        assert!(form["name"].as_str().is_some_and(|v| !v.is_empty()));
        assert_eq!(form["labels"][0], expected_label);
        let mut ids = HashSet::new();
        let body = form["body"].as_array().unwrap();
        for field in body {
            if field["type"] != "markdown" {
                let id = field["id"].as_str().expect("non-markdown fields need ids");
                assert!(ids.insert(id), "duplicate issue field: {id}");
                assert!(field["attributes"]["label"].is_string());
            }
        }
        assert!(body.iter().any(|f| f["validations"]["required"] == true));
    }
    let bug: Value =
        serde_yaml::from_str(include_str!("../.github/ISSUE_TEMPLATE/bug_report.yml")).unwrap();
    for id in ["version", "platform", "reproduce", "expected", "actual"] {
        let field = bug["body"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["id"] == id)
            .unwrap();
        assert_eq!(field["validations"]["required"], true, "{id}");
    }
    let config: Value =
        serde_yaml::from_str(include_str!("../.github/ISSUE_TEMPLATE/config.yml")).unwrap();
    assert_eq!(config["blank_issues_enabled"], false);
    assert!(config["contact_links"]
        .as_array()
        .unwrap()
        .iter()
        .any(|link| {
            link["url"] == "https://github.com/IamK77/Lattice/security/advisories/new"
        }));
    assert!(include_str!("../SECURITY.md").contains("security/advisories/new"));
    assert!(include_str!("../.github/CODEOWNERS").contains("* @IamK77"));
}
