//! 데몬이 여는 Streamable HTTP MCP 서버. 주소 하나(`/mcp`)를 한 번만 연결하면 된다.
//!
//! 세션이 쓰는 인덱스는 이 순서로 정한다:
//! 1. 세션 안에서 `use_index`로 바꾼 인덱스 (`Mcp-Session-Id`별로 기억)
//! 2. 주소에 붙인 `?index=<이름>` (프로젝트마다 고정하고 싶을 때)
//! 3. 앱에서 정한 기본 인덱스

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::core::store::{self, Source, Store};
use crate::core::{cache, content, web};
use crate::i18n::t;
use crate::tr;

const SUPPORTED_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
pub const SERVER_NAME: &str = "octo";
/// 이름을 바꾸기 전 MCP 등록 이름. 연결할 때 남아 있으면 지운다.
pub const LEGACY_SERVER_NAME: &str = "octopuser";

pub fn url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

/// Claude Code 외 도구의 MCP 설정에 붙여 넣을 JSON
pub fn config_snippet(port: u16) -> String {
    let config = json!({ "mcpServers": { SERVER_NAME: { "type": "http", "url": url(port) } } });
    serde_json::to_string_pretty(&config).unwrap_or_default()
}

#[derive(Default)]
struct Session {
    /// `use_index`로 고른 인덱스
    index: Option<String>,
    /// initialize의 clientInfo.name. 에이전트가 보고한 접근 방식에 함께 남긴다.
    client: Option<String>,
}

/// 세션 id → 세션
type Sessions = Arc<Mutex<HashMap<String, Session>>>;

/// 백그라운드 스레드에서 서버를 띄운다. 포트를 못 잡으면 사유를 돌려준다.
pub fn spawn(store: Store, port: u16) -> Result<(), String> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let server = Server::http(addr).map_err(|err| tr!("MCP 포트 {port} 사용 불가: {err}", "MCP port {port} unavailable: {err}"))?;
    let sessions = Sessions::default();

    thread::spawn(move || {
        for request in server.incoming_requests() {
            let store = store.clone();
            let sessions = sessions.clone();
            // 그래프 쿼리가 오래 걸려도 다른 세션을 막지 않게 요청마다 스레드
            thread::spawn(move || handle(&store, &sessions, request));
        }
    });
    Ok(())
}

fn handle(store: &Store, sessions: &Sessions, mut request: Request) {
    let (path, query) = request.url().split_once('?').unwrap_or((request.url(), ""));
    if path != "/mcp" {
        return respond(request, 404, "");
    }
    // DNS rebinding 방지: 브라우저발 요청은 로컬 Origin만 허용
    if let Some(origin) = header(&request, "Origin") {
        let local = ["http://127.0.0.1", "http://localhost"]
            .iter()
            .any(|allowed| origin == *allowed || origin.starts_with(&format!("{allowed}:")));
        if !local {
            return respond(request, 403, "");
        }
    }

    let session = header(&request, "Mcp-Session-Id");
    // 세션 종료 통지
    if *request.method() == Method::Delete {
        if let Some(session) = &session {
            sessions.lock().unwrap().remove(session);
        }
        return respond(request, 200, "");
    }
    if *request.method() != Method::Post {
        let response = Response::from_string("")
            .with_status_code(405)
            .with_header(Header::from_bytes("Allow", "POST, DELETE").expect("static header"));
        let _ = request.respond(response);
        return;
    }

    let pinned = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == "index")
        .map(|(_, value)| value.to_owned());

    let mut body = String::new();
    if request.as_reader().read_to_string(&mut body).is_err() {
        return respond(request, 400, "");
    }
    let Ok(message) = serde_json::from_str::<Value>(&body) else {
        return respond_json(request, &error(Value::Null, -32700, "parse error"), None);
    };

    // 알림(id 없음)이나 클라이언트의 응답은 받기만 한다
    let Some(id) = message.get("id").cloned() else {
        return respond(request, 202, "");
    };
    let Some(method) = message["method"].as_str() else {
        return respond(request, 202, "");
    };

    let mut ctx = Ctx { store, sessions, session, pinned };
    let (reply, new_session) = match method {
        "initialize" => {
            let session = store::new_id();
            let client = message["params"]["clientInfo"]["name"].as_str().map(str::to_owned);
            ctx.sessions.lock().unwrap().insert(session.clone(), Session { index: None, client });
            ctx.session = Some(session.clone());
            (success(id, initialize(&ctx, &message["params"])), Some(session))
        }
        "ping" => (success(id, json!({})), None),
        "tools/list" => (success(id, json!({ "tools": tools() })), None),
        "tools/call" => (success(id, call(&ctx, &message["params"])), None),
        _ => (error(id, -32601, &format!("method not found: {method}")), None),
    };
    respond_json(request, &reply, new_session.as_deref());
}

/// 요청 하나를 처리하는 데 필요한 것
struct Ctx<'a> {
    store: &'a Store,
    sessions: &'a Sessions,
    session: Option<String>,
    pinned: Option<String>,
}

impl Ctx<'_> {
    /// 지금 이 세션이 쓰는 인덱스
    fn index(&self) -> Option<String> {
        // use_index로 고른 인덱스가 이름이 바뀌었거나 지워졌으면 다음 순위로 넘어간다
        let chosen = self
            .session
            .as_ref()
            .and_then(|s| self.sessions.lock().unwrap().get(s).and_then(|s| s.index.clone()))
            .filter(|name| self.store.index(name).is_some());
        chosen.or_else(|| self.pinned.clone()).or_else(|| self.store.default_index())
    }

    fn client(&self) -> String {
        self.session
            .as_ref()
            .and_then(|s| self.sessions.lock().unwrap().get(s).and_then(|s| s.client.clone()))
            .unwrap_or_else(|| "unknown".into())
    }
}

fn initialize(ctx: &Ctx, params: &Value) -> Value {
    let requested = params["protocolVersion"].as_str().unwrap_or_default();
    let version = SUPPORTED_VERSIONS
        .into_iter()
        .find(|v| *v == requested)
        .unwrap_or(SUPPORTED_VERSIONS[0]);

    let instructions = match ctx.index() {
        Some(index) => tr!(
            "Octo 컨텍스트 인덱스 '{index}'가 연결되어 있습니다. \
             작업 전에 get_index로 목차를 보고, 필요한 항목만 load_context로 본문을 가져오세요. \
             사용자가 다른 인덱스를 원하면 list_indexes로 확인하고 use_index로 바꾸세요. \
             octo가 열지 못한 링크를 다른 도구로 읽었다면 report_access로 방식과 본문을 남기세요.",
            "The Octo context index '{index}' is connected. \
             Before working, call get_index to see the table of contents, then load only the items you need with load_context. \
             If the user wants a different index, check list_indexes and switch with use_index. \
             If you read a link octo couldn't open with another tool, leave the method and content with report_access."
        ),
        None => t(
            "Octo에 아직 인덱스가 없습니다. 사용자에게 앱에서 인덱스를 만들어 달라고 안내하세요.",
            "Octo has no index yet. Ask the user to create one in the app.",
        )
        .into(),
    };

    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
        "instructions": instructions,
    })
}

fn tools() -> Value {
    json!([
        {
            "name": "get_index",
            "description": t(
                "이 세션이 쓰는 컨텍스트 인덱스의 목차를 가져온다. 항목마다 id, 종류, 제목, 한 줄 요약이 있다.",
                "Get the table of contents of this session's context index. Each item has an id, kind, title, and one-line summary.",
            ),
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "load_context",
            "description": t(
                "목차의 id로 컨텍스트 본문을 가져온다. 문서는 본문, 경로는 파일 내용이나 폴더 목록, 그래프는 쿼리 결과.",
                "Load a context's content by its table-of-contents id. Doc: the text. Path: file content, folder listing, or page text for links. Graph: query result.",
            ),
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string", "description": t("get_index 목차의 id", "id from the get_index table of contents") } },
                "required": ["id"],
            },
        },
        {
            "name": "list_indexes",
            "description": t(
                "쓸 수 있는 인덱스 목록. 지금 세션이 쓰는 인덱스와 기본 인덱스를 표시한다.",
                "List available indexes, marking the one this session uses and the default.",
            ),
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "use_index",
            "description": t(
                "이 세션이 쓸 인덱스를 바꾼다. 다른 세션에는 영향이 없다.",
                "Switch the index this session uses. Other sessions are not affected.",
            ),
            "inputSchema": {
                "type": "object",
                "properties": { "name": { "type": "string", "description": t("list_indexes의 인덱스 이름", "index name from list_indexes") } },
                "required": ["name"],
            },
        },
        {
            "name": "report_access",
            "description": t(
                "octo가 열지 못한 링크 컨텍스트를 다른 도구로 읽었을 때, 연 방식과 본문을 남긴다. 다음 세션부터 본문은 1일 동안 캐시로, 방식은 힌트로 제공된다.",
                "After reading a link context octo couldn't open with another tool, leave how you opened it and its content. Later sessions get the content from cache for 1 day and the method as a hint.",
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": t("get_index 목차의 링크 컨텍스트 id", "id of a link context from get_index") },
                    "method": { "type": "string", "description": t("연 방식. 다른 에이전트가 따라 할 수 있게 도구 이름과 인자까지", "How you opened it, with the tool name and arguments so another agent can repeat it") },
                    "content": { "type": "string", "description": t("읽은 본문 텍스트. 없으면 방식만 남긴다", "The text you read. Omit to leave only the method") },
                    "version": { "type": "string", "description": t("원본이 알려준 버전이나 수정 시각. 나중에 최신인지 비교하는 데 쓴다", "Version or modified time the source reported, used later to check freshness") },
                },
                "required": ["id", "method"],
            },
        },
    ])
}

fn call(ctx: &Ctx, params: &Value) -> Value {
    let no_index = || {
        t(
            "Octo에 인덱스가 없습니다. 앱에서 인덱스를 먼저 만들어 주세요",
            "Octo has no index. Create one in the app first",
        )
        .to_owned()
    };
    let result = match params["name"].as_str() {
        Some("get_index") => ctx.index().ok_or_else(no_index).and_then(|index| content::toc(ctx.store, &index)),
        Some("load_context") => match params["arguments"]["id"].as_str() {
            Some(id) => ctx
                .index()
                .ok_or_else(no_index)
                .and_then(|index| content::load_in_index(ctx.store, &index, id)),
            None => Err(t("id 인자가 필요합니다", "The id argument is required").into()),
        },
        Some("list_indexes") => Ok(list_indexes(ctx)),
        Some("report_access") => ctx.index().ok_or_else(no_index).and_then(|index| report_access(ctx, &index, &params["arguments"])),
        Some("use_index") => use_index(ctx, params["arguments"]["name"].as_str().unwrap_or_default()),
        other => Err(tr!("알 수 없는 도구: {}", "Unknown tool: {}", other.unwrap_or(""))),
    };
    tool_result(result)
}

fn list_indexes(ctx: &Ctx) -> String {
    let current = ctx.index();
    let default = ctx.store.default_index();
    let indexes = ctx.store.indexes();
    if indexes.is_empty() {
        return t("(인덱스 없음)", "(no indexes)").into();
    }
    indexes
        .iter()
        .map(|index| {
            let mut marks = Vec::new();
            if current.as_deref() == Some(index.name.as_str()) {
                marks.push(t("현재 세션", "current session"));
            }
            if default.as_deref() == Some(index.name.as_str()) {
                marks.push(t("기본", "default"));
            }
            let marks = if marks.is_empty() { String::new() } else { format!(" [{}]", marks.join(", ")) };
            tr!("- {} — 컨텍스트 {}개{marks}", "- {} — {} contexts{marks}", index.name, index.contexts.len())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn use_index(ctx: &Ctx, name: &str) -> Result<String, String> {
    if ctx.store.index(name).is_none() {
        return Err(tr!("인덱스 없음: '{name}'. list_indexes로 이름을 확인하세요", "No index '{name}'. Check the name with list_indexes"));
    }
    let Some(session) = &ctx.session else {
        return Err(t(
            "세션 id가 없어 바꿀 수 없습니다 (Mcp-Session-Id 헤더 필요)",
            "Can't switch without a session id (Mcp-Session-Id header required)",
        )
        .into());
    };
    ctx.sessions.lock().unwrap().entry(session.clone()).or_default().index = Some(name.to_owned());
    content::toc(ctx.store, name).map(|toc| tr!("이 세션은 이제 '{name}' 인덱스를 씁니다.\n\n{toc}", "This session now uses the '{name}' index.\n\n{toc}"))
}

fn report_access(ctx: &Ctx, index_name: &str, args: &Value) -> Result<String, String> {
    let id = args["id"].as_str().ok_or(t("id 인자가 필요합니다", "The id argument is required"))?;
    let method = args["method"].as_str().map(str::trim).filter(|m| !m.is_empty());
    let method = method.ok_or(t("method 인자가 필요합니다", "The method argument is required"))?;
    let index = ctx.store.index(index_name).ok_or_else(|| tr!("인덱스 없음: '{index_name}'", "No index '{index_name}'"))?;
    if !index.contexts.iter().any(|c| c == id) {
        return Err(tr!("'{index_name}' 인덱스에 없는 컨텍스트: {id}", "Context not in index '{index_name}': {id}"));
    }
    let context = ctx.store.context(id).ok_or_else(|| tr!("컨텍스트 없음: {id}", "No context: {id}"))?;
    let url = match &context.source {
        Source::Path { path } if web::is_url(path) => path.clone(),
        _ => return Err(t("링크(URL) 컨텍스트에만 남길 수 있습니다", "Only link (URL) contexts can be reported").into()),
    };

    let content = args["content"].as_str();
    cache::report(ctx.store, &url, &ctx.client(), method, content, args["version"].as_str())
        .map_err(|err| tr!("저장하지 못했습니다: {err}", "Couldn't save: {err}"))?;
    Ok(if content.is_some_and(|c| !c.trim().is_empty()) {
        t("방식과 본문을 남겼습니다. 본문은 1일 동안 캐시로 제공됩니다.", "Saved the method and content. The content is served from cache for 1 day.")
    } else {
        t("방식을 남겼습니다.", "Saved the method.")
    }
    .into())
}

fn tool_result(result: Result<String, String>) -> Value {
    let (text, is_error) = match result {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn header(request: &Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_owned())
}

fn respond(request: Request, status: u16, body: &str) {
    let _ = request.respond(Response::from_string(body).with_status_code(status));
}

fn respond_json(request: Request, value: &Value, session: Option<&str>) {
    let mut response = Response::from_string(value.to_string())
        .with_header(Header::from_bytes("Content-Type", "application/json").expect("static header"));
    if let Some(session) = session {
        response = response.with_header(Header::from_bytes("Mcp-Session-Id", session).expect("session header"));
    }
    let _ = request.respond(response);
}
