//! `~/.octopuser/` 파일 저장소. 파일이 SSOT이고, GUI와 MCP가 같은 파일을 읽고 쓴다.
//!
//! ```text
//! ~/.octopuser/
//!   config.json
//!   contexts/<id>.json   메타
//!   contexts/<id>.md     문서형 본문
//!   indexes/<name>.json
//!   connections/<id>.json
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::i18n::{Lang, t};
use crate::tr;

pub const DEFAULT_MCP_PORT: u16 = 47_614;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub mcp_port: u16,
    /// MCP를 한 번 연결한 세션이 처음 받는 인덱스
    #[serde(default)]
    pub default_index: Option<String>,
    #[serde(default)]
    pub language: Lang,
}

impl Default for Config {
    fn default() -> Self {
        Self { mcp_port: DEFAULT_MCP_PORT, default_index: None, language: Lang::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
    /// 본문은 `contexts/<id>.md`
    Document,
    /// 원본을 항상 따라간다
    Path { path: String },
    /// 저장된 읽기 전용 Cypher
    Graph { connection: String, cypher: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Document,
    Path,
    Graph,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Document, Kind::Path, Kind::Graph];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Document => t("문서", "Doc"),
            Kind::Path => t("경로", "Path"),
            Kind::Graph => t("그래프", "Graph"),
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl Source {
    pub fn kind(&self) -> Kind {
        match self {
            Source::Document => Kind::Document,
            Source::Path { .. } => Kind::Path,
            Source::Graph { .. } => Kind::Graph,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Context {
    pub id: String,
    pub title: String,
    /// 사용자가 쓴 한 줄 요약. 비면 자동 추출한다.
    #[serde(default)]
    pub summary: String,
    pub source: Source,
    /// MCP로 에이전트가 만든 항목이면 그 클라이언트 이름. 사람이 앱에서 저장하면 지워지고,
    /// 그때부터는 에이전트가 `update_context`로 고칠 수 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub name: String,
    /// 목차 순서 그대로
    pub contexts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    pub name: String,
    /// neo4j HTTP 주소, 예: http://localhost:7474
    pub uri: String,
    pub user: String,
    #[serde(default = "default_database")]
    pub database: String,
}

impl std::fmt::Display for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

fn default_database() -> String {
    "neo4j".into()
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// `OCTO_HOME`이 있으면 그 경로, 없으면 `~/.octo`.
    /// 이름을 바꾸기 전의 `~/.octopuser`만 있으면 그대로 옮겨 쓴다.
    pub fn open() -> io::Result<Self> {
        if let Some(root) = std::env::var_os("OCTO_HOME") {
            return Self::at(PathBuf::from(root));
        }
        let home = dirs::home_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory not found"))?;
        let root = home.join(".octo");
        let legacy = home.join(".octopuser");
        if !root.exists() && legacy.is_dir() {
            fs::rename(&legacy, &root)?;
        }
        Self::at(root)
    }

    pub fn at(root: PathBuf) -> io::Result<Self> {
        for dir in ["contexts", "indexes", "connections", "cache/results"] {
            fs::create_dir_all(root.join(dir))?;
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config(&self) -> Config {
        read_json(&self.root.join("config.json")).unwrap_or_default()
    }

    pub fn save_config(&self, config: &Config) -> io::Result<()> {
        write_json(&self.root.join("config.json"), config)
    }

    /// 기본 인덱스. 지정한 것이 없거나 지워졌으면 이름순 첫 인덱스.
    pub fn default_index(&self) -> Option<String> {
        self.config()
            .default_index
            .filter(|name| self.index(name).is_some())
            .or_else(|| self.indexes().first().map(|i| i.name.clone()))
    }

    pub fn set_language(&self, language: Lang) -> io::Result<()> {
        let mut config = self.config();
        config.language = language;
        self.save_config(&config)
    }

    pub fn set_default_index(&self, name: &str) -> io::Result<()> {
        let mut config = self.config();
        config.default_index = Some(name.to_owned());
        self.save_config(&config)
    }

    // ── contexts ──

    pub fn contexts(&self) -> Vec<Context> {
        let mut contexts: Vec<Context> = list_json(&self.root.join("contexts"));
        contexts.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        contexts
    }

    pub fn context(&self, id: &str) -> Option<Context> {
        read_json(&self.context_path(id)).ok()
    }

    pub fn save_context(&self, context: &Context) -> io::Result<()> {
        write_json(&self.context_path(&context.id), context)
    }

    pub fn document(&self, id: &str) -> String {
        fs::read_to_string(self.document_path(id)).unwrap_or_default()
    }

    pub fn save_document(&self, id: &str, body: &str) -> io::Result<()> {
        write_atomic(&self.document_path(id), body.as_bytes())
    }

    /// 컨텍스트를 지우고, 이를 담고 있던 인덱스에서도 뺀다.
    pub fn delete_context(&self, id: &str) -> io::Result<()> {
        for mut index in self.indexes_using(id) {
            index.contexts.retain(|c| c != id);
            self.save_index(&index)?;
        }
        remove_if_exists(&self.document_path(id))?;
        remove_if_exists(&self.context_path(id))
    }

    pub fn indexes_using(&self, context_id: &str) -> Vec<Index> {
        self.indexes()
            .into_iter()
            .filter(|index| index.contexts.iter().any(|c| c == context_id))
            .collect()
    }

    fn context_path(&self, id: &str) -> PathBuf {
        self.root.join("contexts").join(format!("{id}.json"))
    }

    fn document_path(&self, id: &str) -> PathBuf {
        self.root.join("contexts").join(format!("{id}.md"))
    }

    // ── indexes ──

    pub fn indexes(&self) -> Vec<Index> {
        let mut indexes: Vec<Index> = list_json(&self.root.join("indexes"));
        indexes.sort_by(|a, b| a.name.cmp(&b.name));
        indexes
    }

    pub fn index(&self, name: &str) -> Option<Index> {
        if !is_valid_index_name(name) {
            return None;
        }
        read_json(&self.index_path(name)).ok()
    }

    pub fn save_index(&self, index: &Index) -> io::Result<()> {
        write_json(&self.index_path(&index.name), index)
    }

    pub fn delete_index(&self, name: &str) -> io::Result<()> {
        remove_if_exists(&self.index_path(name))
    }

    /// 새 이름으로 쓴 뒤 옛 파일을 지운다. 기본 인덱스였으면 기본 지정도 따라간다.
    pub fn rename_index(&self, old: &str, new: &str) -> io::Result<()> {
        let mut index = self
            .index(old)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, tr!("인덱스 없음: '{old}'", "No index '{old}'")))?;
        index.name = new.to_owned();
        self.save_index(&index)?;
        let mut config = self.config();
        if config.default_index.as_deref() == Some(old) {
            config.default_index = Some(new.to_owned());
            self.save_config(&config)?;
        }
        self.delete_index(old)
    }

    fn index_path(&self, name: &str) -> PathBuf {
        self.root.join("indexes").join(format!("{name}.json"))
    }

    // ── connections ──

    pub fn connections(&self) -> Vec<Connection> {
        let mut connections: Vec<Connection> = list_json(&self.root.join("connections"));
        connections.sort_by(|a, b| a.name.cmp(&b.name));
        connections
    }

    pub fn connection(&self, id: &str) -> Option<Connection> {
        read_json(&self.connection_path(id)).ok()
    }

    pub fn save_connection(&self, connection: &Connection) -> io::Result<()> {
        write_json(&self.connection_path(&connection.id), connection)
    }

    /// 그래프 컨텍스트가 참조 중이면 지우지 않고 그 컨텍스트들을 돌려준다.
    pub fn delete_connection(&self, id: &str) -> Result<(), DeleteBlocked> {
        let users = self.contexts_using_connection(id);
        if !users.is_empty() {
            return Err(DeleteBlocked::InUse(users));
        }
        crate::core::secret::delete(&self.root, id);
        remove_if_exists(&self.connection_path(id)).map_err(DeleteBlocked::Io)
    }

    pub fn contexts_using_connection(&self, id: &str) -> Vec<Context> {
        self.contexts()
            .into_iter()
            .filter(|c| matches!(&c.source, Source::Graph { connection, .. } if connection == id))
            .collect()
    }

    fn connection_path(&self, id: &str) -> PathBuf {
        self.root.join("connections").join(format!("{id}.json"))
    }
}

#[derive(Debug)]
pub enum DeleteBlocked {
    InUse(Vec<Context>),
    Io(io::Error),
}

/// 인덱스 이름은 MCP URL 쿼리와 파일명에 그대로 쓰이므로 제한한다.
pub fn is_valid_index_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{nanos:x}")
}

fn list_json<T: DeserializeOwned>(dir: &Path) -> Vec<T> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| read_json(&path).ok())
        .collect()
}

fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<T> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    write_atomic(path, &bytes)
}

/// MCP 스레드가 쓰는 도중의 파일을 읽지 않도록 임시 파일에 쓰고 교체한다.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> Store {
        let root = std::env::temp_dir().join(format!("octo-test-{name}-{}", new_id()));
        Store::at(root).unwrap()
    }

    fn doc(id: &str) -> Context {
        Context { id: id.into(), title: id.into(), summary: String::new(), source: Source::Document, author: None }
    }

    #[test]
    fn deleting_context_removes_it_from_indexes() {
        let store = temp_store("ctx");
        store.save_context(&doc("a")).unwrap();
        store.save_context(&doc("b")).unwrap();
        store.save_index(&Index { name: "x".into(), contexts: vec!["a".into(), "b".into()] }).unwrap();

        assert_eq!(store.indexes_using("a").len(), 1);
        store.delete_context("a").unwrap();

        assert!(store.context("a").is_none());
        assert_eq!(store.index("x").unwrap().contexts, vec!["b".to_string()]);
    }

    #[test]
    fn connection_in_use_cannot_be_deleted() {
        let store = temp_store("conn");
        let connection = Connection {
            id: "c".into(),
            name: "local".into(),
            uri: "http://localhost:7474".into(),
            user: "neo4j".into(),
            database: "neo4j".into(),
        };
        store.save_connection(&connection).unwrap();
        let graph = Context {
            source: Source::Graph { connection: "c".into(), cypher: "RETURN 1".into() },
            ..doc("g")
        };
        store.save_context(&graph).unwrap();

        assert!(matches!(store.delete_connection("c"), Err(DeleteBlocked::InUse(users)) if users.len() == 1));
        store.delete_context("g").unwrap();
        assert!(store.delete_connection("c").is_ok());
        assert!(store.connection("c").is_none());
    }

    #[test]
    fn rename_keeps_contexts_and_default() {
        let store = temp_store("rename");
        store.save_index(&Index { name: "old".into(), contexts: vec!["c1".into()] }).unwrap();
        store.save_index(&Index { name: "other".into(), contexts: vec![] }).unwrap();
        store.set_default_index("old").unwrap();

        store.rename_index("old", "new").unwrap();
        assert!(store.index("old").is_none());
        assert_eq!(store.index("new").unwrap().contexts, vec!["c1".to_owned()]);
        assert_eq!(store.default_index().as_deref(), Some("new"));
    }

    #[test]
    fn default_index_falls_back_to_first() {
        let store = temp_store("default");
        assert_eq!(store.default_index(), None);
        store.save_index(&Index { name: "b".into(), contexts: vec![] }).unwrap();
        store.save_index(&Index { name: "a".into(), contexts: vec![] }).unwrap();
        assert_eq!(store.default_index().as_deref(), Some("a"));

        store.set_default_index("b").unwrap();
        assert_eq!(store.default_index().as_deref(), Some("b"));
        // 지정했던 인덱스가 지워지면 다시 첫 인덱스
        store.delete_index("b").unwrap();
        assert_eq!(store.default_index().as_deref(), Some("a"));
    }

    #[test]
    fn index_names_are_url_safe() {
        assert!(is_valid_index_name("backend-v2_x"));
        assert!(!is_valid_index_name(""));
        assert!(!is_valid_index_name("a b"));
        assert!(!is_valid_index_name("../etc"));
        assert!(!is_valid_index_name("한글"));
    }
}
