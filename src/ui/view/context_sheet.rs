//! 컨텍스트 추가·편집 시트.

use iced::widget::{button, checkbox, column, container, row, scrollable, space, text, text_editor, text_input};
use iced::{Center, Element, Fill, Padding};

use crate::core::cache;
use crate::core::store::Kind;
use crate::i18n::t;
use crate::tr;
use crate::ui::app::{App, ContextEditor, Loadable, Message};
use crate::ui::theme::{self, BOLD, MONO};
use crate::ui::widgets::{
    caption, danger_button, error, field, ghost_button, icon_button, kind_badge, primary_button, title, weak_button,
};

pub fn view<'a>(app: &'a App, editor: &'a ContextEditor) -> Element<'a, Message> {
    let is_new = editor.id.is_none();

    let header = row![
        title(if is_new { t("새 컨텍스트", "New context") } else { t("컨텍스트 편집", "Edit context") }),
        space::horizontal(),
        icon_button("✕", Some(Message::CloseSheet)),
    ]
    .align_y(Center);

    let kind: Element<'_, Message> = if is_new {
        field(t("종류", "Type"), None, kind_picker(editor.kind))
    } else {
        row![kind_badge(editor.kind)].into()
    };

    let mut body = column![
        kind,
        field(
            t("제목", "Title"),
            None,
            text_input(t("예: 결제 정책", "e.g. Payment policy"), &editor.title).on_input(Message::TitleChanged).padding(12).style(theme::input),
        ),
        field(
            t("한 줄 요약", "One-line summary"),
            Some(t("비워두면 원본에서 자동으로 뽑아요", "Leave empty to pull it from the source")),
            text_input(t("선택 사항", "Optional"), &editor.summary).on_input(Message::SummaryChanged).padding(12).style(theme::input),
        ),
        source_fields(app, editor),
    ]
    .spacing(18);

    if is_new && let Some(index) = &app.selected_index {
        body = body.push(
            checkbox(editor.add_to_index)
                .label(tr!("'{index}'에 담기", "Add to '{index}'"))
                .on_toggle(Message::AddToIndexToggled)
                .size(18)
                .text_size(14),
        );
    }

    if let Some(info) = &editor.cache {
        body = body.push(cache_panel(info));
    }

    if !is_new {
        let mut preview = column![ghost_button(
            if editor.preview_open { t("미리보기 접기", "Hide preview") } else { t("본문 미리보기", "Preview content") },
            Message::TogglePreview
        )]
        .spacing(8);
        if editor.preview_open {
            let content: Element<'_, Message> = match &editor.preview {
                Loadable::Loading => caption(t("불러오는 중…", "Loading…")),
                Loadable::Ready(text_body) => text(text_body.as_str()).size(12).font(MONO).color(theme::GREY700).into(),
                Loadable::Failed(err) => error(err.as_str()),
            };
            preview = preview.push(
                container(scrollable(content).style(theme::scroll).width(Fill))
                    .padding(12)
                    .height(180)
                    .width(Fill)
                    .style(theme::well),
            );
        }
        body = body.push(preview);
    }

    let mut footer = row![].spacing(8).align_y(Center);
    if !is_new {
        footer = if editor.confirm_delete {
            footer
                .push(text(t("인덱스에서도 함께 빠져요", "It will be removed from indexes too")).size(13).color(theme::RED))
                .push(ghost_button(t("취소", "Cancel"), Message::CancelDeleteContext))
                .push(danger_button(t("삭제", "Delete"), Message::ConfirmDeleteContext))
        } else {
            footer.push(ghost_button(t("삭제", "Delete"), Message::AskDeleteContext))
        };
    }
    footer = footer
        .push(space::horizontal())
        .push(ghost_button(t("닫기", "Close"), Message::CloseSheet))
        .push(primary_button(t("저장", "Save"), Message::SaveContext));

    container(
        column![
            header,
            // 머리와 바닥(저장 버튼)은 고정하고 본문만 스크롤한다
            scrollable(body.padding(Padding { right: 14.0, ..Padding::ZERO })).style(theme::scroll).height(Fill),
            footer,
        ]
        .spacing(20)
        .height(Fill),
    )
    .padding(28)
    .width(680)
    .height(Fill)
    .max_height(720)
    .style(theme::sheet)
    .into()
}

/// 링크의 캐시 상태: 조회 결과와 에이전트가 남긴 접근 방식
fn cache_panel(info: &cache::Info) -> Element<'_, Message> {
    let line = |label: &'static str, value: Element<'static, Message>, clear: Option<(&'static str, Message)>| {
        // 설명은 남은 폭 안에서 줄바꿈해 지우기 버튼이 밀려나지 않게 한다
        let mut r = row![text(label).size(13).font(BOLD).color(theme::GREY700).width(72), container(value).width(Fill)]
            .spacing(8)
            .align_y(Center);
        if let Some((label, message)) = clear {
            r = r.push(ghost_button(label, message));
        }
        r
    };

    let result = match &info.result {
        Some(label) => line(
            t("캐시", "Cache"),
            caption(label.clone()),
            Some((t("캐시 지우기", "Clear cache"), Message::ClearCachedResult)),
        ),
        None => line(t("캐시", "Cache"), caption(t("없음", "None")), None),
    };
    let method: Element<'_, Message> = match &info.method {
        Some((from, how)) => column![
            line(t("접근 방식", "Access"), caption(from.clone()), Some((t("방식 지우기", "Clear method"), Message::ClearAccessMethod))),
            text(how.as_str()).size(12).font(MONO).color(theme::GREY700),
        ]
        .spacing(6)
        .into(),
        None => line(t("접근 방식", "Access"), caption(t("보고된 방식 없음", "No method reported")), None).into(),
    };

    field(
        t("링크 캐시", "Link cache"),
        Some(t("에이전트가 report_access로 남긴 내용은 1일 동안만 쓰여요", "What agents leave with report_access is used for 1 day only")),
        container(column![result, method].spacing(10)).padding(12).width(Fill).style(theme::well),
    )
}

/// 종류 고르기: 무엇을 넣는지 설명과 함께
fn kind_picker<'a>(selected: Kind) -> Element<'a, Message> {
    let option = |kind: Kind, description: &'static str| {
        let active = kind == selected;
        button(
            column![
                row![kind_badge(kind)],
                text(description).size(13).color(if active { theme::GREY900 } else { theme::GREY600 }),
            ]
            .spacing(8),
        )
        .padding(14)
        .width(Fill)
        .style(move |theme_ref, status| {
            let mut style = theme::list_item(active)(theme_ref, status);
            style.border.width = if active { 1.5 } else { 1.0 };
            style.border.color = if active { theme::BLUE } else { theme::GREY200 };
            style
        })
        .on_press(Message::KindSelected(kind))
    };

    row![
        option(Kind::Document, t("직접 작성 · md 불러오기", "Write or import .md")),
        option(Kind::Path, t("파일·폴더·링크 원본 참조", "Link to a file, folder or URL")),
        option(Kind::Graph, t("neo4j Cypher 결과", "neo4j Cypher result")),
    ]
    .spacing(8)
    .into()
}

fn source_fields<'a>(app: &'a App, editor: &'a ContextEditor) -> Element<'a, Message> {
    match editor.kind {
        Kind::Document => column![
            field(
                t("본문", "Content"),
                None,
                text_editor(&editor.body)
                    .on_action(Message::BodyEdited)
                    .placeholder("Markdown")
                    .padding(12)
                    .height(200)
                    .style(theme::editor),
            ),
            row![
                text_input(t("md 파일 경로", ".md file path"), &editor.import_path)
                    .on_input(Message::ImportPathChanged)
                    .on_submit(Message::ImportIntoEditor)
                    .padding(10)
                    .size(14)
                    .style(theme::input),
                weak_button(t("불러오기", "Import"), Message::ImportIntoEditor),
            ]
            .spacing(6)
            .align_y(Center),
        ]
        .spacing(10)
        .into(),
        Kind::Path => field(
            t("파일·폴더 경로 또는 링크", "File, folder path or link"),
            Some(t("폴더면 파일 목록, 링크면 페이지 텍스트를 가져가요", "Folders give a file list, links give the page text")),
            text_input(t("예: C:\\workspace\\project\\docs 또는 https://…", "e.g. C:\\workspace\\project\\docs or https://…"), &editor.path)
                .on_input(Message::PathChanged)
                .padding(12)
                .style(theme::input),
        ),
        Kind::Graph => {
            let mut connections = row![].spacing(6).align_y(Center);
            for connection in &app.connections {
                let selected = editor.connection.as_deref() == Some(connection.id.as_str());
                connections = connections.push(
                    button(text(&connection.name).size(13).font(BOLD))
                        .padding(Padding::from([6, 12]))
                        .style(theme::chip(selected))
                        .on_press(Message::ConnectionPicked(connection.id.clone())),
                );
            }
            let connection_field: Element<'_, Message> = if app.connections.is_empty() {
                row![caption(t("연결 없음", "No connections")), weak_button(t("연결 추가", "Add connection"), Message::OpenConnections)]
                    .spacing(8)
                    .align_y(Center)
                    .into()
            } else {
                connections.push(ghost_button(t("연결 관리", "Connections"), Message::OpenConnections)).into()
            };
            let missing = editor
                .connection
                .as_ref()
                .filter(|id| !app.connections.iter().any(|c| &c.id == *id))
                .map(|_| error(t("연결이 삭제됐어요", "This connection was deleted")));

            let mut col = column![field(t("연결", "Connection"), None, connection_field)].spacing(10);
            if let Some(missing) = missing {
                col = col.push(missing);
            }
            col.push(field(
                t("Cypher 쿼리", "Cypher query"),
                Some(t("읽기 전용으로 실행돼요", "Runs read-only")),
                text_editor(&editor.cypher)
                    .on_action(Message::CypherEdited)
                    .placeholder("MATCH (d:domain)-[r]->(n) RETURN d, r, n LIMIT 50")
                    .font(MONO)
                    .padding(12)
                    .height(120)
                    .style(theme::editor),
            ))
            .into()
        }
    }
}
