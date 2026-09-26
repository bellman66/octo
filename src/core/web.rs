//! 경로 컨텍스트가 외부 링크일 때 본문을 가져온다. HTML은 태그를 벗겨 텍스트로 넘긴다.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::i18n::t;
use crate::tr;

const MAX_BYTES: u64 = 5 * 1024 * 1024;
const MAX_CHARS: usize = 100_000;
const MIN_HTML_CHARS: usize = 100;

pub fn is_url(path: &str) -> bool {
    let lower = path.trim_start().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

pub struct Page {
    /// `<title>`. HTML이 아니면 None
    pub title: Option<String>,
    pub text: String,
}

/// 조건부 요청에 쓰는 응답 헤더. 둘 다 없으면 서버가 지원하지 않는 것이다.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Validators {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

pub enum Fetched {
    /// 304: 넘긴 validators의 본문이 아직 최신이다
    NotModified,
    Page(Page, Validators),
}

/// `validators`가 있으면 조건부 요청을 보낸다.
pub fn fetch(url: &str, validators: &Validators) -> Result<Fetched, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .into();

    let mut request = agent.get(url).header("User-Agent", concat!("octo/", env!("CARGO_PKG_VERSION")));
    if let Some(etag) = &validators.etag {
        request = request.header("If-None-Match", etag);
    }
    if let Some(last_modified) = &validators.last_modified {
        request = request.header("If-Modified-Since", last_modified);
    }
    let mut response = request
        .call()
        .map_err(|err| tr!("연결 끊김: {url} ({err})", "Broken: {url} ({err})"))?;

    let status = response.status();
    if status.as_u16() == 304 {
        return Ok(Fetched::NotModified);
    }
    if !status.is_success() {
        let hint = match status.as_u16() {
            401 | 403 => t(
                " · 로그인이 필요한 페이지는 내용을 복사해 문서로 넣어 주세요",
                " · For pages that need a login, copy the content into a Doc",
            ),
            _ => "",
        };
        return Err(tr!("연결 끊김: HTTP {status}{hint}", "Broken: HTTP {status}{hint}"));
    }

    let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let html = header("Content-Type").is_some_and(|v| v.contains("html"));
    let validators = Validators { etag: header("ETag"), last_modified: header("Last-Modified") };
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_BYTES)
        .read_to_string()
        .map_err(|err| tr!("본문을 읽지 못했습니다: {err}", "Couldn't read the body: {err}"))?;

    let page = if html {
        let text = html_to_text(&body);
        // 본문을 스크립트로 그리는 페이지는 받아봐야 껍데기뿐이다
        if text.chars().count() < MIN_HTML_CHARS && body.to_ascii_lowercase().contains("<script") {
            return Err(t(
                "연결 끊김: 스크립트로 그리는 페이지라 본문이 없습니다 · 내용을 복사해 문서로 넣어 주세요",
                "Broken: the page is rendered by scripts and has no content · copy the content into a Doc",
            )
            .into());
        }
        Page { title: tag_text(&body, "title"), text }
    } else {
        Page { title: None, text: body }
    };
    Ok(Fetched::Page(Page { text: truncate(page.text), ..page }, validators))
}

pub fn truncate(text: String) -> String {
    if text.chars().count() <= MAX_CHARS {
        return text;
    }
    let mut out: String = text.chars().take(MAX_CHARS).collect();
    out.push_str(&tr!("\n… {MAX_CHARS}자까지만 표시", "\n… showing first {MAX_CHARS} characters only"));
    out
}

/// 첫 `<tag>…</tag>` 안의 텍스트
fn tag_text(html: &str, tag: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = lower.find(&format!("<{tag}"))?;
    let start = open + lower[open..].find('>')? + 1;
    let end = start + lower[start..].find(&format!("</{tag}"))?;
    let text = collapse(&decode(&html[start..end]));
    (!text.is_empty()).then_some(text)
}

/// script·style·head를 버리고 태그를 벗긴다. 블록 태그는 줄바꿈으로 남긴다.
fn html_to_text(html: &str) -> String {
    const SKIP: [&str; 5] = ["script", "style", "head", "noscript", "svg"];
    const BLOCK: [&str; 16] = [
        "p", "div", "br", "li", "tr", "h1", "h2", "h3", "h4", "h5", "h6", "pre", "section", "header", "footer", "table",
    ];

    let lower = html.to_ascii_lowercase();
    let mut out = String::new();
    let mut i = 0;
    while let Some(offset) = lower[i..].find('<') {
        out.push_str(&html[i..i + offset]);
        let start = i + offset;
        let Some(len) = lower[start..].find('>') else { break };
        let tag = &lower[start + 1..start + len];
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        i = start + len + 1;

        if !tag.starts_with('/') && SKIP.contains(&name.as_str()) {
            // 닫는 태그까지 통째로 건너뛴다
            match lower[i..].find(&format!("</{name}")) {
                Some(close) => i += close + lower[i + close..].find('>').map_or(0, |gt| gt + 1),
                None => break,
            }
        } else if BLOCK.contains(&name.as_str()) {
            out.push('\n');
        }
    }
    if i < html.len() && !lower[i..].contains('<') {
        out.push_str(&html[i..]);
    }

    decode(&out)
        .lines()
        .map(collapse)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_urls() {
        assert!(is_url("https://example.com"));
        assert!(is_url("HTTP://example.com"));
        assert!(!is_url("C:\\workspace\\docs"));
    }

    #[test]
    fn strips_html() {
        let html = "<html><head><title>My &amp; Page</title><style>p{}</style></head>\
                    <body><h1>Hello</h1><p>one <b>two</b></p><script>x()</script><p>a &lt; b</p></body></html>";
        assert_eq!(tag_text(html, "title").as_deref(), Some("My & Page"));
        assert_eq!(html_to_text(html), "Hello\none two\na < b");
    }
}
