//! 연결 관리 시트. 그래프 컨텍스트가 쓰는 neo4j 접속 정보.

use iced::widget::{button, column, container, row, space, text, text_input};
use iced::{Center, Element, Fill};

use crate::ui::app::{App, ConnectionsSheet, Loadable, Message};
use crate::i18n::t;
use crate::ui::theme::{self, BOLD};
use crate::ui::widgets::{
    caption, danger_button, empty_state, error, field, ghost_button, icon_button, primary_button, scroll, title,
    weak_button,
};

pub fn view<'a>(app: &'a App, sheet: &'a ConnectionsSheet) -> Element<'a, Message> {
    let header = row![
        title(t("연결 관리", "Connections")),
        space::horizontal(),
        icon_button("✕", Some(Message::CloseSheet)),
    ]
    .align_y(Center);

    let selected = sheet.editor.as_ref().and_then(|e| e.id.as_deref());
    let mut list = column![].spacing(4);
    for connection in &app.connections {
        let active = selected == Some(connection.id.as_str());
        list = list.push(
            button(
                column![
                    text(&connection.name).size(14).font(BOLD).color(if active { theme::BLUE } else { theme::GREY900 }),
                    caption(connection.uri.as_str()),
                ]
                .spacing(2),
            )
            .padding(12)
            .width(Fill)
            .style(theme::list_item(active))
            .on_press(Message::SelectConnection(connection.id.clone())),
        );
    }
    let list: Element<'_, Message> = if app.connections.is_empty() {
        empty_state(t("연결이 없어요", "No connections yet"), "")
    } else {
        scroll(list)
    };
    let left = column![list, weak_button(t("+ 새 연결", "+ New connection"), Message::NewConnection).width(Fill)]
        .spacing(8)
        .width(220)
        .height(Fill);

    let form: Element<'_, Message> = match &sheet.editor {
        None => empty_state(t("연결을 골라주세요", "Pick a connection"), ""),
        Some(editor) => {
            let input = |placeholder, value, on_input: fn(String) -> Message| {
                text_input(placeholder, value).on_input(on_input).padding(10).size(14).style(theme::input)
            };
            let mut actions = row![].spacing(8).align_y(Center);
            if editor.id.is_some() {
                actions = actions.push(danger_button(t("삭제", "Delete"), Message::DeleteConnection));
            }
            actions = actions
                .push(space::horizontal())
                .push(weak_button(t("연결 테스트", "Test connection"), Message::TestConnection))
                .push(primary_button(t("저장", "Save"), Message::SaveConnection));

            let test: Element<'_, Message> = match &sheet.test {
                None => space::vertical().height(0).into(),
                Some(Loadable::Loading) => caption(t("확인하는 중…", "Checking…")),
                Some(Loadable::Ready(ok)) => text(ok.as_str()).size(13).color(theme::GREEN).into(),
                Some(Loadable::Failed(err)) => error(err.as_str()),
            };

            column![
                field(t("이름", "Name"), None, input(t("예: local-neo4j", "e.g. local-neo4j"), &editor.name, Message::ConnectionNameChanged)),
                field(
                    t("HTTP 주소", "HTTP address"),
                    Some(t("bolt:// 가 아닌 HTTP 주소", "An HTTP address, not bolt://")),
                    input("http://localhost:7474", &editor.uri, Message::ConnectionUriChanged),
                ),
                row![
                    field(t("사용자", "User"), None, input("neo4j", &editor.user, Message::ConnectionUserChanged)),
                    field(t("데이터베이스", "Database"), None, input("neo4j", &editor.database, Message::ConnectionDatabaseChanged)),
                ]
                .spacing(12),
                field(
                    t("비밀번호", "Password"),
                    Some(t("OS 키체인에 보관돼요", "Stored in your OS keychain")),
                    text_input("", &editor.password)
                        .secure(true)
                        .on_input(Message::ConnectionPasswordChanged)
                        .padding(10)
                        .size(14)
                        .style(theme::input),
                ),
                actions,
                test,
            ]
            .spacing(14)
            .width(Fill)
            .into()
        }
    };

    let close = if app.returns_to_context() { t("컨텍스트로 돌아가기", "Back to context") } else { t("닫기", "Close") };

    container(
        column![
            header,
            row![left, form].spacing(20).height(Fill),
            row![space::horizontal(), ghost_button(close, Message::CloseSheet)],
        ]
        .spacing(20),
    )
    .padding(28)
    .width(760)
    .height(600)
    .style(theme::sheet)
    .into()
}
