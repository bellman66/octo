//! 컨텍스트 본문 로드와 목차(압축된 인덱스) 생성. GUI 미리보기와 MCP가 같은 함수를 쓴다.

use std::fs;
use std::path::Path;

use crate::core::cache;
use crate::core::graph;
use crate::core::secret;
use crate::core::store::{Context, Source, Store};
use crate::core::web;
use crate::i18n::t;
use crate::tr;

const SUMMARY_MAX: usize = 80;
const MAX_TREE_ENTRIES: usize = 2_000;
/// 폴더 목차와 내보내기 첨부에서 건너뛰는 폴더
pub(crate) const SKIP_DIRS: [&str; 4] = [".git", "target", "node_modules", ".idea"];

/// 세션이 받는 본문. 실패 사유는 사용자에게 그대로 보여줄 문장이다.
pub fn load(store: &Store, context: &Context) -> Result<String, String> {
    match &context.source {
        Source::Document => Ok(store.document(&context.id)),
        Source::Path { path } if web::is_url(path) => cache::load(store, path).map(cache::Loaded::into_text),
        Source::Path { path } => load_path(Path::new(path)),
        Source::Graph { connection, cypher } => {
            let connection = store
                .connection(connection)
                .ok_or(t("연결 끊김: 참조하던 연결이 없습니다", "Broken: the referenced connection no longer exists"))?;
            let password = secret::get(store.root(), &connection.id).unwrap_or_default();
            graph::run(&connection, &password, cypher).map(|result| result.text)
        }
    }
}

pub struct Entry {
    pub summary: String,
    /// 원본을 읽을 수 없으면 사유
    pub broken: Option<String>,
    /// 원본 대신 캐시본을 쓰고 있으면 그 출처
    pub cached: Option<String>,
}

/// 목차 한 줄. 사용자 요약이 있으면 그대로, 비었으면 원본에서 뽑는다.
pub fn entry(store: &Store, context: &Context) -> Entry {
    let user = context.summary.trim();

    let mut cached = None;
    let (auto, broken) = match &context.source {
        Source::Document => (first_line(&store.document(&context.id)), None),
        // 사용자 요약이 있으면 링크를 가져오지 않는다
        Source::Path { path } if web::is_url(path) && !user.is_empty() => (String::new(), None),
        Source::Path { path } if web::is_url(path) => match cache::load(store, path) {
            Ok(loaded) => {
                // 목차에는 출처 한 줄만. 갱신 안내는 본문을 열 때 붙는다
                cached = loaded.note.and_then(|note| note.lines().next().map(str::to_owned));
                (loaded.page.title.unwrap_or_else(|| first_line(&loaded.page.text)), None)
            }
            Err(err) => (String::new(), Some(err)),
        },
        Source::Path { path } => {
            let path = Path::new(path);
            if path.is_dir() {
                (tr!("폴더 · 항목 {}개", "Folder · {} entries", tree(path).len()), None)
            } else {
                match fs::read_to_string(path) {
                    Ok(text) => (first_line(&text), None),
                    Err(err) => (String::new(), Some(tr!("연결 끊김: {err}", "Broken: {err}"))),
                }
            }
        }
        Source::Graph { connection, cypher } => match store.connection(connection) {
            None => (String::new(), Some(t("연결 끊김: 참조하던 연결이 없습니다", "Broken: the referenced connection no longer exists").into())),
            // 사용자 요약이 있으면 쿼리를 돌리지 않는다
            Some(_) if !user.is_empty() => (String::new(), None),
            Some(connection) => {
                let password = secret::get(store.root(), &connection.id).unwrap_or_default();
                match graph::run(&connection, &password, cypher) {
                    Ok(result) => (result.summary, None),
                    Err(err) => (String::new(), Some(err)),
                }
            }
        },
    };

    Entry {
        summary: if user.is_empty() { auto } else { user.to_owned() },
        broken,
        cached,
    }
}

/// `get_index`가 돌려주는 목차.
pub fn toc(store: &Store, index_name: &str) -> Result<String, String> {
    let index = store
        .index(index_name)
        .ok_or_else(|| tr!("인덱스 없음: '{index_name}'", "No index '{index_name}'"))?;

    let mut out = tr!(
        "# 컨텍스트 인덱스: {}\n\n아래는 목차입니다. 필요한 항목만 `load_context`에 id를 넘겨 본문을 가져오세요.\n\n",
        "# Context index: {}\n\nTable of contents below. Pass an id to `load_context` to get only the items you need.\n\n",
        index.name
    );
    let contexts: Vec<Context> = index.contexts.iter().filter_map(|id| store.context(id)).collect();
    if contexts.is_empty() {
        out.push_str(t("(비어 있음)\n", "(empty)\n"));
    }
    for (i, context) in contexts.iter().enumerate() {
        let entry = entry(store, context);
        out.push_str(&format!(
            "{}. [{}] {} — {}\n   id: {}\n",
            i + 1,
            context.source.kind().label(),
            context.title,
            if entry.summary.is_empty() { t("(요약 없음)", "(no summary)") } else { &entry.summary },
            context.id,
        ));
        if let Source::Path { path } = &context.source {
            out.push_str(&format!("   path: {path}\n"));
        }
        if let Some(author) = &context.author {
            out.push_str(&tr!("   작성: 에이전트({author}) · update_context로 고칠 수 있음\n", "   author: agent ({author}) · editable with update_context\n"));
        }
        if let Some(status) = entry.broken.or(entry.cached) {
            // 힌트가 여러 줄이면 목록 들여쓰기에 맞춘다
            let status = status.replace('\n', "\n     ");
            out.push_str(&tr!("   상태: {status}\n", "   status: {status}\n"));
        }
    }
    Ok(out)
}

/// `load_context`: 이 인덱스에 속한 컨텍스트만 열어준다.
pub fn load_in_index(store: &Store, index_name: &str, id: &str) -> Result<String, String> {
    let index = store
        .index(index_name)
        .ok_or_else(|| tr!("인덱스 없음: '{index_name}'", "No index '{index_name}'"))?;
    if !index.contexts.iter().any(|c| c == id) {
        return Err(tr!("'{index_name}' 인덱스에 없는 컨텍스트: {id}", "Context not in index '{index_name}': {id}"));
    }
    let context = store.context(id).ok_or_else(|| tr!("컨텍스트 없음: {id}", "No context: {id}"))?;
    load(store, &context)
}

fn load_path(path: &Path) -> Result<String, String> {
    if path.is_dir() {
        let entries = tree(path);
        let mut out = tr!("폴더: {}\n", "Folder: {}\n", path.display());
        for entry in &entries {
            out.push_str(entry);
            out.push('\n');
        }
        if entries.len() >= MAX_TREE_ENTRIES {
            out.push_str(&tr!("… {MAX_TREE_ENTRIES}개까지만 표시\n", "… showing first {MAX_TREE_ENTRIES} only\n"));
        }
        return Ok(out);
    }
    fs::read_to_string(path).map_err(|err| tr!("연결 끊김: {} ({err})", "Broken: {} ({err})", path.display()))
}

/// 폴더 안의 상대 경로 목록. 빌드 산출물·VCS 폴더는 건너뛴다.
fn tree(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = read.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries.into_iter().rev() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = path.is_dir();
            if is_dir && SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            out.push(if is_dir { format!("{relative}/") } else { relative });
            if out.len() >= MAX_TREE_ENTRIES {
                out.sort();
                return out;
            }
            if is_dir {
                stack.push(path);
            }
        }
    }
    out.sort();
    out
}

fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    if line.chars().count() > SUMMARY_MAX {
        format!("{}…", line.chars().take(SUMMARY_MAX).collect::<String>())
    } else {
        line.to_owned()
    }
}
