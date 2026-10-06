//! Optimistic, non-destructive catalog editing shared by frontend writers.
//! The lock coordinates participating writers, not arbitrary external editors.
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Snapshot {
    path: PathBuf,
    original: Option<Vec<u8>>,
    document: Value,
}

fn bytes(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

impl Snapshot {
    pub fn read(path: &Path) -> Result<Self, String> {
        let original = bytes(path)?;
        let document = match &original {
            Some(bytes) => serde_json::from_slice::<Value>(bytes).map_err(|e| {
                // Do not echo parser text: unexpected tokens can contain credentials.
                format!(
                    "{} is not valid JSON at line {}, column {}; refusing to rewrite it",
                    path.display(),
                    e.line(),
                    e.column()
                )
            })?,
            None => json!({"models": {}}),
        };
        if !document.is_object() || document.get("models").is_some_and(|v| !v.is_object()) {
            return Err(format!(
                "{} must contain a models object; refusing to rewrite it",
                path.display()
            ));
        }
        Ok(Self {
            path: path.into(),
            original,
            document,
        })
    }

    pub fn entry(&self, id: &str) -> Option<&Value> {
        self.document.get("models")?.get(id)
    }

    pub fn entries(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.document
            .get("models")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|models| models.iter())
            .map(|(id, spec)| (id.as_str(), spec))
    }

    pub fn insert(&mut self, id: &str, spec: Value) -> Result<(), String> {
        validate_name(id)?;
        if self.document.get("models").is_none() {
            self.document["models"] = json!({});
        }
        let models = self.document["models"]
            .as_object_mut()
            .expect("validated object");
        if models.contains_key(id) {
            return Err(format!("{id:?} is already in the catalog"));
        }
        models.insert(id.to_owned(), spec);
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        let removed = self
            .document
            .get_mut("models")
            .and_then(Value::as_object_mut)
            .and_then(|models| models.remove(id));
        removed.map(|_| ()).ok_or_else(|| {
            format!(
                "{} does not have a model called {id:?}",
                self.path.display()
            )
        })
    }

    /// Repair only the credential fields; preserve the target and unknown data.
    pub fn credential(&mut self, id: &str, field: &str, value: &str) -> Result<(), String> {
        if !matches!(field, "apiKey" | "apiKeyEnv") || value.is_empty() {
            return Err("a nonempty credential or environment reference is required".into());
        }
        let spec = self
            .document
            .get_mut("models")
            .and_then(|m| m.get_mut(id))
            .and_then(Value::as_object_mut)
            .ok_or("the selected model is not an object")?;
        spec.remove("apiKey");
        spec.remove("apiKeyEnv");
        spec.insert(field.into(), json!(value));
        Ok(())
    }

    pub fn save(self) -> Result<(), String> {
        if !cfg!(unix) && self.entries().any(|(_, spec)| spec.get("apiKey").is_some()) {
            return Err("cannot safely rewrite a catalog containing local keys on this platform; use environment references or a supported protected writer".into());
        }
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(self.path.with_extension("json.lock"))
            .map_err(|e| format!("cannot open catalog lock: {e}"))?;
        lock.try_lock()
            .map_err(|_| "catalog is being edited; reload and try again".to_string())?;
        if bytes(&self.path)? != self.original {
            return Err("catalog changed during setup; reload before saving".into());
        }
        let mut staged = tempfile::NamedTempFile::new_in(parent)
            .map_err(|e| format!("cannot create private catalog staging file: {e}"))?;
        let result = (|| {
            verify_private(staged.as_file())?;
            serde_json::to_writer_pretty(staged.as_file_mut(), &self.document)
                .map_err(|_| "cannot serialize catalog".to_string())?;
            staged
                .write_all(b"\n")
                .map_err(|e| format!("cannot write catalog: {e}"))?;
            staged
                .as_file()
                .sync_all()
                .map_err(|e| format!("cannot flush catalog: {e}"))?;
            // Detect ordinary external edits during staging as well.
            if bytes(&self.path)? != self.original {
                return Err("catalog changed during save; reload before saving".into());
            }
            Ok(())
        })();
        if let Err(error) = result {
            return Err(cleanup(staged, error));
        }
        match staged.persist(&self.path) {
            Ok(_) => Ok(()),
            Err(error) => Err(cleanup(
                error.file,
                format!("cannot replace catalog: {}", error.error),
            )),
        }
    }
}

fn verify_private(file: &std::fs::File) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = file
            .metadata()
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o777;
        if mode != 0o600 {
            return Err(
                "catalog staging file is not owner-only; refusing to write credentials".into(),
            );
        }
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

fn cleanup(file: tempfile::NamedTempFile, error: String) -> String {
    let path = file.path().to_owned();
    match file.close() {
        Ok(()) => error,
        Err(e) => format!(
            "{error}; sensitive staging data may remain at {}: {e}",
            path.display()
        ),
    }
}

pub fn validate_name(id: &str) -> Result<(), String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
        || !id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return Err("use lower-case letters, digits, dot, dash or underscore, starting with a letter or digit".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
