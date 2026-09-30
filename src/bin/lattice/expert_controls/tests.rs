use super::*;
fn definition() -> Value {
    json!({"v":1,"id":"reviewer","name":"Reviewer","description":"Review changes","instructions":"Check boundaries","model":"configured-model","capabilities":["read"]})
}
fn details() -> Value {
    json!({"target":{"scope":"personal","root":"/fixture/home","id":"reviewer"},"definition":definition(),"fileVersion":"old-file","activation":null,
        "activateArguments":{"operation":"activate","target":{"scope":"personal","id":"reviewer"},"expectedActivation":null}})
}
fn deliver(ui: &mut ExpertControls, payload: Value) {
    let mut payload = payload;
    payload["request"] = json!(ui.pending.as_ref().unwrap().0);
    let event: EventEnvelope = serde_json::from_value(json!({"v":1,"id":"receipt","seq":1,"stream":"fixture","time":"fixture","source":"expert-ui","type":lattice::components::expert_ui::RESULT,"causes":[],"payload":payload})).unwrap();
    ui.observe(&event);
}
#[test]
fn multiline_unicode_editor_preserves_text_and_uses_an_explicit_save_key() {
    let mut form = Form::new(&definition(), None);
    form.at = 6;
    form.values[6].clear();
    form.cursor = 0;
    form.insert("检查边界\r\n第二行").unwrap();
    form.key(KeyCode::Left);
    form.key(KeyCode::Backspace);
    assert_eq!(form.values[6], "检查边界\n第行");
    form.key(KeyCode::Enter);
    assert_eq!(form.values[6], "检查边界\n第\n行");
    form.at = 2;
    form.cursor = 0;
    assert!(form.insert("bad\nname").is_err());
    let mut ui = ExpertControls {
        form: Some(form),
        ..Default::default()
    };
    ui.key(KeyCode::Enter);
    assert!(ui.outgoing.is_empty());
    ui.key(KeyCode::Esc);
    assert!(ui.form.is_none() && ui.outgoing.is_empty());
}
#[test]
fn new_identity_is_inspected_before_save_and_existing_content_is_not_overwritten() {
    let mut ui = ExpertControls {
        form: Some(Form::new(&definition(), None)),
        ..Default::default()
    };
    ui.key(KeyCode::F(2));
    assert_eq!(ui.outgoing[0].1, "inspect");
    assert_eq!(ui.outgoing[0].2["expert"], "project:reviewer");
    ui.outgoing.clear();
    deliver(&mut ui, json!({"status":"ok","result":details()}));
    assert!(ui.outgoing.is_empty());
    assert!(ui.form.is_some());
    assert!(ui.notice.contains("already exists"));
}
#[test]
fn creation_uses_inspected_destination_and_never_inherits_source_activation() {
    let mut ui = ExpertControls {
        form: Some(Form::new(&definition(), None)),
        ..Default::default()
    };
    ui.key(KeyCode::F(2));
    ui.outgoing.clear();
    let target = json!({"scope":"project","root":"/fixture/project","id":"reviewer"});
    deliver(
        &mut ui,
        json!({"status":"ok","result":{"target":target,"fileVersion":null,"activation":{"v":2,"state":"deleted"}}}),
    );
    assert_eq!(ui.outgoing.len(), 1);
    assert_eq!(ui.outgoing[0].1, "save");
    assert_eq!(ui.outgoing[0].2["target"], target);
    assert_eq!(ui.outgoing[0].2["expectedActivation"]["state"], "deleted");
    assert!(ui.outgoing[0].2["fileVersion"].is_null());
}
#[test]
fn editing_keeps_exact_tokens_and_retains_draft_after_conflict() {
    let original = details();
    let mut ui = ExpertControls {
        details: Some(original.clone()),
        ..Default::default()
    };
    ui.key(KeyCode::Char('e'));
    assert_eq!(ui.form.as_ref().unwrap().at, 2);
    ui.key(KeyCode::BackTab);
    assert_eq!(
        ui.form.as_ref().unwrap().at,
        6,
        "identity fields cannot rename an existing definition"
    );
    ui.key(KeyCode::F(2));
    let args = &ui.outgoing[0].2;
    assert_eq!(args["fileVersion"], original["fileVersion"]);
    assert_eq!(args["target"], original["target"]);
    deliver(
        &mut ui,
        json!({"status":"error","error":{"message":"file changed"}}),
    );
    assert!(ui.form.is_some());
    assert!(ui.notice.contains("file changed"));
    assert!(ui.pending.is_none());
}
#[test]
fn builtins_are_read_only_but_can_be_copied_without_their_identity() {
    let mut ui = ExpertControls {
        details: Some(json!({"builtin":true,"copyTemplate":definition()})),
        ..Default::default()
    };
    ui.key(KeyCode::Char('e'));
    assert!(ui.form.is_none());
    ui.key(KeyCode::Char('c'));
    let form = ui.form.as_ref().unwrap();
    assert!(form.original.is_none());
    assert_eq!(form.values[0], "project");
    assert!(form.values[1].is_empty());
    assert_eq!(form.values[6], "Check boundaries");
}
#[test]
fn activation_refreshes_exact_state_instead_of_reusing_stale_delete_arguments() {
    let mut ui = ExpertControls {
        details: Some(details()),
        ..Default::default()
    };
    ui.key(KeyCode::Char('a'));
    ui.outgoing.clear();
    deliver(
        &mut ui,
        json!({"status":"ok","result":{"activation":{"v":1}}}),
    );
    assert_eq!(ui.outgoing.len(), 1);
    assert_eq!(ui.outgoing[0].1, "inspect");
    assert_eq!(ui.outgoing[0].2["expert"], "personal:reviewer");
}
