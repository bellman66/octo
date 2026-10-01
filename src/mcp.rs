//! 데몬이 여는 Streamable HTTP MCP 서버. 주소 하나(`/mcp`)를 한 번만 연결하면 된다.
//!
//! 인덱스는 세션에 고정되지 않는다. 에이전트가 사용자 메시지마다 주제에 맞는 인덱스를 골라
//! `get_index(index)`로 목차를 열고, `load_context(id)`로 어느 인덱스의 항목이든 연다. 기본 인덱스는 없다.
//!
//! 고르는 데 쓰는 힌트는 목록 맨 앞에 둔다:
//! 1. 주소에 붙인 `?index=<이름>` (roots를 모르는 클라이언트용)
//! 2. 작업 폴더에서 최근 연 인덱스. 폴더는 클라이언트가 `roots`를 지원하면 첫 도구 호출 때
//!    `roots/list`로 묻는다. 그 응답을 SSE로 흘려 보내는 동안 요청 안에서 물어볼 수 있다.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::io::Write;
use std::sync::{Arc, LazyLock, Mutex, mpsc as sync_mpsc};
use std::thread;
use std::time::Duration;

use futures::channel::mpsc;
use futures::Stream;
use iced::Subscription;
use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::core::store::{self, Context, Source, Store};
use crate::core::transfer::{self, ExportOptions, Scope};
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
    /// initialize의 clientInfo.name. 에이전트가 보고한 접근 방식에 함께 남긴다.
    client: Option<String>,
    /// initialize에서 클라이언트가 `roots`를 지원한다고 했는지
    roots: bool,
    /// `roots/list`로 받은 작업 폴더. None이면 아직 묻지 않았다 (물었는데 실패하면 빈 목록).
    folders: Option<Vec<String>>,
}

/// 세션 id → 세션
type Sessions = Arc<Mutex<HashMap<String, Session>>>;

static CHANGED: Mutex<Option<mpsc::UnboundedSender<()>>> = Mutex::new(None);

/// 서버가 클라이언트에 보낸 요청 id → 그 응답을 기다리는 요청 스레드
static PENDING: LazyLock<Mutex<HashMap<String, sync_mpsc::Sender<Value>>>> = LazyLock::new(Mutex::default);

/// `roots/list` 응답을 기다리는 시간. 넘기면 폴더 없이 진행한다.
const ROOTS_TIMEOUT: Duration = Duration::from_secs(5);

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

    // 알림(id 없음)은 받기만 한다. 작업 폴더가 바뀌었다는 알림이면 다음 도구 호출 때 다시 묻는다.
    let Some(id) = message.get("id").cloned() else {
        if message["method"] == "notifications/roots/list_changed"
            && let Some(session) = sessions.lock().unwrap().get_mut(session.as_deref().unwrap_or_default())
        {
            session.folders = None;
        }
        return respond(request, 202, "");
    };
    // method가 없으면 서버가 보낸 요청에 대한 클라이언트의 응답이다
    let Some(method) = message["method"].as_str() else {
        let waiting = id.as_str().and_then(|id| PENDING.lock().unwrap().remove(id));
        if let Some(waiting) = waiting {
            let _ = waiting.send(message);
        }
        return respond(request, 202, "");
    };

    let mut ctx = Ctx { store, sessions, session, pinned };
    let (reply, new_session) = match method {
        "initialize" => {
            let session = store::new_id();
            let client = message["params"]["clientInfo"]["name"].as_str().map(str::to_owned);
            let roots = message["params"]["capabilities"]["roots"].is_object();
            ctx.sessions.lock().unwrap().insert(session.clone(), Session { client, roots, ..Session::default() });
            ctx.session = Some(session.clone());
            (success(id, initialize(&ctx, &message["params"])), Some(session))
        }
        "ping" => (success(id, json!({})), None),
        "tools/list" => (success(id, json!({ "tools": tools() })), None),
        "tools/call" if ctx.should_ask_folders() && accepts_stream(&request) => {
            return call_asking_folders(&ctx, request, id, &message["params"]);
        }
        "tools/call" => (success(id, call(&ctx, &message["params"])), None),
        _ => (error(id, -32601, &format!("method not found: {method}")), None),
    };
    respond_json(request, &reply, new_session.as_deref());
}

/// 응답을 SSE로 열고, 먼저 클라이언트에 `roots/list`를 물어 세션의 작업 폴더를 채운 뒤 도구를 실행한다.
fn call_asking_folders(ctx: &Ctx, request: Request, id: Value, params: &Value) {
    let mut stream = request.into_writer();
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nTransfer-Encoding: chunked\r\n\r\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let folders = ask_folders(&mut stream);
    if let Some(session) = ctx.sessions.lock().unwrap().get_mut(ctx.session.as_deref().unwrap_or_default()) {
        session.folders = Some(folders);
    }
    let reply = success(id, call(ctx, params));
    let _ = send_event(&mut stream, &reply).and_then(|()| stream.write_all(b"0\r\n\r\n")).and_then(|()| stream.flush());
}

/// 열린 SSE 응답으로 `roots/list`를 보내고 응답을 기다린다. 못 받으면 빈 목록.
fn ask_folders(stream: &mut impl Write) -> Vec<String> {
    let request_id = format!("octo-roots-{}", store::new_id());
    let (tx, rx) = sync_mpsc::channel();
    PENDING.lock().unwrap().insert(request_id.clone(), tx);
    let asked = send_event(stream, &json!({ "jsonrpc": "2.0", "id": request_id, "method": "roots/list" }));
    let answer = asked.ok().and_then(|()| rx.recv_timeout(ROOTS_TIMEOUT).ok());
    PENDING.lock().unwrap().remove(&request_id);

    let roots = answer.as_ref().and_then(|answer| answer["result"]["roots"].as_array()).into_iter().flatten();
    roots.filter_map(|root| root["uri"].as_str()).filter_map(file_uri_path).collect()
}

/// SSE 이벤트 하나를 청크 하나로 보내고 바로 내보낸다.
fn send_event(stream: &mut impl Write, message: &Value) -> std::io::Result<()> {
    let event = format!("event: message\ndata: {message}\n\n");
    write!(stream, "{:x}\r\n{event}\r\n", event.len())?;
    stream.flush()
}

/// `file:///Users/me/my%20app` → `/Users/me/my app`. file URI가 아니면 None.
fn file_uri_path(uri: &str) -> Option<String> {
    let path = uri.strip_prefix("file://")?;
    let path = path.strip_prefix("localhost").unwrap_or(path);
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                decoded.push(byte);
                i += 3;
            }
            (byte, _) => {
                decoded.push(byte);
                i += 1;
            }
        }
    }
    let path = String::from_utf8(decoded).ok()?;
    let path = path.trim_end_matches('/');
    path.starts_with('/').then(|| path.to_owned())
}

/// 클라이언트가 SSE 응답을 받겠다고 했는지 (Streamable HTTP의 Accept)
fn accepts_stream(request: &Request) -> bool {
    header(request, "Accept").is_some_and(|accept| accept.contains("text/event-stream"))
}

/// 요청 하나를 처리하는 데 필요한 것
struct Ctx<'a> {
    store: &'a Store,
    sessions: &'a Sessions,
    session: Option<String>,
    /// 주소의 `?index=`. 고정이 아니라 목록 맨 앞에 두는 힌트다.
    pinned: Option<String>,
}

impl Ctx<'_> {
    /// 인덱스 목록 맨 앞에 둘 힌트: 주소의 `?index=`, 그다음 작업 폴더에서 최근 연 것
    fn hints(&self) -> Vec<String> {
        let mut hints: Vec<String> = self.pinned.iter().filter(|name| self.store.index(name).is_some()).cloned().collect();
        for name in self.store.folder_hints(&self.folders()) {
            if !hints.contains(&name) {
                hints.push(name);
            }
        }
        hints
    }

    /// 작업 폴더에서 이 인덱스를 열었다고 기억한다. 다음 세션의 목록 정렬에 쓴다.
    fn remember_opened(&self, name: &str) {
        if let Some(folder) = self.folders().into_iter().next() {
            let _ = self.store.remember_folder_index(&folder, name);
        }
    }

    /// 인덱스 이름이 필요한 도구에 이름이 없을 때: 고를 수 있는 인덱스를 함께 보여준다
    fn needs_index(&self) -> String {
        if self.store.indexes().is_empty() {
            return t(
                "Octo에 인덱스가 없습니다. 사용자가 원하면 create_index로 만들고 add_context로 채우세요.",
                "Octo has no index yet. If the user wants one, create it with create_index and fill it with add_context.",
            )
            .into();
        }
        tr!(
            "index 인자가 필요합니다. 아래에서 메시지 주제에 맞는 인덱스를 고르세요. 어느 것인지 애매하면 사용자에게 물어보세요.\n\n{}",
            "The index argument is required. Pick the one that fits the message from below. If it's unclear which, ask the user.\n\n{}",
            list_indexes(self)
        )
    }

    fn with_session<T>(&self, read: impl FnOnce(&Session) -> T) -> Option<T> {
        let session = self.session.as_ref()?;
        self.sessions.lock().unwrap().get(session).map(read)
    }

    /// `roots/list`로 받은 이 세션의 작업 폴더 (모르면 빈 목록)
    fn folders(&self) -> Vec<String> {
        self.with_session(|s| s.folders.clone()).flatten().unwrap_or_default()
    }

    /// 클라이언트가 roots를 지원하는데 아직 작업 폴더를 묻지 않았는지
    fn should_ask_folders(&self) -> bool {
        self.with_session(|s| s.roots && s.folders.is_none()).unwrap_or(false)
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

    let instructions = if ctx.store.indexes().is_empty() {
        t(
            "Octo에 아직 인덱스가 없습니다. 사용자가 원하면 create_index로 만들고 add_context로 채우세요.",
            "Octo has no index yet. If the user wants one, create it with create_index and fill it with add_context.",
        )
        .to_owned()
    } else {
        // 작업 폴더는 첫 도구 호출 때 알게 되므로, 이 프로젝트에서 최근 쓴 인덱스 표시는 list_indexes에서 보인다
        tr!(
            "Octo 컨텍스트 인덱스는 세션에 고정되지 않습니다. 사용자 메시지마다 주제에 맞는 인덱스를 아래에서 골라 \
             get_index(index)로 목차를 보고, 필요한 항목만 load_context(id)로 본문을 가져오세요. \
             주제가 여러 인덱스에 걸치면 여러 개를 열어도 됩니다. 어느 인덱스인지 애매하면 사용자에게 물어보세요. \
             list_indexes는 이 프로젝트에서 최근 쓴 인덱스를 맨 앞에 보여줍니다. \
             octo가 열지 못한 링크를 다른 도구로 읽었다면 report_access로 방식과 본문을 남기세요. \
             다음 세션도 알아야 할 내용은 add_context(index, …)로 남기고, 직접 남긴 항목은 update_context·delete_context로 고치거나 지우세요. \
             인덱스는 create_index·update_index·delete_index로 관리합니다. 사람이 만든 항목은 바꾸지 못하니 바꿀 내용을 사용자에게 알려 주세요.\n\n\
             인덱스:\n{}",
            "Octo context indexes aren't fixed to the session. For each user message, pick the index that fits the topic from below, \
             read its table of contents with get_index(index), and load only the items you need with load_context(id). \
             If the topic spans several indexes, open several. If it's unclear which index, ask the user. \
             list_indexes shows the indexes recently used in this project first. \
             If you read a link octo couldn't open with another tool, leave the method and content with report_access. \
             Save what later sessions should know with add_context(index, …), and fix or delete items you saved with update_context and delete_context. \
             Manage indexes with create_index, update_index, and delete_index. Person-made items can't be changed; tell the user what should change.\n\n\
             Indexes:\n{}",
            list_indexes(ctx)
        )
    };

    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
        "instructions": instructions,
    })
}

fn tools() -> Value {
    let id = json!({ "type": "string", "description": t("get_index 목차의 id", "id from a get_index table of contents") });
    json!([
        {
            "name": "get_index",
            "description": t(
                "인덱스 하나의 목차를 가져온다. 사용자 메시지의 주제에 맞는 인덱스를 고르고, 여러 주제면 여러 번 부른다. \
                 항목마다 id, 종류, 제목, 한 줄 요약이 있다.",
                "Get one index's table of contents. Pick the index that fits the user message's topic; call it again for other topics. \
                 Each item has an id, kind, title, and one-line summary.",
            ),
            "inputSchema": {
                "type": "object",
                "properties": { "index": { "type": "string", "description": t("list_indexes의 인덱스 이름", "Index name from list_indexes") } },
                "required": ["index"],
            },
        },
        {
            "name": "load_context",
            "description": t(
                "목차의 id로 컨텍스트 본문을 가져온다. 어느 인덱스의 목차든 된다. 문서는 본문, 경로는 파일 내용이나 폴더 목록, 그래프는 쿼리 결과.",
                "Load a context's content by its id from any index's table of contents. Doc: the text. Path: file content, folder listing, or page text for links. Graph: query result.",
            ),
            "inputSchema": { "type": "object", "properties": { "id": id }, "required": ["id"] },
        },
        {
            "name": "list_indexes",
            "description": t(
                "쓸 수 있는 인덱스의 이름·설명·개수. 이 프로젝트에서 최근 쓴 인덱스를 맨 앞에 둔다.",
                "Names, descriptions, and sizes of available indexes, with the ones recently used in this project first.",
            ),
            "inputSchema": { "type": "object", "properties": {} },
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
                    "id": { "type": "string", "description": t("목차의 링크 컨텍스트 id", "id of a link context from a table of contents") },
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
                "다음 세션도 알아야 할 내용을 인덱스에 새 컨텍스트로 남긴다. 어느 인덱스에 넣을지 index로 꼭 정하고, 애매하면 사용자에게 묻는다. \
                 먼저 get_index로 비슷한 항목이 있는지 보고, 있으면 update_context를 쓴다. \
                 이미 파일이나 링크로 있는 내용은 복사하지 말고 path로 가리킨다. \
                 body는 결정 사항, 조사 결과처럼 어디에도 없는 내용에만 쓴다. body와 path 중 하나만 준다.",
                "Save something later sessions should know as a new context in an index. Always choose the index; ask the user if unclear. \
                 Check get_index for a similar item first and use update_context if there is one. \
                 Don't copy content that already lives in a file or link; point to it with path. \
                 Use body only for things that exist nowhere else, such as decisions or findings. Give exactly one of body or path.",
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index": { "type": "string", "description": t("넣을 인덱스 이름", "Index to add it to") },
                    "title": { "type": "string", "description": t("목차에 보일 제목", "Title shown in the table of contents") },
                    "summary": { "type": "string", "description": t("목차에 보일 한 줄 요약. 없으면 본문 첫 줄", "One-line summary for the table of contents. Defaults to the first line") },
                    "body": { "type": "string", "description": t("문서 본문 (Markdown)", "Document text (Markdown)") },
                    "path": { "type": "string", "description": t("원본 파일·폴더의 절대 경로나 링크(URL)", "Absolute path to the source file or folder, or a link (URL)") },
                },
                "required": ["index", "title"],
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
                    "id": id,
                    "title": { "type": "string", "description": t("새 제목", "New title") },
                    "summary": { "type": "string", "description": t("새 한 줄 요약. 빈 문자열이면 본문 첫 줄로 돌아간다", "New one-line summary. An empty string falls back to the first line") },
                    "body": { "type": "string", "description": t("새 문서 본문 전체 (Markdown)", "Full new document text (Markdown)") },
                    "path": { "type": "string", "description": t("새 절대 경로나 링크(URL)", "New absolute path or link (URL)") },
                },
                "required": ["id"],
            },
        },
        {
            "name": "delete_context",
            "description": t(
                "에이전트가 남긴 컨텍스트를 지운다. 담겨 있던 모든 인덱스에서도 빠진다. 사람이 만들거나 고친 항목은 지울 수 없다.",
                "Delete a context an agent saved. It is also removed from every index. Items a person created or edited can't be deleted.",
            ),
            "inputSchema": { "type": "object", "properties": { "id": id }, "required": ["id"] },
        },
        {
            "name": "create_index",
            "description": t(
                "빈 인덱스를 만든다. 다른 세션이 주제로 고를 수 있게 description에 한 줄 설명을 꼭 넣는다. 이름은 영문·숫자·-·_ (64자 이내).",
                "Create an empty index. Give it a one-line description so other sessions can pick it by topic. Names use letters, numbers, - and _ (up to 64).",
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": t("새 인덱스 이름", "New index name") },
                    "description": { "type": "string", "description": t("무엇에 관한 인덱스인지 한 줄", "One line on what the index is about") },
                },
                "required": ["name"],
            },
        },
        {
            "name": "update_index",
            "description": t(
                "인덱스 목차에 기존 컨텍스트를 넣거나 빼고, 이름·설명을 바꾼다. \
                 넣기는 어느 인덱스든 되고, 빼기는 인덱스나 컨텍스트가 에이전트 것일 때만, 이름·설명 바꾸기는 에이전트가 만든 인덱스만 된다.",
                "Add existing contexts to an index's table of contents, remove them, or change its name or description. \
                 Adding works on any index; removing needs the index or the context to be agent-made; name and description changes need an agent-made index.",
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": t("고칠 인덱스 이름", "Index to edit") },
                    "new_name": { "type": "string", "description": t("새 이름", "New name") },
                    "description": { "type": "string", "description": t("새 한 줄 설명", "New one-line description") },
                    "add": { "type": "array", "items": { "type": "string" }, "description": t("목차 끝에 넣을 컨텍스트 id", "Context ids to append") },
                    "remove": { "type": "array", "items": { "type": "string" }, "description": t("목차에서 뺄 컨텍스트 id (컨텍스트는 남는다)", "Context ids to take out (the contexts remain)") },
                },
                "required": ["name"],
            },
        },
        {
            "name": "delete_index",
            "description": t(
                "에이전트가 만든 인덱스를 지운다. 담겨 있던 컨텍스트는 남는다. 사람이 만들거나 고친 인덱스는 지울 수 없다.",
                "Delete an index an agent created. Its contexts remain. Indexes a person created or edited can't be deleted.",
            ),
            "inputSchema": {
                "type": "object",
                "properties": { "name": { "type": "string", "description": t("지울 인덱스 이름", "Index to delete") } },
                "required": ["name"],
            },
        },
        {
            "name": "export_indexes",
            "description": t(
                "인덱스를 zip(.octo.zip)으로 내보낸다. 목차의 컨텍스트와 문서 본문, 경로가 가리키는 파일·폴더까지 담는다. \
                 indexes나 all 중 하나를 준다. 첨부가 100MB를 넘는 컨텍스트는 attach_large가 없으면 경로만 담는다.",
                "Export indexes as a zip (.octo.zip) with their contexts, document text, and the files and folders paths point to. \
                 Pass indexes or all. Contexts whose attachment exceeds 100MB get only their path unless attach_large is set.",
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "indexes": { "type": "array", "items": { "type": "string" }, "description": t("내보낼 인덱스 이름", "Index names to export") },
                    "all": { "type": "boolean", "description": t("모든 인덱스와 컨텍스트 (다른 PC 이전·백업용)", "Every index and context (for moving PCs or backups)") },
                    "path": { "type": "string", "description": t("저장할 경로. 없으면 ~/Downloads/<이름>-<날짜>.octo.zip", "Where to save. Defaults to ~/Downloads/<name>-<date>.octo.zip") },
                    "include_secrets": { "type": "boolean", "description": t("all일 때만: 그래프 연결 비밀번호를 평문으로 넣는다. 사용자가 명시적으로 원할 때만", "Only with all: put graph connection passwords in as plain text. Only when the user explicitly asks") },
                    "attach_large": { "type": "boolean", "description": t("100MB 넘는 첨부도 넣는다", "Include attachments over 100MB too") },
                },
            },
        },
        {
            "name": "import_indexes",
            "description": t(
                "octo 내보내기 파일(.octo.zip)을 가져온다. 먼저 dry_run으로 요약을 사용자에게 보여주고 확인받은 뒤 가져온다. \
                 가져온 항목은 사람 소유가 되어 에이전트가 고치거나 지울 수 없다. 같은 이름 인덱스는 새 이름으로 만들고, 같은 id 컨텍스트는 내용이 다르면 갱신한다. \
                 가져오기 직전 상태는 스냅샷으로 남는다.",
                "Import an octo export file (.octo.zip). Show the dry_run summary to the user and get confirmation first. \
                 Imported items belong to the person, so agents can't edit or delete them. Same-name indexes get a new name; same-id contexts are updated if they differ. \
                 The state just before importing is kept as a snapshot.",
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": t(".octo.zip 파일 경로", "Path to the .octo.zip file") },
                    "dry_run": { "type": "boolean", "description": t("true면 바꿀 내용 요약만 돌려준다", "If true, only return a summary of what would change") },
                },
                "required": ["path"],
            },
        },
    ])
}

fn call(ctx: &Ctx, params: &Value) -> Value {
    let args = &params["arguments"];
    let result = match params["name"].as_str() {
        Some("get_index") => get_index(ctx, args),
        Some("load_context") => match args["id"].as_str() {
            Some(id) => content::load_listed(ctx.store, id),
            None => Err(t("id 인자가 필요합니다", "The id argument is required").into()),
        },
        Some("list_indexes") => Ok(list_indexes(ctx)),
        Some("report_access") => report_access(ctx, args),
        Some("add_context") => match text_arg(args, "index") {
            Some(index) => add_context(ctx, &index, args),
            None => Err(ctx.needs_index()),
        },
        Some("update_context") => update_context(ctx, args),
        Some("delete_context") => delete_context(ctx, args),
        Some("create_index") => create_index(ctx, args),
        Some("update_index") => update_index(ctx, args),
        Some("delete_index") => delete_index(ctx, args["name"].as_str().unwrap_or_default()),
        Some("export_indexes") => export_indexes(ctx, args),
        Some("import_indexes") => import_indexes(ctx, args),
        other => Err(tr!("알 수 없는 도구: {}", "Unknown tool: {}", other.unwrap_or(""))),
    };
    tool_result(result)
}

/// 메시지 주제에 맞는 인덱스의 목차. 연 인덱스는 작업 폴더의 최근 목록에 쌓인다.
fn get_index(ctx: &Ctx, args: &Value) -> Result<String, String> {
    let name = text_arg(args, "index").ok_or_else(|| ctx.needs_index())?;
    if ctx.store.index(&name).is_none() {
        return Err(tr!("인덱스 없음: '{name}'\n\n{}", "No index '{name}'\n\n{}", list_indexes(ctx)));
    }
    let toc = content::toc(ctx.store, &name)?;
    ctx.remember_opened(&name);
    Ok(toc)
}

/// 이름·설명·개수. 이 프로젝트에서 최근 쓴 인덱스(힌트)를 앞에 둔다.
fn list_indexes(ctx: &Ctx) -> String {
    let mut indexes = ctx.store.indexes();
    if indexes.is_empty() {
        return t("(인덱스 없음)", "(no indexes)").into();
    }
    let hints = ctx.hints();
    indexes.sort_by_key(|index| hints.iter().position(|h| *h == index.name).unwrap_or(usize::MAX));
    indexes
        .iter()
        .map(|index| {
            let mut marks = Vec::new();
            if hints.contains(&index.name) {
                marks.push(t("이 프로젝트에서 최근 씀", "recently used in this project"));
            }
            if index.author.is_some() {
                marks.push(t("에이전트 작성", "agent-made"));
            }
            let marks = if marks.is_empty() { String::new() } else { format!(" [{}]", marks.join(", ")) };
            tr!(
                "- {} — {} (컨텍스트 {}개){marks}",
                "- {} — {} ({} contexts){marks}",
                index.name,
                describe(ctx.store, index),
                index.contexts.len()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 인덱스 한 줄 설명. 비어 있으면 목차 제목 몇 개로 대신한다.
fn describe(store: &Store, index: &store::Index) -> String {
    let description = index.description.trim();
    if !description.is_empty() {
        return description.to_owned();
    }
    let titles: Vec<String> = index.contexts.iter().filter_map(|id| store.context(id)).take(3).map(|c| c.title).collect();
    match titles.len() {
        0 => t("(비어 있음)", "(empty)").into(),
        n if n < index.contexts.len() => tr!("{} 외", "{}, …", titles.join(", ")),
        _ => titles.join(", "),
    }
}

fn report_access(ctx: &Ctx, args: &Value) -> Result<String, String> {
    let id = args["id"].as_str().ok_or(t("id 인자가 필요합니다", "The id argument is required"))?;
    let method = args["method"].as_str().map(str::trim).filter(|m| !m.is_empty());
    let method = method.ok_or(t("method 인자가 필요합니다", "The method argument is required"))?;
    let context = content::listed(ctx.store, id)?;
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
    let mut index = ctx
        .store
        .index(index_name)
        .ok_or_else(|| tr!("인덱스 없음: '{index_name}'\n\n{}", "No index '{index_name}'\n\n{}", list_indexes(ctx)))?;
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

fn update_context(ctx: &Ctx, args: &Value) -> Result<String, String> {
    let id = args["id"].as_str().ok_or(t("id 인자가 필요합니다", "The id argument is required"))?;
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
    // 이 컨텍스트가 담긴 모든 인덱스에서 제목이 겹치지 않아야 한다
    if let Some(title) = &title {
        for index in ctx.store.indexes_using(id) {
            if let Some(existing) = same_title(ctx.store, &index.contexts, title, Some(id)) {
                return Err(duplicate(&index.name, &existing));
            }
        }
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

fn delete_context(ctx: &Ctx, args: &Value) -> Result<String, String> {
    let id = args["id"].as_str().ok_or(t("id 인자가 필요합니다", "The id argument is required"))?;
    let context = ctx.store.context(id).ok_or_else(|| tr!("컨텍스트 없음: {id}", "No context: {id}"))?;
    if context.author.is_none() {
        return Err(t(
            "사람이 만들거나 고친 컨텍스트는 지울 수 없습니다. 목차에서만 빼려면 update_index의 remove를 쓰거나 사용자에게 알려 주세요",
            "Contexts a person created or edited can't be deleted. To only take it out of an index, use update_index remove or tell the user",
        )
        .into());
    }
    ctx.store.delete_context(id).map_err(|err| tr!("지우지 못했습니다: {err}", "Couldn't delete: {err}"))?;
    if let Source::Path { path } = &context.source
        && web::is_url(path)
    {
        cache::forget_if_unused(ctx.store, path);
    }

    notify_changed();
    Ok(tr!("지웠습니다: {} (id: {id})", "Deleted: {} (id: {id})", context.title))
}

fn create_index(ctx: &Ctx, args: &Value) -> Result<String, String> {
    let name = args["name"].as_str().unwrap_or_default().trim();
    if !store::is_valid_index_name(name) {
        return Err(t("이름은 영문·숫자·-·_ 만, 64자 이내로 쓸 수 있습니다", "Names can only use letters, numbers, - and _, up to 64").into());
    }
    if ctx.store.index(name).is_some() {
        return Err(tr!("'{name}' 인덱스가 이미 있습니다. get_index로 여세요", "Index '{name}' already exists. Open it with get_index"));
    }
    let index = store::Index {
        name: name.to_owned(),
        description: text_arg(args, "description").unwrap_or_default(),
        contexts: Vec::new(),
        author: Some(ctx.client()),
    };
    ctx.store.save_index(&index).map_err(|err| tr!("만들지 못했습니다: {err}", "Couldn't create it: {err}"))?;
    // 이 프로젝트를 위해 만든 것이니 최근 목록에 둔다
    ctx.remember_opened(name);

    notify_changed();
    let mut reply = tr!("'{name}' 인덱스를 만들었습니다. add_context에 index로 넘겨 채우세요.", "Created index '{name}'. Fill it by passing it as index to add_context.");
    if index.description.is_empty() {
        reply.push_str(t(
            " 설명(description)이 없으면 다른 세션이 이 인덱스를 고르기 어려우니 update_index로 한 줄 설명을 넣으세요.",
            " Without a description other sessions will struggle to pick it; add a one-line description with update_index.",
        ));
    }
    Ok(reply)
}

fn update_index(ctx: &Ctx, args: &Value) -> Result<String, String> {
    let name = text_arg(args, "name").ok_or(t("name 인자가 필요합니다", "The name argument is required"))?;
    let mut index = ctx.store.index(&name).ok_or_else(|| tr!("인덱스 없음: '{name}'", "No index '{name}'"))?;
    let ids = |key: &str| -> Vec<String> {
        args[key].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect()
    };
    let (add, remove) = (ids("add"), ids("remove"));
    let new_name = text_arg(args, "new_name").filter(|n| *n != name);
    let description = args["description"].as_str().map(|d| d.trim().to_owned()).filter(|d| *d != index.description);
    if add.is_empty() && remove.is_empty() && new_name.is_none() && description.is_none() {
        return Err(t(
            "바꿀 값이 없습니다 (add, remove, new_name, description 중 하나 이상)",
            "Nothing to change (pass add, remove, new_name, or description)",
        )
        .into());
    }

    // 모두 확인한 뒤에 한꺼번에 바꾼다
    for id in &add {
        if ctx.store.context(id).is_none() {
            return Err(tr!("컨텍스트 없음: {id}", "No context: {id}"));
        }
    }
    for id in &remove {
        if !index.contexts.contains(id) {
            return Err(tr!("'{name}' 인덱스에 없는 컨텍스트: {id}", "Context not in index '{name}': {id}"));
        }
        let agent_context = ctx.store.context(id).is_some_and(|c| c.author.is_some());
        if index.author.is_none() && !agent_context {
            return Err(tr!(
                "사람이 만든 인덱스에서 사람이 만든 컨텍스트는 뺄 수 없습니다: {id}. 사용자에게 알려 주세요",
                "Can't take a person-made context out of a person-made index: {id}. Tell the user",
            ));
        }
    }
    if (new_name.is_some() || description.is_some()) && index.author.is_none() {
        return Err(t(
            "사람이 만들거나 고친 인덱스는 이름·설명을 바꿀 수 없습니다. 바꿀 내용을 사용자에게 알려 주세요",
            "Indexes a person created or edited can't have their name or description changed. Tell the user what should change",
        )
        .into());
    }
    if let Some(new_name) = &new_name {
        if !store::is_valid_index_name(new_name) {
            return Err(t("이름은 영문·숫자·-·_ 만, 64자 이내로 쓸 수 있습니다", "Names can only use letters, numbers, - and _, up to 64").into());
        }
        if ctx.store.index(new_name).is_some() {
            return Err(tr!("'{new_name}' 인덱스가 이미 있습니다", "Index '{new_name}' already exists"));
        }
    }

    let before = index.contexts.len();
    index.contexts.retain(|c| !remove.contains(c));
    let removed = before - index.contexts.len();
    let mut added = 0;
    for id in add {
        if !index.contexts.contains(&id) {
            index.contexts.push(id);
            added += 1;
        }
    }
    let described = description.is_some();
    if let Some(description) = description {
        index.description = description;
    }
    let save = |err| tr!("저장하지 못했습니다: {err}", "Couldn't save: {err}");
    ctx.store.save_index(&index).map_err(save)?;
    let mut current = name.clone();
    if let Some(new_name) = new_name {
        ctx.store.rename_index(&name, &new_name).map_err(save)?;
        current = new_name;
    }

    notify_changed();
    let mut changes = vec![tr!("{added}개 넣음, {removed}개 뺌", "{added} added, {removed} removed")];
    if current != name {
        changes.push(tr!("이름 '{name}' → '{current}'", "renamed '{name}' → '{current}'"));
    }
    if described {
        changes.push(t("설명 바꿈", "description changed").into());
    }
    Ok(tr!("'{current}' 인덱스를 고쳤습니다: {}", "Updated index '{current}': {}", changes.join(", ")))
}

fn delete_index(ctx: &Ctx, name: &str) -> Result<String, String> {
    let index = ctx.store.index(name).ok_or_else(|| tr!("인덱스 없음: '{name}'", "No index '{name}'"))?;
    if index.author.is_none() {
        return Err(t(
            "사람이 만들거나 고친 인덱스는 지울 수 없습니다. 사용자에게 알려 주세요",
            "Indexes a person created or edited can't be deleted. Tell the user",
        )
        .into());
    }
    ctx.store.delete_index(name).map_err(|err| tr!("지우지 못했습니다: {err}", "Couldn't delete: {err}"))?;

    notify_changed();
    Ok(tr!("'{name}' 인덱스를 지웠습니다. 담겨 있던 컨텍스트 {}개는 남아 있습니다.", "Deleted index '{name}'. Its {} contexts remain.", index.contexts.len()))
}

fn export_indexes(ctx: &Ctx, args: &Value) -> Result<String, String> {
    let all = args["all"].as_bool().unwrap_or(false);
    let names: Vec<String> = args["indexes"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
    let scope = if all {
        Scope::All
    } else if !names.is_empty() {
        Scope::Indexes(names)
    } else {
        return Err(t("indexes(인덱스 이름 목록)나 all을 주세요", "Pass indexes (a list of index names) or all").into());
    };
    let include_secrets = args["include_secrets"].as_bool().unwrap_or(false);
    if include_secrets && !all {
        return Err(t("비밀번호는 전체 내보내기(all)에만 넣을 수 있습니다", "Passwords can only go into a full export (all)").into());
    }
    let dest = match text_arg(args, "path") {
        Some(path) => std::path::PathBuf::from(path),
        None => transfer::default_export_path(match &scope {
            Scope::All => "octo-all",
            Scope::Indexes(names) if names.len() == 1 => &names[0],
            Scope::Indexes(_) => "octo-indexes",
        }),
    };
    let options = ExportOptions { include_secrets, skip_large: !args["attach_large"].as_bool().unwrap_or(false), ..ExportOptions::new(scope) };
    let exported = transfer::export(ctx.store, &options, &dest)?;

    let mut reply = tr!(
        "내보냈습니다: {} ({})\n인덱스 {}개 · 컨텍스트 {}개 · 첨부 {}개",
        "Exported: {} ({})\n{} indexes · {} contexts · {} attachments",
        exported.path.display(),
        transfer::human_bytes(exported.bytes),
        exported.indexes,
        exported.contexts,
        exported.attached
    );
    if !exported.skipped.is_empty() {
        reply.push_str(&tr!("\n100MB가 넘어 경로만 담음: {}", "\nOver 100MB, path only: {}", exported.skipped.join(", ")));
    }
    if !exported.missing.is_empty() {
        reply.push_str(&tr!("\n원본이 없어 경로만 담음: {}", "\nSource missing, path only: {}", exported.missing.join(", ")));
    }
    if exported.secrets > 0 {
        reply.push_str(&tr!(
            "\n⚠ 비밀번호 {}개가 평문으로 들어 있습니다. 파일을 안전하게 다루도록 사용자에게 알리세요.",
            "\n⚠ Contains {} passwords in plain text. Tell the user to handle the file carefully.",
            exported.secrets
        ));
    }
    Ok(reply)
}

fn import_indexes(ctx: &Ctx, args: &Value) -> Result<String, String> {
    let path = text_arg(args, "path").ok_or(t("path 인자가 필요합니다", "The path argument is required"))?;
    let path = std::path::Path::new(&path);
    if args["dry_run"].as_bool().unwrap_or(false) {
        let plan = transfer::preview(ctx.store, path)?;
        return Ok(tr!("가져오면 이렇게 바뀝니다 (아직 바뀐 것 없음)\n{}", "Importing would change this (nothing changed yet)\n{}", plan_text(&plan)));
    }
    let imported = transfer::import(ctx.store, path)?;
    notify_changed();
    Ok(tr!(
        "가져왔습니다. 가져온 항목은 사람 소유입니다.\n{}\n되돌리기용 스냅샷: {}",
        "Imported. Imported items belong to the person.\n{}\nSnapshot for undo: {}",
        plan_text(&imported.plan),
        imported.snapshot.display()
    ))
}

fn plan_text(plan: &transfer::Plan) -> String {
    let indexes = plan
        .indexes
        .iter()
        .map(|(from, to)| if from == to { from.clone() } else { format!("{from} → {to}") })
        .collect::<Vec<_>>()
        .join(", ");
    let mut text = tr!(
        "인덱스: {indexes}\n컨텍스트: 새로 {} · 갱신 {} · 그대로 {}\n연결 {}개 · 풀 첨부 {}",
        "Indexes: {indexes}\nContexts: {} new · {} updated · {} unchanged\n{} connections · {} of attachments to extract",
        plan.new,
        plan.updated,
        plan.unchanged,
        plan.connections,
        transfer::human_bytes(plan.attach_bytes)
    );
    if plan.secrets {
        text.push_str(t("\n비밀번호 포함", "\nIncludes passwords"));
    }
    text
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
        store.save_index(&Index { name: "x".into(), ..Index::default() }).unwrap();
        (store, Sessions::default())
    }

    fn ctx<'a>(store: &'a Store, sessions: &'a Sessions) -> Ctx<'a> {
        Ctx { store, sessions, session: None, pinned: None }
    }

    fn text(reply: &Value) -> &str {
        reply["content"][0]["text"].as_str().unwrap()
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
    fn indexes_are_chosen_per_call_not_per_session() {
        let (store, sessions) = setup("dispatch");
        store.save_index(&Index { name: "pay".into(), description: "결제 정책".into(), ..Index::default() }).unwrap();
        let ctx = ctx(&store, &sessions);
        let names: Vec<_> = tools().as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect();
        assert!(!names.contains(&"use_index".to_owned()));
        for name in ["get_index", "load_context", "add_context", "create_index", "update_index", "export_indexes", "import_indexes"] {
            assert!(names.contains(&name.to_owned()), "{name}");
        }

        // 인덱스를 안 정하면 목록과 함께 되묻는다
        let missing = call(&ctx, &json!({ "name": "add_context", "arguments": { "title": "t", "body": "b" } }));
        assert_eq!(missing["isError"], true);
        assert!(text(&missing).contains("pay — 결제 정책"), "{}", text(&missing));
        assert_eq!(call(&ctx, &json!({ "name": "get_index" }))["isError"], true);

        // 한 세션에서 두 인덱스를 오가며 쓰고, 어느 인덱스의 id든 연다
        call(&ctx, &json!({ "name": "add_context", "arguments": { "index": "x", "title": "t", "body": "from x" } }));
        call(&ctx, &json!({ "name": "add_context", "arguments": { "index": "pay", "title": "refund", "body": "7일" } }));
        assert!(text(&call(&ctx, &json!({ "name": "get_index", "arguments": { "index": "pay" } }))).contains("결제 정책"));
        let id = store.index("x").unwrap().contexts[0].clone();
        assert_eq!(text(&call(&ctx, &json!({ "name": "load_context", "arguments": { "id": id } }))), "from x");

        // 어느 인덱스에도 없는 건 열지 않는다
        let loose = Context { id: "loose".into(), title: "l".into(), summary: String::new(), source: Source::Document, author: None };
        store.save_context(&loose).unwrap();
        assert_eq!(call(&ctx, &json!({ "name": "load_context", "arguments": { "id": "loose" } }))["isError"], true);
    }

    #[test]
    fn instructions_list_indexes_without_a_default() {
        let (store, sessions) = setup("instructions");
        store.save_index(&Index { name: "pay".into(), description: "결제 정책".into(), ..Index::default() }).unwrap();
        let instructions = initialize(&ctx(&store, &sessions), &json!({}))["instructions"].as_str().unwrap().to_owned();
        assert!(instructions.contains("pay — 결제 정책"), "{instructions}");
        assert!(instructions.contains("get_index(index)"), "{instructions}");
        // 설명이 없으면 목차 제목으로 대신한다
        assert!(instructions.contains("x — "), "{instructions}");
    }

    fn human_context(store: &Store, id: &str) {
        let context = Context { id: id.into(), title: id.into(), summary: String::new(), source: Source::Document, author: None };
        store.save_context(&context).unwrap();
    }

    #[test]
    fn delete_context_only_removes_agent_contexts_everywhere() {
        let (store, sessions) = setup("delete-ctx");
        let ctx = ctx(&store, &sessions);
        human_context(&store, "h");
        store.save_index(&Index { name: "x".into(), contexts: vec!["h".into()], ..Index::default() }).unwrap();
        assert!(delete_context(&ctx, &json!({ "id": "h" })).is_err());

        add_context(&ctx, "x", &json!({ "title": "note", "body": "b" })).unwrap();
        let id = store.index("x").unwrap().contexts[1].clone();
        store.save_index(&Index { name: "y".into(), contexts: vec![id.clone()], ..Index::default() }).unwrap();
        delete_context(&ctx, &json!({ "id": id })).unwrap();
        assert!(store.context(&id).is_none());
        assert_eq!(store.index("x").unwrap().contexts, vec!["h".to_string()]);
        assert!(store.index("y").unwrap().contexts.is_empty());
    }

    #[test]
    fn create_index_takes_a_description() {
        let (store, sessions) = setup("create");
        let ctx = ctx(&store, &sessions);
        assert!(create_index(&ctx, &json!({ "name": "통신" })).is_err());
        assert!(create_index(&ctx, &json!({ "name": "x" })).is_err());
        let reply = create_index(&ctx, &json!({ "name": "bare" })).unwrap();
        assert!(reply.contains("description"), "{reply}");
        create_index(&ctx, &json!({ "name": "telecom", "description": "통신 마이데이터" })).unwrap();
        let index = store.index("telecom").unwrap();
        assert!(index.author.is_some());
        assert_eq!(index.description, "통신 마이데이터");
    }

    #[test]
    fn update_index_respects_ownership() {
        let (store, sessions) = setup("update-index");
        let ctx = ctx(&store, &sessions);
        human_context(&store, "h");
        // 사람 인덱스: 넣기는 되고, 사람 컨텍스트 빼기와 이름·설명 바꾸기는 안 된다
        assert!(update_index(&ctx, &json!({ "name": "x", "add": ["h"] })).is_ok());
        assert!(update_index(&ctx, &json!({ "name": "x", "add": ["missing"] })).is_err());
        assert!(update_index(&ctx, &json!({ "name": "x", "remove": ["h"] })).is_err());
        assert!(update_index(&ctx, &json!({ "name": "x", "new_name": "z" })).is_err());
        assert!(update_index(&ctx, &json!({ "name": "x", "description": "내 설명" })).is_err());
        assert!(update_index(&ctx, &json!({ "name": "x" })).is_err());
        assert!(update_index(&ctx, &json!({ "add": ["h"] })).is_err(), "name is required");

        // 에이전트 인덱스: 다 된다
        create_index(&ctx, &json!({ "name": "mine" })).unwrap();
        update_index(&ctx, &json!({ "name": "mine", "add": ["h"], "description": "설명" })).unwrap();
        update_index(&ctx, &json!({ "name": "mine", "remove": ["h"], "new_name": "mine2" })).unwrap();
        assert!(store.index("mine").is_none());
        let renamed = store.index("mine2").unwrap();
        assert!(renamed.contexts.is_empty());
        assert_eq!(renamed.description, "설명");
    }

    #[test]
    fn delete_index_keeps_contexts() {
        let (store, sessions) = setup("delete-index");
        let ctx = ctx(&store, &sessions);
        assert!(delete_index(&ctx, "x").is_err());
        create_index(&ctx, &json!({ "name": "tmp" })).unwrap();
        human_context(&store, "h");
        update_index(&ctx, &json!({ "name": "tmp", "add": ["h"] })).unwrap();
        delete_index(&ctx, "tmp").unwrap();
        assert!(store.index("tmp").is_none());
        assert!(store.context("h").is_some());
    }

    #[test]
    fn export_then_import_through_tools() {
        let (store, sessions) = setup("transfer");
        let ctx = ctx(&store, &sessions);
        add_context(&ctx, "x", &json!({ "title": "note", "body": "b" })).unwrap();
        let zip = store.root().join("x.octo.zip");
        let path = zip.to_string_lossy();
        assert!(export_indexes(&ctx, &json!({ "path": path })).is_err(), "needs indexes or all");
        assert!(export_indexes(&ctx, &json!({ "indexes": ["x"], "include_secrets": true })).is_err());
        export_indexes(&ctx, &json!({ "indexes": ["x"], "path": path })).unwrap();

        let (other, other_sessions) = setup("transfer-other");
        let other_ctx = self::ctx(&other, &other_sessions);
        let dry = import_indexes(&other_ctx, &json!({ "path": path, "dry_run": true })).unwrap();
        assert!(dry.contains("x → x-2"), "{dry}");
        assert!(other.contexts().is_empty());
        import_indexes(&other_ctx, &json!({ "path": path })).unwrap();
        let id = other.index("x-2").unwrap().contexts[0].clone();
        // 가져온 건 사람 것이라 에이전트가 못 지운다
        assert!(delete_context(&other_ctx, &json!({ "id": id })).is_err());
    }

    #[test]
    fn file_uris_become_paths() {
        assert_eq!(file_uri_path("file:///Users/me/my%20app/").as_deref(), Some("/Users/me/my app"));
        assert_eq!(file_uri_path("file://localhost/w/%ED%86%B5").as_deref(), Some("/w/통"));
        assert_eq!(file_uri_path("file:///w/100%").as_deref(), Some("/w/100%"));
        assert_eq!(file_uri_path("https://example.com"), None);
    }

    fn folder_session<'a>(store: &'a Store, sessions: &'a Sessions, folder: &str) -> Ctx<'a> {
        sessions.lock().unwrap().insert("s".into(), Session { roots: true, folders: Some(vec![folder.into()]), ..Session::default() });
        Ctx { store, sessions, session: Some("s".into()), pinned: None }
    }

    #[test]
    fn opened_indexes_become_hints_for_the_folder() {
        let (store, sessions) = setup("folder");
        store.save_index(&Index { name: "y".into(), ..Index::default() }).unwrap();
        let ctx = folder_session(&store, &sessions, "/w/app");
        assert!(ctx.hints().is_empty());

        // 목차를 연 인덱스가 최근 것부터 쌓이고, 같은 폴더의 다음 세션 목록 맨 앞에 온다
        get_index(&ctx, &json!({ "index": "y" })).unwrap();
        get_index(&ctx, &json!({ "index": "x" })).unwrap();
        let next_sessions = Sessions::default();
        let next = folder_session(&store, &next_sessions, "/w/app/src");
        assert_eq!(next.hints(), vec!["x", "y"]);
        let list = list_indexes(&next);
        assert!(list.starts_with("- x"), "{list}");
        assert!(list.contains("최근") || list.contains("recently"), "{list}");

        // 주소의 ?index=는 고정이 아니라 첫 힌트
        let pinned = Ctx { pinned: Some("y".into()), ..folder_session(&store, &next_sessions, "/w/app") };
        assert_eq!(pinned.hints(), vec!["y", "x"]);

        // 새로 만든 인덱스도 이 프로젝트의 최근 목록에 든다
        create_index(&ctx, &json!({ "name": "made" })).unwrap();
        assert_eq!(folder_session(&store, &Sessions::default(), "/w/app").hints()[0], "made");
    }

    #[test]
    fn folders_are_asked_over_the_open_stream() {
        let mut stream = Vec::new();
        let answer = thread::spawn(|| loop {
            let pending = PENDING.lock().unwrap().iter().map(|(id, tx)| (id.clone(), tx.clone())).next();
            if let Some((id, tx)) = pending {
                let roots = json!([{ "uri": "file:///w/app", "name": "app" }, { "uri": "https://not-a-folder" }]);
                tx.send(json!({ "jsonrpc": "2.0", "id": id, "result": { "roots": roots } })).unwrap();
                return;
            }
            thread::sleep(Duration::from_millis(5));
        });
        assert_eq!(ask_folders(&mut stream), vec!["/w/app".to_string()]);
        answer.join().unwrap();

        let sent = String::from_utf8(stream).unwrap();
        assert!(sent.contains("\"method\":\"roots/list\""), "{sent}");
        assert!(sent.contains("event: message\ndata: "), "{sent}");
    }

    /// 실제 HTTP로: initialize(roots 지원) → tools/call이 SSE로 열려 roots/list를 묻고, 다른 연결로 답하면 결과가 이어서 온다
    #[test]
    fn http_call_asks_roots_then_lists_the_folder_hints_first() {
        use std::io::{BufRead, BufReader};

        let (store, _) = setup("http");
        store.save_index(&Index { name: "app".into(), ..Index::default() }).unwrap();
        store.remember_folder_index("/w/app", "app").unwrap();
        let port = 47_700 + (std::process::id() % 200) as u16;
        spawn(store, port).unwrap();
        let url = super::url(port);
        let post = |session: Option<&str>, body: Value| {
            let request = ureq::post(&url).header("Accept", "application/json, text/event-stream");
            let request = match session {
                Some(session) => request.header("Mcp-Session-Id", session),
                None => request,
            };
            request.send(body.to_string()).unwrap()
        };

        let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": { "roots": {} }, "clientInfo": { "name": "t" } } });
        let response = post(None, init);
        let session = response.headers().get("Mcp-Session-Id").unwrap().to_str().unwrap().to_owned();
        let instructions = response.into_body().read_json::<Value>().unwrap()["result"]["instructions"].as_str().unwrap().to_owned();
        assert!(instructions.contains("list_indexes"), "{instructions}");

        let call = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "list_indexes" } });
        let stream = post(Some(&session), call);
        assert!(stream.headers().get("Content-Type").unwrap().to_str().unwrap().starts_with("text/event-stream"));
        let events = BufReader::new(stream.into_body().into_reader());
        let mut messages = events.lines().map(Result::unwrap).filter_map(|line| line.strip_prefix("data: ").map(str::to_owned));

        let ask: Value = serde_json::from_str(&messages.next().unwrap()).unwrap();
        assert_eq!(ask["method"], "roots/list");
        let answer = json!({ "jsonrpc": "2.0", "id": ask["id"], "result": { "roots": [{ "uri": "file:///w/app/sub" }] } });
        assert_eq!(post(Some(&session), answer).status(), 202);

        let result: Value = serde_json::from_str(&messages.next().unwrap()).unwrap();
        assert_eq!(result["id"], 2);
        let text = result["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("- app"), "{text}");

        // 폴더는 한 번만 묻는다: 다음 호출은 바로 JSON
        let again = post(Some(&session), json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "list_indexes" } }));
        assert!(again.headers().get("Content-Type").unwrap().to_str().unwrap().starts_with("application/json"));
    }

    #[test]
    fn update_only_touches_agent_contexts() {
        let (store, sessions) = setup("update");
        let ctx = ctx(&store, &sessions);
        human_context(&store, "h");
        store.save_index(&Index { name: "x".into(), contexts: vec!["h".into()], ..Index::default() }).unwrap();
        assert!(update_context(&ctx, &json!({ "id": "h", "body": "x" })).is_err());

        add_context(&ctx, "x", &json!({ "title": "note", "body": "v1" })).unwrap();
        let id = store.index("x").unwrap().contexts[1].clone();
        assert!(update_context(&ctx, &json!({ "id": id })).is_err());
        assert!(update_context(&ctx, &json!({ "id": id, "path": std::env::temp_dir() })).is_err());
        // 같은 인덱스의 사람이 만든 항목과 같은 제목으로는 못 바꾼다
        assert!(update_context(&ctx, &json!({ "id": id, "title": "H" })).is_err());
        update_context(&ctx, &json!({ "id": id, "body": "v2", "summary": "요약" })).unwrap();
        assert_eq!(store.document(&id), "v2");
        assert_eq!(store.context(&id).unwrap().summary, "요약");
    }
}
