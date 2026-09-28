//! Deterministic HTML reading, not an LLM summary. Raw bytes remain available
//! separately. Links are resolved but never followed.
use html2text::{Element, Handle};

pub(super) fn extract(html: &[u8], source: &str) -> Result<(String, &'static str), String> {
    let config = html2text::config::plain()
        .no_link_wrapping()
        .allow_width_overflow()
        .no_table_borders();
    let dom = config.parse_html(html).map_err(|e| e.to_string())?;
    let base = reqwest::Url::parse(source).map_err(|e| e.to_string())?;
    let mut stack = vec![(dom.document.clone(), 0usize, false, false)];
    let mut articles = Vec::new();
    let mut mains = Vec::new();
    let mut visited = 0;
    while let Some((node, depth, in_article, in_main)) = stack.pop() {
        visited += 1;
        if visited > 100_000 || depth > 256 {
            return Err(
                "HTML exceeds the reading complexity limit; read the raw artifact instead"
                    .to_string(),
            );
        }
        let tag = tag(&node);
        if tag == "article" && !in_article {
            articles.push(node.clone());
        }
        if tag == "main" && !in_main {
            mains.push(node.clone());
        }
        if let Element { attrs, .. } = &node.data {
            for attr in attrs.borrow_mut().iter_mut() {
                if attr.name.local.as_ref() == "href" {
                    if let Ok(url) = base.join(attr.value.as_ref()) {
                        attr.value = url.to_string().into();
                    }
                }
            }
        }
        node.children.borrow_mut().retain(|child| !noise(child));
        for child in node.children.borrow().iter().rev() {
            stack.push((
                child.clone(),
                depth + 1,
                in_article || tag == "article",
                in_main || tag == "main",
            ));
        }
    }
    // RcDom's destructor drains descendants even when other handles still
    // reference them. Keep the old roots alive until rendering has finished.
    let _original_roots = dom.document.children.borrow().clone();
    let scope = if !articles.is_empty() {
        *dom.document.children.borrow_mut() = articles;
        "article"
    } else if !mains.is_empty() {
        *dom.document.children.borrow_mut() = mains;
        "main"
    } else {
        "document"
    };
    // The prose renderer hard-wraps even <pre>. Temporarily replace code
    // blocks, then restore decoded text without reflow or tab expansion.
    let mut blocks = Vec::new();
    let mut markers = Vec::new();
    let mut stack = vec![dom.document.clone()];
    while let Some(node) = stack.pop() {
        if tag(&node) != "pre" {
            stack.extend(node.children.borrow().iter().rev().cloned());
            continue;
        }
        if blocks.len() >= 4096 {
            return Err("HTML exceeds the code block limit; read the raw artifact instead".into());
        }
        let mut serialized = Vec::new();
        node.serialize(&mut serialized).map_err(|e| e.to_string())?;
        let code = pre_text(&serialized)?;
        let marker = format!("LATTICE_CODE_{}_END", blocks.len());
        let marker_html = format!("<pre>{marker}</pre>");
        let marker_dom = config
            .parse_html(marker_html.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut search = vec![marker_dom.document.clone()];
        let mut found = None;
        while let Some(candidate) = search.pop() {
            if tag(&candidate) == "pre" {
                found = Some(candidate);
                break;
            }
            search.extend(candidate.children.borrow().iter().cloned());
        }
        *node.children.borrow_mut() = found
            .ok_or("code marker was not parsed")?
            .children
            .borrow()
            .clone();
        blocks.push((marker, code));
        markers.push(marker_dom);
    }
    let tree = config.dom_to_render_tree(&dom).map_err(|e| e.to_string())?;
    let text = config
        .render_to_string(tree, 160)
        .map_err(|e| e.to_string())?;
    let text = restore_code(&text, &blocks)?;
    Ok((text, scope))
}

fn pre_text(html: &[u8]) -> Result<String, String> {
    use html5ever::tokenizer::{
        BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer,
    };
    use std::cell::RefCell;
    struct TextSink(RefCell<String>);
    impl TokenSink for TextSink {
        type Handle = ();
        fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
            match token {
                Token::CharacterTokens(text) => self.0.borrow_mut().push_str(&text),
                Token::TagToken(tag)
                    if tag.kind == TagKind::StartTag && tag.name.as_ref() == "br" =>
                {
                    self.0.borrow_mut().push('\n')
                }
                _ => (),
            }
            TokenSinkResult::Continue
        }
    }
    let input = BufferQueue::default();
    input.push_back(std::str::from_utf8(html).map_err(|e| e.to_string())?.into());
    let tokenizer = Tokenizer::new(TextSink(RefCell::new(String::new())), Default::default());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    Ok(tokenizer.sink.0.into_inner())
}

fn restore_code(text: &str, blocks: &[(String, String)]) -> Result<String, String> {
    let mut spans = Vec::new();
    for (marker, code) in blocks {
        let mut matches = text.match_indices(marker);
        let Some((start, _)) = matches.next() else {
            return Err(
                "code marker was lost during rendering; read the raw artifact instead".into(),
            );
        };
        if matches.next().is_some() {
            return Err("code marker collision; read the raw artifact instead".into());
        }
        spans.push((start, start + marker.len(), code));
    }
    spans.sort_by_key(|span| span.0);
    let mut output = String::new();
    let mut from = 0;
    for (start, end, code) in spans {
        output.push_str(&text[from..start]);
        output.push_str(code);
        from = end;
    }
    output.push_str(&text[from..]);
    Ok(output)
}

fn tag(node: &Handle) -> String {
    match &node.data {
        Element { name, .. } => name.local.to_string(),
        _ => String::new(),
    }
}

fn noise(node: &Handle) -> bool {
    if matches!(
        tag(node).as_str(),
        "script" | "style" | "svg" | "nav" | "template"
    ) {
        return true;
    }
    if let Element { attrs, .. } = &node.data {
        return attrs.borrow().iter().any(|a| {
            a.name.local.as_ref() == "hidden"
                || (a.name.local.as_ref() == "aria-hidden" && a.value.as_ref() == "true")
        });
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn article_keeps_code_entities_and_links_but_not_page_chrome() {
        let html = br#"<html><head><style>CSS_NOISE</style></head><body><header>PAGE_CHROME</header>
            <article><h1>Evidence &amp; facts</h1><nav>NAV_NOISE</nav><p>Read <a href="../paper">the paper</a>.</p>
            <pre>let x = a &lt; b;
  keep_indent();</pre><svg><text>SVG_NOISE</text></svg><p hidden>HIDDEN_NOISE</p></article>
            <script>SCRIPT_NOISE</script></body></html>"#;
        let (text, scope) = extract(html, "https://example.com/blog/post").unwrap();
        assert_eq!(scope, "article");
        for word in [
            "Evidence & facts",
            "let x = a < b;",
            "  keep_indent();",
            "https://example.com/paper",
        ] {
            assert!(text.contains(word), "missing {word}: {text}");
        }
        for word in [
            "CSS_NOISE",
            "NAV_NOISE",
            "SVG_NOISE",
            "SCRIPT_NOISE",
            "PAGE_CHROME",
            "HIDDEN_NOISE",
        ] {
            assert!(!text.contains(word), "{text}");
        }
    }

    #[test]
    fn preformatted_code_is_not_reflowed_to_the_prose_width() {
        let code = format!("  let text = \"{}\";\n\n    finish();", "word ".repeat(100));
        let html = format!("<article><pre><code>{code}</code></pre></article>");
        let (text, _) = extract(html.as_bytes(), "https://example.com/").unwrap();
        assert!(text.contains(&code), "code was reflowed: {text}");
    }

    #[test]
    fn code_preserves_tabs_entities_and_marker_literals_without_replacement_recursion() {
        let html = b"<article><pre><code>\tlet x = a &lt; b;\nLATTICE_CODE_1_END</code></pre><pre>next();</pre></article>";
        let text = extract(html, "https://example.com/").unwrap().0;
        assert!(text.contains("\tlet x = a < b;\nLATTICE_CODE_1_END"));
        assert_eq!(text.matches("next();").count(), 1);
        let collision = extract(
            b"<article>LATTICE_CODE_0_END<pre>actual();</pre></article>",
            "https://example.com/",
        );
        assert!(collision.unwrap_err().contains("collision"));
    }

    #[test]
    fn main_and_plain_documents_remain_readable() {
        assert_eq!(
            extract(b"<main><p>body</p></main>", "https://example.com/")
                .unwrap()
                .1,
            "main"
        );
        assert!(
            extract(b"<p>body without article</p>", "https://example.com/")
                .unwrap()
                .0
                .contains("body without article")
        );
    }
}
