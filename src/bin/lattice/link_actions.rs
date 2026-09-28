//! Frontend-local link launchers and their pending results. Each frontend owns
//! its queue; polling order and presentation remain with the coordinator.
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};

#[cfg(test)]
#[path = "link_actions/tests.rs"]
mod tests;

/// Preparing is separate from launching so the caller displays the notice
/// before starting the process, as it did before this responsibility moved.
pub(super) fn prepare(url: &str) -> Result<(String, Command), String> {
    let command = url_open_command(url)
        .ok_or_else(|| "Only HTTP(S) links without credentials can be opened".to_string())?;
    Ok((format!("Opening {url}"), command))
}

fn url_open_command(url: &str) -> Option<Command> {
    let url = lattice::richtext::http_url(url)?;
    // No shell: punctuation in a URL is always part of ONE argument.
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut command = Command::new(program);
    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Some(command)
}

#[derive(Default)]
pub(super) struct LinkOpener {
    results: Vec<Receiver<Result<(), String>>>,
}

impl LinkOpener {
    pub fn launch(&mut self, mut command: Command) {
        let (tx, rx) = mpsc::channel();
        self.results.push(rx);
        // Some desktop launchers wait for the browser. Wait off the rendering
        // thread, reap the child, then send its result to this frontend only.
        std::thread::spawn(move || {
            let result = command
                .status()
                .map_err(|e| format!("Could not open link: {e}"))
                .and_then(|status| {
                    if status.success() {
                        Ok(())
                    } else {
                        Err(format!(
                            "Could not open link: launcher exited with {status}"
                        ))
                    }
                });
            let _ = tx.send(result);
        });
    }

    /// Only failures require new notices. Preserve launch order, retain pending
    /// work, and discard success/disconnected results without clearing notices.
    pub fn drain(&mut self) -> Vec<String> {
        let mut failures = Vec::new();
        self.results.retain(|rx| match rx.try_recv() {
            Ok(Err(error)) => {
                failures.push(error);
                false
            }
            Ok(Ok(())) | Err(TryRecvError::Disconnected) => false,
            Err(TryRecvError::Empty) => true,
        });
        failures
    }

    #[cfg(test)]
    pub fn fixture_results(&mut self) -> &mut Vec<Receiver<Result<(), String>>> {
        &mut self.results
    }
}
