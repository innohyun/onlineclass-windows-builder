use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

pub(crate) fn normalized(value: &str) -> String {
    value.nfc().collect::<String>().to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn block_text(value: &Value) -> String {
    if let Some(items) = value.as_array() { return items.iter().map(block_text).collect::<Vec<_>>().join(" "); }
    if !value.is_object() { return String::new(); }
    let kind = value.get("type").and_then(Value::as_str).unwrap_or_default();
    if ["attachment", "attachmentBlock", "studentMaterialImage"].contains(&kind) { return String::new(); }
    if value.get("content").is_some_and(Value::is_object) { return block_text(&value["content"]); }
    [value.get("text").and_then(Value::as_str).unwrap_or_default().to_string(),
        if kind == "pageLinkBlock" { value["attrs"]["title"].as_str().unwrap_or_default().to_string() } else { String::new() },
        block_text(&value["content"]), block_text(&value["children"])].join(" ")
}

pub(crate) fn matches(page: &Value, query: &str) -> bool {
    let body = if page.get("blocks").and_then(Value::as_array).is_some_and(|blocks| !blocks.is_empty()) {
        block_text(&page["blocks"])
    } else { page["markdown"].as_str().unwrap_or_default().to_string() };
    matches_text(&format!("{} {}", page["title"].as_str().unwrap_or_default(), body), query)
}

pub(crate) fn matches_text(text: &str, query: &str) -> bool {
    let haystack = normalized(text);
    normalized(query).split_whitespace().all(|term| haystack.contains(term))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn substring_nfc_and_canonical_blocks_cover_missing_markdown_heading() {
        let page = json!({"title":"회의", "markdown":"# 회의", "blocks":[{"type":"h1","content":{"type":"heading","content":[{"type":"text","text":"소스코드 Café 한글"}]}}]});
        assert!(matches(&page, "코드"));
        assert!(matches(&page, "Cafe\u{301} 한글"));
        assert!(!matches(&page, "없는말"));
    }
}
