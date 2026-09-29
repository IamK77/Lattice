use super::{capture, command, succeeded};
use lattice::core_events as ce;
use serde_json::{json, Value};
use std::time::Duration;

#[test]
fn inspection_keeps_the_selected_baseline_and_prompt_sections() {
    let home = tempfile::tempdir().unwrap();
    let overlay = home.path().join("overlay.json");
    std::fs::write(
        &overlay,
        json!({
            "instances":{"extra":{"component":"silent-ui"}},
            "wires":[{"from":"loop.out","to":"extra.display"}]
        })
        .to_string(),
    )
    .unwrap();
    let output = command(home.path())
        .args(["assembly", "ignored"])
        .env("LATTICE_SCRIPTED", "1")
        .env("LATTICE_OVERLAY", &overlay)
        .output()
        .unwrap();
    let document: Value = serde_json::from_str(succeeded(&output)).unwrap();
    assert!(output.stderr.is_empty());
    assert_eq!(document["runtimeSlots"], json!(["model", "cmodel", "ctx"]));
    assert!(document["assembly"]["instances"].get("extra").is_none());
    let baseline = home.path().join("baseline.json");
    std::fs::write(&baseline, serde_json::to_vec(&document).unwrap()).unwrap();
    let again = command(home.path())
        .arg("assembly")
        .env("LATTICE_SCRIPTED", "1")
        .env("LATTICE_ASSEMBLY", &baseline)
        .output()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(succeeded(&again)).unwrap(),
        document
    );

    let output = command(home.path())
        .args(["prompt", "ignored"])
        .env("LATTICE_SCRIPTED", "1")
        .output()
        .unwrap();
    let text = succeeded(&output);
    assert!(text.contains("─── settings ───\nthinking  (no parameter sent)\n"));
    let (_, tools) = text.split_once("─── in the schema (").unwrap();
    let names: Vec<_> = tools.lines().nth(1).unwrap().split_whitespace().collect();
    assert!(names.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(names.contains(&lattice::DEFERRED_DISPATCHER));
    assert!(text.contains(&format!(
        "─── found by {} (",
        lattice::components::tool_catalog::FIND_TOOLS
    )));
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("characters of system prompt)"));
    assert!(!home.path().join(".lattice/ledgers").exists());
}

#[test]
fn invalid_assembly_stops_each_inspector_without_falling_back() {
    let home = tempfile::tempdir().unwrap();
    for mode in ["assembly", "prompt", "serve"] {
        let mut launch = command(home.path());
        launch
            .arg(mode)
            .env("LATTICE_SCRIPTED", "1")
            .env("LATTICE_ASSEMBLY", home.path().join("missing.json"));
        let output = capture::output(launch, &[], Duration::from_secs(10)).unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("missing.json"), "{error}");
        if mode == "prompt" {
            assert!(
                error.starts_with("the standard assembly did not build:"),
                "{error}"
            );
        } else {
            assert!(error.starts_with("Error:"), "{error}");
        }
        assert!(!home.path().join(".lattice/daemon.sock").exists());
    }
}

#[test]
fn component_usage_never_writes_human_output_to_the_protocol_channel() {
    let home = tempfile::tempdir().unwrap();
    for args in [vec!["component"], vec!["component", "missing", "ignored"]] {
        let output = command(home.path()).args(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        if args.len() == 1 {
            assert!(
                error.starts_with("lattice component <name> — run a builtin as a bridge child\n")
            );
        } else {
            assert!(error.starts_with("lattice: unknown builtin component 'missing'\n"));
        }
        assert!(error.contains(
            "available: skill-consumer, skill-installer, fs-reader, fs-writer, shell-tools, net-tools, timer-tools, fs-watch\n"
        ));
    }
}

#[test]
fn builtin_file_component_speaks_only_bridge_messages_and_consumes_one_name() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("fixture.txt"), "private fixture body\n").unwrap();
    let request = json!({"v":1,"id":"ev_1_fixture","seq":1,"stream":"fixture",
        "time":"2000-01-01T00:00:00Z","type":ce::TOOL_EXEC_STARTED,"source":"loop",
        "causes":[],"payload":{"call":"read-fixture","tool":"Read","arguments":{"path":"fixture.txt"}}});
    let input = [
        json!({"hello":{"v":1,"instance":"fs","stream":"fixture","config":{"workspace":home.path()}}}),
        json!({"deliver":{"port":"execute","event":request}}),
        json!({"stop":{}}),
    ].into_iter().map(|message| format!("{message}\n")).collect::<String>();
    let mut launch = command(home.path());
    launch.args(["component", "fs-reader", "ignored"]);
    let output = capture::output(launch, input.as_bytes(), Duration::from_secs(30)).unwrap();
    let messages: Vec<Value> = succeeded(&output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(output.stderr.is_empty());
    assert_eq!(
        messages.len(),
        2,
        "one tool outcome, then its receipt: {messages:?}"
    );
    assert_eq!(messages[1], json!({"processed":true}));
    let first = messages[0].as_object().unwrap();
    assert_eq!(first.len(), 1);
    let reply = &first["emit"];
    assert_eq!(reply.as_object().unwrap().len(), 6);
    assert_eq!(reply.get("origin"), Some(&Value::Null));
    assert_eq!(reply.get("reason"), Some(&Value::Null));
    assert_eq!(reply["type"], ce::TOOL_EXEC_COMPLETED);
    assert_eq!(reply["port"], "outcome");
    assert_eq!(reply["causes"], json!(["ev_1_fixture"]));
    assert_eq!(reply["payload"]["call"], "read-fixture");
    assert_eq!(reply["payload"]["status"], "ok");
    assert!(reply["payload"]
        .to_string()
        .contains("private fixture body"));
}
