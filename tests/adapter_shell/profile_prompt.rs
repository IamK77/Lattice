//! Selected-entry text reaches real adapter wires; switching uses the public session path.
use super::*;
use lattice::components::{context_gate, minimal_loop, silent_ui};
use lattice::preset::{self, PresetConfig};
use lattice::session::{RenderEvent, Session};

struct Environment(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl Environment {
    fn set(values: &[(&'static str, &std::path::Path)]) -> Self {
        let previous = values
            .iter()
            .map(|(key, path)| {
                let previous = std::env::var_os(key);
                std::env::set_var(key, path);
                (*key, previous)
            })
            .collect();
        Self(previous)
    }
}
impl Drop for Environment {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

struct Running(Option<Session>);
impl Drop for Running {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            session.shutdown();
        }
    }
}

fn drain(session: &Session, model_change: bool) -> (Vec<EventEnvelope>, Vec<String>) {
    let mut events = Vec::new();
    let mut notices = Vec::new();
    loop {
        match session
            .next_render_timeout(Duration::from_secs(10))
            .expect("bounded session progress")
        {
            RenderEvent::Appended(event) => events.push(*event),
            RenderEvent::Notice { source, payload } if source == "model" => {
                if let Some(note) = payload["note"].as_str() {
                    notices.push(note.to_owned());
                }
            }
            // A dial also wakes the worker. An older quiet notification is
            // not the completion of the command this test just issued.
            RenderEvent::Quiescent
                if (model_change && !notices.is_empty())
                    || (!model_change
                        && events
                            .iter()
                            .any(|event| event.event_type == ce::TURN_COMPLETED)) =>
            {
                return (events, notices);
            }
            _ => {}
        }
    }
}

fn launch_config(entry: &lattice::models::Entry) -> PresetConfig {
    PresetConfig {
        adapter: entry.adapter.clone(),
        model: entry.model.clone(),
        entry_id: Some(entry.id.clone()),
        base_url: entry.base_url.clone(),
        key_env: entry.key_env.clone(),
        workspace: None,
        system: "SYNTHETIC_COMMON {model}".into(),
        context_window: 100_000,
        usage_input_field: "input_tokens".into(),
        profile: entry.profile.clone(),
        catalog_problems: vec![],
        scripted: None,
        assembly: None,
        overlay: None,
        thinking: None,
    }
}

fn launch(candidate: Candidate, cfg: PresetConfig) -> Session {
    Session::spawn("ui", move |render_tx| {
        let model_name = candidate.manifest.name.clone();
        let registry: HashMap<String, ComponentManifest> = [
            silent_ui::manifest(),
            minimal_loop::manifest(),
            context_gate::manifest(),
            candidate.manifest,
        ]
        .into_iter()
        .map(|m| (m.name.clone(), m))
        .collect();
        let mut factories: HashMap<String, Factory> = HashMap::new();
        factories.insert(
            silent_ui::NAME.into(),
            Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::new(Mutex::new(Vec::new()))))),
        );
        factories.insert(
            minimal_loop::NAME.into(),
            Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
        );
        factories.insert(
            context_gate::NAME.into(),
            Box::new(|c| Box::new(context_gate::ContextGate::from_config(c))),
        );
        factories.insert(model_name.clone(), candidate.factory);
        let entry = preset::running_entry(&cfg);
        let assembly = AssemblyManifest {
            instances: [
                ("ui", ComponentInstance::new(silent_ui::NAME, None)),
                ("loop", ComponentInstance::new(minimal_loop::NAME, None)),
                (
                    "ctx",
                    ComponentInstance::new(
                        context_gate::NAME,
                        Some(json!({
                            "system": cfg.system, "modelName": cfg.model,
                            "profile": preset::model_profile(&cfg).unwrap(),
                        })),
                    ),
                ),
                (
                    preset::MAIN_MODEL,
                    ComponentInstance::new(
                        &model_name,
                        Some(preset::main_model_config(&entry, None)),
                    ),
                ),
            ]
            .into_iter()
            .map(|(id, instance)| (id.into(), instance))
            .collect(),
            wires: vec![
                Wire::new("ui.user", "loop.input"),
                Wire::new("ui.answer", "ctx.dial"),
                Wire::new("loop.ask", "ctx.ask"),
                Wire::new("ctx.forward", "model.request"),
                Wire::new("model.result", "loop.model"),
                Wire::new("loop.out", "ui.display"),
            ],
        };
        let mut kernel = Kernel::start(
            &assembly,
            &registry,
            &mut factories,
            KernelOptions::default(),
        )?;
        kernel.adopt_factories(factories);
        let for_log = render_tx.clone();
        kernel.subscribe_log(move |event| {
            let _ = for_log.send(RenderEvent::Appended(Box::new(event.clone())));
        });
        kernel.set_notice_handler(move |source, payload| {
            let _ = render_tx.send(RenderEvent::Notice {
                source: source.to_owned(),
                payload: payload.clone(),
            });
        });
        Ok(kernel)
    })
    .unwrap()
}

fn wire_system(adapter: &str, request: &Value) -> String {
    if adapter == "responses" {
        return request["instructions"].as_str().unwrap().to_owned();
    }
    let value = if adapter == "anthropic" {
        &request["system"]
    } else {
        &request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "system")
            .unwrap()["content"]
    };
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|block| block["text"].as_str().unwrap())
        .collect()
}

#[test]
fn selection_switch_inspection_and_actual_wires_use_the_effective_entry() {
    // These catalog/preference variables are used only by this test in this binary.
    // The adapters' shared credential fixture remains owned by ensure_key().
    ensure_key();
    for candidate in transport_candidates() {
        let adapter = preset::adapter_of(&candidate.manifest.name).to_owned();
        let sse = happy_sse(&candidate.manifest.name);
        let (url, hits, bodies) = fake_api(vec![Canned::Sse(sse)]);
        let home = tempfile::tempdir().unwrap();
        let catalog = home.path().join("models.json");
        let preferences = home.path().join("preferences.json");
        let _environment = Environment::set(&[
            ("LATTICE_MODELS", &catalog),
            ("LATTICE_PREFERENCES", &preferences),
        ]);
        let a = " \nSYNTHETIC_A {model}\n ";
        let b = "SYNTHETIC_B {model}";
        let mut models = serde_json::Map::new();
        for (id, supplement) in [
            ("entry-a", Some(json!(a))),
            ("entry-b", Some(json!(b))),
            ("entry-c", None),
            ("bad-entry", Some(json!(17))),
        ] {
            let mut profile = json!({"contextWindow":100_000});
            if let Some(text) = supplement {
                profile["systemPromptSupplement"] = text;
            }
            models.insert(
                id.into(),
                json!({"adapter":adapter, "model":"shared-model", "baseUrl":url,
                "apiKeyEnv":"LATTICE_SHELL_TEST_KEY_UNSET", "profile":profile}),
            );
        }
        std::fs::write(
            &catalog,
            serde_json::to_vec(&json!({"models":models})).unwrap(),
        )
        .unwrap();
        let entry = lattice::models::load_from(&catalog)
            .0
            .into_iter()
            .find(|entry| entry.id == "entry-a")
            .unwrap();
        let running = Running(Some(launch(candidate, launch_config(&entry))));
        let session = running.0.as_ref().unwrap();
        let mut common = String::new();

        for (index, (id, supplement)) in [
            ("entry-a", Some(a)),
            ("entry-b", Some(b)),
            ("entry-c", None),
        ]
        .into_iter()
        .enumerate()
        {
            if index > 0 {
                session.set_model(id);
                let (events, notices) = drain(session, true);
                assert!(
                    notices.iter().any(|note| note.contains("model is now")),
                    "{adapter}: {notices:?}"
                );
                assert!(events
                    .iter()
                    .any(|event| event.event_type == ce::COMPONENT_REPLACED));
                assert_eq!(lattice::preferences::load()["model"].as_str(), Some(id));
            }
            session.send_text("synthetic input");
            let (events, _) = drain(session, false);
            let forwarded = events
                .iter()
                .find(|event| event.event_type == ce::MODEL_CALL_STARTED && event.source == "ctx")
                .unwrap();
            if index == 0 {
                common = forwarded.payload["system"]
                    .as_str()
                    .unwrap()
                    .strip_suffix(&format!("\n\n{a}"))
                    .expect("the authored text is appended verbatim")
                    .to_owned();
                assert!(common.starts_with("SYNTHETIC_COMMON shared-model"));
                assert!(!common.contains("SYNTHETIC_A"));
                assert!(!common.contains("SYNTHETIC_B"));
            }
            let expected = match supplement {
                Some(text) => format!("{common}\n\n{text}"),
                None => common.clone(),
            };
            assert_eq!(
                forwarded.payload["system"], expected,
                "audited {adapter} request"
            );
            assert_eq!(hits.load(Ordering::SeqCst), index + 1);
            let body: Value = serde_json::from_str(bodies.lock().unwrap().last().unwrap()).unwrap();
            assert_eq!(
                wire_system(&adapter, &body),
                expected,
                "actual {adapter} wire"
            );
            assert!(body.get("contextSystemProfile").is_none());

            if id == "entry-b" {
                let output = std::process::Command::new(env!("CARGO_BIN_EXE_lattice"))
                    .arg("prompt")
                    .current_dir(home.path())
                    .env("HOME", home.path())
                    .env_remove("LATTICE_SCRIPTED")
                    .env_remove("LATTICE_ADAPTER")
                    .env_remove("LATTICE_MODEL")
                    .env_remove("LATTICE_BASE_URL")
                    .env_remove("LATTICE_API_KEY_ENV")
                    .env_remove("LATTICE_ASSEMBLY")
                    .env("LATTICE_OVERLAY", "")
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let inspected = String::from_utf8(output.stdout).unwrap();
                assert_eq!(inspected.matches(b).count(), 1);
                assert!(!inspected.contains("SYNTHETIC_A"));
                assert_eq!(
                    hits.load(Ordering::SeqCst),
                    index + 1,
                    "inspection sends no request"
                );
            }
        }

        session.set_model("bad-entry");
        let (events, notices) = drain(session, true);
        assert!(notices.iter().any(|note| note.contains("bad-entry")
            && note.contains("profile.systemPromptSupplement must be a string")));
        assert!(!events
            .iter()
            .any(|event| event.event_type == ce::COMPONENT_REPLACED));
        assert_eq!(
            lattice::preferences::load()["model"].as_str(),
            Some("entry-c")
        );
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        session.send_text("after the rejected selection");
        let (events, _) = drain(session, false);
        let forwarded = events
            .iter()
            .find(|event| event.event_type == ce::MODEL_CALL_STARTED && event.source == "ctx")
            .unwrap();
        assert_eq!(forwarded.payload["system"], common);
        assert_eq!(hits.load(Ordering::SeqCst), 4);
    }
}
