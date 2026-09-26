//! 한 화면, 왼쪽에서 오른쪽으로 흐른다: ① 컨텍스트 › ② 인덱스 › ③ 세션 연결. 편집은 위에 뜨는 시트에서.
//!
//! ```text
//! ┌ [로고] Octo                             ● MCP 켜짐   연결 관리 ┐
//! ├ ① 컨텍스트 (?) [+ 추가] ─┐ › ┌ ② 인덱스 [+ 만들기]┐ › ┌ ③ 세션 연결 (?) ┐
//! │ [문서] 결제 정책  [담기 ›]│   │ ▾ backend ★ 3개    │   │ [한 번에 연결]   │
//! │ [경로] 소스 폴더  [✓ 담김]│   │   ✎ 이름 바꾸기 삭제│   │ 세션이 받는 목차 │
//! │                          │   │   1 결제 정책 ↑↓✕  │   │                  │
//! │                          │   │ ▸ front      1개   │   │                  │
//! └──────────────────────────┘   └────────────────┘   └──────────────────┘
//! ```

mod connection_sheet;
mod context_sheet;
mod contexts;
mod indexes;
mod session;

use iced::widget::{column, container, image, opaque, row, space, stack, text};
use iced::{Bottom, Center, Element, Fill, Padding, window};

use super::app::{App, Message, Sheet};
use super::theme;
use super::widgets::{chip, flow_arrow, ghost_button, title};
use crate::i18n::{self, Lang};
use crate::tr;

pub fn view(app: &App, _window: window::Id) -> Element<'_, Message> {
    let page = column![
        header(app),
        row![contexts::view(app), flow_arrow(), indexes::view(app), flow_arrow(), session::view(app)]
            .spacing(8)
            .height(Fill),
    ]
    .spacing(20);

    let base = container(page).padding(24).width(Fill).height(Fill).style(theme::tinted(theme::GREY100, 0.0));
    let mut layers = stack![base];

    if let Some(sheet) = &app.sheet {
        let content = match sheet {
            Sheet::Context(editor) => context_sheet::view(app, editor),
            Sheet::Connections(sheet) => connection_sheet::view(app, sheet),
        };
        layers = layers.push(opaque(container(content).center(Fill).padding(32).style(theme::backdrop)));
    }

    if let Some((_, message)) = &app.toast {
        layers = layers.push(
            container(container(text(message).size(14)).padding(Padding::from([12, 20])).style(theme::toast))
                .width(Fill)
                .height(Fill)
                .align_x(Center)
                .align_y(Bottom)
                .padding(32),
        );
    }

    layers.into()
}

fn header(app: &App) -> Element<'_, Message> {
    let (dot, status) = match &app.mcp_status {
        Ok(()) => (theme::GREEN, tr!("MCP 켜짐 · 127.0.0.1:{}", "MCP on · 127.0.0.1:{}", app.mcp_port)),
        Err(err) => (theme::RED, err.clone()),
    };
    let mcp = container(
        row![text("●").size(10).color(dot), text(status).size(13).color(theme::GREY700)]
            .spacing(8)
            .align_y(Center),
    )
    .padding(Padding::from([8, 14]))
    .style(theme::tinted(theme::WHITE, 999.0));

    row![
        image(app.logo.clone()).width(36).height(36),
        title("Octo"),
        space::horizontal(),
        mcp,
        language_picker(),
        ghost_button(i18n::t("연결 관리", "Connections"), Message::OpenConnections),
    ]
    .spacing(12)
    .align_y(Center)
    .into()
}

/// 한국어 | English
fn language_picker<'a>() -> Element<'a, Message> {
    let mut picker = row![].spacing(4);
    for lang in Lang::ALL {
        picker = picker.push(chip(lang.label(), i18n::lang() == lang, Message::LanguagePicked(lang)));
    }
    picker.into()
}
