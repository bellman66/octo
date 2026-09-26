//! 컨텍스트 도메인: 저장소, 본문 로드·목차, neo4j, 외부 링크와 그 캐시, 비밀번호 보관. UI·MCP가 공통으로 쓴다.

pub mod cache;
pub mod content;
pub mod graph;
pub mod secret;
pub mod store;
pub mod web;
