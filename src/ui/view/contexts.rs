//! ① 컨텍스트: 저장소를 훑어보고, 고른 인덱스에 담는다.

use iced::widget::{button, column, container, row, space, text, text_input};
use iced::{Center, Element, Fill, Padding};

use crate::core::store::Kind;
use crate::i18n::{self, t};
use crate::ui::app::{App, Help, Message, Row};
use crate::ui::theme::{self, BOLD};
use crate::ui::widgets::{caption, card, chip, empty_state, error, help_box, kind_badge, primary_button, scroll, step_header};

const HELP_KO: [&str; 5] = [
    "AI 세션에 줄 자료를 보관하는 곳이에요.",
    "· 문서 — 직접 쓰거나 md 파일을 불러와 복사본으로 보관해요",
    "· 경로 — 파일·폴더나 외부 링크를 가리키고, 읽을 때마다 원본을 가져가요",
    "· 그래프 — neo4j에 Cypher를 읽기 전용으로 실행한 결과예요",
    "인덱스를 펼친 뒤 '담기'를 누르면 그 인덱스에 들어가요.",
];

const HELP_EN: [&str; 5] = [
    "Where you keep material for AI sessions.",
    "· Doc — write it or import an .md file; a copy is kept here",
    "· Path — points to a file, folder or link; the source is read every time",
    "· Graph — the result of a read-only Cypher query on neo4j",
    "Open an index, then press 'Add' to put a context into it.",
];

pub fn view(app: &App) -> Element<'_, Message> {
    let help_open = app.help == Some(Help::Contexts);
    let header = step_header(
        "1",
        t("컨텍스트", "Contexts"),
        Some(app.rows.len()),
        help_open,
        Message::ToggleHelp(Help::Contexts),
        Some(primary_button(t("+ 추가", "+ Add"), Message::NewContext).into()),
    );

    let mut filters = row![chip(t("전체", "All"), app.filter.is_none(), Message::FilterSelected(None))].spacing(6);
    for kind in Kind::ALL {
        filters = filters.push(chip(kind.label(), app.filter == Some(kind), Message::FilterSelected(Some(kind))));
    }
    let tools = row![
        filters,
        space::horizontal(),
        text_input(t("검색", "Search"), &app.search)
            .on_input(Message::SearchChanged)
            .padding(10)
            .size(14)
            .width(200)
            .style(theme::input),
    ]
    .spacing(8)
    .align_y(Center);

    let search = app.search.to_lowercase();
    let visible: Vec<&Row> = app
        .rows
        .iter()
        .filter(|r| app.filter.is_none_or(|kind| kind == r.context.source.kind()))
        .filter(|r| search.is_empty() || r.context.title.to_lowercase().contains(&search))
        .collect();

    let list: Element<'_, Message> = if app.rows.is_empty() {
        empty_state(t("비어 있어요", "Nothing here yet"), t("+ 추가로 시작해요", "Start with + Add"))
    } else if visible.is_empty() {
        empty_state(t("찾는 컨텍스트가 없어요", "No matching contexts"), "")
    } else {
        let mut list = column![].spacing(4);
        for row_data in visible {
            list = list.push(context_row(app, row_data));
        }
        scroll(list)
    };

    let mut body = column![header].spacing(14).height(Fill);
    if help_open {
        body = body.push(help_box(if i18n::is_en() { &HELP_EN } else { &HELP_KO }));
    }
    card(body.push(tools).push(list)).width(Fill).height(Fill).into()
}

fn context_row<'a>(app: &'a App, row_data: &'a Row) -> Element<'a, Message> {
    let context = &row_data.context;
    let mut info = column![text(&context.title).size(15).font(BOLD).color(theme::GREY900)]
        .spacing(3)
        .width(Fill);
    if !row_data.summary.is_empty() {
        info = info.push(caption(row_data.summary.as_str()));
    }
    if let Some(reason) = &row_data.broken {
        info = info.push(error(format!("⚠ {reason}")));
    }

    let mut line = row![kind_badge(context.source.kind()), info].spacing(12).align_y(Center);

    // 고른 인덱스에 담기/빼기 — 흐름의 다음 단계로 넘기는 버튼
    if let Some(index) = app.selected_index() {
        let included = index.contexts.contains(&context.id);
        line = line.push(
            button(text(if included { t("✓ 담김", "✓ Added") } else { t("담기 ›", "Add ›") }).size(13).font(BOLD))
                .padding(Padding::from([7, 14]))
                .style(if included { theme::primary } else { theme::weak })
                .on_press(Message::ToggleInIndex(context.id.clone())),
        );
    }

    button(container(line).padding(4))
        .padding(10)
        .width(Fill)
        .style(theme::list_item(false))
        .on_press(Message::OpenContext(context.id.clone()))
        .into()
}
