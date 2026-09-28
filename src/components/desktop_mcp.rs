//! Private JSON-lines MCP connection to an already selected desktop daemon.
//! Killing this bridge is NOT proof that a desktop action was rolled back.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(test)]
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const MAX_REPLY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopTransportError {
    pub message: String,
    pub interrupted: bool,
    pub may_have_run: bool,
}
impl DesktopTransportError {
    fn new(message: impl Into<String>, may_have_run: bool) -> Self {
        Self {
            message: message.into(),
            interrupted: false,
            may_have_run,
        }
    }
}

pub struct DesktopMcp {
    child: Option<Child>,
    input: Option<mpsc::Sender<Vec<u8>>>,
    replies: Receiver<Result<Value, String>>,
    next_id: u64,
}
impl DesktopMcp {
    /// Host-owned runtime: no shared daemon, socket discovery or auto-launch.
    /// Backend-specific argv and environment are supplied by the adapter.
    pub fn spawn(
        mut command: Command,
        cancel: &CancellationToken,
    ) -> Result<Self, DesktopTransportError> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
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
        if cancel.is_cancelled() {
            return Err(DesktopTransportError {
                message: "desktop connection cancelled before startup".into(),
                interrupted: true,
                may_have_run: false,
            });
        }
        let mut child = command.spawn().map_err(|e| {
            DesktopTransportError::new(format!("cannot start desktop MCP bridge: {e}"), false)
        })?;
        let mut input = child.stdin.take().expect("piped stdin");
        let output = child.stdout.take().expect("piped stdout");
        let (sender, replies) = mpsc::sync_channel(2);
        let (requests, writer) = mpsc::channel::<Vec<u8>>();
        let mut connection = Self {
            child: Some(child),
            input: Some(requests),
            replies,
            next_id: 0,
        };
        let failures = sender.clone();
        std::thread::Builder::new()
            .name("desktop-mcp-writer".into())
            .spawn(move || {
                for bytes in writer {
                    if let Err(error) = input.write_all(&bytes) {
                        let _ = failures.send(Err(format!("desktop MCP write failed: {error}")));
                        break;
                    }
                }
            })
            .map_err(|e| {
                DesktopTransportError::new(
                    format!("cannot start desktop request writer: {e}"),
                    false,
                )
            })?;
        std::thread::Builder::new()
            .name("desktop-mcp-reader".into())
            .spawn(move || {
                let mut reader = BufReader::new(output);
                loop {
                    let mut line = Vec::new();
                    let result = std::io::Read::by_ref(&mut reader)
                        .take(MAX_REPLY_BYTES as u64 + 1)
                        .read_until(b'\n', &mut line);
                    match result {
                        Ok(0) => {
                            let _ = sender.send(Err("desktop MCP bridge closed its output".into()));
                            break;
                        }
                        Ok(_) if line.len() > MAX_REPLY_BYTES => {
                            let _ = sender.send(Err("desktop MCP reply exceeds 32 MiB".into()));
                            break;
                        }
                        Ok(_) => {
                            let decoded = serde_json::from_slice(&line)
                                .map_err(|e| format!("invalid desktop MCP JSON: {e}"));
                            let failed = decoded.is_err();
                            if sender.send(decoded).is_err() || failed {
                                break;
                            }
                        }
                        Err(error) => {
                            let _ = sender.send(Err(format!("desktop MCP read failed: {error}")));
                            break;
                        }
                    }
                }
            })
            .map_err(|e| {
                DesktopTransportError::new(
                    format!("cannot start desktop response reader: {e}"),
                    false,
                )
            })?;
        connection.request("initialize",json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"Lattice","version":"1"}}),cancel,false)?;
        connection.write(
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            false,
        )?;
        Ok(connection)
    }

    pub fn tools(&mut self, cancel: &CancellationToken) -> Result<Value, DesktopTransportError> {
        self.request("tools/list", json!({}), cancel, false)
    }

    /// Not exposed to the model. The desktop component must map a fixed,
    /// validated action subset, not forward arbitrary tool names or arguments.
    pub fn call(
        &mut self,
        name: &str,
        arguments: Value,
        cancel: &CancellationToken,
    ) -> Result<Value, DesktopTransportError> {
        self.request(
            "tools/call",
            json!({"name":name,"arguments":arguments}),
            cancel,
            true,
        )
    }

    fn write(&mut self, request: &Value, may_have_run: bool) -> Result<(), DesktopTransportError> {
        let mut bytes = serde_json::to_vec(request)
            .map_err(|e| DesktopTransportError::new(e.to_string(), false))?;
        bytes.push(b'\n');
        if bytes.len() > 64 * 1024 {
            return Err(DesktopTransportError::new(
                "desktop request exceeds 64 KiB",
                false,
            ));
        }
        self.input
            .as_ref()
            .ok_or_else(|| DesktopTransportError::new("desktop bridge is closed", false))?
            .send(bytes)
            .map_err(|e| {
                DesktopTransportError::new(
                    format!("desktop MCP writer is closed: {e}"),
                    may_have_run,
                )
            })
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        cancel: &CancellationToken,
        action: bool,
    ) -> Result<Value, DesktopTransportError> {
        if cancel.is_cancelled() {
            return Err(DesktopTransportError {
                message: "desktop request cancelled before dispatch".into(),
                interrupted: true,
                may_have_run: false,
            });
        }
        self.next_id += 1;
        let id = self.next_id;
        self.write(
            &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            action,
        )?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if cancel.is_cancelled() {
                self.stop();
                return Err(DesktopTransportError {message:"desktop request interrupted; daemon-side action state is unknown and must be recovered before further input".into(),interrupted:true,may_have_run:action});
            }
            if Instant::now() >= deadline {
                self.stop();
                return Err(DesktopTransportError::new(
                    "desktop reply timed out; do not repeat the action",
                    action,
                ));
            }
            match self.replies.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(reply)) => {
                    if reply.get("id").is_none() {
                        continue;
                    }
                    if reply["id"] != id {
                        self.stop();
                        return Err(DesktopTransportError::new(
                            "desktop MCP response ID does not match the outstanding call",
                            action,
                        ));
                    }
                    if let Some(error) = reply.get("error") {
                        return Err(DesktopTransportError::new(
                            format!("desktop MCP rejected the request: {error}"),
                            action,
                        ));
                    }
                    return reply.get("result").cloned().ok_or_else(|| {
                        DesktopTransportError::new("desktop MCP response has no result", action)
                    });
                }
                Ok(Err(message)) => {
                    self.stop();
                    return Err(DesktopTransportError::new(message, action));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => {
                    self.stop();
                    return Err(DesktopTransportError::new(
                        "desktop MCP reader stopped",
                        action,
                    ));
                }
            }
        }
    }
    fn stop(&mut self) {
        self.input.take();
        if let Some(mut child) = self.child.take() {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl Drop for DesktopMcp {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn fixture(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("fake-desktop");
        std::fs::write(
            &path,
            r#"#!/usr/bin/python3
import json,sys
for line in sys.stdin:
    request=json.loads(line)
    if 'id' not in request: continue
    result={}
    if request['method']=='initialize': result={'protocolVersion':'2025-06-18'}
    elif request['method']=='tools/list': result={'tools':[]}
    else:
        name=request['params']['name']
        args=request['params']['arguments']
        if name=='cancel':
            with open(args['signal'],'w') as signal: signal.write('ready')
            sys.stdin.read()
            continue
        if name=='wrong_id': request['id']+=1
        result={'content':[{'type':'text','text':'ok'}],'structuredContent':args}
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
"#,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    #[test]
    fn mcp_is_json_lines_and_preserves_strings_without_running_them() {
        let dir = tempfile::tempdir().unwrap();
        let executable = fixture(dir.path());
        let cancel = CancellationToken::new();
        let mut command = Command::new(&executable);
        command.env_clear().env("PATH", "/usr/bin:/bin");
        let mut client = DesktopMcp::spawn(command, &cancel).unwrap();
        assert_eq!(client.tools(&cancel).unwrap()["tools"], json!([]));
        let result = client
            .call("echo", json!({"text":"first\nsecond"}), &cancel)
            .unwrap();
        assert_eq!(result["structuredContent"]["text"], "first\nsecond");
        let error = client.call("wrong_id", json!({}), &cancel).unwrap_err();
        assert!(error.may_have_run);
        assert!(
            client.child.is_none(),
            "a broken stream is not silently reused"
        );
    }
    #[test]
    fn cancellation_reports_unknown_daemon_state_not_a_safe_failure() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let executable = fixture(dir.path());
        let signal = dir.path().join("ready");
        let name = std::ffi::CString::new(signal.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let cancel = CancellationToken::new();
        let mut command = Command::new(&executable);
        command.env_clear().env("PATH", "/usr/bin:/bin");
        let mut client = DesktopMcp::spawn(command, &cancel).unwrap();
        let stop = cancel.clone();
        let reader = signal.clone();
        let controller = std::thread::spawn(move || {
            let mut text = String::new();
            std::fs::File::open(reader)
                .unwrap()
                .read_to_string(&mut text)
                .unwrap();
            assert_eq!(text, "ready");
            stop.cancel();
        });
        let error = client
            .call("cancel", json!({"signal":signal}), &cancel)
            .unwrap_err();
        controller.join().unwrap();
        assert!(error.interrupted && error.may_have_run);
        assert!(client.child.is_none());
        let error = client.call("echo", json!({}), &cancel).unwrap_err();
        assert!(error.interrupted && !error.may_have_run);
    }
}
