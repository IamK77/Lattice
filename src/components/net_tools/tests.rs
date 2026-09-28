use super::*;

#[test]
fn html_is_extracted_before_preview_limits_and_both_artifacts_are_readable() {
    let dir = tempfile::tempdir().unwrap();
    let html = format!(
        "<nav>{}</nav><article><h1>Actual evidence</h1><p>last line</p></article>",
        "navigation ".repeat(9000)
    );
    let result = present(
        FetchBody {
            bytes: html.as_bytes(),
            source: "https://example.com/x",
            content_type: "text/html; charset=utf-8",
            http_status: 200,
            source_truncated: false,
        },
        "auto",
        16,
        dir.path(),
    )
    .unwrap();
    assert!(result["body"].as_str().unwrap().contains("Actual"));
    assert_eq!(result["truncated"], true);
    assert_eq!(result["raw"]["complete"], true);
    assert_eq!(
        std::fs::read(result["raw"]["path"].as_str().unwrap()).unwrap(),
        html.as_bytes()
    );
    let full = std::fs::read_to_string(result["document"]["path"].as_str().unwrap()).unwrap();
    assert!(full.contains("last line"));
    assert!(!full.contains("navigation"));
}

#[test]
fn raw_json_and_incomplete_sources_are_not_misrepresented() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = br#"{"literal":"<article>not HTML</article>"}"#;
    let result = present(
        FetchBody {
            bytes,
            source: "https://example.com/x",
            content_type: "application/json",
            http_status: 404,
            source_truncated: true,
        },
        "auto",
        65536,
        dir.path(),
    )
    .unwrap();
    assert_eq!(result["body"], std::str::from_utf8(bytes).unwrap());
    assert_eq!(result["raw"]["complete"], false);
    assert_eq!(result["sourceTruncated"], true);
    assert_eq!(result["http_status"], 404);
    let raw = present(
        FetchBody {
            bytes: b"<article>keep tags</article>",
            source: "https://example.com/x",
            content_type: "text/html",
            http_status: 200,
            source_truncated: false,
        },
        "raw",
        65536,
        dir.path(),
    )
    .unwrap();
    assert_eq!(raw["body"], "<article>keep tags</article>");
}
