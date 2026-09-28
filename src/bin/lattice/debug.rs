//! Headless drivers over the same terminal host: no duplicate input, fold or
//! rendering rules. Script spelling and process results are compatibility APIs.
use super::*;

pub(crate) fn run_debug_frame(args: Vec<String>) -> std::io::Result<()> {
    let mut path: Option<String> = None;
    let mut html: Option<String> = None;
    let (mut width, mut height, mut at) = (100u16, 40u16, usize::MAX);
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--width" => width = it.next().and_then(|v| v.parse().ok()).unwrap_or(width),
            "--height" => height = it.next().and_then(|v| v.parse().ok()).unwrap_or(height),
            "--at" => at = it.next().and_then(|v| v.parse().ok()).unwrap_or(at),
            "--html" => html = it.next(),
            other => path = Some(other.to_string()),
        }
    }
    let Some(path) = path else {
        eprintln!("usage: lattice debug-frame <ledger.jsonl> [--width W] [--height H] [--at N] [--html OUT]");
        std::process::exit(2);
    };
    let mut events = Vec::new();
    for line in lattice::ledgers::lines(std::path::Path::new(&path))?.take(at) {
        let line = line?;
        if !line.trim().is_empty() {
            events.push(serde_json::from_str::<EventEnvelope>(&line)?);
        }
    }
    let ui = Ui::replayed(&events);
    if let Some(out) = html {
        std::fs::write(&out, frame_html(&ui, width, height))?;
        eprintln!("wrote {out}");
        return Ok(());
    }
    let snap = snapshot(&ui, width, height);
    println!("{}", serde_json::to_string_pretty(&snap.to_json()).unwrap());
    Ok(())
}

/// Actions, one per line (`#` starts a comment): type, paste, key, wait, frame,
/// resize. The fixed brain and private workspace need no key or daemon.
pub(crate) fn run_debug_tui(args: Vec<String>) -> std::io::Result<()> {
    let (mut width, mut height) = (100u16, 30u16);
    let mut as_json = false;
    // A prefix, not a path: the script can draw many frames.
    let mut html: Option<String> = None;
    let mut shot = 0usize;
    let mut vision = false;
    let mut brain: Option<String> = None;
    let mut carry_on = false;
    let mut script: Option<String> = None;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--width" => width = it.next().and_then(|v| v.parse().ok()).unwrap_or(width),
            "--height" => height = it.next().and_then(|v| v.parse().ok()).unwrap_or(height),
            "--json" => as_json = true,
            "--html" => html = it.next(),
            "--continue" => carry_on = true,
            // A deliberate harness override, never a real model declaration.
            "--images" => vision = true,
            "--brain" => brain = it.next(),
            other => script = Some(other.to_string()),
        }
    }
    let actions = match script.as_deref() {
        None | Some("-") => {
            let mut buf = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
            buf
        }
        Some(path) => std::fs::read_to_string(path)?,
    };
    let replies: Value = match brain {
        Some(path) => {
            serde_json::from_str(&std::fs::read_to_string(path)?).map_err(std::io::Error::other)?
        }
        None => serde_json::json!({"script": []}),
    };
    let workspace = std::env::temp_dir().join("lattice-debug-tui");
    std::fs::create_dir_all(&workspace)?;
    let ledger = workspace.join("stream.jsonl");
    let documents = lattice::contracts::document::documents_dir(&ledger);
    // Fresh by default; continuation preserves the original permissive reader.
    let past: Vec<EventEnvelope> = if carry_on {
        std::fs::read_to_string(&ledger)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    } else {
        tabs::archive_index(&ledger)?;
        let _ = std::fs::remove_file(&ledger);
        Vec::new()
    };
    let cfg = PresetConfig {
        adapter: "scripted".to_string(),
        model: "scripted".to_string(),
        base_url: String::new(),
        key_env: String::new(),
        workspace: Some(workspace.display().to_string()),
        context_window: 64000,
        usage_input_field: "input_tokens".to_string(),
        profile: None,
        catalog_problems: Vec::new(),
        system: "debug".to_string(),
        scripted: Some(replies),
        thinking: None,
        // Never inherit or write the user's real overlay.
        overlay: None,
        assembly: None,
    };
    let models = model_catalog::from_config(&cfg);
    let debug_parts = session_build::preview(&cfg);
    let tab_config = cfg.clone();
    let main_ledger = ledger.clone();
    let session = Session::spawn("ui", move |tx| {
        session_build::build(tx, &cfg, ledger.clone())
    })
    .map_err(std::io::Error::other)?;
    let mut ui = initialization::Debug {
        models,
        config: tab_config.clone(),
        parts: debug_parts,
        workspace: workspace.clone(),
        documents,
        vision,
    }
    .install(&past);
    let mut tabs = tabs::Tabs::interactive(&session, tab_config, &main_ledger, &mut ui, false);
    let mut term = Terminal::new(ratatui::backend::TestBackend::new(width, height))?;
    let mut hit = draw_ui(&mut term, &mut ui)?;
    for (number, raw) in actions.lines().enumerate() {
        let session = tabs.session();
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
        match verb {
            // Typing must pass through real modal priority and receipt clearing.
            "type" => {
                for c in rest.chars() {
                    let key = ratatui::crossterm::event::KeyEvent::from(KeyCode::Char(c));
                    if on_key(&mut ui, Some(session), key, &hit) {
                        break;
                    }
                }
            }
            "paste" => {
                ui.flash = None;
                let text = rest.replace("\\n", "\n");
                absorb_paste(&mut ui, &text);
                ui.draft.reset_selection();
            }
            "key" => match parse_key(rest.trim()) {
                Some(key) => {
                    if on_key(&mut ui, Some(session), key, &hit) {
                        break;
                    }
                }
                None => {
                    eprintln!("line {}: unknown key '{rest}'", number + 1);
                    std::process::exit(2);
                }
            },
            "wait" => {
                tabs.wait_startups(&mut ui).map_err(std::io::Error::other)?;
                settle(&mut ui, tabs.session())?;
            }
            "resize" => {
                if let Some((w, h)) = rest.trim().split_once('x') {
                    width = w.trim().parse().unwrap_or(width);
                    height = h.trim().parse().unwrap_or(height);
                    term = Terminal::new(ratatui::backend::TestBackend::new(width, height))?;
                }
            }
            "frame" => {}
            other => {
                eprintln!("line {}: unknown action '{other}'", number + 1);
                std::process::exit(2);
            }
        }
        if let Some(navigation) = ui.navigation.take() {
            tabs.navigate(&mut ui, navigation);
        }
        tabs.drain(&mut ui)?;
        hit = draw_ui(&mut term, &mut ui)?;
        if verb == "frame" {
            match &html {
                Some(prefix) => {
                    shot += 1;
                    let at = format!("{prefix}-{shot}.html");
                    std::fs::write(&at, frame_html(&ui, width, height))?;
                    eprintln!("wrote {at}");
                }
                None => print_frame(&ui, width, height, as_json),
            }
        }
    }
    drop(tabs);
    session.shutdown();
    Ok(())
}

fn settle(ui: &mut Ui, session: &Session) -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        drain_render(ui, session)?;
        if !ui.domain.turns.busy() || std::time::Instant::now() > deadline {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn print_frame(ui: &Ui, width: u16, height: u16, as_json: bool) {
    let snap = snapshot(ui, width, height);
    if as_json {
        println!("{}", snap.to_json());
        return;
    }
    println!("┌{}┐", "─".repeat(width as usize));
    for row in &snap.rows {
        println!("│{row}│");
    }
    println!("└{}┘", "─".repeat(width as usize));
}

fn parse_key(name: &str) -> Option<ratatui::crossterm::event::KeyEvent> {
    use ratatui::crossterm::event::KeyEvent;
    let (mods, bare) = match name.strip_prefix("ctrl-") {
        Some(rest) => (KeyModifiers::CONTROL, rest),
        None => match name.strip_prefix("alt-") {
            Some(rest) => (KeyModifiers::ALT, rest),
            None => (KeyModifiers::NONE, name),
        },
    };
    let code = match bare {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pgup" => KeyCode::PageUp,
        "pgdn" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "space" => KeyCode::Char(' '),
        other if other.starts_with('f') && other[1..].parse::<u8>().is_ok() => {
            KeyCode::F(other[1..].parse().ok()?)
        }
        other => {
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => KeyCode::Char(c),
                _ => return None,
            }
        }
    };
    Some(KeyEvent::new(code, mods))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Pin the script vocabulary; renaming a key breaks saved reproductions.
    #[test]
    fn the_driver_understands_the_keys_a_script_can_name() {
        use ratatui::crossterm::event::KeyEvent;
        assert_eq!(
            parse_key("enter"),
            Some(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        );
        assert_eq!(
            parse_key("ctrl-o"),
            Some(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
        );
        assert_eq!(
            parse_key("f1"),
            Some(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))
        );
        assert_eq!(
            parse_key("?"),
            Some(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE))
        );
        assert_eq!(parse_key("wiggle"), None);
    }
}
