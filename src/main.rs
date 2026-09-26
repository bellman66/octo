#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Octo: 컨텍스트 저장소 + 인덱스 + 세션용 MCP를 제공하는 상주 앱.
//!
//! - `core`   컨텍스트 도메인 (저장소·본문·neo4j·비밀번호)
//! - `mcp`    AI 세션이 인덱스에 접근하는 HTTP MCP 서버
//! - `daemon` 창을 닫아도 살아 있는 상주 (중복 실행 방지·트레이)
//! - `ui`     한 화면에서 컨텍스트와 인덱스를 구성하는 GUI
//! - `i18n`   한국어/영어 표시

mod core;
mod daemon;
mod i18n;
mod icon;
mod mcp;
mod ui;

use crate::core::store::Store;
use crate::ui::App;

fn main() -> iced::Result {
    if !daemon::instance::acquire() {
        return Ok(());
    }

    let store = Store::open().expect("open ~/.octo");
    core::cache::sweep(&store);
    let config = store.config();
    i18n::set(config.language);
    let port = config.mcp_port;
    let mcp_status = mcp::spawn(store.clone(), port);

    iced::daemon(
        move || App::new(store.clone(), port, mcp_status.clone()),
        App::update,
        ui::view,
    )
    .title("Octo")
    .theme(ui::theme())
    .default_font(ui::FONT)
    .subscription(App::subscription)
    .run()
}
