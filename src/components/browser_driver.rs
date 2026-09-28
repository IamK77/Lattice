//! A private headless Chromium instance. No desktop input or existing profile.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tungstenite::{client, Message, WebSocket};

pub struct Browser {
    watch: CancellationWatch,
    process: BrowserProcess,
    cancel_socket: Arc<OnceLock<TcpStream>>,
    socket: WebSocket<TcpStream>,
    session: String,
    next_id: u64,
    events: std::collections::VecDeque<Value>,
    _profile: tempfile::TempDir,
}

impl Browser {
    pub fn launch(executable: &str) -> Result<Self, String> {
        Self::launch_cancellable(executable, &CancellationToken::new())
    }

    pub fn launch_cancellable(
        executable: &str,
        cancel: &CancellationToken,
    ) -> Result<Self, String> {
        if cancel.is_cancelled() {
            return Err("browser startup interrupted".into());
        }
        let profile = tempfile::Builder::new()
            .prefix("lattice-browser-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let mut command = Command::new(executable);
        command
            .env_clear()
            .env("HOME", profile.path())
            .env("TMPDIR", std::env::temp_dir())
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin")
            .env("LANG", "en_US.UTF-8");
        command
            .args([
                "--headless=new",
                "--remote-debugging-address=127.0.0.1",
                "--remote-debugging-port=0",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-extensions",
                "--disable-sync",
                "--password-store=basic",
                "--use-mock-keychain",
                "--disable-background-networking",
                "--disable-component-update",
                "--window-size=1280,800",
            ])
            .arg(format!("--user-data-dir={}", profile.path().display()))
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                command.pre_exec(|| {
                    if libc::setpgid(0, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot start isolated browser: {e}"))?;
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                stop(&mut child);
                return Err("browser stderr missing".into());
            }
        };
        let process = BrowserProcess(Arc::new(Mutex::new(Some(child))));
        let cancel_socket = Arc::new(OnceLock::new());
        let watch = CancellationWatch::new(cancel, process.0.clone(), cancel_socket.clone())?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(url) = line.strip_prefix("DevTools listening on ") {
                    let _ = tx.send(url.to_owned());
                }
            }
        });
        let result = (|| {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            let url = loop {
                if cancel.is_cancelled() {
                    return Err("browser startup interrupted".into());
                }
                if std::time::Instant::now() >= deadline {
                    return Err("browser did not publish its control endpoint".into());
                }
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(url) => break url,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => {
                        return Err("browser exited before publishing its control endpoint".into())
                    }
                }
            };
            if !url.starts_with("ws://127.0.0.1:") {
                return Err("browser control endpoint is not loopback".to_string());
            }
            let parsed = reqwest::Url::parse(&url).map_err(|e| e.to_string())?;
            let port = parsed.port().ok_or("browser control port missing")?;
            let stream =
                TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(1))
                    .map_err(|e| e.to_string())?;
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .map_err(|e| e.to_string())?;
            stream
                .set_write_timeout(Some(Duration::from_secs(1)))
                .map_err(|e| e.to_string())?;
            cancel_socket
                .set(stream.try_clone().map_err(|e| e.to_string())?)
                .map_err(|_| "browser cancel socket already set")?;
            if cancel.is_cancelled() {
                return Err("browser connection interrupted".into());
            }
            let (socket, _) = client(url.as_str(), stream).map_err(|e| e.to_string())?;
            socket
                .get_ref()
                .set_read_timeout(Some(Duration::from_secs(10)))
                .map_err(|e| e.to_string())?;
            socket
                .get_ref()
                .set_write_timeout(Some(Duration::from_secs(10)))
                .map_err(|e| e.to_string())?;
            Ok(socket)
        })();
        let socket = result?;
        let mut browser = Self {
            watch,
            process,
            cancel_socket,
            socket,
            session: String::new(),
            next_id: 0,
            events: std::collections::VecDeque::new(),
            _profile: profile,
        };
        let target = browser.rpc("Target.createTarget", json!({"url":"about:blank"}))?;
        let attached = browser.rpc(
            "Target.attachToTarget",
            json!({"targetId":target["targetId"],"flatten":true}),
        )?;
        browser.session = attached["sessionId"]
            .as_str()
            .ok_or("browser session missing")?
            .to_owned();
        browser.rpc("Page.enable", json!({}))?;
        browser.rpc("Page.setLifecycleEventsEnabled", json!({"enabled":true}))?;
        browser.rpc(
            "Emulation.setDeviceMetricsOverride",
            json!({"width":1280,"height":800,"deviceScaleFactor":1,"mobile":false}),
        )?;
        browser.rpc("Browser.setDownloadBehavior", json!({"behavior":"deny"}))?;
        Ok(browser)
    }

    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        let mut request = json!({"id":id,"method":method,"params":params});
        if !self.session.is_empty()
            && !method.starts_with("Browser.")
            && !method.starts_with("Target.")
        {
            request["sessionId"] = json!(self.session);
        }
        self.socket
            .send(Message::Text(request.to_string().into()))
            .map_err(|e| e.to_string())?;
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if std::time::Instant::now() >= deadline {
                return Err(
                    "browser command deadline reached; it may already have taken effect".into(),
                );
            }
            let response = self.socket.read().map_err(|e| format!("{method}: {e}"))?;
            if let Message::Text(text) = response {
                let value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                if value["id"] == id {
                    if value.get("error").is_some() {
                        return Err(format!("browser command rejected: {}", value["error"]));
                    }
                    return Ok(value["result"].clone());
                }
                if self.events.len() == 256 {
                    self.events.pop_front();
                }
                self.events.push_back(value);
            }
        }
    }

    fn wait_loaded(&mut self, loader: &str) -> Result<(), String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let event = if let Some(event) = self.events.pop_front() {
                event
            } else {
                if std::time::Instant::now() >= deadline {
                    return Err(
                        "navigation load deadline reached; navigation may have taken effect".into(),
                    );
                }
                match self
                    .socket
                    .read()
                    .map_err(|e| format!("waiting for page load: {e}"))?
                {
                    Message::Text(text) => {
                        serde_json::from_str::<Value>(&text).map_err(|e| e.to_string())?
                    }
                    _ => continue,
                }
            };
            if event["sessionId"] == self.session
                && event["method"] == "Page.lifecycleEvent"
                && event["params"]["loaderId"] == loader
                && event["params"]["name"] == "load"
            {
                return Ok(());
            }
        }
    }

    pub fn set_cancellation(&mut self, cancel: &CancellationToken) -> Result<(), String> {
        self.watch =
            CancellationWatch::new(cancel, self.process.0.clone(), self.cancel_socket.clone())?;
        Ok(())
    }

    pub fn action(&mut self, action: &Value) -> Result<(), String> {
        self.action_cancellable(action, &tokio_util::sync::CancellationToken::new())
    }

    pub fn action_cancellable(
        &mut self,
        action: &Value,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(), String> {
        if cancel.is_cancelled() {
            return Err("browser action interrupted before execution".into());
        }
        self.set_cancellation(cancel)?;
        match action["type"].as_str().ok_or("missing action type")? {
            "navigate" => {
                let url = action["url"].as_str().ok_or("missing URL")?;
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return Err("only HTTP(S) navigation is permitted".into());
                }
                let result = self.rpc("Page.navigate", json!({"url":url}))?;
                if result["errorText"].as_str().is_some_and(|s| !s.is_empty()) {
                    return Err(format!("navigation failed: {}", result["errorText"]));
                }
                if let Some(loader) = result["loaderId"].as_str() {
                    self.wait_loaded(loader)?;
                }
            }
            "screenshot" => {}
            "click" | "double_click" => {
                let x = coordinate(action, "x", 1280)?;
                let y = coordinate(action, "y", 800)?;
                let button = action["button"].as_str().unwrap_or("left");
                if !["left", "right", "middle"].contains(&button) {
                    return Err("unsupported mouse button".into());
                }
                let count = if action["type"] == "double_click" {
                    2
                } else {
                    1
                };
                for kind in ["mousePressed", "mouseReleased"] {
                    if cancel.is_cancelled() {
                        return Err(
                            "interrupted during mouse action; it may have partially taken effect"
                                .into(),
                        );
                    }
                    self.rpc(
                        "Input.dispatchMouseEvent",
                        json!({"type":kind,"x":x,"y":y,"button":button,"clickCount":count}),
                    )?;
                }
            }
            "move" => {
                self.rpc("Input.dispatchMouseEvent", json!({"type":"mouseMoved","x":coordinate(action,"x",1280)?,"y":coordinate(action,"y",800)?}))?;
            }
            "scroll" => {
                self.rpc("Input.dispatchMouseEvent", json!({"type":"mouseWheel","x":coordinate(action,"x",1280)?,"y":coordinate(action,"y",800)?,"deltaX":action["scroll_x"].as_i64().unwrap_or(0).clamp(-4000,4000),"deltaY":action["scroll_y"].as_i64().unwrap_or(0).clamp(-4000,4000)}))?;
            }
            "type" => {
                let text = action["text"].as_str().ok_or("missing input text")?;
                if text.len() > 16384 {
                    return Err("input text exceeds 16 KiB".into());
                }
                self.rpc("Input.insertText", json!({"text":text}))?;
            }
            "keypress" => {
                let key = action["key"].as_str().ok_or("missing key")?;
                let code = match key {
                    "Enter" => 13,
                    "Tab" => 9,
                    "Escape" => 27,
                    "Backspace" => 8,
                    "ArrowDown" => 40,
                    "ArrowUp" => 38,
                    "ArrowLeft" => 37,
                    "ArrowRight" => 39,
                    "Delete" => 46,
                    _ => return Err("unsupported key".into()),
                };
                for kind in ["keyDown", "keyUp"] {
                    if cancel.is_cancelled() {
                        return Err(
                            "interrupted during key action; it may have partially taken effect"
                                .into(),
                        );
                    }
                    self.rpc(
                        "Input.dispatchKeyEvent",
                        json!({"type":kind,"key":key,"windowsVirtualKeyCode":code}),
                    )?;
                }
            }
            _ => return Err("unsupported browser action".into()),
        }
        Ok(())
    }

    pub fn screenshot(&mut self) -> Result<String, String> {
        let result = self.rpc(
            "Page.captureScreenshot",
            json!({"format":"png","captureBeyondViewport":false}),
        )?;
        result["data"]
            .as_str()
            .map(str::to_owned)
            .ok_or("screenshot data missing".into())
    }
}
fn coordinate(action: &Value, name: &str, limit: u64) -> Result<u64, String> {
    action[name]
        .as_u64()
        .filter(|n| *n < limit)
        .ok_or_else(|| format!("{name} must be within the 1280 by 800 viewport"))
}
fn stop(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}
// A Child is consumed exactly once, before reaping its PID. A stale watcher
// can therefore never send a signal to a later process that reused that PID.
struct BrowserProcess(Arc<Mutex<Option<Child>>>);
fn stop_process(child: &Arc<Mutex<Option<Child>>>) {
    let owned = child.lock().unwrap_or_else(|p| p.into_inner()).take();
    if let Some(mut child) = owned {
        stop(&mut child);
    }
}
impl Drop for BrowserProcess {
    fn drop(&mut self) {
        stop_process(&self.0);
    }
}

struct CancellationWatch {
    done: CancellationToken,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl CancellationWatch {
    fn new(
        cancel: &CancellationToken,
        child: Arc<Mutex<Option<Child>>>,
        socket: Arc<OnceLock<TcpStream>>,
    ) -> Result<Self, String> {
        let done = CancellationToken::new();
        let finished = done.clone();
        let cancel = cancel.clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|e| e.to_string())?;
        let worker = std::thread::Builder::new().name("browser-cancel".into()).spawn(move || {
            runtime.block_on(async {
                tokio::select! {
                    biased;
                    _ = finished.cancelled() => {},
                    _ = cancel.cancelled() => {
                        // Wake a blocking CDP read/handshake before waiting for
                        // the owned browser to die. This also covers startup.
                        if let Some(stream) = socket.get() { let _ = stream.shutdown(std::net::Shutdown::Both); }
                        stop_process(&child);
                    }
                }
            });
        }).map_err(|e|e.to_string())?;
        Ok(Self {
            done,
            worker: Some(worker),
        })
    }
}
impl Drop for CancellationWatch {
    fn drop(&mut self) {
        self.done.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
