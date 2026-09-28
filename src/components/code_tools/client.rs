//! A single LSP peer, owned by the tool component. No shell expansion, server
//! installation, editor actions, or workspace edits are performed by this client.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

use super::source::Source;

const MAX_FRAME: usize = 16 * 1024 * 1024;
const MAX_HEADER: usize = 8192;

pub(super) struct Peer {
    child: Child,
    input: Arc<Mutex<ChildStdin>>,
    output: mpsc::Receiver<Result<Value, String>>,
    reader: JoinHandle<()>,
    next: u64,
    initialized: bool,
    pub capabilities: Value,
    pub documents: BTreeMap<String, (String, i32)>,
    pub log: PathBuf,
    folders: Value,
    options: Value,
}

impl Peer {
    /// Requires an entered Tokio runtime. Commands come from assembly config,
    /// never from source files or language-server messages.
    pub fn spawn(
        command: &[String],
        root: &Path,
        config: &Value,
        dir: &Path,
    ) -> Result<Self, String> {
        let command = prepare_command(command, root, config, dir)?;
        let program = command
            .first()
            .ok_or("no language server command configured")?;
        let staged = tempfile::Builder::new()
            .prefix("language-server-")
            .suffix(".log")
            .tempfile_in(dir)
            .map_err(|e| e.to_string())?;
        let (stderr, log) = staged.keep().map_err(|e| e.to_string())?;
        let mut cmd = Command::new(program);
        cmd.args(&command[1..])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true);
        if let Some(names) = config["removeEnv"].as_array() {
            for name in names {
                cmd.env_remove(
                    name.as_str()
                        .ok_or("removeEnv entries must be variable names")?,
                );
            }
        }
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn().map_err(|e| format!("cannot start language server {program}: {e}; install it explicitly or configure its command"))?;
        let uri = reqwest::Url::from_directory_path(root)
            .map_err(|_| "invalid workspace URI")?
            .to_string();
        let folders = json!([{"uri":uri,"name":root.file_name().and_then(|n|n.to_str()).unwrap_or("workspace")}]);
        let input = Arc::new(Mutex::new(
            child.stdin.take().ok_or("language server stdin missing")?,
        ));
        let stdout = child
            .stdout
            .take()
            .ok_or("language server stdout missing")?;
        let (sender, output) = mpsc::channel(1);
        let reader = tokio::spawn(read_messages(
            stdout,
            input.clone(),
            sender,
            folders.clone(),
            config["settings"].clone(),
        ));
        Ok(Self {
            input,
            output,
            reader,
            child,
            next: 1,
            initialized: false,
            capabilities: Value::Null,
            documents: BTreeMap::new(),
            log,
            folders,
            options: config["initializationOptions"].clone(),
        })
    }

    pub async fn initialize(&mut self) -> Result<(), String> {
        if self.initialized {
            return Ok(());
        }
        let response = self.request("initialize", json!({
            "processId":std::process::id(), "clientInfo":{"name":"Lattice"},
            "rootUri":self.folders[0]["uri"], "workspaceFolders":self.folders,
            "initializationOptions":self.options,
            "capabilities":{
                "general":{"positionEncodings":["utf-16"]},
                "workspace":{"configuration":true,"workspaceFolders":true,"applyEdit":false},
                "textDocument":{
                    "synchronization":{"dynamicRegistration":false},
                    "documentSymbol":{"hierarchicalDocumentSymbolSupport":true},
                    "definition":{"linkSupport":true}
                }
            }
        })).await?;
        self.capabilities = response["capabilities"].clone();
        if self.capabilities["positionEncoding"]
            .as_str()
            .is_some_and(|s| s != "utf-16")
        {
            return Err("server selected an unoffered position encoding".into());
        }
        self.notify("initialized", json!({})).await?;
        self.initialized = true;
        Ok(())
    }

    pub async fn close_other_documents(&mut self, current: &str) -> Result<(), String> {
        let previous: Vec<_> = self
            .documents
            .keys()
            .filter(|uri| uri.as_str() != current)
            .cloned()
            .collect();
        for uri in previous {
            self.notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}))
                .await?;
            self.documents.remove(&uri);
        }
        Ok(())
    }

    pub async fn sync(&mut self, source: &Source, language: &str) -> Result<(), String> {
        let sync = &self.capabilities["textDocumentSync"];
        let changes = sync
            .as_u64()
            .or_else(|| sync["change"].as_u64())
            .unwrap_or(0);
        if changes == 0 || (sync.is_object() && sync["openClose"] != true) {
            return Err("server does not accept versioned open-document synchronization".into());
        }
        let uri = source.uri()?;
        if let Some((hash, version)) = self.documents.get(&uri) {
            if hash == &source.version {
                return Ok(());
            }
            let version = version.checked_add(1).ok_or("document version exhausted")?;
            self.notify("textDocument/didChange", json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[{"text":source.text}]})).await?;
            self.documents
                .insert(uri, (source.version.clone(), version));
        } else {
            if self.documents.len() >= 64 {
                return Err(
                    "language server open-document limit reached; reset the workspace peer".into(),
                );
            }
            self.notify("textDocument/didOpen", json!({"textDocument":{"uri":uri,"languageId":language,"version":1,"text":source.text}})).await?;
            self.documents.insert(uri, (source.version.clone(), 1));
        }
        Ok(())
    }

    pub async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        write_frame(
            &mut *self.input.lock().await,
            &json!({"jsonrpc":"2.0","method":method,"params":params}),
        )
        .await
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next;
        self.next = self.next.checked_add(1).ok_or("request id exhausted")?;
        write_frame(
            &mut *self.input.lock().await,
            &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
        )
        .await?;
        let message = self
            .output
            .recv()
            .await
            .ok_or("language server reader stopped")??;
        if message["id"] != id {
            return Err("language server returned an unexpected response id".into());
        }
        if !message["error"].is_null() {
            return Err(format!("language server error: {}", message["error"]));
        }
        message
            .get("result")
            .cloned()
            .ok_or_else(|| "language server response has no result".into())
    }

    fn server_request(message: &Value, folders: &Value, settings: &Value) -> Value {
        let id = &message["id"];
        let result = match message["method"].as_str().unwrap_or("") {
            "workspace/configuration" => Value::Array(
                message["params"]["items"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .map(|item| {
                                let section = item["section"].as_str().unwrap_or("");
                                if section.is_empty() {
                                    settings.clone()
                                } else {
                                    settings
                                        .get(section)
                                        .or_else(|| {
                                            section
                                                .split('.')
                                                .try_fold(settings, |value, key| value.get(key))
                                        })
                                        .cloned()
                                        .unwrap_or(Value::Null)
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            "workspace/workspaceFolders" => folders.clone(),
            "workspace/applyEdit" => {
                json!({"applied":false,"failureReason":"navigation never applies workspace edits"})
            }
            "window/showDocument" => json!({"success":false}),
            "window/workDoneProgress/create" | "window/showMessageRequest" => Value::Null,
            _ => {
                return json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"client capability not supported"}})
            }
        };
        json!({"jsonrpc":"2.0","id":id,"result":result})
    }

    pub async fn stop(&mut self) {
        self.reader.abort();
        self.kill_group();
        let _ = self.child.kill().await;
        let _ = (&mut self.reader).await;
    }

    fn kill_group(&mut self) {
        // The child has not been reaped while id() is Some. Kill the group
        // before waiting, including descendants of an already-exited leader.
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.reader.abort();
        self.kill_group();
    }
}

fn prepare_command(
    command: &[String],
    root: &Path,
    config: &Value,
    dir: &Path,
) -> Result<Vec<String>, String> {
    use sha2::{Digest, Sha256};
    if !command
        .iter()
        .any(|arg| arg == "{state}" || arg.starts_with("{state}/"))
    {
        return Ok(command.to_vec());
    }
    let identity = serde_json::to_vec(&json!({"workspace":root,"server":config}))
        .map_err(|e| e.to_string())?;
    let state = dir.join(format!("code-state-{:x}", Sha256::digest(identity)));
    std::fs::create_dir_all(&state).map_err(|e| e.to_string())?;
    command
        .iter()
        .map(|arg| {
            let path = if arg == "{state}" {
                Some(state.clone())
            } else {
                arg.strip_prefix("{state}/")
                    .map(|suffix| state.join(suffix))
            };
            match path {
                Some(path) => path
                    .to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "language-server state path is not UTF-8".into()),
                None => Ok(arg.clone()),
            }
        })
        .collect()
}

async fn read_messages(
    stdout: ChildStdout,
    input: Arc<Mutex<ChildStdin>>,
    sender: mpsc::Sender<Result<Value, String>>,
    folders: Value,
    settings: Value,
) {
    let mut output = BufReader::new(stdout);
    loop {
        let message = match read_frame(&mut output).await {
            Ok(message) => message,
            Err(error) => {
                let _ = sender.send(Err(error)).await;
                break;
            }
        };
        if message["jsonrpc"] != "2.0" {
            let _ = sender
                .send(Err("language server message is not JSON-RPC 2.0".into()))
                .await;
            break;
        }
        if message["method"].is_string() {
            if !message["id"].is_null() {
                let reply = Peer::server_request(&message, &folders, &settings);
                if let Err(error) = write_frame(&mut *input.lock().await, &reply).await {
                    let _ = sender.send(Err(error)).await;
                    break;
                }
            }
            // Drain notifications even while no tool request is active. A
            // quiet client must not leave the server blocked on a full pipe.
        } else if sender.send(Ok(message)).await.is_err() {
            break;
        }
    }
}

async fn write_frame(output: &mut (impl AsyncWrite + Unpin), value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FRAME {
        return Err("outgoing language-server frame exceeds 16 MiB".into());
    }
    output
        .write_all(format!("Content-Length: {}\r\n\r\n", bytes.len()).as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    output.write_all(&bytes).await.map_err(|e| e.to_string())?;
    output.flush().await.map_err(|e| e.to_string())
}

async fn read_frame(input: &mut (impl AsyncRead + Unpin)) -> Result<Value, String> {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= MAX_HEADER {
            return Err("language-server header exceeds 8 KiB".into());
        }
        header.push(
            input
                .read_u8()
                .await
                .map_err(|e| format!("language server pipe: {e}"))?,
        );
    }
    let header = std::str::from_utf8(&header).map_err(|e| e.to_string())?;
    let mut length = None;
    for line in header.split("\r\n").filter(|line| !line.is_empty()) {
        let (key, value) = line
            .split_once(':')
            .ok_or("malformed language-server header")?;
        if key.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() {
                return Err("duplicate Content-Length".into());
            }
            length = Some(value.trim().parse::<usize>().map_err(|e| e.to_string())?);
        }
    }
    let length = length.ok_or("language-server frame has no Content-Length")?;
    if length > MAX_FRAME {
        return Err("incoming language-server frame exceeds 16 MiB".into());
    }
    let mut body = vec![0; length];
    input
        .read_exact(&mut body)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_sections_accept_nested_and_literal_keys() {
        let settings =
            json!({"language":{"check":{"enabled":true}},"language.check.enabled":false});
        let reply = Peer::server_request(
            &json!({"id":1,"method":"workspace/configuration","params":{"items":[{}, {"section":"language.check"}, {"section":"language.check.enabled"}, {"section":"absent"}]}}),
            &json!([]),
            &settings,
        );
        assert_eq!(
            reply["result"],
            json!([settings, {"enabled":true}, false, null])
        );
    }

    #[test]
    fn state_directories_are_bound_to_full_workspace_and_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let command = vec![
            "server".into(),
            "{state}/data".into(),
            "literal-{state}".into(),
        ];
        let config = json!({"command":command});
        let first =
            prepare_command(&command, Path::new("/first/project"), &config, dir.path()).unwrap();
        let second =
            prepare_command(&command, Path::new("/second/project"), &config, dir.path()).unwrap();
        assert_ne!(first[1], second[1]);
        assert_eq!(
            first,
            prepare_command(&command, Path::new("/first/project"), &config, dir.path()).unwrap()
        );
        assert_ne!(
            first,
            prepare_command(
                &command,
                Path::new("/first/project"),
                &json!({"different":true}),
                dir.path()
            )
            .unwrap()
        );
        assert_eq!(first[2], "literal-{state}");
        assert!(Path::new(&first[1]).parent().unwrap().is_dir());
    }

    #[test]
    fn frames_count_utf8_bytes_and_reject_ambiguous_or_unbounded_headers() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let value = json!({"text":"中文😀"});
            let mut bytes = Vec::new();
            write_frame(&mut bytes, &value).await.unwrap();
            assert_eq!(read_frame(&mut bytes.as_slice()).await.unwrap(), value);
            for bytes in [
                b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}".as_slice(),
                b"Content-Length: 9\r\n\r\n{}",
            ] {
                assert!(read_frame(&mut &bytes[..]).await.is_err());
            }
            let oversized = format!("Content-Length: {}\r\n\r\n", MAX_FRAME + 1);
            let error = read_frame(&mut oversized.as_bytes()).await.unwrap_err();
            assert!(error.contains("exceeds 16 MiB"), "{error}");
            let oversized_header = vec![b'x'; MAX_HEADER + 1];
            let error = read_frame(&mut oversized_header.as_slice())
                .await
                .unwrap_err();
            assert!(error.contains("header exceeds 8 KiB"), "{error}");
        });
    }
}
