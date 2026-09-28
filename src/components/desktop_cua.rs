//! Cua is one backend, not the Desktop tool contract. All native tool names,
//! process launch choices and response shapes terminate in this adapter.
use super::desktop_driver::{Action, DesktopDriver, Failure, Frame, Modifier, Target};
use super::desktop_mcp::{DesktopMcp, DesktopTransportError};
use super::desktop_scope::{process_info, DesktopScope, WindowIdentity};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use tokio_util::sync::CancellationToken;

pub struct CuaDesktop {
    executable: PathBuf,
    protected_applications: Vec<String>,
    scope: Option<DesktopScope>,
    connection: Option<DesktopMcp>,
    home: Option<tempfile::TempDir>,
    targets: HashMap<String, WindowIdentity>,
    frames: HashMap<String, (u32, u32, f64, f64)>,
    uncertain: bool,
}
impl CuaDesktop {
    pub fn from_config(config: Option<&Value>) -> Self {
        let executable = config
            .and_then(|c| c["executable"].as_str())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("LATTICE_DESKTOP_DRIVER").map(PathBuf::from))
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                    .join(".lattice/drivers/desktop")
            });
        let protected_applications = config
            .and_then(|c| c["hostProtection"]["applications"].as_array())
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            executable,
            protected_applications,
            scope: None,
            connection: None,
            home: None,
            targets: HashMap::new(),
            frames: HashMap::new(),
            uncertain: false,
        }
    }
    fn connection(&mut self, cancel: &CancellationToken) -> Result<&mut DesktopMcp, Failure> {
        if self.connection.is_none() {
            if !cfg!(target_os = "macos") {
                return Err(Failure::new("the bundled desktop backend currently requires macOS; another backend can implement DesktopDriver"));
            }
            let executable = self.executable.canonicalize().map_err(|e| Failure::new(format!("desktop driver unavailable at {}: {e}. Install a desktop backend or configure its executable; no application was controlled",self.executable.display())))?;
            let home = tempfile::Builder::new()
                .prefix("lattice-desktop-")
                .tempdir()
                .map_err(|e| Failure::new(e.to_string()))?;
            let command = launch_command(&executable, home.path());
            self.connection = Some(DesktopMcp::spawn(command, cancel).map_err(transport_failure)?);
            self.home = Some(home);
        }
        self.connection
            .as_mut()
            .ok_or_else(|| Failure::new("desktop connection unavailable"))
    }
    fn call(
        &mut self,
        name: &str,
        args: Value,
        action: bool,
        cancel: &CancellationToken,
    ) -> Result<Value, Failure> {
        let answer = self.connection(cancel)?.call(name, args, cancel);
        let value = match answer {
            Ok(value) => value,
            Err(error) => {
                let mut failure = transport_failure(error);
                failure.may_have_run &= action;
                self.uncertain |= failure.may_have_run;
                self.connection.take();
                self.frames.clear();
                return Err(failure);
            }
        };
        if value["isError"] == true {
            let message = value["content"]
                .as_array()
                .and_then(|a| a.iter().find_map(|p| p["text"].as_str()))
                .unwrap_or("desktop driver refused the operation");
            // A tool-level error can follow a partially executed gesture too.
            let mut failure = Failure::new(message.chars().take(2048).collect::<String>());
            failure.may_have_run = action;
            self.uncertain |= action;
            return Err(failure);
        }
        Ok(value)
    }
    /// Backend readiness diagnostic; this is not an additional Desktop operation.
    pub fn check_permissions(
        &mut self,
        input: bool,
        cancel: &CancellationToken,
    ) -> Result<(), Failure> {
        let result = self.call("check_permissions", json!({}), false, cancel)?;
        require_permissions(structured(&result)?, input)
    }
    fn resolve(
        &mut self,
        target: &str,
        cancel: &CancellationToken,
    ) -> Result<WindowIdentity, Failure> {
        let previous = self.targets.get(target).cloned().ok_or_else(|| {
            Failure::new("unknown desktop target; list targets and observe the intended one")
        })?;
        self.targets(cancel)?;
        let current = self.targets.get(target).ok_or_else(|| {
            Failure::new(
                "desktop target disappeared or changed identity; do not substitute another window",
            )
        })?;
        self.scope
            .as_ref()
            .ok_or_else(|| Failure::new("desktop host protection unavailable"))?
            .revalidate(&previous, current)?;
        Ok(current.clone())
    }
}
fn require_permissions(state: &Value, input: bool) -> Result<(), Failure> {
    if state["screen_recording"] != true {
        return Err(Failure::new("macOS Screen Recording permission is missing for the host application running Lattice (Terminal in a CLI session). Grant it in System Settings; the driver will not request or bypass it."));
    }
    if input && state["accessibility"] != true {
        return Err(Failure::new("macOS Accessibility permission is missing for the host application running Lattice. Grant it in System Settings; no desktop input was sent."));
    }
    Ok(())
}

fn transport_failure(error: DesktopTransportError) -> Failure {
    Failure {
        message: error.message,
        interrupted: error.interrupted,
        may_have_run: error.may_have_run,
    }
}
fn launch_command(executable: &Path, home: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .args(["mcp", "--direct"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", home)
        .env("LANG", "en_US.UTF-8")
        .env("CUA_DRIVER_RS_TELEMETRY_ENABLED", "false")
        .env("CUA_DRIVER_RS_UPDATE_CHECK", "false");
    command
}
fn target_id(identity: &WindowIdentity) -> String {
    let mut hash = Sha256::new();
    hash.update(b"lattice:desktop:target:v1\0");
    hash.update(
        serde_json::to_vec(&(
            identity.pid,
            &identity.process_identity,
            identity.window_id,
            &identity.bundle_id,
        ))
        .expect("identity tuple"),
    );
    format!("window-{:x}", hash.finalize())
}
fn structured(value: &Value) -> Result<&Value, Failure> {
    value
        .get("structuredContent")
        .filter(|v| v.is_object())
        .ok_or_else(|| Failure::new("desktop driver returned no structured result"))
}
impl DesktopDriver for CuaDesktop {
    fn targets(&mut self, cancel: &CancellationToken) -> Result<Vec<Target>, Failure> {
        if self.scope.is_none() {
            self.scope = Some(DesktopScope::for_host(self.protected_applications.clone())?);
        }
        let apps = self.call("list_apps", json!({}), false, cancel)?;
        let windows = self.call(
            "list_windows",
            json!({"on_screen_only":true}),
            false,
            cancel,
        )?;
        let apps = structured(&apps)?["apps"]
            .as_array()
            .ok_or_else(|| Failure::new("desktop app inventory is malformed"))?;
        let windows = structured(&windows)?["windows"]
            .as_array()
            .ok_or_else(|| Failure::new("desktop window inventory is malformed"))?;
        let scope = self
            .scope
            .as_ref()
            .ok_or_else(|| Failure::new("desktop host protection unavailable"))?;
        let mut targets = HashMap::new();
        let mut result = Vec::new();
        for window in windows {
            let Some(pid) = window["pid"].as_u64().and_then(|p| u32::try_from(p).ok()) else {
                continue;
            };
            let Some(app) = apps
                .iter()
                .find(|a| a["pid"].as_u64() == Some(pid as u64) && a["running"] == true)
            else {
                continue;
            };
            let Some(window_id) = window["window_id"].as_u64() else {
                continue;
            };
            let Ok((process_identity, _)) = process_info(pid) else {
                continue;
            };
            let identity = WindowIdentity {
                pid,
                process_identity,
                window_id,
                bundle_id: app["bundle_id"].as_str().unwrap_or_default().into(),
                application: app["name"].as_str().unwrap_or_default().into(),
                title: window["title"].as_str().unwrap_or_default().into(),
            };
            if scope.check(&identity).is_err() {
                continue;
            }
            let id = target_id(&identity);
            result.push(Target {
                id: id.clone(),
                application: identity.application.clone(),
                title: identity.title.clone(),
            });
            targets.insert(id, identity);
        }
        self.frames.retain(|id, _| targets.contains_key(id));
        self.targets = targets;
        result.sort_by(|a, b| {
            (&a.application, &a.title, &a.id).cmp(&(&b.application, &b.title, &b.id))
        });
        Ok(result)
    }
    fn observe(&mut self, target: &str, cancel: &CancellationToken) -> Result<Frame, Failure> {
        self.check_permissions(false, cancel)?;
        let identity = self.resolve(target, cancel)?;
        let result = self.call("get_window_state",json!({"pid":identity.pid,"window_id":identity.window_id,"include_accessibility_tree":false,"include_screenshot":true}),false,cancel)?;
        let metadata = structured(&result)?;
        if metadata["screenshot_frame_valid"] != true {
            return Err(Failure::new(
                "driver could not establish a valid image coordinate frame for this window",
            ));
        }
        let width = metadata["screenshot_width"]
            .as_u64()
            .and_then(|x| u32::try_from(x).ok())
            .ok_or_else(|| Failure::new("screenshot width missing"))?;
        let height = metadata["screenshot_height"]
            .as_u64()
            .and_then(|x| u32::try_from(x).ok())
            .ok_or_else(|| Failure::new("screenshot height missing"))?;
        let image = result["content"]
            .as_array()
            .and_then(|a| {
                a.iter()
                    .find(|v| v["type"] == "image" && v["mimeType"] == "image/png")
            })
            .ok_or_else(|| {
                Failure::new(
                    "desktop driver returned no PNG screenshot; verify Screen Recording permission",
                )
            })?;
        let data = image["data"]
            .as_str()
            .filter(|s| s.len() <= 28 * 1024 * 1024)
            .ok_or_else(|| Failure::new("desktop screenshot encoding is missing or oversized"))?;
        let png = STANDARD
            .decode(data)
            .map_err(|_| Failure::new("desktop screenshot is not valid base64"))?;
        super::media_document::validate_png(&png)?;
        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let reader = decoder
            .read_info()
            .map_err(|e| Failure::new(e.to_string()))?;
        if (reader.info().width, reader.info().height) != (width, height) {
            return Err(Failure::new(
                "desktop image dimensions do not match the coordinate frame",
            ));
        }
        let bounds = &metadata["window_bounds"];
        let bw = bounds["width"]
            .as_f64()
            .ok_or_else(|| Failure::new("desktop window width missing"))?;
        let bh = bounds["height"]
            .as_f64()
            .ok_or_else(|| Failure::new("desktop window height missing"))?;
        // Revalidate identity after capture too; discarded pixels never go on the ledger.
        self.resolve(target, cancel)?;
        self.frames.insert(target.into(), (width, height, bw, bh));
        Ok(Frame { width, height, png })
    }
    fn act(
        &mut self,
        target: &str,
        action: &Action,
        cancel: &CancellationToken,
    ) -> Result<(), Failure> {
        if self.uncertain {
            return Err(Failure::new("a previous desktop action has unknown completion; inspect the target and recover its state before restarting the desktop connection; no further input was sent"));
        }
        let (width, height, bw, bh) = *self
            .frames
            .get(target)
            .ok_or_else(|| Failure::new("observe the desktop target before sending input"))?;
        action.validate_frame(width, height)?;
        self.check_permissions(true, cancel)?;
        let identity = self.resolve(target, cancel)?;
        let windows = self.call("list_windows", json!({"pid":identity.pid}), false, cancel)?;
        let window = structured(&windows)?["windows"]
            .as_array()
            .and_then(|a| a.iter().find(|w| w["window_id"] == identity.window_id))
            .ok_or_else(|| Failure::new("desktop target is no longer live"))?;
        let bounds = &window["bounds"];
        if bounds["width"].as_f64() != Some(bw) || bounds["height"].as_f64() != Some(bh) {
            return Err(Failure::new(
                "desktop window resized; observe its new image before acting",
            ));
        }
        let (name, args) = action_arguments(&identity, action);
        self.call(name, args, true, cancel)?;
        Ok(())
    }
    fn close(&mut self) {
        self.connection.take();
        self.home.take();
        self.targets.clear();
        self.frames.clear();
        // Closing is explicit, not an automatic retry of the uncertain action.
        self.uncertain = false;
    }
}
fn action_arguments(identity: &WindowIdentity, action: &Action) -> (&'static str, Value) {
    let mut args =
        json!({"pid":identity.pid,"window_id":identity.window_id,"delivery_mode":"foreground"});
    let name = match action {
        Action::Click {
            x,
            y,
            button,
            count,
        } => {
            args["x"] = json!(x);
            args["y"] = json!(y);
            args["button"] = json!(button);
            args["count"] = json!(count);
            "click"
        }
        Action::Type { text } => {
            args["text"] = json!(text);
            args["delay_ms"] = json!(0);
            "type_text"
        }
        Action::Key { key, modifiers } => {
            args["key"] = json!(match key.to_ascii_lowercase().as_str() {
                "enter" => "return".into(),
                "delete" => "forward_delete".into(),
                other => other.to_owned(),
            });
            args["modifiers"] = json!(modifiers
                .iter()
                .map(|m| match m {
                    Modifier::Command => "cmd",
                    Modifier::Control => "ctrl",
                    Modifier::Shift => "shift",
                    Modifier::Option => "option",
                })
                .collect::<Vec<_>>());
            "press_key"
        }
        Action::Scroll {
            x,
            y,
            direction,
            amount,
        } => {
            args["x"] = json!(x);
            args["y"] = json!(y);
            args["direction"] = json!(direction);
            args["amount"] = json!(amount);
            "scroll"
        }
        Action::Drag {
            from_x,
            from_y,
            to_x,
            to_y,
        } => {
            args["from_x"] = json!(from_x);
            args["from_y"] = json!(from_y);
            args["to_x"] = json!(to_x);
            args["to_y"] = json!(to_y);
            args["duration_ms"] = json!(500);
            "drag"
        }
    };
    (name, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn losing_a_read_connection_invalidates_its_coordinate_frames() {
        let cancel = CancellationToken::new();
        let mut command = Command::new("/usr/bin/python3");
        command.env_clear().arg("-c").arg(
            r#"import sys,json
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 ident=r['id'] if r['method']=='initialize' else r['id']+1
 print(json.dumps({'jsonrpc':'2.0','id':ident,'result':{}}),flush=True)
"#,
        );
        let mut driver = CuaDesktop::from_config(None);
        driver.connection = Some(DesktopMcp::spawn(command, &cancel).unwrap());
        driver
            .frames
            .insert("target".into(), (100, 100, 50.0, 50.0));
        let error = driver
            .call("list_apps", json!({}), false, &cancel)
            .unwrap_err();
        assert!(!error.may_have_run);
        assert!(!driver.uncertain);
        assert!(driver.connection.is_none());
        assert!(
            driver.frames.is_empty(),
            "a new connection must not use an old coordinate frame"
        );
    }
    #[test]
    fn missing_or_unknown_os_permissions_refuse_before_input() {
        for state in [
            json!({}),
            json!({"screen_recording":false,"accessibility":true}),
            json!({"screen_recording":true,"accessibility":false}),
        ] {
            let error = require_permissions(&state, true).unwrap_err();
            assert!(!error.may_have_run);
            assert!(!error.interrupted);
        }
        assert!(require_permissions(
            &json!({"screen_recording":true,"accessibility":false}),
            false
        )
        .is_ok());
        assert!(
            require_permissions(&json!({"screen_recording":true,"accessibility":true}), true)
                .is_ok()
        );
    }
    #[test]
    fn backend_launch_is_private_and_has_no_telemetry_or_update_requests() {
        let command = launch_command(Path::new("/driver"), Path::new("/private/state"));
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["mcp", "--direct"]);
        let vars: HashMap<_, _> = command
            .get_envs()
            .map(|(k, v)| (k.to_str().unwrap(), v.and_then(|v| v.to_str())))
            .collect();
        assert_eq!(vars["HOME"], Some("/private/state"));
        assert_eq!(vars["CUA_DRIVER_RS_TELEMETRY_ENABLED"], Some("false"));
        assert_eq!(vars["CUA_DRIVER_RS_UPDATE_CHECK"], Some("false"));
        assert!(!vars.contains_key("CUA_DRIVER_DANGEROUSLY_BYPASS_APPROVALS"));
    }
    #[test]
    fn public_targets_ignore_title_changes_but_not_native_identity_changes() {
        let mut identity = WindowIdentity {
            pid: 42,
            process_identity: "birth".into(),
            window_id: 7,
            bundle_id: "app".into(),
            application: "App".into(),
            title: "First".into(),
        };
        let id = target_id(&identity);
        identity.title = "Second".into();
        assert_eq!(id, target_id(&identity));
        identity.process_identity = "new birth".into();
        assert_ne!(id, target_id(&identity));
        let (_, args) = action_arguments(
            &identity,
            &Action::Type {
                text: "literal\ntext".into(),
            },
        );
        assert_eq!(args["pid"], 42);
        assert_eq!(args["window_id"], 7);
        assert_eq!(args["text"], "literal\ntext");
        assert!(args.get("scope").is_none());
        for (key, native) in [
            ("enter", "return"),
            ("delete", "forward_delete"),
            ("backspace", "backspace"),
        ] {
            let (_, args) = action_arguments(
                &identity,
                &Action::Key {
                    key: key.into(),
                    modifiers: vec![],
                },
            );
            assert_eq!(args["key"], native);
        }
    }
}
