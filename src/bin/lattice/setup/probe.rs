//! Explicit setup probe, not a chat turn. Never retries or follows redirects.
//! A separate private journal records intent/outcome without credentials or bodies.
use super::{validate_target, SetupNetwork};
use lattice::models::Entry;
use serde_json::{json, Value};
use std::{io::Write, path::PathBuf, time::Duration};

pub(super) struct HttpTest {
    journal: PathBuf,
}
impl HttpTest {
    pub fn new(journal: PathBuf) -> Self {
        Self { journal }
    }

    fn new_record(&self) -> Result<std::fs::File, String> {
        std::fs::create_dir_all(&self.journal)
            .map_err(|_| "cannot create setup-test journal directory")?;
        // Exclusive creation: never append to a catalog, preference, symlink,
        // or pre-existing journal, regardless of configured file names.
        let staged = tempfile::Builder::new()
            .prefix("connection-")
            .suffix(".jsonl")
            .tempfile_in(&self.journal)
            .map_err(|_| "cannot create setup-test record")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if staged
                .as_file()
                .metadata()
                .map_err(|_| "cannot inspect setup-test record")?
                .permissions()
                .mode()
                & 0o077
                != 0
            {
                return Err("setup-test record is not private".into());
            }
        }
        let (file, _) = staged
            .keep()
            .map_err(|_| "cannot preserve setup-test record")?;
        Ok(file)
    }

    fn record(file: &mut std::fs::File, value: &Value) -> Result<(), String> {
        let mut line = serde_json::to_vec(value).map_err(|_| "cannot encode setup-test record")?;
        line.push(b'\n');
        file.write_all(&line)
            .and_then(|_| file.sync_all())
            .map_err(|_| "cannot persist setup-test record".into())
    }
}

impl SetupNetwork for HttpTest {
    fn models(&mut self, spec: &Value) -> Result<Vec<super::discovery::Model>, String> {
        let (url, _) = super::discovery::endpoint(spec)?;
        let key = super::discovery::key(spec)?;
        let mut journal = self.new_record()?;
        Self::record(
            &mut journal,
            &json!({"v":1,"phase":"requested","operation":"models","reason":"user explicitly selected model discovery","url":url.as_str()}),
        )?;
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "cannot start model-list executor".to_owned())
            .and_then(|runtime| {
                runtime.block_on(async {
                    tokio::time::timeout(
                        Duration::from_secs(30),
                        super::discovery::fetch(spec, &key),
                    )
                    .await
                    .map_err(|_| "model discovery timed out; it was not repeated".to_owned())?
                })
            });
        Self::record(&mut journal, &json!({"v":1,"phase":"completed","operation":"models","ok":result.is_ok(),"count":result.as_ref().ok().map(Vec::len),"detail":result.as_ref().err()}))
            .map_err(|e| format!("{e}; discovery may already have completed; it was not repeated"))?;
        result
    }

    fn test(&mut self, entry: &Entry) -> Result<(), String> {
        validate_target(
            &json!({"adapter":entry.adapter,"model":entry.model,"baseUrl":entry.base_url}),
        )?;
        let key = std::env::var(&entry.key_env)
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or("the configured credential is missing or empty")?;
        let attempt = format!(
            "{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let mut journal = self.new_record()?;
        Self::record(
            &mut journal,
            &json!({"v":1,"attempt":attempt,"phase":"requested","reason":"user explicitly selected connection test","adapter":entry.adapter,"model":entry.model,"baseUrl":entry.base_url}),
        )?;
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "cannot start connection-test executor".to_owned())
            .and_then(|runtime| runtime.block_on(send(entry, &key)));
        Self::record(&mut journal, &json!({"v":1,"attempt":attempt,"phase":"completed","ok":result.is_ok(),"detail":result.as_ref().err()}))
            .map_err(|e| format!("{e}; the request may already have completed; it was not repeated"))?;
        result
    }
}

async fn send(entry: &Entry, key: &str) -> Result<(), String> {
    let (url, body) = request(entry)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "cannot initialize the HTTP client")?;
    let request = if entry.adapter == "anthropic" {
        client
            .post(url)
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
    } else {
        client.post(url).bearer_auth(key)
    };
    let mut response = request.json(&body).send().await.map_err(|e| {
        if e.is_timeout() {
            "request timed out; outcome unknown"
        } else if e.is_connect() {
            "connection could not be established"
        } else {
            "transport failed; outcome unknown"
        }
        .to_owned()
    })?;
    if !response.status().is_success() {
        return Err(format!(
            "provider returned HTTP {}; no automatic retry was made",
            response.status().as_u16()
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "response interrupted; request was not repeated")?
    {
        if bytes.len() + chunk.len() > 65_536 {
            return Err("test response exceeded the size limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "provider returned invalid JSON")?;
    if valid_response(&entry.adapter, &value) {
        Ok(())
    } else {
        Err("provider response did not contain a completed text answer".into())
    }
}

fn request(entry: &Entry) -> Result<(String, Value), String> {
    let base = entry.base_url.trim_end_matches('/');
    let message = json!([{"role":"user","content":"Reply with OK."}]);
    Ok(match entry.adapter.as_str() {
        "openai" => {
            let mut body =
                json!({"model":entry.model,"messages":message,"max_tokens":32,"stream":false});
            if reqwest::Url::parse(base)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .as_deref()
                == Some("api.deepseek.com")
            {
                body["thinking"] = json!({"type":"disabled"});
            }
            (format!("{base}/chat/completions"), body)
        }
        "responses" => (
            format!("{base}/responses"),
            json!({"model":entry.model,"input":"Reply with OK.","max_output_tokens":32,"stream":false,"store":false}),
        ),
        "anthropic" => (
            format!("{base}/v1/messages"),
            json!({"model":entry.model,"messages":message,"max_tokens":32,"stream":false}),
        ),
        _ => return Err("unsupported test protocol".into()),
    })
}

fn valid_response(adapter: &str, value: &Value) -> bool {
    fn text(value: &Value) -> bool {
        value.as_str().is_some_and(|s| !s.trim().is_empty())
    }
    if value.get("error").is_some_and(|e| !e.is_null()) {
        return false;
    }
    match adapter {
        "openai" => value["choices"].as_array().is_some_and(|choices| {
            choices
                .iter()
                .any(|c| text(&c["message"]["content"]) && c["finish_reason"] == "stop")
        }),
        "anthropic" => {
            value["stop_reason"] == "end_turn"
                && value["content"].as_array().is_some_and(|parts| {
                    parts
                        .iter()
                        .any(|p| p["type"] == "text" && text(&p["text"]))
                })
        }
        "responses" => {
            value["status"] == "completed"
                && value["output"].as_array().is_some_and(|output| {
                    output.iter().any(|item| {
                        item["content"].as_array().is_some_and(|parts| {
                            parts
                                .iter()
                                .any(|p| p["type"] == "output_text" && text(&p["text"]))
                        })
                    })
                })
        }
        _ => false,
    }
}

#[cfg(test)]
#[path = "probe_tests.rs"]
mod tests;
