//! 인덱스·컨텍스트를 zip 하나로 내보내고 가져온다. 팀 공유, 다른 PC 이전, 백업에 쓴다.
//!
//! zip 구조 (`FORMAT` 1):
//! - `manifest.json`  인덱스·컨텍스트·연결·링크 접근 방식 (전체 내보내기에서 고르면 비밀번호도)
//! - `docs/<id>.md`   문서 본문
//! - `files/<id>/<이름>…` 경로 컨텍스트가 가리키던 파일·폴더
//!
//! 경로는 홈 기준(`~/…`, 구분자 `/`)으로 적어 다른 PC·OS에서도 풀린다.
//! 가져오면 원본이 있는 경로는 원본을, 없으면 첨부본(`<root>/attachments/<id>/`)을 가리킨다.
//! 가져온 것은 모두 사람 소유가 되고, 가져오기 직전 상태는 `<root>/backups/`에 스냅샷으로 남는다.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::core::cache::{self, Method};
use crate::core::content::SKIP_DIRS;
use crate::core::secret;
use crate::core::store::{self, Connection, Context, Index, Source, Store};
use crate::core::web;
use crate::i18n::t;
use crate::tr;

pub const FORMAT: u32 = 1;
pub const EXTENSION: &str = "octo.zip";
/// 컨텍스트 하나의 첨부가 이보다 크면 사용자에게 묻는다
pub const LARGE_BYTES: u64 = 100 * 1024 * 1024;
const SNAPSHOTS_KEPT: usize = 5;
/// OS가 폴더마다 만드는 파일. 첨부에 넣지 않는다.
const SKIP_FILES: [&str; 3] = [".DS_Store", "Thumbs.db", "desktop.ini"];

#[derive(Debug, Clone, PartialEq)]
pub enum Scope {
    /// 이 인덱스들과 그 목차의 컨텍스트
    Indexes(Vec<String>),
    /// 모든 인덱스와 모든 컨텍스트
    All,
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub scope: Scope,
    /// 전체 내보내기에서만 쓴다. 평문으로 들어간다.
    pub include_secrets: bool,
    /// false면 경로만 적고 파일은 넣지 않는다 (스냅샷)
    pub attach: bool,
    /// true면 `large_limit`을 넘는 첨부는 빼고 경로만 남긴다
    pub skip_large: bool,
    pub large_limit: u64,
}

impl ExportOptions {
    pub fn new(scope: Scope) -> Self {
        Self { scope, include_secrets: false, attach: true, skip_large: true, large_limit: LARGE_BYTES }
    }
}

/// 첨부가 큰 컨텍스트
#[derive(Debug, Clone)]
pub struct Large {
    pub title: String,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub struct Exported {
    pub path: PathBuf,
    pub bytes: u64,
    pub indexes: usize,
    pub contexts: usize,
    pub attached: usize,
    /// 커서 경로만 남긴 것
    pub skipped: Vec<String>,
    /// 원본이 없어 첨부하지 못한 것
    pub missing: Vec<String>,
    pub secrets: usize,
}

/// 가져오기 미리보기. 실제 가져오기 결과도 같은 모양이다.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// (파일 속 이름, 가져온 뒤 이름)
    pub indexes: Vec<(String, String)>,
    pub new: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub connections: usize,
    /// 원본이 없어 첨부본을 풀 크기
    pub attach_bytes: u64,
    pub secrets: bool,
}

#[derive(Debug, Clone)]
pub struct Imported {
    pub plan: Plan,
    /// 되돌릴 때 쓰는 가져오기 직전 스냅샷
    pub snapshot: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    format: u32,
    exported_at: u64,
    indexes: Vec<Index>,
    contexts: Vec<Entry>,
    #[serde(default)]
    connections: Vec<Connection>,
    /// 연결 id → 비밀번호
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    secrets: BTreeMap<String, String>,
    /// 링크 → 에이전트가 보고한 접근 방식
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    methods: BTreeMap<String, Method>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    context: Context,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attached: Option<Attached>,
}

/// `files/<id>/<name>`에 든 첨부. 폴더면 그 아래에 내용이 이어진다.
#[derive(Serialize, Deserialize)]
struct Attached {
    name: String,
    dir: bool,
}

// ───────────────────────── 내보내기 ─────────────────────────

/// 내보내기 전에 사용자에게 물을 큰 첨부
pub fn large_attachments(store: &Store, scope: &Scope, limit: u64) -> Vec<Large> {
    let (_, contexts) = select(store, scope);
    contexts
        .iter()
        .filter_map(|c| local_path(c).map(|p| (c, attachment_size(Path::new(p)))))
        .filter(|(_, bytes)| *bytes > limit)
        .map(|(c, bytes)| Large { title: c.title.clone(), bytes })
        .collect()
}

pub fn export(store: &Store, options: &ExportOptions, dest: &Path) -> Result<Exported, String> {
    let (indexes, contexts) = select(store, &options.scope);
    if let Scope::Indexes(names) = &options.scope
        && let Some(missing) = names.iter().find(|n| !indexes.iter().any(|i| &i.name == *n))
    {
        return Err(tr!("인덱스 없음: '{missing}'", "No index '{missing}'"));
    }

    let used: BTreeSet<&str> = contexts
        .iter()
        .filter_map(|c| match &c.source {
            Source::Graph { connection, .. } => Some(connection.as_str()),
            _ => None,
        })
        .collect();
    let connections: Vec<Connection> = store
        .connections()
        .into_iter()
        .filter(|c| options.scope == Scope::All || used.contains(c.id.as_str()))
        .collect();
    let secrets: BTreeMap<String, String> = if options.include_secrets && options.scope == Scope::All {
        connections.iter().filter_map(|c| secret::get(store.root(), &c.id).map(|p| (c.id.clone(), p))).collect()
    } else {
        BTreeMap::new()
    };
    let methods: BTreeMap<String, Method> = contexts
        .iter()
        .filter_map(|c| match &c.source {
            Source::Path { path } if web::is_url(path) => cache::url_method(store, path).map(|m| (path.clone(), m)),
            _ => None,
        })
        .collect();

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|err| tr!("폴더를 만들지 못했어요: {err}", "Couldn't create the folder: {err}"))?;
    }
    let part = dest.with_extension("part");
    let result = write_zip(store, options, &part, &indexes, &contexts, connections, secrets, methods);
    let mut exported = match result {
        Ok(exported) => exported,
        Err(err) => {
            let _ = fs::remove_file(&part);
            return Err(err);
        }
    };
    fs::rename(&part, dest).map_err(|err| tr!("저장하지 못했어요: {err}", "Couldn't save: {err}"))?;
    exported.path = dest.to_path_buf();
    exported.bytes = fs::metadata(dest).map(|m| m.len()).unwrap_or_default();
    Ok(exported)
}

#[allow(clippy::too_many_arguments)]
fn write_zip(
    store: &Store,
    options: &ExportOptions,
    part: &Path,
    indexes: &[Index],
    contexts: &[Context],
    connections: Vec<Connection>,
    secrets: BTreeMap<String, String>,
    methods: BTreeMap<String, Method>,
) -> Result<Exported, String> {
    let file = File::create(part).map_err(|err| tr!("파일을 만들지 못했어요: {err}", "Couldn't create the file: {err}"))?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated).large_file(true);
    let mut exported = Exported {
        path: PathBuf::new(),
        bytes: 0,
        indexes: indexes.len(),
        contexts: contexts.len(),
        attached: 0,
        skipped: Vec::new(),
        missing: Vec::new(),
        secrets: secrets.len(),
    };

    let mut entries = Vec::new();
    for context in contexts {
        let mut attached = None;
        if let Source::Document = context.source {
            zip.start_file(format!("docs/{}.md", context.id), opts).map_err(zip_err)?;
            zip.write_all(store.document(&context.id).as_bytes()).map_err(zip_err)?;
        }
        if let Some(path) = local_path(context).filter(|_| options.attach) {
            let path = Path::new(path);
            if !path.exists() {
                exported.missing.push(context.title.clone());
            } else if options.skip_large && attachment_size(path) > options.large_limit {
                exported.skipped.push(context.title.clone());
            } else {
                attached = Some(attach(&mut zip, opts, &context.id, path)?);
                exported.attached += 1;
            }
        }
        let mut context = context.clone();
        if let Source::Path { path } = &mut context.source {
            *path = to_portable(path);
        }
        entries.push(Entry { context, attached });
    }

    let manifest = Manifest {
        format: FORMAT,
        exported_at: now(),
        indexes: indexes.to_vec(),
        contexts: entries,
        connections,
        secrets,
        methods,
    };
    zip.start_file("manifest.json", opts).map_err(zip_err)?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest).map_err(zip_err)?).map_err(zip_err)?;
    zip.finish().map_err(zip_err)?;
    Ok(exported)
}

fn attach(zip: &mut ZipWriter<File>, opts: SimpleFileOptions, id: &str, path: &Path) -> Result<Attached, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "root".into());
    let prefix = format!("files/{id}/{name}");
    if path.is_dir() {
        for (file, rel) in walk(path) {
            zip.start_file(format!("{prefix}/{rel}"), opts).map_err(zip_err)?;
            io::copy(&mut File::open(&file).map_err(zip_err)?, zip).map_err(zip_err)?;
        }
        Ok(Attached { name, dir: true })
    } else {
        zip.start_file(prefix, opts).map_err(zip_err)?;
        io::copy(&mut File::open(path).map_err(zip_err)?, zip).map_err(zip_err)?;
        Ok(Attached { name, dir: false })
    }
}

/// 범위에 드는 인덱스와 컨텍스트. 컨텍스트는 목차 순서대로 한 번씩.
fn select(store: &Store, scope: &Scope) -> (Vec<Index>, Vec<Context>) {
    match scope {
        Scope::All => (store.indexes(), store.contexts()),
        Scope::Indexes(names) => {
            let indexes: Vec<Index> = names.iter().filter_map(|n| store.index(n)).collect();
            let mut seen = BTreeSet::new();
            let contexts = indexes
                .iter()
                .flat_map(|i| i.contexts.iter())
                .filter(|id| seen.insert(id.as_str()))
                .filter_map(|id| store.context(id))
                .collect();
            (indexes, contexts)
        }
    }
}

/// 첨부할 수 있는 로컬 경로 (링크는 주소만 옮긴다)
fn local_path(context: &Context) -> Option<&str> {
    match &context.source {
        Source::Path { path } if !web::is_url(path) => Some(path),
        _ => None,
    }
}

fn attachment_size(path: &Path) -> u64 {
    if path.is_dir() {
        walk(path).iter().filter_map(|(file, _)| fs::metadata(file).ok()).map(|m| m.len()).sum()
    } else {
        fs::metadata(path).map(|m| m.len()).unwrap_or_default()
    }
}

/// 폴더 안 파일과 `/`로 이은 상대 경로. 목차와 같은 폴더와 OS 파일을 건너뛰고, 링크는 따라가지 않는다.
fn walk(root: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else { continue };
            let child = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            if kind.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push((entry.path(), child));
                }
            } else if kind.is_file() && !SKIP_FILES.contains(&name.as_str()) {
                out.push((entry.path(), child));
            }
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

// ───────────────────────── 가져오기 ─────────────────────────

pub fn preview(store: &Store, zip_path: &Path) -> Result<Plan, String> {
    let (mut archive, manifest) = open(zip_path)?;
    apply(store, &mut archive, &manifest, false)
}

pub fn import(store: &Store, zip_path: &Path) -> Result<Imported, String> {
    let (mut archive, manifest) = open(zip_path)?;
    let snapshot = snapshot(store)?;
    let plan = apply(store, &mut archive, &manifest, true)?;
    Ok(Imported { plan, snapshot })
}

/// 스냅샷(또는 전체 내보내기)으로 저장소를 통째로 되돌린다. 이름·소유권도 그대로.
pub fn restore(store: &Store, zip_path: &Path) -> Result<(), String> {
    let (mut archive, manifest) = open(zip_path)?;
    let fail = |err: io::Error| tr!("되돌리지 못했어요: {err}", "Couldn't restore: {err}");
    for context in store.contexts() {
        store.delete_context(&context.id).map_err(fail)?;
    }
    for index in store.indexes() {
        store.delete_index(&index.name).map_err(fail)?;
    }
    for connection in store.connections() {
        let _ = store.delete_connection(&connection.id);
    }
    for connection in &manifest.connections {
        store.save_connection(connection).map_err(fail)?;
    }
    for entry in &manifest.contexts {
        let mut context = entry.context.clone();
        if let Source::Path { path } = &mut context.source {
            *path = from_portable(path);
        }
        store.save_context(&context).map_err(fail)?;
        if let Source::Document = context.source {
            store.save_document(&context.id, &read_entry(&mut archive, &format!("docs/{}.md", context.id))?).map_err(fail)?;
        }
    }
    for index in &manifest.indexes {
        store.save_index(index).map_err(fail)?;
    }
    Ok(())
}

fn open(zip_path: &Path) -> Result<(ZipArchive<File>, Manifest), String> {
    let file = File::open(zip_path).map_err(|err| tr!("파일을 열지 못했어요: {err}", "Couldn't open the file: {err}"))?;
    let mut archive = ZipArchive::new(file).map_err(|_| t("Octo 내보내기 파일(zip)이 아니에요", "Not an Octo export file (zip)").to_owned())?;
    let manifest: Manifest = serde_json::from_str(&read_entry(&mut archive, "manifest.json")?)
        .map_err(|err| tr!("내보내기 파일이 손상됐어요: {err}", "The export file is damaged: {err}"))?;
    if manifest.format > FORMAT {
        return Err(t("더 새 버전의 Octo에서 만든 파일이에요. Octo를 업데이트해 주세요", "Made by a newer Octo. Please update Octo").into());
    }
    // 파일 속 id·이름은 경로가 되므로 믿지 않는다
    let bad = manifest
        .contexts
        .iter()
        .map(|e| e.context.id.as_str())
        .chain(manifest.connections.iter().map(|c| c.id.as_str()))
        .find(|id| !is_safe_id(id))
        .or_else(|| manifest.indexes.iter().map(|i| i.name.as_str()).find(|n| !store::is_valid_index_name(n)));
    if let Some(bad) = bad {
        return Err(tr!("쓸 수 없는 id·이름이 들어 있어요: {bad}", "Contains an unusable id or name: {bad}"));
    }
    Ok((archive, manifest))
}

/// `write`가 false면 바꿀 내용만 세고 아무것도 쓰지 않는다 (미리보기).
fn apply(store: &Store, archive: &mut ZipArchive<File>, manifest: &Manifest, write: bool) -> Result<Plan, String> {
    let fail = |err: io::Error| tr!("가져오지 못했어요: {err}", "Couldn't import: {err}");
    let mut plan = Plan { secrets: !manifest.secrets.is_empty(), ..Plan::default() };

    for connection in &manifest.connections {
        if store.connection(&connection.id).as_ref() != Some(connection) {
            plan.connections += 1;
            if write {
                store.save_connection(connection).map_err(fail)?;
            }
        }
        if write && let Some(password) = manifest.secrets.get(&connection.id) {
            secret::set(store.root(), &connection.id, password);
        }
    }

    for entry in &manifest.contexts {
        let mut context = entry.context.clone();
        // 받으면 사람 것
        context.author = None;
        let mut extract = None;
        if let Source::Path { path } = &mut context.source {
            let original = from_portable(path);
            *path = original.clone();
            if !web::is_url(&original)
                && !Path::new(&original).exists()
                && let Some(attached) = &entry.attached
            {
                let target = attachments_dir(store, &context.id).join(&attached.name);
                plan.attach_bytes += attached_bytes(archive, &context.id);
                *path = target.to_string_lossy().into_owned();
                extract = Some(attached);
            }
        }
        let body = match context.source {
            Source::Document => Some(read_entry(archive, &format!("docs/{}.md", context.id))?),
            _ => None,
        };

        let existing = store.context(&context.id);
        let same = existing.as_ref().is_some_and(|old| {
            old.title == context.title
                && old.summary == context.summary
                && old.source == context.source
                && body.as_ref().is_none_or(|b| store.document(&old.id) == *b)
        });
        match (&existing, same) {
            (_, true) => plan.unchanged += 1,
            (Some(_), false) => plan.updated += 1,
            (None, _) => plan.new += 1,
        }
        if !write {
            continue;
        }
        if extract.is_some() {
            extract_attachment(store, archive, &context.id)?;
        }
        if !same {
            store.save_context(&context).map_err(fail)?;
            if let Some(body) = &body {
                store.save_document(&context.id, body).map_err(fail)?;
            }
        }
    }

    if write {
        for (url, method) in &manifest.methods {
            let _ = cache::import_method(store, url, method);
        }
    }

    // 같은 이름 인덱스가 있으면 내 목차를 덮지 않고 새 이름으로 만든다
    let mut taken: BTreeSet<String> = store.indexes().into_iter().map(|i| i.name).collect();
    for index in &manifest.indexes {
        let name = unique_name(&index.name, &taken);
        taken.insert(name.clone());
        plan.indexes.push((index.name.clone(), name.clone()));
        if write {
            let contexts = index.contexts.iter().filter(|id| manifest.contexts.iter().any(|e| &e.context.id == *id)).cloned().collect();
            store.save_index(&Index { name, contexts, author: None, description: index.description.clone() }).map_err(fail)?;
        }
    }
    Ok(plan)
}

fn unique_name(name: &str, taken: &BTreeSet<String>) -> String {
    if !taken.contains(name) {
        return name.to_owned();
    }
    (2..)
        .map(|n| {
            let suffix = format!("-{n}");
            let base: String = name.chars().take(64 - suffix.len()).collect();
            format!("{base}{suffix}")
        })
        .find(|candidate| !taken.contains(candidate))
        .expect("infinite range")
}

fn attached_bytes(archive: &mut ZipArchive<File>, id: &str) -> u64 {
    let prefix = format!("files/{id}/");
    (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().filter(|f| f.name().starts_with(&prefix)).map(|f| f.size()))
        .sum()
}

fn extract_attachment(store: &Store, archive: &mut ZipArchive<File>, id: &str) -> Result<(), String> {
    let fail = |err: io::Error| tr!("첨부를 풀지 못했어요: {err}", "Couldn't extract the attachment: {err}");
    let dest = attachments_dir(store, id);
    let prefix = Path::new("files").join(id);
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(zip_err)?;
        // zip slip 방지: 안전한 상대 경로만 받는다
        let Some(name) = file.enclosed_name() else { continue };
        let Ok(rel) = name.strip_prefix(&prefix) else { continue };
        if file.is_dir() || rel.as_os_str().is_empty() {
            continue;
        }
        let target = dest.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(fail)?;
        }
        io::copy(&mut file, &mut File::create(&target).map_err(fail)?).map_err(fail)?;
    }
    Ok(())
}

fn read_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<String, String> {
    let mut file = archive.by_name(name).map_err(|_| tr!("내보내기 파일에 {name}이 없어요", "The export file has no {name}"))?;
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(zip_err)?;
    Ok(text)
}

/// 가져오기 직전 상태. 경로만 적어 가볍게 남기고, 최근 것만 둔다.
fn snapshot(store: &Store) -> Result<PathBuf, String> {
    let dir = store.root().join("backups");
    let dest = dir.join(format!("before-import-{}.{EXTENSION}", now()));
    let options = ExportOptions { attach: false, ..ExportOptions::new(Scope::All) };
    export(store, &options, &dest).map_err(|err| tr!("가져오기 전 스냅샷을 만들지 못했어요: {err}", "Couldn't snapshot before importing: {err}"))?;

    let mut snapshots: Vec<PathBuf> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("before-import-")))
        .collect();
    snapshots.sort();
    let excess = snapshots.len().saturating_sub(SNAPSHOTS_KEPT);
    for old in &snapshots[..excess] {
        let _ = fs::remove_file(old);
    }
    Ok(dest)
}

fn attachments_dir(store: &Store, id: &str) -> PathBuf {
    store.root().join("attachments").join(id)
}

// ───────────────────────── 경로 ─────────────────────────

/// 홈 아래 경로면 `~/a/b`로 바꾼다. 다른 PC·OS의 홈에서도 풀린다.
pub fn to_portable(path: &str) -> String {
    match dirs::home_dir() {
        Some(home) => portable_with(&home, path),
        None => path.to_owned(),
    }
}

/// `~/a/b`를 이 PC의 홈 경로로 푼다. 그 밖의 경로는 그대로.
pub fn from_portable(path: &str) -> String {
    match dirs::home_dir() {
        Some(home) => local_with(&home, path),
        None => path.to_owned(),
    }
}

fn portable_with(home: &Path, path: &str) -> String {
    if web::is_url(path) {
        return path.to_owned();
    }
    match Path::new(path).strip_prefix(home) {
        Ok(rest) => {
            let parts: Vec<String> = rest.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            if parts.is_empty() { "~".into() } else { format!("~/{}", parts.join("/")) }
        }
        Err(_) => path.to_owned(),
    }
}

fn local_with(home: &Path, path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => rest.split('/').filter(|p| !p.is_empty()).fold(home.to_path_buf(), |acc, p| acc.join(p)).to_string_lossy().into_owned(),
        None if path == "~" => home.to_string_lossy().into_owned(),
        None => path.to_owned(),
    }
}

// ───────────────────────── 기타 ─────────────────────────

/// `~/Downloads/<이름>-<날짜>.octo.zip`
pub fn default_export_path(name: &str) -> PathBuf {
    let dir = dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from("."));
    dir.join(default_file_name(name))
}

pub fn default_file_name(name: &str) -> String {
    format!("{name}-{}.{EXTENSION}", today())
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn zip_err(err: impl std::fmt::Display) -> String {
    tr!("zip 처리 중 오류: {err}", "zip error: {err}")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}

/// UTC 기준 `YYYY-MM-DD`
fn today() -> String {
    let days = (now() / 86_400) as i64;
    // Howard Hinnant의 civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

pub fn human_bytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= MB {
        format!("{:.1}MB", bytes as f64 / MB)
    } else {
        format!("{}KB", bytes.div_ceil(1024))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("octo-transfer-{name}-{}", store::new_id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn doc(store: &Store, id: &str, body: &str, author: Option<&str>) {
        let context = Context { id: id.into(), title: id.into(), summary: String::new(), source: Source::Document, author: author.map(str::to_owned) };
        store.save_context(&context).unwrap();
        store.save_document(id, body).unwrap();
    }

    fn path_ctx(store: &Store, id: &str, path: &Path) {
        let context = Context {
            id: id.into(),
            title: id.into(),
            summary: String::new(),
            source: Source::Path { path: path.to_string_lossy().into_owned() },
            author: None,
        };
        store.save_context(&context).unwrap();
    }

    #[test]
    fn paths_become_home_relative_and_back() {
        let home = Path::new("/Users/youn");
        assert_eq!(portable_with(home, "/Users/youn/Downloads/a b/x.pdf"), "~/Downloads/a b/x.pdf");
        assert_eq!(portable_with(home, "/opt/data"), "/opt/data");
        assert_eq!(portable_with(home, "https://example.com/Users/youn"), "https://example.com/Users/youn");
        let other = Path::new("/home/kim");
        assert_eq!(local_with(other, "~/Downloads/a b/x.pdf"), "/home/kim/Downloads/a b/x.pdf");
        assert_eq!(local_with(other, "/opt/data"), "/opt/data");
    }

    #[test]
    fn index_export_roundtrips_with_attachments_when_originals_are_gone() {
        let work = temp("roundtrip");
        let file = work.join("spec.md");
        fs::write(&file, "API 규격").unwrap();
        let folder = work.join("samples");
        fs::create_dir_all(folder.join("nested")).unwrap();
        fs::write(folder.join("nested/a.json"), "{}").unwrap();
        fs::create_dir_all(folder.join(".git")).unwrap();
        fs::write(folder.join(".git/HEAD"), "skip me").unwrap();
        fs::write(folder.join(".DS_Store"), "skip me").unwrap();

        let from = Store::at(temp("from")).unwrap();
        doc(&from, "d", "# 결정\n7일 환불", Some("claude-code"));
        path_ctx(&from, "f", &file);
        path_ctx(&from, "s", &folder);
        doc(&from, "outside", "not exported", None);
        from.save_index(&Index { name: "mydata".into(), contexts: vec!["d".into(), "f".into(), "s".into()], author: Some("claude-code".into()), description: String::new() }).unwrap();

        let zip = work.join("out.octo.zip");
        let exported = export(&from, &ExportOptions::new(Scope::Indexes(vec!["mydata".into()])), &zip).unwrap();
        assert_eq!((exported.contexts, exported.attached), (3, 2));

        // 받는 쪽엔 원본이 없다
        fs::remove_dir_all(&folder).unwrap();
        fs::remove_file(&file).unwrap();
        let to = Store::at(temp("to")).unwrap();
        let plan = preview(&to, &zip).unwrap();
        assert_eq!((plan.new, plan.updated, plan.unchanged), (3, 0, 0));
        assert!(to.contexts().is_empty(), "preview must not write");

        import(&to, &zip).unwrap();
        let index = to.index("mydata").unwrap();
        assert_eq!(index.contexts, vec!["d", "f", "s"]);
        assert!(index.author.is_none());
        let d = to.context("d").unwrap();
        assert!(d.author.is_none(), "imported items belong to the person");
        assert_eq!(to.document("d"), "# 결정\n7일 환불");
        assert!(to.context("outside").is_none());

        let Source::Path { path } = to.context("f").unwrap().source else { panic!() };
        assert_eq!(fs::read_to_string(&path).unwrap(), "API 규격");
        assert!(path.starts_with(to.root().to_string_lossy().as_ref()));
        let Source::Path { path } = to.context("s").unwrap().source else { panic!() };
        assert!(Path::new(&path).join("nested/a.json").exists());
        assert!(!Path::new(&path).join(".git").exists());
        assert!(!Path::new(&path).join(".DS_Store").exists());
    }

    #[test]
    fn existing_originals_are_used_and_reimport_is_idempotent() {
        let work = temp("idem");
        let file = work.join("keep.md");
        fs::write(&file, "원본").unwrap();
        let store = Store::at(temp("idem-store")).unwrap();
        path_ctx(&store, "f", &file);
        doc(&store, "d", "v1", None);
        store.save_index(&Index { name: "x".into(), contexts: vec!["f".into(), "d".into()], author: None, description: String::new() }).unwrap();
        let zip = work.join("x.octo.zip");
        export(&store, &ExportOptions::new(Scope::Indexes(vec!["x".into()])), &zip).unwrap();

        // 같은 저장소로 다시 가져오면: 컨텍스트는 그대로, 인덱스는 새 이름
        let plan = import(&store, &zip).unwrap().plan;
        assert_eq!((plan.new, plan.updated, plan.unchanged), (0, 0, 2));
        assert_eq!(plan.indexes, vec![("x".to_string(), "x-2".to_string())]);
        let Source::Path { path } = store.context("f").unwrap().source else { panic!() };
        assert_eq!(Path::new(&path), file, "original path kept when it exists");

        // 본문이 바뀐 걸 다시 가져오면 갱신
        store.save_document("d", "v2 local").unwrap();
        let plan = import(&store, &zip).unwrap().plan;
        assert_eq!(plan.updated, 1);
        assert_eq!(store.document("d"), "v1");
        assert!(store.index("x-3").is_some());
    }

    #[test]
    fn large_attachments_are_skipped_on_request() {
        let work = temp("large");
        let file = work.join("big.bin");
        fs::write(&file, vec![0u8; 2048]).unwrap();
        let store = Store::at(temp("large-store")).unwrap();
        path_ctx(&store, "big", &file);
        store.save_index(&Index { name: "x".into(), contexts: vec!["big".into()], author: None, description: String::new() }).unwrap();
        let scope = Scope::Indexes(vec!["x".into()]);
        assert_eq!(large_attachments(&store, &scope, 1024).len(), 1);

        let options = ExportOptions { large_limit: 1024, ..ExportOptions::new(scope.clone()) };
        let exported = export(&store, &options, &work.join("a.octo.zip")).unwrap();
        assert_eq!((exported.attached, exported.skipped.len()), (0, 1));
        let options = ExportOptions { large_limit: 1024, skip_large: false, ..ExportOptions::new(scope) };
        assert_eq!(export(&store, &options, &work.join("b.octo.zip")).unwrap().attached, 1);
    }

    #[test]
    fn secrets_only_travel_in_full_exports() {
        let work = temp("secrets");
        let store = Store::at(temp("secrets-store")).unwrap();
        let connection = Connection { id: "c1".into(), name: "local".into(), uri: "http://localhost:7474".into(), user: "neo4j".into(), database: "neo4j".into() };
        store.save_connection(&connection).unwrap();
        secret::set(store.root(), "c1", "pw");
        let graph = Context {
            id: "g".into(),
            title: "g".into(),
            summary: String::new(),
            source: Source::Graph { connection: "c1".into(), cypher: "RETURN 1".into() },
            author: None,
        };
        store.save_context(&graph).unwrap();
        store.save_index(&Index { name: "x".into(), contexts: vec!["g".into()], author: None, description: String::new() }).unwrap();

        let shared = ExportOptions { include_secrets: true, ..ExportOptions::new(Scope::Indexes(vec!["x".into()])) };
        assert_eq!(export(&store, &shared, &work.join("s.octo.zip")).unwrap().secrets, 0);
        let full = ExportOptions { include_secrets: true, ..ExportOptions::new(Scope::All) };
        let exported = export(&store, &full, &work.join("f.octo.zip")).unwrap();
        assert_eq!(exported.secrets, 1);
        secret::delete(store.root(), "c1");
    }

    #[test]
    fn restore_brings_back_the_snapshot_exactly() {
        let work = temp("restore");
        let store = Store::at(temp("restore-store")).unwrap();
        doc(&store, "a", "before", Some("claude-code"));
        store.save_index(&Index { name: "x".into(), contexts: vec!["a".into()], author: None, description: String::new() }).unwrap();

        let other = Store::at(temp("restore-other")).unwrap();
        doc(&other, "a", "after", None);
        doc(&other, "b", "new", None);
        other.save_index(&Index { name: "x".into(), contexts: vec!["a".into(), "b".into()], author: None, description: String::new() }).unwrap();
        let incoming = work.join("in.octo.zip");
        export(&other, &ExportOptions::new(Scope::All), &incoming).unwrap();

        let imported = import(&store, &incoming).unwrap();
        assert_eq!(store.document("a"), "after");
        assert!(store.index("x-2").is_some());

        restore(&store, &imported.snapshot).unwrap();
        assert_eq!(store.document("a"), "before");
        assert_eq!(store.context("a").unwrap().author.as_deref(), Some("claude-code"));
        assert!(store.context("b").is_none());
        assert!(store.index("x-2").is_none());
        assert_eq!(store.index("x").unwrap().contexts, vec!["a"]);
    }

    #[test]
    fn unsafe_ids_in_the_file_are_rejected() {
        let work = temp("unsafe");
        let zip = work.join("evil.octo.zip");
        let mut writer = ZipWriter::new(File::create(&zip).unwrap());
        writer.start_file("manifest.json", SimpleFileOptions::default()).unwrap();
        let manifest = json_manifest("../../etc");
        writer.write_all(manifest.as_bytes()).unwrap();
        writer.finish().unwrap();
        let store = Store::at(temp("unsafe-store")).unwrap();
        assert!(preview(&store, &zip).is_err());
    }

    fn json_manifest(id: &str) -> String {
        serde_json::json!({
            "format": 1, "exported_at": 0, "indexes": [],
            "contexts": [{ "context": { "id": id, "title": "t", "source": { "type": "document" } } }],
        })
        .to_string()
    }
}
