//! Deterministic, stateless search pages. Cursors bind the query and the
//! directory metadata generation; they are not a transactional filesystem snapshot.
use grep_matcher::Matcher;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{clip, cut_short, err, too_wide, SearchTools, DEFAULT_LIMIT};

const MAX_RESULTS: usize = 1000;
const DEFAULT_BYTES: usize = 64 * 1024;

struct Page {
    rows: Vec<Value>,
    skip: usize,
    offset: usize,
    limit: usize,
    budget: usize,
    bytes: usize,
    more: bool,
    reason: &'static str,
}

impl Page {
    fn push(&mut self, value: Value) -> bool {
        if self.skip > 0 {
            self.skip -= 1;
            return true;
        }
        let bytes = value.to_string().len() + 1;
        if self.rows.len() >= self.limit || self.bytes + bytes > self.budget {
            self.more = true;
            self.reason = if self.rows.len() >= self.limit {
                "limit"
            } else {
                "bytes"
            };
            return false;
        }
        self.bytes += bytes;
        self.rows.push(value);
        true
    }
}

pub(super) fn validate_path(path: &std::path::Path) -> Result<(), Value> {
    if path.to_str().is_none() {
        return Err(err(
            "tool.non_utf8_path",
            "a selected path cannot be represented losslessly as UTF-8; narrow the search",
        ));
    }
    Ok(())
}

fn snapshot(
    tools: &SearchTools,
    args: &Value,
    cancelled: &dyn Fn() -> bool,
) -> Result<(Vec<PathBuf>, String), Value> {
    let start = tools
        .start_dir(args["path"].as_str())
        .map_err(|e| err("tool.path_refused", &e))?;
    let hidden = args["hidden"].as_bool().unwrap_or(false);
    let ignored = args["ignored"].as_bool().unwrap_or(false);
    let filter = args["glob"]
        .as_str()
        .map(|glob| {
            globset::Glob::new(glob)
                .map(|g| g.compile_matcher())
                .map_err(|e| err("tool.bad_pattern", &e.to_string()))
        })
        .transpose()?;
    let mut paths = Vec::new();
    for (visited, entry) in tools.walker(&start, hidden, ignored).enumerate() {
        if cancelled() {
            return Err(cut_short(0));
        }
        if visited >= tools.max_visited {
            return Err(too_wide(paths.len(), ignored, tools.max_visited));
        }
        let entry = entry.map_err(|e| err("tool.search_io", &e.to_string()))?;
        if entry.file_type().is_some_and(|t| t.is_file())
            && filter
                .as_ref()
                .is_none_or(|g| g.is_match(SearchTools::matched(&start, entry.path())))
        {
            paths.push(entry.into_path());
        }
    }
    paths.sort();
    let mut hash = Sha256::new();
    hash.update(
        start
            .canonicalize()
            .map_err(|e| err("tool.search_io", &e.to_string()))?
            .as_os_str()
            .as_encoded_bytes(),
    );
    for path in &paths {
        if cancelled() {
            return Err(cut_short(0));
        }
        validate_path(path)?;
        let meta = std::fs::metadata(path).map_err(|e| err("tool.search_io", &e.to_string()))?;
        hash.update(path.as_os_str().as_encoded_bytes());
        hash.update(meta.len().to_le_bytes());
        hash.update(format!("{:?}", meta.modified()).as_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            hash.update(meta.ino().to_le_bytes());
            hash.update(meta.ctime().to_le_bytes());
            hash.update(meta.ctime_nsec().to_le_bytes());
        }
    }
    Ok((paths, format!("{:x}", hash.finalize())))
}

pub(super) fn search(
    tools: &SearchTools,
    args: &Value,
    grep: bool,
    cancelled: &dyn Fn() -> bool,
) -> Value {
    let required = if grep { "pattern" } else { "glob" };
    let Some(pattern) = args[required].as_str() else {
        return err(
            "tool.bad_arguments",
            &format!("missing '{required}' argument"),
        );
    };
    let mode = if grep {
        args["output"].as_str().unwrap_or("content")
    } else {
        "files"
    };
    if !matches!(mode, "content" | "files" | "count") {
        return err(
            "tool.bad_arguments",
            "output must be content, files, or count",
        );
    }
    let context = args["context"].as_u64().unwrap_or(0).min(10) as usize;
    let max_bytes = args["maxBytes"]
        .as_u64()
        .unwrap_or(DEFAULT_BYTES as u64)
        .clamp(4096, DEFAULT_BYTES as u64) as usize;
    let (paths, generation) = match snapshot(tools, args, cancelled) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let mut query = args.clone();
    if let Some(object) = query.as_object_mut() {
        for key in ["cursor", "limit", "maxBytes"] {
            object.remove(key);
        }
    }
    let query_hash = format!("{:x}", Sha256::digest(format!("{grep}:{query}")));
    let cursor = &args["cursor"];
    let offset = if cursor.is_null() {
        0
    } else {
        if cursor["v"] != 1 || cursor["query"] != query_hash || cursor["generation"] != generation {
            return err(
                "tool.stale_cursor",
                "search query or directory metadata changed; start a new search without cursor",
            );
        }
        let Some(offset) = cursor["offset"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
        else {
            return err("tool.bad_arguments", "invalid search cursor offset");
        };
        offset
    };
    let mut page = Page {
        rows: Vec::new(),
        skip: offset,
        offset,
        limit: args["limit"]
            .as_u64()
            .unwrap_or(DEFAULT_LIMIT as u64)
            .clamp(1, MAX_RESULTS as u64) as usize,
        budget: max_bytes - 1024,
        bytes: 0,
        more: false,
        reason: "",
    };
    let matcher = if grep {
        match grep_regex::RegexMatcherBuilder::new()
            .case_insensitive(args["ignoreCase"].as_bool().unwrap_or(false))
            .fixed_strings(args["fixed"].as_bool().unwrap_or(false))
            .line_terminator(Some(b'\n'))
            .build(pattern)
        {
            Ok(m) => Some(m),
            Err(e) => return err("tool.bad_pattern", &e.to_string()),
        }
    } else {
        None
    };
    for path in &paths {
        if cancelled() {
            return cut_short(page.rows.len());
        }
        let shown = tools.shown(path);
        let Some(matcher) = &matcher else {
            if !page.push(json!(shown)) {
                break;
            }
            continue;
        };
        let mut count = 0u64;
        let outcome = super::scan::path(path, matcher, cancelled, |line, text| {
            if cancelled() {
                return Ok(false);
            }
            count += 1;
            if mode == "files" {
                return Ok(false);
            }
            if mode == "count" {
                return Ok(true);
            }
            let text = text.trim_end_matches('\n');
            let mut row = json!({"path": shown, "line": line, "text": clip(text)});
            if context > 0 {
                row["match"] = json!(true);
                row["selected"] = json!(true);
            }
            if args["matchWindow"] == true {
                use grep_matcher::Matcher;
                let position = matcher
                    .find(text.as_bytes())
                    .ok()
                    .flatten()
                    .map(|m| m.start());
                if let Some(at) = position {
                    let mut boundary = at;
                    while !text.is_char_boundary(boundary) {
                        boundary -= 1;
                    }
                    let start = text[..boundary]
                        .char_indices()
                        .rev()
                        .nth(80)
                        .map_or(0, |(i, _)| i);
                    row["matchByte"] = json!(at);
                    row["windowByte"] = json!(start);
                    row["text"] = json!(clip(&text[start..]));
                }
            }
            Ok(page.push(row))
        });
        if cancelled() {
            return cut_short(page.rows.len());
        }
        match outcome {
            Err(e) => return err("tool.search_io", &format!("{}: {e}", path.display())),
            Ok(true) if count > 0 => return err(
                "tool.binary_data",
                "binary data interrupted the scan after matches; no complete result is available",
            ),
            Ok(true) => continue,
            Ok(false) => {}
        }
        if mode == "files" && count > 0 && !page.push(json!(shown)) {
            break;
        }
        if mode == "count" && count > 0 && !page.push(json!({"path": shown, "count": count})) {
            break;
        }
        if page.more {
            break;
        }
    }
    if page.more && page.rows.is_empty() {
        return err(
            "tool.result_too_large",
            "one search record exceeds maxBytes; narrow the search or increase maxBytes",
        );
    }
    if page.skip > 0 {
        return err(
            "tool.stale_cursor",
            "cursor exceeds the current search; start a new search",
        );
    }
    let consumed = page.rows.len();
    let mut context_truncated = false;
    if grep && mode == "content" && context > 0 {
        match add_context(
            tools,
            &paths,
            &page.rows,
            context,
            page.budget,
            cancelled,
            matcher.as_ref().unwrap(),
        ) {
            Ok((rows, truncated)) => {
                page.rows = rows;
                context_truncated = truncated;
            }
            Err(e) => return e,
        }
    }
    // A change during the operation must not produce a cursor for mixed generations.
    match snapshot(tools, args, cancelled) {
        Ok((_, after)) if after == generation => {}
        Ok(_) => {
            return err(
                "tool.stale_cursor",
                "directory metadata changed during search; retry without cursor",
            )
        }
        Err(e) => return e,
    }
    let key = match mode {
        "files" => "paths",
        "count" => "counts",
        _ => "matches",
    };
    let mut result = json!({key: page.rows, "more": page.more});
    if context_truncated {
        result["contextTruncated"] = json!(true);
    }
    if page.more {
        result["next"] = json!({"v": 1, "query": query_hash, "generation": generation, "offset": page.offset + consumed});
        result["truncatedBy"] = json!(page.reason);
    }
    json!({"status": "ok", "result": result})
}

fn add_context(
    tools: &SearchTools,
    paths: &[PathBuf],
    hits: &[Value],
    context: usize,
    budget: usize,
    cancelled: &dyn Fn() -> bool,
    matcher: &grep_regex::RegexMatcher,
) -> Result<(Vec<Value>, bool), Value> {
    let mut rows: BTreeMap<(String, u64), Value> = BTreeMap::new();
    let mut wanted: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    let mut bytes = 0;
    for hit in hits {
        let path = hit["path"].as_str().unwrap().to_string();
        let line = hit["line"].as_u64().unwrap();
        let mut row = hit.clone();
        row["match"] = json!(true);
        bytes += row.to_string().len() + 1;
        rows.insert((path.clone(), line), row);
        wanted.entry(path).or_default().extend(
            line.saturating_sub(context as u64).max(1)..=line.saturating_add(context as u64),
        );
    }
    let mut truncated = false;
    for path in paths {
        let shown = tools.shown(path);
        let Some(lines) = wanted.get(&shown) else {
            continue;
        };
        let last = *lines.last().unwrap();
        let every_line = grep_regex::RegexMatcherBuilder::new()
            .line_terminator(Some(b'\n'))
            .build("")
            .expect("empty pattern is valid");
        let outcome = super::scan::path(path, &every_line, cancelled, |line, text| {
            if lines.contains(&line) && !rows.contains_key(&(shown.clone(), line)) {
                let text = text.trim_end_matches('\n');
                let matched = matcher
                    .is_match(text.as_bytes())
                    .map_err(std::io::Error::other)?;
                let row = json!({"path": shown, "line": line, "text": clip(text), "match": matched, "selected": false});
                let size = row.to_string().len() + 1;
                if bytes + size <= budget {
                    rows.insert((shown.clone(), line), row);
                    bytes += size;
                } else {
                    truncated = true;
                }
            }
            Ok(line < last)
        });
        if cancelled() {
            return Err(cut_short(hits.len()));
        }
        match outcome {
            Ok(false) => {}
            Ok(true) => {
                return Err(err(
                    "tool.binary_data",
                    "binary data interrupted context retrieval",
                ))
            }
            Err(e) => return Err(err("tool.search_io", &e.to_string())),
        }
    }
    Ok((rows.into_values().collect(), truncated))
}
