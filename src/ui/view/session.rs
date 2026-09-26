//! ③ 세션 연결: MCP를 한 번만 연결하면, 세션은 기본 인덱스를 받는다.

use iced::widget::{button, column, container, row, space, text};
use iced::{Center, Element, Fill, Padding};

use crate::i18n;
use crate::mcp;
use crate::ui::app::{App, ClaudeState, Help, Loadable, Message};
use crate::ui::theme::{self, BOLD, MONO};
use crate::ui::widgets::{
    caption, card, empty_state, error, ghost_button, heading, help_box, primary_button, scroll, step_header, weak_button,
};

const HELP_KO: [&str; 4] = [
    "한 번만 연결하면 모든 AI 세션이 기본 인덱스를 받아요.",
    "· 세션에서 “frontend 인덱스로 바꿔줘”라고 하면 그 세션만 바뀌어요",
    "· 세션은 get_index로 목차를, load_context로 필요한 본문을 가져가요",
    "· 창을 닫아도 트레이에서 계속 동작해요",
];

const HELP_EN: [&str; 4] = [
    "Connect once and every AI session gets the default index.",
    "· Say “switch to the frontend index” in a session to change just that session",
    "· Sessions read the table of contents with get_index and fetch what they need with load_context",
    "· Keeps running in the tray after you close the window",
];

pub fn view(app: &App) -> Element<'_, Message> {
    let help_open = app.help == Some(Help::Session);
    let header = step_header("3", i18n::t("세션 연결", "Session"), None, help_open, Message::ToggleHelp(Help::Session), None);

    let mut body = column![header].spacing(14).height(Fill);
    if help_open {
        body = body.push(help_box(if i18n::is_en() { &HELP_EN } else { &HELP_KO }));
    }

    body = body.push(claude_card(app)).push(other_tools(app));

    body = match &app.default_index {
        None => body.push(empty_state(i18n::t("인덱스를 만들면 여기로 연결돼요", "Create an index and it connects here"), "")),
        Some(name) => {
            let toc: Element<'_, Message> = match &app.toc {
                Loadable::Loading => caption(i18n::t("불러오는 중…", "Loading…")),
                Loadable::Ready(body) => text(body.as_str()).size(12).font(MONO).color(theme::GREY700).into(),
                Loadable::Failed(err) => error(err.as_str()),
            };
            body.push(
                row![
                    heading(i18n::t("세션이 받는 인덱스", "Index sessions get")),
                    space::horizontal(),
                    container(text(format!("★ {name}")).size(13).font(BOLD).color(theme::BLUE))
                        .padding(Padding::from([4, 10]))
                        .style(theme::tinted(theme::BLUE_LIGHT, 999.0)),
                ]
                .align_y(Center),
            )
            .push(container(scroll(toc)).padding(12).height(Fill).width(Fill).style(theme::well))
        }
    };

    card(body).width(360).height(Fill).into()
}

/// Claude Code: 버튼 하나로 사용자 범위에 등록
fn claude_card(app: &App) -> Element<'_, Message> {
    let (dot, status) = match app.claude {
        ClaudeState::Checking => (theme::GREY400, i18n::t("확인 중", "Checking")),
        ClaudeState::Working => (theme::GREY400, i18n::t("처리 중", "Working")),
        ClaudeState::Connected => (theme::GREEN, i18n::t("연결됨", "Connected")),
        ClaudeState::NotConnected => (theme::GREY400, i18n::t("연결 안 됨", "Not connected")),
        ClaudeState::Unavailable => (theme::RED, i18n::t("claude CLI 없음", "No claude CLI")),
    };
    let action: Element<'_, Message> = match app.claude {
        ClaudeState::NotConnected => primary_button(i18n::t("한 번에 연결", "Connect in one click"), Message::ConnectClaude).width(Fill).into(),
        ClaudeState::Connected => ghost_button(i18n::t("연결 해제", "Disconnect"), Message::DisconnectClaude).into(),
        ClaudeState::Checking | ClaudeState::Working => button(text(i18n::t("잠시만요…", "One moment…")).size(15).font(BOLD))
            .padding(Padding::from([10, 18]))
            .width(Fill)
            .style(theme::primary)
            .into(),
        ClaudeState::Unavailable => caption(i18n::t("아래 주소를 직접 등록해 주세요", "Register the address below yourself")),
    };

    container(
        column![
            row![
                text("Claude Code").size(15).font(BOLD).color(theme::GREY900),
                space::horizontal(),
                text("●").size(10).color(dot),
                text(status).size(13).color(theme::GREY700),
            ]
            .spacing(6)
            .align_y(Center),
            action,
        ]
        .spacing(12),
    )
    .padding(16)
    .width(Fill)
    .style(theme::tinted(theme::BLUE_LIGHT, 16.0))
    .into()
}

/// 그 밖의 MCP 클라이언트: 주소 하나만 등록하면 된다
fn other_tools(app: &App) -> Element<'_, Message> {
    container(
        column![
            text(i18n::t("다른 도구", "Other tools")).size(14).font(BOLD).color(theme::GREY900),
            text(mcp::url(app.mcp_port)).size(12).font(MONO).color(theme::GREY700),
            row![weak_button(i18n::t("주소 복사", "Copy address"), Message::CopyMcpUrl), weak_button(i18n::t("JSON 복사", "Copy JSON"), Message::CopyMcpConfig)].spacing(6),
        ]
        .spacing(8),
    )
    .padding(16)
    .width(Fill)
    .style(theme::well)
    .into()
}
