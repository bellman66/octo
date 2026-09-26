//! neo4j HTTP 트랜잭션 API로 저장된 Cypher를 읽기 전용으로 실행한다.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use base64::Engine;
use serde_json::{Value, json};

use crate::core::store::Connection;
use crate::tr;

const MAX_ROWS: usize = 500;

pub struct GraphResult {
    /// 세션에 넘길 본문
    pub text: String,
    /// 자동 요약: 노드 수·타입, 관계 수
    pub summary: String,
}

pub fn run(connection: &Connection, password: &str, cypher: &str) -> Result<GraphResult, String> {
    let url = format!(
        "{}/db/{}/tx/commit",
        connection.uri.trim_end_matches('/'),
        connection.database
    );
    let auth = base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{}", connection.user, password));

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .into();

    let mut response = agent
        .post(&url)
        .header("Authorization", format!("Basic {auth}"))
        .header("Accept", "application/json")
        // neo4j 5: 쓰기 쿼리를 서버에서 거부하게 한다
        .header("access-mode", "READ")
        .send_json(json!({
            "statements": [{ "statement": cypher, "resultDataContents": ["row", "graph"] }]
        }))
        .map_err(|err| tr!("neo4j 요청 실패: {err}", "neo4j request failed: {err}"))?;

    let status = response.status();
    let body: Value = response
        .body_mut()
        .read_json()
        .map_err(|err| tr!("neo4j 응답 해석 실패 (HTTP {status}): {err}", "Couldn't parse neo4j response (HTTP {status}): {err}"))?;

    if let Some(error) = body["errors"].as_array().and_then(|errors| errors.first()) {
        return Err(tr!(
            "neo4j 오류 {}: {}",
            "neo4j error {}: {}",
            error["code"].as_str().unwrap_or("?"),
            error["message"].as_str().unwrap_or("")
        ));
    }
    if !status.is_success() {
        return Err(format!("neo4j HTTP {status}"));
    }

    Ok(render(&body["results"][0]))
}

fn render(result: &Value) -> GraphResult {
    let columns: Vec<&str> = result["columns"]
        .as_array()
        .map(|cols| cols.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let data = result["data"].as_array().map(Vec::as_slice).unwrap_or_default();

    let mut nodes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut relationships = BTreeSet::new();
    let mut lines = vec![columns.join(" | ")];

    for (i, item) in data.iter().enumerate() {
        if i < MAX_ROWS {
            lines.push(item["row"].to_string());
        }
        for node in item["graph"]["nodes"].as_array().into_iter().flatten() {
            let labels = node["labels"]
                .as_array()
                .map(|l| l.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default();
            nodes.insert(node["id"].to_string(), labels);
        }
        for rel in item["graph"]["relationships"].as_array().into_iter().flatten() {
            relationships.insert(rel["id"].to_string());
        }
    }
    if data.len() > MAX_ROWS {
        lines.push(tr!("… {}행 중 {MAX_ROWS}행만 표시", "… showing {MAX_ROWS} of {} rows", data.len()));
    }

    let mut by_label: BTreeMap<&str, usize> = BTreeMap::new();
    for labels in nodes.values() {
        for label in labels {
            *by_label.entry(label).or_default() += 1;
        }
    }
    let labels = by_label
        .iter()
        .map(|(label, count)| format!("{label} {count}"))
        .collect::<Vec<_>>()
        .join(", ");

    let summary = if nodes.is_empty() {
        tr!("{}행", "{} rows", data.len())
    } else {
        tr!("노드 {}개 ({labels}), 관계 {}개", "{} nodes ({labels}), {} relationships", nodes.len(), relationships.len())
    };

    GraphResult { text: lines.join("\n"), summary }
}
