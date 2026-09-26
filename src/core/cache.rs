//! 외부 링크(URL)용 캐시 두 가지. 디스크(`<root>/cache`)에 둔다.
//!
//! - 접근 방식: octo가 못 여는 링크를 에이전트가 어떻게 열었는지. URL별로 찾고, 없으면 같은 도메인, 그다음 기본 규칙.
//! - 조회 결과: 받아 온 본문. octo가 받았거나 에이전트가 `report_access`로 제출한 것.
//!   1일이 지나면 오래된 값이 컨텍스트를 오염시키지 않도록 내주지 않고 지운다.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::core::store::{Source, Store};
use crate::core::web::{self, Fetched, Page, Validators};
use crate::i18n::{is_en, t};
use crate::tr;

const TTL_SECS: u64 = 24 * 60 * 60;
/// 깨진 링크 때문에 목차가 매번 타임아웃을 기다리지 않게
const FAILURE_SECS: u64 = 10 * 60;

/// 도메인별 기본 접근 방식. 에이전트가 보고한 방식이 없을 때만 쓴다.
const RULES: [(&str, &str, &str); 3] = [
    (
        "claude.ai",
        "claude.ai 아티팩트는 로그인이 필요해 octo가 열 수 없습니다. Claude Code라면 Artifact 도구에 action \"read\"와 이 URL을 넘기세요.",
        "claude.ai artifacts need a login, so octo can't open them. In Claude Code, pass this URL to the Artifact tool with action \"read\".",
    ),
    (
        "docs.google.com",
        "Google 문서는 로그인이 필요합니다. Google Drive MCP가 있으면 URL의 파일 id로 read_file_content를 쓰세요.",
        "Google Docs need a login. If a Google Drive MCP is available, use read_file_content with the file id from the URL.",
    ),
    (
        "atlassian.net",
        "Confluence·Jira는 로그인이 필요합니다. Atlassian MCP가 있으면 getConfluenceContent·getJiraIssue로 읽으세요.",
        "Confluence and Jira need a login. If an Atlassian MCP is available, read it with getConfluenceContent or getJiraIssue.",
    ),
];

/// 에이전트가 보고한 접근 방식
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Method {
    pub text: String,
    /// 보고한 MCP 클라이언트 이름. 방식은 클라이언트마다 다를 수 있다.
    pub client: String,
    pub reported_at: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Methods {
    #[serde(default)]
    by_url: BTreeMap<String, Method>,
    #[serde(default)]
    by_domain: BTreeMap<String, Method>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Origin {
    Octo { validators: Validators },
    Agent { client: String, version: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cached {
    url: String,
    title: Option<String>,
    text: String,
    origin: Origin,
    fetched_at: u64,
}

/// 받아 온 본문. `note`가 있으면 원본이 아니라 캐시본이다.
pub struct Loaded {
    pub page: Page,
    pub note: Option<String>,
}

impl Loaded {
    /// 세션이 받는 본문. 캐시본이면 출처를 맨 위에 붙인다.
    pub fn into_text(self) -> String {
        match self.note {
            Some(note) => format!("[{note}]\n\n{}", self.page.text),
            None => self.page.text,
        }
    }
}

/// 앱의 컨텍스트 상세에 보여줄 캐시 상태
pub struct Info {
    pub result: Option<String>,
    /// (어디서 온 방식인지, 안내문). 기본 규칙은 지울 수 없어 넣지 않는다.
    pub method: Option<(String, String)>,
}

/// 파일 읽고 쓰기를 한 줄로 세운다. MCP 요청은 스레드마다 오기 때문.
static LOCK: Mutex<()> = Mutex::new(());
/// URL → (실패 시각, 사유). 짧게만 기억하므로 메모리에 둔다.
static FAILURES: LazyLock<Mutex<HashMap<String, (u64, String)>>> = LazyLock::new(Default::default);

/// 캐시를 거쳐 URL 본문을 가져온다.
pub fn load(store: &Store, url: &str) -> Result<Loaded, String> {
    let now = now();
    let cached = fresh_result(store, url, now);

    // 에이전트가 제출한 본문은 octo가 원래 못 읽는 링크라 다시 받아 오지 않는다
    if let Some(cached @ Cached { origin: Origin::Agent { .. }, .. }) = &cached {
        return Ok(served(cached, now));
    }
    if let Some(reason) = recent_failure(url, now) {
        return Err(with_hint(store, url, &reason));
    }

    let validators = match &cached {
        Some(Cached { origin: Origin::Octo { validators }, .. }) => validators.clone(),
        _ => Validators::default(),
    };
    match web::fetch(url, &validators) {
        Ok(Fetched::NotModified) if cached.is_some() => {
            let mut cached = cached.expect("checked above");
            // 원본이 그대로임을 방금 확인했으니 다시 1일 유효하다
            cached.fetched_at = now;
            let _ = save_result(store, &cached);
            Ok(Loaded { page: Page { title: cached.title, text: cached.text }, note: None })
        }
        Ok(Fetched::NotModified) => Err(with_hint(store, url, t("연결 끊김: 서버가 본문 없이 304를 보냈습니다", "Broken: the server sent 304 with no content"))),
        Ok(Fetched::Page(page, validators)) => {
            FAILURES.lock().unwrap().remove(url);
            let _ = save_result(
                store,
                &Cached {
                    url: url.to_owned(),
                    title: page.title.clone(),
                    text: page.text.clone(),
                    origin: Origin::Octo { validators },
                    fetched_at: now,
                },
            );
            Ok(Loaded { page, note: None })
        }
        // 확인은 못 했지만 아직 1일이 안 됐으면 출처를 밝히고 내준다
        Err(_) if cached.is_some() => Ok(served(cached.as_ref().expect("checked above"), now)),
        Err(reason) => {
            FAILURES.lock().unwrap().insert(url.to_owned(), (now, reason.clone()));
            Err(with_hint(store, url, &reason))
        }
    }
}

/// `report_access`: 에이전트가 원본을 연 방식과, 있으면 본문을 남긴다.
pub fn report(store: &Store, url: &str, client: &str, method: &str, content: Option<&str>, version: Option<&str>) -> io::Result<()> {
    let now = now();
    let method = Method { text: method.trim().to_owned(), client: client.to_owned(), reported_at: now };
    {
        let _guard = LOCK.lock().unwrap();
        let mut methods = read_methods(store);
        if let Some(domain) = domain(url) {
            methods.by_domain.insert(domain, method.clone());
        }
        methods.by_url.insert(url.to_owned(), method);
        write(&methods_path(store), &serde_json::to_vec_pretty(&methods).map_err(io::Error::other)?)?;
    }
    if let Some(content) = content.map(str::trim).filter(|c| !c.is_empty()) {
        FAILURES.lock().unwrap().remove(url);
        save_result(
            store,
            &Cached {
                url: url.to_owned(),
                title: None,
                text: web::truncate(content.to_owned()),
                origin: Origin::Agent { client: client.to_owned(), version: version.map(str::to_owned).filter(|v| !v.is_empty()) },
                fetched_at: now,
            },
        )?;
    }
    Ok(())
}

pub fn info(store: &Store, url: &str) -> Info {
    let now = now();
    let methods = read_methods(store);
    let method = match methods.by_url.get(url) {
        Some(m) => Some((tr!("이 링크 · {} · {}", "This link · {} · {}", m.client, ago(now, m.reported_at)), m.text.clone())),
        None => domain(url).and_then(|d| {
            methods.by_domain.get(&d).map(|m| {
                (tr!("같은 도메인 {d} · {} · {}", "Same domain {d} · {} · {}", m.client, ago(now, m.reported_at)), m.text.clone())
            })
        }),
    };
    Info { result: fresh_result(store, url, now).map(|c| label(&c, now)), method }
}

pub fn clear_result(store: &Store, url: &str) {
    FAILURES.lock().unwrap().remove(url);
    let _guard = LOCK.lock().unwrap();
    let _ = fs::remove_file(result_path(store, url));
}

/// 상세에 보이던 방식(이 링크 것, 없으면 같은 도메인 것)을 지운다.
pub fn clear_method(store: &Store, url: &str) -> io::Result<()> {
    let _guard = LOCK.lock().unwrap();
    let mut methods = read_methods(store);
    if methods.by_url.remove(url).is_none()
        && let Some(domain) = domain(url)
    {
        methods.by_domain.remove(&domain);
    }
    write(&methods_path(store), &serde_json::to_vec_pretty(&methods).map_err(io::Error::other)?)
}

/// 컨텍스트를 지우거나 경로를 바꾼 뒤, 그 URL을 쓰는 컨텍스트가 더 없으면 조회 결과를 버린다.
/// 접근 방식은 다른 링크도 쓸 수 있으니 남긴다.
pub fn forget_if_unused(store: &Store, url: &str) {
    if !web::is_url(url) {
        return;
    }
    let used = store.contexts().iter().any(|c| matches!(&c.source, Source::Path { path } if path == url));
    if !used {
        clear_result(store, url);
    }
}

/// 만료된 조회 결과를 지운다. 시작할 때 한 번 돈다.
pub fn sweep(store: &Store) {
    let now = now();
    let Ok(entries) = fs::read_dir(results_dir(store)) else { return };
    let _guard = LOCK.lock().unwrap();
    for path in entries.flatten().map(|e| e.path()) {
        let expired = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Cached>(&bytes).ok())
            .is_none_or(|c| expired(&c, now));
        if expired {
            let _ = fs::remove_file(path);
        }
    }
}

// ── 내부 ──

/// 1일이 안 된 조회 결과. 만료됐으면 지우고 None.
fn fresh_result(store: &Store, url: &str, now: u64) -> Option<Cached> {
    let path = result_path(store, url);
    let _guard = LOCK.lock().unwrap();
    let cached: Cached = serde_json::from_slice(&fs::read(&path).ok()?).ok()?;
    // 해시가 겹친 다른 URL이면 없는 것으로 본다
    if cached.url != url {
        return None;
    }
    if expired(&cached, now) {
        let _ = fs::remove_file(&path);
        return None;
    }
    Some(cached)
}

fn expired(cached: &Cached, now: u64) -> bool {
    now.saturating_sub(cached.fetched_at) >= TTL_SECS
}

fn served(cached: &Cached, now: u64) -> Loaded {
    let mut note = label(cached, now);
    if let Origin::Agent { .. } = cached.origin {
        note.push_str(t(
            "\n원본에 접근할 수 있으면 현재 버전을 확인하고, 다르면 다시 읽어 report_access로 갱신하세요.",
            "\nIf you can open the source, check its current version; if it differs, read it again and update it with report_access.",
        ));
    }
    Loaded { page: Page { title: cached.title.clone(), text: cached.text.clone() }, note: Some(note) }
}

fn label(cached: &Cached, now: u64) -> String {
    let when = ago(now, cached.fetched_at);
    match &cached.origin {
        Origin::Agent { client, version: Some(version) } => {
            tr!("캐시 · 에이전트 제출 ({client}) · {when} · 버전 {version}", "Cached · submitted by agent ({client}) · {when} · version {version}")
        }
        Origin::Agent { client, version: None } => tr!("캐시 · 에이전트 제출 ({client}) · {when}", "Cached · submitted by agent ({client}) · {when}"),
        Origin::Octo { .. } => tr!("캐시 · 원본 확인 실패, octo가 {when} 받은 내용", "Cached · couldn't reach the source, fetched by octo {when}"),
    }
}

fn recent_failure(url: &str, now: u64) -> Option<String> {
    let failures = FAILURES.lock().unwrap();
    let (at, reason) = failures.get(url)?;
    (now.saturating_sub(*at) < FAILURE_SECS).then(|| reason.clone())
}

/// 실패 사유에 접근 방식 힌트와 보고 요청을 붙인다.
fn with_hint(store: &Store, url: &str, reason: &str) -> String {
    let now = now();
    let methods = read_methods(store);
    let domain = domain(url);
    let hint = if let Some(m) = methods.by_url.get(url) {
        Some(tr!("접근 방식 ({} · {} 보고): {}", "How to open it ({} · reported {}): {}", m.client, ago(now, m.reported_at), m.text))
    } else if let Some((d, m)) = domain.as_ref().and_then(|d| methods.by_domain.get(d).map(|m| (d, m))) {
        Some(tr!(
            "접근 방식 (같은 도메인 {d}에서 {} · {} 보고): {}",
            "How to open it (reported for {d} by {} · {}): {}",
            m.client,
            ago(now, m.reported_at),
            m.text
        ))
    } else {
        domain.as_deref().and_then(rule).map(|text| tr!("접근 방식 (기본 규칙): {text}", "How to open it (default rule): {text}"))
    };

    let mut out = reason.to_owned();
    if let Some(hint) = hint {
        out.push('\n');
        out.push_str(&hint);
    }
    out.push('\n');
    out.push_str(t(
        "원본을 읽었다면 report_access로 방식과 본문을 남겨 주세요. 다음 세션부터 바로 씁니다.",
        "If you managed to read the source, leave the method and content with report_access so later sessions can use it directly.",
    ));
    out
}

fn rule(domain: &str) -> Option<&'static str> {
    RULES
        .iter()
        .find(|(d, ..)| domain == *d || domain.ends_with(&format!(".{d}")))
        .map(|(_, ko, en)| if is_en() { *en } else { *ko })
}

/// `https://user@Host:8080/path` → `host`
fn domain(url: &str) -> Option<String> {
    let rest = url.trim().split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn ago(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..60 => t("방금", "just now").to_owned(),
        60..3600 => tr!("{}분 전", "{}m ago", secs / 60),
        3600..86400 => tr!("{}시간 전", "{}h ago", secs / 3600),
        _ => tr!("{}일 전", "{}d ago", secs / 86400),
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn read_methods(store: &Store) -> Methods {
    fs::read(methods_path(store)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

fn save_result(store: &Store, cached: &Cached) -> io::Result<()> {
    let _guard = LOCK.lock().unwrap();
    write(&result_path(store, &cached.url), &serde_json::to_vec_pretty(cached).map_err(io::Error::other)?)
}

fn write(path: &PathBuf, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

fn methods_path(store: &Store) -> PathBuf {
    store.root().join("cache").join("access.json")
}

fn results_dir(store: &Store) -> PathBuf {
    store.root().join("cache").join("results")
}

/// 파일 이름은 URL의 FNV-1a 해시. 버전이 바뀌어도 같은 값이 나와야 해서 직접 계산한다.
fn result_path(store: &Store, url: &str) -> PathBuf {
    let hash = url.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    results_dir(store).join(format!("{hash:016x}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> Store {
        let root = std::env::temp_dir().join(format!("octo-cache-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        Store::at(root).unwrap()
    }

    #[test]
    fn parses_domains() {
        assert_eq!(domain("https://claude.ai/artifact/x?sk=1").as_deref(), Some("claude.ai"));
        assert_eq!(domain("http://me@Example.com:8080/a").as_deref(), Some("example.com"));
        assert_eq!(domain("C:\\docs"), None);
    }

    #[test]
    fn rules_match_subdomains() {
        assert!(rule("claude.ai").is_some());
        assert!(rule("team.atlassian.net").is_some());
        assert!(rule("notclaude.ai").is_none());
    }

    #[test]
    fn agent_report_is_served_with_its_origin() {
        let store = temp_store("report");
        let url = "https://claude.ai/artifact/abc";
        report(&store, url, "claude-code", "Artifact read", Some("iced 0.14 guide"), Some("v1")).unwrap();

        let loaded = load(&store, url).unwrap();
        assert!(loaded.note.as_deref().unwrap().contains("claude-code"));
        assert!(loaded.into_text().ends_with("iced 0.14 guide"));
        // 같은 도메인의 다른 링크는 방식만 물려받는다
        assert!(info(&store, "https://claude.ai/artifact/other").method.is_some());
        assert!(info(&store, "https://claude.ai/artifact/other").result.is_none());
    }

    #[test]
    fn expired_results_are_dropped() {
        let store = temp_store("expired");
        let url = "https://claude.ai/artifact/old";
        save_result(
            &store,
            &Cached {
                url: url.into(),
                title: None,
                text: "stale".into(),
                origin: Origin::Agent { client: "x".into(), version: None },
                fetched_at: now() - TTL_SECS,
            },
        )
        .unwrap();
        assert!(fresh_result(&store, url, now()).is_none());
        assert!(!result_path(&store, url).exists());
    }

    #[test]
    fn clearing_method_falls_back_to_domain_then_nothing() {
        let store = temp_store("clear");
        report(&store, "https://claude.ai/artifact/a", "c", "m", None, None).unwrap();
        clear_method(&store, "https://claude.ai/artifact/a").unwrap();
        assert!(info(&store, "https://claude.ai/artifact/a").method.unwrap().0.contains("claude.ai"));
        clear_method(&store, "https://claude.ai/artifact/a").unwrap();
        assert!(info(&store, "https://claude.ai/artifact/a").method.is_none());
    }
}
