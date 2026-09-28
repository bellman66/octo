//! 데몬이 여는 Streamable HTTP MCP 서버. 주소 하나(`/mcp`)를 한 번만 연결하면 된다.
//!
//! 세션이 쓰는 인덱스는 이 순서로 정한다:
//! 1. 세션 안에서 `use_index`로 바꾼 인덱스 (`Mcp-Session-Id`별로 기억)
//! 2. 주소에 붙인 `?index=<이름>` (프로젝트마다 고정하고 싶을 때)
//! 3. 앱에서 정한 기본 인덱스

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use futures::channel::mpsc;
use futures::Stream;
use iced::Subscription;
use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::core::store::{self, Context, Source, Store};
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

static CHANGED: Mutex<Option<mpsc::UnboundedSender<()>>> = Mutex::new(None);

/// 에이전트가 컨텍스트나 인덱스를 바꿀 때마다 이벤트를 낸다. 열린 창이 목록을 다시 읽는 데 쓴다.
pub fn changes() -> Subscription<()> {
    Subscription::run(change_events)
}

fn change_events() -> impl Stream<Item = ()> {
    let (tx, rx) = mpsc::unbounded();
    *CHANGED.lock().unwrap() = Some(tx);
    rx
}

fn notify_changed() {
    if let Some(tx) = CHANGED.lock().ok().and_then(|tx| tx.clone()) {
        let _ = tx.unbounded_send(());
    }
}

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
             octo가 열지 못한 링크를 다른 도구로 읽었다면 report_access로 방식과 본문을 남기세요. \
             작업 중 다음 세션도 알아야 할 내용을 알게 되면 add_context로 남기고, 직접 남긴 항목은 update_context로 고치세요.",
            "The Octo context index '{index}' is connected. \
             Before working, call get_index to see the table of contents, then load only the items you need with load_context. \
             If the user wants a different index, check list_indexes and switch with use_index. \
             If you read a link octo couldn't open with another tool, leave the method and content with report_access. \
             When you learn something later sessions should know, save it with add_context, and fix items you saved with update_context."
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
        {
            "name": "add_context",
            "description": t(
                "다음 세션도 알아야 할 내용을 이 세션의 인덱스에 새 컨텍스트로 남긴다. \
                 먼저 get_index로 비슷한 항목이 있는지 보고, 있으면 update_context를 쓴다. \
                 이미 파일이나 링크로 있는 내용은 복사하지 말고 path로 가리킨다. \
                 body는 결정 사항, 조사 결과처럼 어디에도 없는 내용에만 쓴다. body와 path 중 하나만 준다.",
                "Save something later sessions should know as a new context in this session's index. \
                 Check get_index for a similar item first and use update_context if there is one. \
                 Don't copy content that already lives in a file or link; point to it with path. \
                 Use body only for things that exist nowhere else, such as decisions or findings. Give exactly one of body or path.",
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": t("목차에 보일 제목", "Title shown in the table of contents") },
                    "summary": { "type": "string", "description": t("목차에 보일 한 줄 요약. 없으면 본문 첫 줄", "One-line summary for the table of contents. Defaults to the first line") },
                    "body": { "type": "string", "description": t("문서 본문 (Markdown)", "Document text (Markdown)") },
                    "path": { "type": "string", "description": t("원본 파일·폴더의 절대 경로나 링크(URL)", "Absolute path to the source file or folder, or a link (URL)") },
                },
                "required": ["title"],
            },
        },
        {
            "name": "update_context",
            "description": t(
                "에이전트가 add_context로 남긴 컨텍스트를 고친다. 사람이 만들거나 고친 항목은 바꿀 수 없다. \
                 준 값만 바뀐다. body는 문서, path는 경로 컨텍스트에만 줄 수 있다.",
                "Edit a context an agent saved with add_context. Items a person created or edited can't be changed. \
                 Only the fields you pass change. body applies to doc contexts, path to path contexts.",
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": t("get_index 목차의 id", "id from the get_index table of contents") },
                    "title": { "type": "string", "description": t("새 제목", "New title") },
                    "summary": { "type": "string", "description": t("새 한 줄 요약. 빈 문자열이면 본문 첫 줄로 돌아간다", "New one-line summary. An empty string falls back to the first line") },
                    "body": { "type": "string", "description": t("새 문서 본문 전체 (Markdown)", "Full new document text (Markdown)") },
                    "path": { "type": "string", "description": t("새 절대 경로나 링크(URL)", "New absolute path or link (URL)") },
                },
                "required": ["id"],
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
        Some("add_context") => ctx.index().ok_or_else(no_index).and_then(|index| add_context(ctx, &index, &params["arguments"])),
        Some("update_context") => ctx.index().ok_or_else(no_index).and_then(|index| update_context(ctx, &index, &params["arguments"])),
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

fn add_context(ctx: &Ctx, index_name: &str, args: &Value) -> Result<String, String> {
    let title = text_arg(args, "title").ok_or(t("title 인자가 필요합니다", "The title argument is required"))?;
    let body = args["body"].as_str().filter(|b| !b.trim().is_empty());
    let source = match (body, text_arg(args, "path")) {
        (Some(_), None) => Source::Document,
        (None, Some(path)) => Source::Path { path: checked_path(path)? },
        _ => return Err(t("body와 path 중 하나만 주세요", "Give exactly one of body or path").into()),
    };
    let mut index = ctx.store.index(index_name).ok_or_else(|| tr!("인덱스 없음: '{index_name}'", "No index '{index_name}'"))?;
    if let Some(existing) = same_title(ctx.store, &index.contexts, &title, None) {
        return Err(duplicate(index_name, &existing));
    }

    let context = Context {
        id: store::new_id(),
        title,
        summary: text_arg(args, "summary").unwrap_or_default(),
        source,
        author: Some(ctx.client()),
    };
    let saved = ctx.store.save_context(&context).and_then(|()| match body {
        Some(body) => ctx.store.save_document(&context.id, body),
        None => Ok(()),
    });
    saved.map_err(|err| tr!("저장하지 못했습니다: {err}", "Couldn't save: {err}"))?;
    index.contexts.push(context.id.clone());
    ctx.store.save_index(&index).map_err(|err| tr!("인덱스에 넣지 못했습니다: {err}", "Couldn't add it to the index: {err}"))?;

    notify_changed();
    Ok(tr!("'{index_name}' 인덱스에 추가했습니다. id: {}", "Added to the '{index_name}' index. id: {}", context.id))
}

fn update_context(ctx: &Ctx, index_name: &str, args: &Value) -> Result<String, String> {
    let id = args["id"].as_str().ok_or(t("id 인자가 필요합니다", "The id argument is required"))?;
    let index = ctx.store.index(index_name).ok_or_else(|| tr!("인덱스 없음: '{index_name}'", "No index '{index_name}'"))?;
    if !index.contexts.iter().any(|c| c == id) {
        return Err(tr!("'{index_name}' 인덱스에 없는 컨텍스트: {id}", "Context not in index '{index_name}': {id}"));
    }
    let mut context = ctx.store.context(id).ok_or_else(|| tr!("컨텍스트 없음: {id}", "No context: {id}"))?;
    if context.author.is_none() {
        return Err(t(
            "사람이 만들거나 고친 컨텍스트는 고칠 수 없습니다. 바꿀 내용을 사용자에게 알려 주세요",
            "Contexts a person created or edited can't be changed. Tell the user what should change",
        )
        .into());
    }

    let title = text_arg(args, "title");
    let summary = args["summary"].as_str().map(|s| s.trim().to_owned());
    let body = args["body"].as_str().filter(|b| !b.trim().is_empty());
    let path = text_arg(args, "path").map(checked_path).transpose()?;
    if title.is_none() && summary.is_none() && body.is_none() && path.is_none() {
        return Err(t("바꿀 값이 없습니다 (title, summary, body, path 중 하나 이상)", "Nothing to change (pass title, summary, body, or path)").into());
    }
    if let Some(title) = &title
        && let Some(existing) = same_title(ctx.store, &index.contexts, title, Some(id))
    {
        return Err(duplicate(index_name, &existing));
    }

    let mut old_url = None;
    match (&mut context.source, body, path) {
        (Source::Document, _, None) | (Source::Path { .. }, None, None) => {}
        (Source::Path { path: current }, None, Some(path)) => {
            if web::is_url(current) {
                old_url = Some(current.clone());
            }
            *current = path;
        }
        (Source::Document, _, Some(_)) => return Err(t("문서 컨텍스트에는 path를 줄 수 없습니다", "Doc contexts don't take a path").into()),
        _ => return Err(t("body는 문서 컨텍스트에만 줄 수 있습니다", "body only applies to doc contexts").into()),
    }
    if let Some(title) = title {
        context.title = title;
    }
    if let Some(summary) = summary {
        context.summary = summary;
    }
    context.author = Some(ctx.client());

    let saved = ctx.store.save_context(&context).and_then(|()| match body {
        Some(body) => ctx.store.save_document(&context.id, body),
        None => Ok(()),
    });
    saved.map_err(|err| tr!("저장하지 못했습니다: {err}", "Couldn't save: {err}"))?;
    // 링크를 바꿨으면 옛 링크의 조회 결과는 더 쓸 데가 없다
    if let Some(old) = old_url {
        cache::forget_if_unused(ctx.store, &old);
    }

    notify_changed();
    Ok(tr!("고쳤습니다: {} (id: {id})", "Updated: {} (id: {id})", context.title))
}

fn text_arg(args: &Value, name: &str) -> Option<String> {
    args[name].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

/// 링크이거나 실제로 있는 절대 경로만 받는다. 없는 경로는 목차에 바로 "연결 끊김"으로 뜬다.
fn checked_path(path: String) -> Result<String, String> {
    if web::is_url(&path) || (Path::new(&path).is_absolute() && Path::new(&path).exists()) {
        Ok(path)
    } else {
        Err(tr!("있는 절대 경로나 링크(URL)가 필요합니다: {path}", "Needs an existing absolute path or a link (URL): {path}"))
    }
}

/// 인덱스 안에서 제목이 같은(대소문자 무시) 다른 컨텍스트
fn same_title(store: &Store, ids: &[String], title: &str, except: Option<&str>) -> Option<Context> {
    ids.iter()
        .filter(|id| Some(id.as_str()) != except)
        .filter_map(|id| store.context(id))
        .find(|c| c.title.trim().eq_ignore_ascii_case(title.trim()))
}

fn duplicate(index_name: &str, existing: &Context) -> String {
    if existing.author.is_some() {
        tr!(
            "'{index_name}' 인덱스에 같은 제목이 있습니다 (id: {}). update_context로 고치세요",
            "The '{index_name}' index already has this title (id: {}). Use update_context instead",
            existing.id
        )
    } else {
        tr!(
            "'{index_name}' 인덱스에 사람이 만든 같은 제목의 항목이 있습니다 (id: {}). 바꿀 내용은 사용자에게 알려 주세요",
            "The '{index_name}' index already has a person-made item with this title (id: {}). Tell the user what should change",
            existing.id
        )
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::store::Index;

    fn setup(name: &str) -> (Store, Sessions) {
        let root = std::env::temp_dir().join(format!("octo-mcp-test-{name}-{}", store::new_id()));
        let store = Store::at(root).unwrap();
        store.save_index(&Index { name: "x".into(), contexts: vec![] }).unwrap();
        (store, Sessions::default())
    }

    fn ctx<'a>(store: &'a Store, sessions: &'a Sessions) -> Ctx<'a> {
        Ctx { store, sessions, session: None, pinned: Some("x".into()) }
    }

    #[test]
    fn added_context_joins_the_index_as_agent_authored() {
        let (store, sessions) = setup("add");
        let ctx = ctx(&store, &sessions);
        let reply = add_context(&ctx, "x", &json!({ "title": "결제 정책", "body": "환불은 7일" })).unwrap();

        let index = store.index("x").unwrap();
        assert_eq!(index.contexts.len(), 1);
        let context = store.context(&index.contexts[0]).unwrap();
        assert!(reply.contains(&context.id));
        assert!(context.author.is_some());
        assert_eq!(store.document(&context.id), "환불은 7일");
        // 같은 제목은 새로 만들지 않는다
        assert!(add_context(&ctx, "x", &json!({ "title": " 결제 정책 ", "body": "다른 내용" })).is_err());
    }

    #[test]
    fn add_needs_exactly_one_existing_source() {
        let (store, sessions) = setup("source");
        let ctx = ctx(&store, &sessions);
        let dir = std::env::temp_dir().to_string_lossy().into_owned();
        assert!(add_context(&ctx, "x", &json!({ "title": "a" })).is_err());
        assert!(add_context(&ctx, "x", &json!({ "title": "a", "body": "b", "path": dir })).is_err());
        assert!(add_context(&ctx, "x", &json!({ "title": "a", "path": "/definitely/not/here" })).is_err());
        assert!(add_context(&ctx, "x", &json!({ "title": "a", "path": "relative/path" })).is_err());
        assert!(add_context(&ctx, "x", &json!({ "title": "a", "path": dir })).is_ok());
    }

    #[test]
    fn tools_are_listed_and_dispatched() {
        let (store, sessions) = setup("dispatch");
        let ctx = ctx(&store, &sessions);
        let names: Vec<_> = tools().as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect();
        assert!(names.contains(&"add_context".into()) && names.contains(&"update_context".into()));

        let reply = call(&ctx, &json!({ "name": "add_context", "arguments": { "title": "t", "body": "b" } }));
        assert_eq!(reply["isError"], false);
        let toc = call(&ctx, &json!({ "name": "get_index" }));
        assert!(toc["content"][0]["text"].as_str().unwrap().contains("unknown"));
    }

    #[test]
    fn update_only_touches_agent_contexts() {
        let (store, sessions) = setup("update");
        let ctx = ctx(&store, &sessions);
        let human = Context { id: "h".into(), title: "h".into(), summary: String::new(), source: Source::Document, author: None };
        store.save_context(&human).unwrap();
        store.save_index(&Index { name: "x".into(), contexts: vec!["h".into()] }).unwrap();
        assert!(update_context(&ctx, "x", &json!({ "id": "h", "body": "x" })).is_err());

        add_context(&ctx, "x", &json!({ "title": "note", "body": "v1" })).unwrap();
        let id = store.index("x").unwrap().contexts[1].clone();
        assert!(update_context(&ctx, "x", &json!({ "id": id })).is_err());
        assert!(update_context(&ctx, "x", &json!({ "id": id, "path": std::env::temp_dir() })).is_err());
        // 사람이 만든 항목과 같은 제목으로는 못 바꾼다
        assert!(update_context(&ctx, "x", &json!({ "id": id, "title": "H" })).is_err());
        update_context(&ctx, "x", &json!({ "id": id, "body": "v2", "summary": "요약" })).unwrap();
        assert_eq!(store.document(&id), "v2");
        assert_eq!(store.context(&id).unwrap().summary, "요약");
    }
}
