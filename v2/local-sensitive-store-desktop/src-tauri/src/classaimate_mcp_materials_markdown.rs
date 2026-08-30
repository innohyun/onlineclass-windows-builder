use regex::Regex;
use std::sync::OnceLock;

pub(crate) fn sanitize(markdown: &str) -> String {
    static HTML_ATTACHMENT: OnceLock<Regex> = OnceLock::new();
    static MARKDOWN_ATTACHMENT: OnceLock<Regex> = OnceLock::new();
    static REFERENCE_IMAGE: OnceLock<Regex> = OnceLock::new();
    static MARKDOWN_IMAGE: OnceLock<Regex> = OnceLock::new();
    static HTML_IMAGE: OnceLock<Regex> = OnceLock::new();
    static HTML_ANCHOR: OnceLock<Regex> = OnceLock::new();
    static HTML_URL_ATTRIBUTE: OnceLock<Regex> = OnceLock::new();
    static WORKNOTE_LINK: OnceLock<Regex> = OnceLock::new();
    static MARKDOWN_LINK: OnceLock<Regex> = OnceLock::new();
    static REFERENCE_LINK: OnceLock<Regex> = OnceLock::new();
    static REFERENCE_DEFINITION: OnceLock<Regex> = OnceLock::new();
    static AUTOLINK: OnceLock<Regex> = OnceLock::new();
    static RAW_URL: OnceLock<Regex> = OnceLock::new();
    static MENTION: OnceLock<Regex> = OnceLock::new();

    let mut safe = HTML_ATTACHMENT
        .get_or_init(|| {
            Regex::new(r#"(?is)<a\b[^>]*href\s*=\s*["']?local-attachment://[^>]*>.*?</a\s*>"#)
                .expect("valid HTML attachment regex")
        })
        .replace_all(markdown, "")
        .into_owned();
    safe = MARKDOWN_ATTACHMENT
        .get_or_init(|| {
            Regex::new(r#"(?m)!?\[(?:\\.|[^\]\r\n])*\]\([ \t]*local-attachment://[^)\s]+[ \t]*\)"#)
                .expect("valid Markdown attachment regex")
        })
        .replace_all(&safe, "")
        .into_owned();
    safe = REFERENCE_IMAGE
        .get_or_init(|| {
            Regex::new(r#"(?m)!\[(?:\\.|[^\]\r\n])*\]\[[^\]\r\n]*\]"#)
                .expect("valid reference image regex")
        })
        .replace_all(&safe, "")
        .into_owned();
    safe = MARKDOWN_IMAGE
        .get_or_init(|| {
            Regex::new(r#"(?m)!\[(?:\\.|[^\]\r\n])*\]\((?:\\.|[^)\r\n])*\)"#)
                .expect("valid Markdown image regex")
        })
        .replace_all(&safe, "")
        .into_owned();
    safe = HTML_IMAGE
        .get_or_init(|| Regex::new(r#"(?is)<img\b[^>]*>"#).expect("valid HTML image regex"))
        .replace_all(&safe, "")
        .into_owned();
    safe = HTML_ANCHOR
        .get_or_init(|| {
            Regex::new(r#"(?is)<a\b[^>]*>(.*?)</a\s*>"#).expect("valid HTML anchor regex")
        })
        .replace_all(&safe, "$1")
        .into_owned();
    safe = HTML_URL_ATTRIBUTE
        .get_or_init(|| {
            Regex::new(r#"(?is)\s+(?:href|src)\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+)"#)
                .expect("valid HTML URL attribute regex")
        })
        .replace_all(&safe, "")
        .into_owned();
    safe = WORKNOTE_LINK
        .get_or_init(|| {
            Regex::new(r#"(?m)\[\[worknote:[^|\]\r\n]*(?:\|([^\]\r\n]*))?\]\]"#)
                .expect("valid work note link regex")
        })
        .replace_all(&safe, "$1")
        .into_owned();
    safe = MARKDOWN_LINK
        .get_or_init(|| {
            Regex::new(r#"(?m)\[((?:\\.|[^\]\r\n])*)\]\((?:\\.|[^)\r\n])*\)"#)
                .expect("valid Markdown link regex")
        })
        .replace_all(&safe, "$1")
        .into_owned();
    safe = REFERENCE_LINK
        .get_or_init(|| {
            Regex::new(r#"(?m)\[((?:\\.|[^\]\r\n])*)\]\[[^\]\r\n]*\]"#)
                .expect("valid reference link regex")
        })
        .replace_all(&safe, "$1")
        .into_owned();
    safe = REFERENCE_DEFINITION
        .get_or_init(|| {
            Regex::new(r#"(?m)^[ \t]{0,3}\[[^\]\r\n]+\]:[^\r\n]*$"#)
                .expect("valid reference definition regex")
        })
        .replace_all(&safe, "")
        .into_owned();
    safe = AUTOLINK
        .get_or_init(|| {
            Regex::new(
                r#"(?i)<(?:https?://|mailto:|tel:|worknote://|local-attachment://)[^>\r\n]*>"#,
            )
            .expect("valid autolink regex")
        })
        .replace_all(&safe, "")
        .into_owned();
    safe = RAW_URL
        .get_or_init(|| {
            Regex::new(
                r#"(?i)(?:https?://|mailto:|tel:|worknote://|local-attachment://)[^\s<>()\[\]{}]+"#,
            )
            .expect("valid raw URL regex")
        })
        .replace_all(&safe, "")
        .into_owned();
    safe = MENTION
        .get_or_init(|| {
            Regex::new(r#"(?m)(^|[\s(\[{>:])@[\p{L}\p{N}_-]{1,40}"#).expect("valid mention regex")
        })
        .replace_all(&safe, "$1[멘션]")
        .into_owned();

    let mut lines = Vec::new();
    let mut previous_blank = false;
    for line in safe.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            if !previous_blank {
                lines.push("");
            }
            previous_blank = true;
        } else {
            lines.push(line);
            previous_blank = false;
        }
    }
    lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_attachments_urls_and_mentions_but_keeps_link_labels() {
        let source = r#"# 공개 가능한 본문
![현장 사진.jpg](local-attachment://photo-secret)
[교육계획.pdf](local-attachment://file-secret)
[공식 안내](https://example.com/private?q=secret)
[[worknote:private-page|내부 문서]]
확인: @이선생
원문 주소 https://secret.example/path
<a href="https://html.example/secret">HTML 링크</a>
<img src="https://html.example/private.png" alt="개인사진.png">
[참조 링크][private-ref]
[private-ref]: https://reference.example/private
"#;
        let safe = sanitize(source);
        for kept in [
            "# 공개 가능한 본문",
            "공식 안내",
            "내부 문서",
            "HTML 링크",
            "참조 링크",
            "[멘션]",
        ] {
            assert!(safe.contains(kept), "missing safe label: {kept}");
        }
        for removed in [
            "현장 사진.jpg",
            "교육계획.pdf",
            "개인사진.png",
            "photo-secret",
            "file-secret",
            "private-page",
            "https://",
            "local-attachment://",
            "@이선생",
            "private-ref",
            "href=",
            "src=",
        ] {
            assert!(!safe.contains(removed), "leaked unsafe Markdown: {removed}");
        }
    }
}
