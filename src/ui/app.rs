//! 화면 상태와 상태 변경. 파일이 SSOT이므로 변경은 곧바로 저장소에 쓰고 다시 읽는다.

use std::path::PathBuf;
use std::time::Duration;

use iced::widget::{image, text_editor};
use iced::{Subscription, Task, window};
use tray_icon::TrayIcon;

use crate::core::store::{self, Connection, Context, DeleteBlocked, Index, Kind, Source, Store};
use crate::core::transfer::{self, ExportOptions, Scope};
use crate::core::{cache, content, graph, secret, web};
use crate::daemon::claude;
use crate::daemon::dock;
use crate::daemon::tray::{self, TrayAction};
use crate::i18n::{self, Lang};
use crate::tr;
use crate::daemon::instance;
use crate::mcp;

pub struct App {
    pub(super) store: Store,
    pub(super) mcp_port: u16,
    pub(super) mcp_status: Result<(), String>,
    window: Option<window::Id>,
    tray: Option<TrayIcon>,
    pub(super) logo: image::Handle,
    /// ? 버튼으로 펼친 설명
    pub(super) help: Option<Help>,

    // 저장소 (가운데)
    pub(super) rows: Vec<Row>,
    pub(super) filter: Option<Kind>,
    pub(super) search: String,

    // 인덱스 (왼쪽) + 구성 (오른쪽)
    pub(super) indexes: Vec<Index>,
    pub(super) selected_index: Option<String>,
    /// + 만들기를 눌러 목록 맨 위에 이름 입력 줄이 떠 있는지
    pub(super) creating_index: bool,
    pub(super) new_index_name: String,
    pub(super) confirm_index_delete: bool,
    /// 이름 바꾸는 중이면 입력 중인 새 이름
    pub(super) rename_draft: Option<String>,
    /// 펼친 인덱스의 한 줄 설명을 고치는 중이면 그 값
    pub(super) description_draft: Option<String>,
    pub(super) toc: Loadable,
    /// 세션이 처음 받는 인덱스
    pub(super) claude: ClaudeState,

    pub(super) connections: Vec<Connection>,

    // 위에 뜨는 시트
    pub(super) sheet: Option<Sheet>,
    /// 컨텍스트 편집 중에 연결 관리를 열면 여기 보관했다가 돌아온다
    stashed_context: Option<ContextEditor>,

    pub(super) toast: Option<(u64, String)>,
    toast_seq: u64,
}

/// 저장소 목록 한 줄. 매 렌더마다 파일을 읽지 않도록 새로고침 때 계산해 둔다.
pub struct Row {
    pub context: Context,
    pub summary: String,
    pub broken: Option<String>,
}

/// Claude Code에 한 번 연결했는지
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeState {
    Checking,
    Working,
    Connected,
    NotConnected,
    /// claude CLI가 없다
    Unavailable,
}

/// 흐름의 각 단계. ? 설명도 단계별로 하나씩.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Help {
    Contexts,
    Index,
    Session,
}

#[derive(Debug, Clone, Default)]
pub enum Loadable {
    #[default]
    Loading,
    Ready(String),
    Failed(String),
}

pub enum Sheet {
    Context(ContextEditor),
    Connections(ConnectionsSheet),
    Export(ExportSheet),
    Import(ImportSheet),
}

pub struct ExportSheet {
    pub scope: Scope,
    /// 100MB가 넘는 첨부. 확인 중이면 None
    pub large: Option<Vec<transfer::Large>>,
    /// 큰 첨부도 넣을지
    pub attach_large: bool,
    /// 전체 내보내기에만 보인다
    pub include_secrets: bool,
    pub working: bool,
}

pub struct ImportSheet {
    pub path: PathBuf,
    pub plan: transfer::Plan,
    pub working: bool,
    /// 가져온 뒤엔 결과와 되돌리기 스냅샷
    pub done: Option<transfer::Imported>,
}

#[derive(Default)]
pub struct ContextEditor {
    /// None이면 새 컨텍스트
    pub id: Option<String>,
    pub kind: Kind,
    pub title: String,
    pub summary: String,
    pub body: text_editor::Content,
    pub import_path: String,
    pub path: String,
    pub connection: Option<String>,
    pub cypher: text_editor::Content,
    /// 새로 만들 때 선택된 인덱스에 바로 담을지
    pub add_to_index: bool,
    pub preview_open: bool,
    pub preview: Loadable,
    pub confirm_delete: bool,
    /// 링크 컨텍스트의 캐시 상태. 시트를 열 때와 지울 때 다시 읽는다.
    pub cache: Option<cache::Info>,
}

#[derive(Default)]
pub struct ConnectionsSheet {
    pub editor: Option<ConnectionEditor>,
    pub test: Option<Loadable>,
}

#[derive(Default)]
pub struct ConnectionEditor {
    pub id: Option<String>,
    pub name: String,
    pub uri: String,
    pub user: String,
    pub password: String,
    pub database: String,
}

#[derive(Debug, Clone)]
pub enum Message {
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    Tray(TrayAction),
    StoreChanged,
    LanguagePicked(Lang),
    DismissToast(u64),
    ToggleHelp(Help),

    // 저장소
    FilterSelected(Option<Kind>),
    SearchChanged(String),
    NewContext,
    OpenContext(String),

    // 인덱스
    SelectIndex(String),
    StartCreateIndex,
    CancelCreateIndex,
    NewIndexNameChanged(String),
    CreateIndex,
    ToggleInIndex(String),
    MoveInIndex(usize, i32),
    CopyMcpUrl,
    CopyMcpConfig,
    ClaudeChecked(claude::Status),
    ConnectClaude,
    DisconnectClaude,
    ClaudeDone(bool, Result<(), String>),
    TocLoaded(String, Result<String, String>),
    AskDeleteIndex,
    ConfirmDeleteIndex,
    CancelDeleteIndex,
    StartRenameIndex,
    RenameDraftChanged(String),
    ConfirmRenameIndex,
    CancelRenameIndex,
    StartDescribeIndex,
    DescriptionDraftChanged(String),
    ConfirmDescribeIndex,
    CancelDescribeIndex,

    // 컨텍스트 시트
    KindSelected(Kind),
    TitleChanged(String),
    SummaryChanged(String),
    BodyEdited(text_editor::Action),
    ImportPathChanged(String),
    ImportIntoEditor,
    PathChanged(String),
    ConnectionPicked(String),
    CypherEdited(text_editor::Action),
    AddToIndexToggled(bool),
    TogglePreview,
    PreviewLoaded(String, Result<String, String>),
    ClearCachedResult,
    ClearAccessMethod,
    SaveContext,
    AskDeleteContext,
    ConfirmDeleteContext,
    CancelDeleteContext,
    CloseSheet,

    // 연결 시트
    OpenConnections,
    NewConnection,
    SelectConnection(String),
    ConnectionNameChanged(String),
    ConnectionUriChanged(String),
    ConnectionUserChanged(String),
    ConnectionPasswordChanged(String),
    ConnectionDatabaseChanged(String),
    SaveConnection,
    DeleteConnection,
    TestConnection,
    ConnectionTested(Result<String, String>),

    // 내보내기·가져오기
    /// None이면 전체
    OpenExport(Option<String>),
    ExportScanned(Vec<transfer::Large>),
    ExportAttachLargeToggled(bool),
    ExportSecretsToggled(bool),
    ChooseExportPath,
    Exported(Result<transfer::Exported, String>),
    OpenImport,
    ImportPreviewed(PathBuf, Result<transfer::Plan, String>),
    ConfirmImport,
    Imported(Result<transfer::Imported, String>),
    UndoImport,
    Restored(Result<(), String>),
}

impl App {
    pub fn new(store: Store, mcp_port: u16, mcp_status: Result<(), String>) -> (Self, Task<Message>) {
        let mut app = Self {
            store,
            mcp_port,
            mcp_status,
            window: None,
            tray: None,
            logo: image::Handle::from_rgba(96, 96, crate::icon::rgba(96)),
            help: None,
            rows: Vec::new(),
            filter: None,
            search: String::new(),
            indexes: Vec::new(),
            selected_index: None,
            creating_index: false,
            new_index_name: String::new(),
            confirm_index_delete: false,
            rename_draft: None,
            toc: Loadable::Loading,
            description_draft: None,
            claude: ClaudeState::Checking,
            connections: Vec::new(),
            sheet: None,
            stashed_context: None,
            toast: None,
            toast_seq: 0,
        };
        app.refresh();
        app.selected_index = app.indexes.first().map(|i| i.name.clone());
        let toc = app.load_toc();
        let check = Task::perform(async { claude::status() }, Message::ClaudeChecked);
        (app, Task::batch([open_window(), toc, check]))
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            window::close_events().map(Message::WindowClosed),
            tray::subscription().map(Message::Tray),
            // 두 번째 실행은 종료되고, 대신 이 인스턴스의 창을 띄운다
            instance::subscription().map(|()| Message::Tray(TrayAction::Show)),
            // 창을 닫은 뒤 Dock 아이콘을 눌러도 창을 띄운다
            dock::subscription().map(|()| Message::Tray(TrayAction::Show)),
            // 에이전트가 MCP로 컨텍스트를 넣거나 고치면 목록과 목차를 다시 읽는다
            mcp::changes().map(|()| Message::StoreChanged),
        ])
    }

    pub fn selected_index(&self) -> Option<&Index> {
        let name = self.selected_index.as_deref()?;
        self.indexes.iter().find(|i| i.name == name)
    }

    /// 연결 관리를 닫으면 편집 중이던 컨텍스트로 돌아가는지
    pub fn returns_to_context(&self) -> bool {
        self.stashed_context.is_some()
    }

    pub fn context_of(&self, id: &str) -> Option<&Context> {
        self.rows.iter().find(|r| r.context.id == id).map(|r| &r.context)
    }

    fn refresh(&mut self) {
        self.rows = self
            .store
            .contexts()
            .into_iter()
            .map(|context| {
                let (summary, broken) = list_summary(&self.store, &context);
                Row { context, summary, broken }
            })
            .collect();
        self.indexes = self.store.indexes();
        self.connections = self.store.connections();
        if self.selected_index().is_none() {
            self.selected_index = None;
        }
    }

    fn toast(&mut self, message: impl Into<String>) -> Task<Message> {
        self.toast_seq += 1;
        let seq = self.toast_seq;
        self.toast = Some((seq, message.into()));
        Task::perform(async { std::thread::sleep(Duration::from_millis(2600)) }, move |()| Message::DismissToast(seq))
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WindowOpened(id) => {
                self.window = Some(id);
                // macOS는 이벤트 루프가 돈 뒤 메인 스레드에서 트레이를 만들어야 하므로
                // boot가 아니라 첫 창이 열린 시점에 생성한다
                if self.tray.is_none() {
                    self.tray = Some(tray::create());
                    dock::install();
                }
                self.refresh();
                return self.load_toc();
            }
            // 창이 닫혀도 데몬 프로세스와 상태는 유지된다
            Message::WindowClosed(id) => {
                if self.window == Some(id) {
                    self.window = None;
                }
            }
            Message::Tray(TrayAction::Show) => {
                return match self.window {
                    Some(id) => window::gain_focus(id),
                    None => open_window(),
                };
            }
            Message::Tray(TrayAction::Quit) => return iced::exit(),
            Message::StoreChanged => {
                self.refresh();
                return self.load_toc();
            }
            Message::LanguagePicked(lang) => {
                i18n::set(lang);
                if let Err(err) = self.store.set_language(lang) {
                    return self.toast(tr!("언어 설정을 저장하지 못했어요: {err}", "Couldn't save the language: {err}"));
                }
                // 트레이 메뉴는 만들 때 문장이 정해지니 새로 만든다
                if self.tray.is_some() {
                    self.tray = Some(tray::create());
                }
                // 요약·오류 문장과 오른쪽 목차도 새 언어로
                self.refresh();
                return self.load_toc();
            }
            Message::ToggleHelp(help) => {
                self.help = if self.help == Some(help) { None } else { Some(help) };
            }
            Message::DismissToast(seq) => {
                if self.toast.as_ref().is_some_and(|(s, _)| *s == seq) {
                    self.toast = None;
                }
            }

            // ── 저장소 ──
            Message::FilterSelected(filter) => self.filter = filter,
            Message::SearchChanged(value) => self.search = value,
            Message::NewContext => {
                self.sheet = Some(Sheet::Context(ContextEditor {
                    add_to_index: self.selected_index.is_some(),
                    ..Default::default()
                }));
            }
            Message::OpenContext(id) => {
                let Some(context) = self.store.context(&id) else {
                    self.refresh();
                    return Task::none();
                };
                self.sheet = Some(Sheet::Context(self.editor_for(&context)));
            }

            // ── 인덱스 ──
            // 접힘 목록: 펼친 인덱스를 다시 누르면 접는다. 펼친 인덱스가 '담기' 대상이다.
            Message::SelectIndex(name) => {
                self.selected_index = (self.selected_index.as_deref() != Some(name.as_str())).then_some(name);
                self.confirm_index_delete = false;
                self.rename_draft = None;
                self.description_draft = None;
                return self.load_toc();
            }
            Message::StartCreateIndex => {
                self.creating_index = true;
                self.new_index_name.clear();
                self.rename_draft = None;
                self.confirm_index_delete = false;
            }
            Message::CancelCreateIndex => self.creating_index = false,
            Message::NewIndexNameChanged(value) => self.new_index_name = value,
            Message::CreateIndex => return self.create_index(),
            Message::ToggleInIndex(id) => {
                return self.change_index(|index| {
                    if index.contexts.contains(&id) {
                        index.contexts.retain(|c| *c != id);
                    } else {
                        index.contexts.push(id);
                    }
                });
            }
            Message::MoveInIndex(position, delta) => {
                return self.change_index(|index| {
                    let target = position as i32 + delta;
                    if target >= 0 && (target as usize) < index.contexts.len() {
                        index.contexts.swap(position, target as usize);
                    }
                });
            }
            Message::CopyMcpUrl => {
                let copy = iced::clipboard::write(mcp::url(self.mcp_port));
                return Task::batch([copy, self.toast(i18n::t("주소를 복사했어요", "Address copied"))]);
            }
            Message::CopyMcpConfig => {
                let copy = iced::clipboard::write(mcp::config_snippet(self.mcp_port));
                return Task::batch([copy, self.toast(i18n::t("설정을 복사했어요", "Config copied"))]);
            }
            Message::ClaudeChecked(status) => {
                self.claude = match status {
                    claude::Status::Connected => ClaudeState::Connected,
                    claude::Status::NotConnected => ClaudeState::NotConnected,
                    claude::Status::Unavailable => ClaudeState::Unavailable,
                };
            }
            Message::ConnectClaude | Message::DisconnectClaude => {
                let connect = matches!(message, Message::ConnectClaude);
                let port = self.mcp_port;
                self.claude = ClaudeState::Working;
                return Task::perform(
                    async move { if connect { claude::connect(port) } else { claude::disconnect() } },
                    move |result| Message::ClaudeDone(connect, result),
                );
            }
            Message::ClaudeDone(connect, result) => {
                let toast = match (&result, connect) {
                    (Ok(()), true) => self.toast(i18n::t("Claude Code에 연결했어요. 새 세션부터 적용돼요", "Connected to Claude Code. Applies from new sessions")),
                    (Ok(()), false) => self.toast(i18n::t("연결을 해제했어요", "Disconnected")),
                    (Err(err), _) => self.toast(tr!("실패했어요: {err}", "Failed: {err}")),
                };
                let check = Task::perform(async { claude::status() }, Message::ClaudeChecked);
                return Task::batch([toast, check]);
            }
            Message::TocLoaded(name, result) => {
                if self.selected_index.as_deref() == Some(name.as_str()) {
                    self.toc = result.into();
                }
            }
            Message::StartRenameIndex => {
                self.confirm_index_delete = false;
                self.description_draft = None;
                self.rename_draft = self.selected_index.clone();
            }
            Message::RenameDraftChanged(value) => self.rename_draft = Some(value),
            Message::ConfirmRenameIndex => return self.rename_index(),
            Message::CancelRenameIndex => self.rename_draft = None,
            Message::StartDescribeIndex => {
                self.confirm_index_delete = false;
                self.rename_draft = None;
                self.description_draft = Some(self.selected_index().map(|i| i.description.clone()).unwrap_or_default());
            }
            Message::DescriptionDraftChanged(value) => self.description_draft = Some(value),
            Message::ConfirmDescribeIndex => {
                let Some(draft) = self.description_draft.take() else { return Task::none() };
                return self.change_index(|index| index.description = draft.trim().to_owned());
            }
            Message::CancelDescribeIndex => self.description_draft = None,
            Message::AskDeleteIndex => self.confirm_index_delete = true,
            Message::CancelDeleteIndex => self.confirm_index_delete = false,
            Message::ConfirmDeleteIndex => {
                self.confirm_index_delete = false;
                self.rename_draft = None;
                if let Some(name) = self.selected_index.take() {
                    let toast = match self.store.delete_index(&name) {
                        Ok(()) => self.toast(tr!("'{name}' 인덱스를 삭제했어요", "Deleted index '{name}'")),
                        Err(err) => self.toast(tr!("삭제하지 못했어요: {err}", "Couldn't delete: {err}")),
                    };
                    self.refresh();
                    return Task::batch([toast, self.load_toc()]);
                }
            }

            // ── 컨텍스트 시트 ──
            Message::KindSelected(kind) => self.edit(|e| {
                if e.id.is_none() {
                    e.kind = kind;
                }
            }),
            Message::TitleChanged(value) => self.edit(|e| e.title = value),
            Message::SummaryChanged(value) => self.edit(|e| e.summary = value),
            Message::BodyEdited(action) => self.edit(|e| e.body.perform(action)),
            Message::ImportPathChanged(value) => self.edit(|e| e.import_path = value),
            Message::ImportIntoEditor => return self.import_into_editor(),
            Message::PathChanged(value) => self.edit(|e| e.path = value),
            Message::ConnectionPicked(id) => self.edit(|e| e.connection = Some(id)),
            Message::CypherEdited(action) => self.edit(|e| e.cypher.perform(action)),
            Message::AddToIndexToggled(value) => self.edit(|e| e.add_to_index = value),
            Message::TogglePreview => {
                let mut open = false;
                self.edit(|e| {
                    e.preview_open = !e.preview_open;
                    open = e.preview_open;
                });
                if open {
                    return self.load_preview();
                }
            }
            Message::PreviewLoaded(id, result) => self.edit(|e| {
                if e.id.as_deref() == Some(id.as_str()) {
                    e.preview = result.into();
                }
            }),
            Message::ClearCachedResult => {
                if let Some(url) = self.editing_url() {
                    cache::clear_result(&self.store, &url);
                    self.reload_cache_info(&url);
                    return self.toast(i18n::t("캐시를 지웠어요", "Cache cleared"));
                }
            }
            Message::ClearAccessMethod => {
                if let Some(url) = self.editing_url() {
                    let toast = match cache::clear_method(&self.store, &url) {
                        Ok(()) => self.toast(i18n::t("접근 방식을 지웠어요", "Access method cleared")),
                        Err(err) => self.toast(tr!("지우지 못했어요: {err}", "Couldn't clear: {err}")),
                    };
                    self.reload_cache_info(&url);
                    return toast;
                }
            }
            Message::SaveContext => return self.save_context(),
            Message::AskDeleteContext => self.edit(|e| e.confirm_delete = true),
            Message::CancelDeleteContext => self.edit(|e| e.confirm_delete = false),
            Message::ConfirmDeleteContext => {
                let Some(Sheet::Context(editor)) = &self.sheet else { return Task::none() };
                let Some(id) = editor.id.clone() else { return Task::none() };
                let old_path = self.store.context(&id).and_then(|c| path_of(&c));
                let toast = match self.store.delete_context(&id) {
                    Ok(()) => {
                        if let Some(path) = old_path {
                            cache::forget_if_unused(&self.store, &path);
                        }
                        self.sheet = None;
                        self.toast(i18n::t("컨텍스트를 삭제했어요", "Context deleted"))
                    }
                    Err(err) => self.toast(tr!("삭제하지 못했어요: {err}", "Couldn't delete: {err}")),
                };
                self.refresh();
                return Task::batch([toast, self.load_toc()]);
            }
            Message::CloseSheet => {
                // 연결 관리에서 닫으면 편집하던 컨텍스트로 돌아간다
                self.sheet = match (self.sheet.take(), self.stashed_context.take()) {
                    (Some(Sheet::Connections(_)), Some(context)) => Some(Sheet::Context(context)),
                    _ => None,
                };
            }

            // ── 내보내기·가져오기 ──
            Message::OpenExport(index) => {
                let scope = match index {
                    Some(name) => Scope::Indexes(vec![name]),
                    None => Scope::All,
                };
                self.sheet = Some(Sheet::Export(ExportSheet {
                    scope: scope.clone(),
                    large: None,
                    attach_large: false,
                    include_secrets: false,
                    working: false,
                }));
                // 폴더 크기를 재는 동안 창이 멈추지 않게
                let store = self.store.clone();
                return Task::perform(async move { transfer::large_attachments(&store, &scope, transfer::LARGE_BYTES) }, Message::ExportScanned);
            }
            Message::ExportScanned(large) => {
                if let Some(Sheet::Export(sheet)) = &mut self.sheet {
                    sheet.large = Some(large);
                }
            }
            Message::ExportAttachLargeToggled(on) => {
                if let Some(Sheet::Export(sheet)) = &mut self.sheet {
                    sheet.attach_large = on;
                }
            }
            Message::ExportSecretsToggled(on) => {
                if let Some(Sheet::Export(sheet)) = &mut self.sheet {
                    sheet.include_secrets = on;
                }
            }
            Message::ChooseExportPath => return self.export(),
            Message::Exported(result) => {
                return match result {
                    Ok(exported) => {
                        self.sheet = None;
                        let mut message = tr!(
                            "내보냈어요: {} ({})",
                            "Exported: {} ({})",
                            exported.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                            transfer::human_bytes(exported.bytes)
                        );
                        let path_only = exported.skipped.len() + exported.missing.len();
                        if path_only > 0 {
                            message.push_str(&tr!(" · {path_only}개는 경로만", " · {path_only} path only"));
                        }
                        self.toast(message)
                    }
                    Err(err) => {
                        if let Some(Sheet::Export(sheet)) = &mut self.sheet {
                            sheet.working = false;
                        }
                        self.toast(err)
                    }
                };
            }
            Message::OpenImport => {
                // 파일 창은 메인 스레드에서 띄워야 한다 (macOS)
                let Some(path) = rfd::FileDialog::new()
                    .set_title(i18n::t("가져올 Octo 파일", "Octo file to import"))
                    .add_filter("Octo", &["zip"])
                    .pick_file()
                else {
                    return Task::none();
                };
                let store = self.store.clone();
                return Task::perform(
                    {
                        let path = path.clone();
                        async move { transfer::preview(&store, &path) }
                    },
                    move |plan| Message::ImportPreviewed(path.clone(), plan),
                );
            }
            Message::ImportPreviewed(path, result) => match result {
                Ok(plan) => self.sheet = Some(Sheet::Import(ImportSheet { path, plan, working: false, done: None })),
                Err(err) => return self.toast(err),
            },
            Message::ConfirmImport => {
                let Some(Sheet::Import(sheet)) = &mut self.sheet else { return Task::none() };
                sheet.working = true;
                let store = self.store.clone();
                let path = sheet.path.clone();
                return Task::perform(async move { transfer::import(&store, &path) }, Message::Imported);
            }
            Message::Imported(result) => {
                if let Some(Sheet::Import(sheet)) = &mut self.sheet {
                    sheet.working = false;
                }
                return match result {
                    Ok(imported) => {
                        if let Some(Sheet::Import(sheet)) = &mut self.sheet {
                            sheet.done = Some(imported);
                        }
                        self.refresh();
                        Task::batch([self.toast(i18n::t("가져왔어요", "Imported")), self.load_toc()])
                    }
                    Err(err) => self.toast(err),
                };
            }
            Message::UndoImport => {
                let Some(Sheet::Import(ImportSheet { done: Some(imported), working, .. })) = &mut self.sheet else {
                    return Task::none();
                };
                *working = true;
                let store = self.store.clone();
                let snapshot = imported.snapshot.clone();
                return Task::perform(async move { transfer::restore(&store, &snapshot) }, Message::Restored);
            }
            Message::Restored(result) => {
                return match result {
                    Ok(()) => {
                        self.sheet = None;
                        self.refresh();
                        Task::batch([self.toast(i18n::t("가져오기 전으로 되돌렸어요", "Restored to before the import")), self.load_toc()])
                    }
                    Err(err) => {
                        if let Some(Sheet::Import(sheet)) = &mut self.sheet {
                            sheet.working = false;
                        }
                        self.toast(err)
                    }
                };
            }

            // ── 연결 시트 ──
            Message::OpenConnections => {
                if let Some(Sheet::Context(editor)) = self.sheet.take() {
                    self.stashed_context = Some(editor);
                }
                self.sheet = Some(Sheet::Connections(ConnectionsSheet::default()));
            }
            Message::NewConnection => self.connections_sheet(|s| {
                s.editor = Some(ConnectionEditor {
                    uri: "http://localhost:7474".into(),
                    user: "neo4j".into(),
                    database: "neo4j".into(),
                    ..Default::default()
                });
                s.test = None;
            }),
            Message::SelectConnection(id) => {
                if let Some(connection) = self.store.connection(&id) {
                    let password = secret::get(self.store.root(), &id).unwrap_or_default();
                    self.connections_sheet(|s| {
                        s.editor = Some(ConnectionEditor {
                            id: Some(connection.id),
                            name: connection.name,
                            uri: connection.uri,
                            user: connection.user,
                            password,
                            database: connection.database,
                        });
                        s.test = None;
                    });
                }
            }
            Message::ConnectionNameChanged(v) => self.edit_connection(|e| e.name = v),
            Message::ConnectionUriChanged(v) => self.edit_connection(|e| e.uri = v),
            Message::ConnectionUserChanged(v) => self.edit_connection(|e| e.user = v),
            Message::ConnectionPasswordChanged(v) => self.edit_connection(|e| e.password = v),
            Message::ConnectionDatabaseChanged(v) => self.edit_connection(|e| e.database = v),
            Message::SaveConnection => return self.save_connection(),
            Message::DeleteConnection => return self.delete_connection(),
            Message::TestConnection => {
                let Some(Sheet::Connections(ConnectionsSheet { editor: Some(editor), .. })) = &self.sheet else {
                    return Task::none();
                };
                let connection = connection_from(editor, String::new());
                let password = editor.password.clone();
                self.connections_sheet(|s| s.test = Some(Loadable::Loading));
                return Task::perform(
                    async move { graph::run(&connection, &password, "RETURN 1 AS ok").map(|_| i18n::t("연결에 성공했어요", "Connected successfully").to_owned()) },
                    Message::ConnectionTested,
                );
            }
            Message::ConnectionTested(result) => self.connections_sheet(|s| s.test = Some(result.into())),
        }
        Task::none()
    }

    fn edit(&mut self, f: impl FnOnce(&mut ContextEditor)) {
        if let Some(Sheet::Context(editor)) = self.sheet.as_mut() {
            f(editor);
        }
    }

    fn connections_sheet(&mut self, f: impl FnOnce(&mut ConnectionsSheet)) {
        if let Some(Sheet::Connections(sheet)) = self.sheet.as_mut() {
            f(sheet);
        }
    }

    fn edit_connection(&mut self, f: impl FnOnce(&mut ConnectionEditor)) {
        self.connections_sheet(|s| {
            if let Some(editor) = s.editor.as_mut() {
                f(editor);
            }
        });
    }

    fn editor_for(&self, context: &Context) -> ContextEditor {
        let mut editor = ContextEditor {
            id: Some(context.id.clone()),
            kind: context.source.kind(),
            title: context.title.clone(),
            summary: context.summary.clone(),
            ..Default::default()
        };
        match &context.source {
            Source::Document => editor.body = text_editor::Content::with_text(&self.store.document(&context.id)),
            Source::Path { path } => {
                editor.path = path.clone();
                editor.cache = web::is_url(path).then(|| cache::info(&self.store, path));
            }
            Source::Graph { connection, cypher } => {
                editor.connection = Some(connection.clone());
                editor.cypher = text_editor::Content::with_text(cypher);
            }
        }
        editor
    }

    /// 편집 중인 링크 컨텍스트의 저장된 URL
    fn editing_url(&self) -> Option<String> {
        let Some(Sheet::Context(editor)) = &self.sheet else { return None };
        editor.id.as_deref().and_then(|id| self.store.context(id)).and_then(|c| path_of(&c)).filter(|p| web::is_url(p))
    }

    fn reload_cache_info(&mut self, url: &str) {
        let info = cache::info(&self.store, url);
        self.edit(|e| e.cache = Some(info));
    }

    fn load_preview(&mut self) -> Task<Message> {
        let Some(Sheet::Context(editor)) = self.sheet.as_mut() else { return Task::none() };
        let Some(context) = editor.id.as_deref().and_then(|id| self.store.context(id)) else {
            return Task::none();
        };
        editor.preview = Loadable::Loading;
        let store = self.store.clone();
        let id = context.id.clone();
        Task::perform(async move { content::load(&store, &context) }, move |result| {
            Message::PreviewLoaded(id.clone(), result)
        })
    }

    fn load_toc(&mut self) -> Task<Message> {
        // 오른쪽 목차는 펼친 인덱스를 세션이 get_index로 열었을 때 보는 모습
        let Some(name) = self.selected_index.clone() else {
            return Task::none();
        };
        self.toc = Loadable::Loading;
        let store = self.store.clone();
        let for_message = name.clone();
        Task::perform(async move { content::toc(&store, &name) }, move |result| {
            Message::TocLoaded(for_message.clone(), result)
        })
    }

    fn create_index(&mut self) -> Task<Message> {
        let name = self.new_index_name.trim().to_owned();
        if !store::is_valid_index_name(&name) {
            return self.toast(i18n::t("이름은 영문·숫자·-·_ 만 쓸 수 있어요", "Names can only use letters, numbers, - and _"));
        }
        if self.store.index(&name).is_some() {
            return self.toast(tr!("'{name}' 인덱스가 이미 있어요", "Index '{name}' already exists"));
        }
        if let Err(err) = self.store.save_index(&Index { name: name.clone(), contexts: Vec::new(), author: None, description: String::new() }) {
            return self.toast(tr!("만들지 못했어요: {err}", "Couldn't create it: {err}"));
        }
        self.new_index_name.clear();
        self.creating_index = false;
        self.refresh();
        self.selected_index = Some(name);
        self.confirm_index_delete = false;
        self.rename_draft = None;
        Task::batch([self.toast(tr!("'{}' 인덱스를 만들었어요", "Created index '{}'", self.selected_index.clone().unwrap_or_default())), self.load_toc()])
    }

    fn rename_index(&mut self) -> Task<Message> {
        let (Some(old), Some(draft)) = (self.selected_index.clone(), self.rename_draft.as_ref()) else {
            return Task::none();
        };
        let new = draft.trim().to_owned();
        if new == old {
            self.rename_draft = None;
            return Task::none();
        }
        if !store::is_valid_index_name(&new) {
            return self.toast(i18n::t("이름은 영문·숫자·-·_ 만 쓸 수 있어요", "Names can only use letters, numbers, - and _"));
        }
        if self.store.index(&new).is_some() {
            return self.toast(tr!("'{new}' 인덱스가 이미 있어요", "Index '{new}' already exists"));
        }
        if let Err(err) = self.store.rename_index(&old, &new) {
            return self.toast(tr!("이름을 바꾸지 못했어요: {err}", "Couldn't rename it: {err}"));
        }
        // 사람이 이름을 바꾸면 사람 것이 된다
        if let Some(mut index) = self.store.index(&new).filter(|i| i.author.is_some()) {
            index.author = None;
            let _ = self.store.save_index(&index);
        }
        self.rename_draft = None;
        self.refresh();
        self.selected_index = Some(new.clone());
        Task::batch([self.toast(tr!("'{old}' → '{new}'로 바꿨어요", "Renamed '{old}' → '{new}'")), self.load_toc()])
    }

    /// 인덱스는 저장 버튼 없이 바로 파일에 쓴다. 세션은 다음 MCP 호출부터 바뀐 목차를 본다.
    fn change_index(&mut self, f: impl FnOnce(&mut Index)) -> Task<Message> {
        let Some(mut index) = self.selected_index.as_deref().and_then(|n| self.store.index(n)) else {
            return Task::none();
        };
        f(&mut index);
        // 사람이 목차를 고치면 사람 것이 된다
        index.author = None;
        if let Err(err) = self.store.save_index(&index) {
            return self.toast(tr!("저장하지 못했어요: {err}", "Couldn't save: {err}"));
        }
        self.refresh();
        self.load_toc()
    }

    fn import_into_editor(&mut self) -> Task<Message> {
        let Some(Sheet::Context(editor)) = self.sheet.as_mut() else { return Task::none() };
        let path = std::path::PathBuf::from(editor.import_path.trim().trim_matches('"'));
        match std::fs::read_to_string(&path) {
            Ok(body) => {
                editor.body = text_editor::Content::with_text(&body);
                if editor.title.trim().is_empty() {
                    editor.title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                }
                editor.import_path.clear();
                self.toast(i18n::t("파일을 불러왔어요", "File loaded"))
            }
            Err(err) => self.toast(tr!("파일을 읽지 못했어요: {err}", "Couldn't read the file: {err}")),
        }
    }

    fn export(&mut self) -> Task<Message> {
        let Some(Sheet::Export(sheet)) = &mut self.sheet else { return Task::none() };
        let name = match &sheet.scope {
            Scope::All => "octo-all".to_owned(),
            Scope::Indexes(names) => names.join("+"),
        };
        // 파일 창은 메인 스레드에서 띄워야 한다 (macOS)
        let mut dialog = rfd::FileDialog::new()
            .set_title(i18n::t("내보낼 위치", "Export to"))
            .set_file_name(transfer::default_file_name(&name))
            .add_filter("Octo", &["zip"]);
        if let Some(dir) = dirs::download_dir() {
            dialog = dialog.set_directory(dir);
        }
        let Some(dest) = dialog.save_file() else { return Task::none() };

        sheet.working = true;
        let options = ExportOptions {
            include_secrets: sheet.include_secrets && sheet.scope == Scope::All,
            skip_large: !sheet.attach_large,
            ..ExportOptions::new(sheet.scope.clone())
        };
        let store = self.store.clone();
        Task::perform(async move { transfer::export(&store, &options, &dest) }, Message::Exported)
    }

    fn save_context(&mut self) -> Task<Message> {
        let Some(Sheet::Context(editor)) = &self.sheet else { return Task::none() };
        let title = editor.title.trim().to_owned();
        if title.is_empty() {
            return self.toast(i18n::t("제목을 입력해 주세요", "Please enter a title"));
        }
        let source = match editor.kind {
            Kind::Document => Source::Document,
            Kind::Path => {
                let path = editor.path.trim().trim_matches('"').to_owned();
                if path.is_empty() {
                    return self.toast(i18n::t("파일·폴더 경로나 링크를 입력해 주세요", "Please enter a file or folder path, or a link"));
                }
                Source::Path { path }
            }
            Kind::Graph => {
                let Some(connection) = editor.connection.clone() else {
                    return self.toast(i18n::t("연결을 선택해 주세요", "Please pick a connection"));
                };
                let cypher = editor.cypher.text().trim().to_owned();
                if cypher.is_empty() {
                    return self.toast(i18n::t("Cypher 쿼리를 입력해 주세요", "Please enter a Cypher query"));
                }
                Source::Graph { connection, cypher }
            }
        };
        let is_new = editor.id.is_none();
        let add_to_index = is_new && editor.add_to_index;
        let context = Context {
            id: editor.id.clone().unwrap_or_else(store::new_id),
            title,
            summary: editor.summary.trim().to_owned(),
            source,
            // 사람이 저장하면 사람 것이 된다
            author: None,
        };
        let body = editor.body.text();
        let old_path = editor.id.as_deref().and_then(|id| self.store.context(id)).and_then(|c| path_of(&c));

        let saved = self.store.save_context(&context).and_then(|()| match context.source {
            Source::Document => self.store.save_document(&context.id, &body),
            _ => Ok(()),
        });
        if let Err(err) = saved {
            return self.toast(tr!("저장하지 못했어요: {err}", "Couldn't save: {err}"));
        }
        // 링크를 바꿨으면 옛 링크의 조회 결과는 더 쓸 데가 없다
        if let Some(old) = old_path.filter(|old| path_of(&context).as_ref() != Some(old)) {
            cache::forget_if_unused(&self.store, &old);
        }

        self.sheet = None;
        let mut tasks = vec![];
        if add_to_index {
            let id = context.id.clone();
            tasks.push(self.change_index(|index| index.contexts.push(id)));
        } else {
            self.refresh();
            tasks.push(self.load_toc());
        }
        let message = match (add_to_index, &self.selected_index) {
            (true, Some(index)) => tr!("저장하고 '{index}'에 담았어요", "Saved and added to '{index}'"),
            _ => i18n::t("저장했어요", "Saved").to_owned(),
        };
        tasks.push(self.toast(message));
        Task::batch(tasks)
    }

    fn save_connection(&mut self) -> Task<Message> {
        let Some(Sheet::Connections(ConnectionsSheet { editor: Some(editor), .. })) = &self.sheet else {
            return Task::none();
        };
        if editor.name.trim().is_empty() || editor.uri.trim().is_empty() {
            return self.toast(i18n::t("이름과 주소를 입력해 주세요", "Please enter a name and address"));
        }
        let connection = connection_from(editor, editor.id.clone().unwrap_or_else(store::new_id));
        let password = editor.password.clone();
        if let Err(err) = self.store.save_connection(&connection) {
            return self.toast(tr!("저장하지 못했어요: {err}", "Couldn't save: {err}"));
        }
        secret::set(self.store.root(), &connection.id, &password);
        self.edit_connection(|e| e.id = Some(connection.id.clone()));
        // 컨텍스트 편집에서 왔다면 방금 만든 연결을 골라 둔다
        if let Some(stashed) = self.stashed_context.as_mut().filter(|c| c.connection.is_none()) {
            stashed.connection = Some(connection.id.clone());
        }
        self.refresh();
        self.toast(i18n::t("연결을 저장했어요", "Connection saved"))
    }

    fn delete_connection(&mut self) -> Task<Message> {
        let Some(Sheet::Connections(ConnectionsSheet { editor: Some(editor), .. })) = &self.sheet else {
            return Task::none();
        };
        let Some(id) = editor.id.clone() else { return Task::none() };
        let toast = match self.store.delete_connection(&id) {
            Ok(()) => {
                self.connections_sheet(|s| s.editor = None);
                self.toast(i18n::t("연결을 삭제했어요", "Connection deleted"))
            }
            Err(DeleteBlocked::InUse(users)) => {
                let titles: Vec<_> = users.iter().map(|c| c.title.as_str()).collect();
                self.toast(tr!("사용 중인 연결이에요: {}", "This connection is in use: {}", titles.join(", ")))
            }
            Err(DeleteBlocked::Io(err)) => self.toast(tr!("삭제하지 못했어요: {err}", "Couldn't delete: {err}")),
        };
        self.refresh();
        toast
    }
}

impl From<Result<String, String>> for Loadable {
    fn from(result: Result<String, String>) -> Self {
        match result {
            Ok(text) => Loadable::Ready(text),
            Err(err) => Loadable::Failed(err),
        }
    }
}

fn connection_from(editor: &ConnectionEditor, id: String) -> Connection {
    let database = editor.database.trim();
    Connection {
        id,
        name: editor.name.trim().to_owned(),
        uri: editor.uri.trim().to_owned(),
        user: editor.user.trim().to_owned(),
        database: if database.is_empty() { "neo4j".into() } else { database.to_owned() },
    }
}

/// 목록용 요약. 그래프는 쿼리를 돌려야 하므로 목록에서는 돌리지 않는다.
fn list_summary(store: &Store, context: &Context) -> (String, Option<String>) {
    match &context.source {
        Source::Graph { connection, .. } => {
            let broken = store.connection(connection).is_none().then(|| i18n::t("연결이 끊겼어요", "Connection is broken").to_owned());
            // 그래프 자동 요약은 쿼리를 돌려야 해서 목록에서는 비워 둔다
            (context.summary.trim().to_owned(), broken)
        }
        _ => {
            let entry = content::entry(store, context);
            (entry.summary, entry.broken.map(|_| i18n::t("원본을 찾을 수 없어요", "Can't find the source").to_owned()))
        }
    }
}

fn path_of(context: &Context) -> Option<String> {
    match &context.source {
        Source::Path { path } => Some(path.clone()),
        _ => None,
    }
}

fn open_window() -> Task<Message> {
    let (_, open) = window::open(window::Settings {
        size: iced::Size::new(1360.0, 820.0),
        min_size: Some(iced::Size::new(1160.0, 660.0)),
        icon: iced::window::icon::from_rgba(crate::icon::rgba(64), 64, 64).ok(),
        ..window::Settings::default()
    });
    open.map(Message::WindowOpened)
}
