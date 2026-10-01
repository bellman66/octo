//! ② 인덱스: 모든 인덱스를 접힘 목록으로 보여준다. 누르면 그 자리에서 펼쳐져 이름·설명·내보내기·삭제와 담긴 순서를 다룬다.

use iced::widget::{button, column, container, row, space, text, text_input};
use iced::{Center, Element, Fill, Padding};

use crate::core::store::Index;
use crate::i18n::{self, t};
use crate::tr;
use crate::ui::app::{App, Help, Message};
use crate::ui::theme::{self, BOLD};
use crate::ui::widgets::{
    caption, card, danger_button, empty_state, error, ghost_button, help_box, icon_button, kind_badge, primary_button,
    scroll, step_header, weak_button,
};

const HELP_KO: [&str; 6] = [
    "세션에 넘길 컨텍스트 묶음이에요.",
    "· 인덱스를 누르면 펼쳐지고, 펼친 인덱스에 컨텍스트를 담아요",
    "· 세션은 메시지 주제에 맞는 인덱스를 그때그때 골라 열어요. 한 줄 설명이 고르는 근거예요",
    "· 위에서부터 순서대로 목차가 만들어져요",
    "· 바꾸면 연결된 세션에 바로 반영돼요",
    "· 이름은 세션 주소에 들어가서 영문·숫자·-·_ 만 써요",
];

const HELP_EN: [&str; 6] = [
    "A bundle of contexts to hand to a session.",
    "· Click an index to open it; contexts are added to the open index",
    "· Sessions pick the index that fits each message; the one-line description is how they choose",
    "· The table of contents follows this order, top to bottom",
    "· Changes reach connected sessions right away",
    "· Names go into the session address, so use only a-z, 0-9, - and _",
];

pub fn view(app: &App) -> Element<'_, Message> {
    let help_open = app.help == Some(Help::Index);
    let header = step_header(
        "2",
        t("인덱스", "Indexes"),
        Some(app.indexes.len()),
        help_open,
        Message::ToggleHelp(Help::Index),
        Some(primary_button(t("+ 만들기", "+ Create"), Message::StartCreateIndex).into()),
    );

    let mut body = column![header].spacing(14).height(Fill);
    if help_open {
        body = body.push(help_box(if i18n::is_en() { &HELP_EN } else { &HELP_KO }));
    }

    let mut list = column![].spacing(6);
    if app.creating_index {
        list = list.push(create_row(app));
    }
    for index in &app.indexes {
        list = list.push(index_item(app, index));
    }

    body = if app.indexes.is_empty() && !app.creating_index {
        body.push(empty_state(t("인덱스가 없어요", "No indexes yet"), t("+ 만들기로 시작해요", "Start with + Create")))
    } else {
        body.push(scroll(list))
    };

    card(body).width(360).height(Fill).into()
}

/// + 만들기를 누르면 목록 맨 위에 뜨는 이름 입력 줄
fn create_row(app: &App) -> Element<'_, Message> {
    let content = column![
        text_input(t("새 인덱스 이름 (영문·숫자·-·_)", "New index name (a-z, 0-9, -, _)"), &app.new_index_name)
            .on_input(Message::NewIndexNameChanged)
            .on_submit(Message::CreateIndex)
            .padding(10)
            .size(14)
            .style(theme::input),
        row![space::horizontal(), ghost_button(t("취소", "Cancel"), Message::CancelCreateIndex), weak_button(t("만들기", "Create"), Message::CreateIndex)]
            .spacing(4)
            .align_y(Center),
    ]
    .spacing(8);
    container(content).padding(12).width(Fill).style(theme::tinted(theme::BLUE_LIGHT, 14.0)).into()
}

/// 인덱스 한 줄. 펼쳐져 있으면 아래에 관리 줄과 담긴 순서가 붙는다.
fn index_item<'a>(app: &'a App, index: &'a Index) -> Element<'a, Message> {
    let open = app.selected_index.as_deref() == Some(index.name.as_str());

    let mut name = column![text(&index.name).size(15).font(BOLD).color(theme::GREY900)].spacing(2).width(Fill);
    if !index.description.is_empty() {
        name = name.push(caption(index.description.as_str()));
    }
    let line = row![
        text(if open { "▾" } else { "▸" }).size(13).color(theme::GREY500),
        name,
        text(tr!("{n}개", "{n}", n = index.contexts.len())).size(13).color(theme::GREY500),
    ]
    .spacing(8)
    .align_y(Center);

    let head = button(line)
        .padding(Padding::from([10, 12]))
        .width(Fill)
        .style(theme::list_item(open))
        .on_press(Message::SelectIndex(index.name.clone()));
    if !open {
        return head.into();
    }

    let panel = column![actions(app), order(app, index)].spacing(8);
    container(column![head, container(panel).padding(Padding { top: 4.0, right: 8.0, bottom: 10.0, left: 8.0 })])
        .style(theme::tinted(theme::GREY50, 14.0))
        .into()
}

/// 펼친 인덱스의 관리 줄: 이름 바꾸기 · 설명 · 내보내기 · 삭제. 이름이나 설명을 고치는 중이면 입력 줄로 바뀐다.
fn actions(app: &App) -> Element<'_, Message> {
    if let Some(draft) = &app.description_draft {
        return column![
            text_input(t("한 줄 설명 — 세션이 이 인덱스를 고르는 근거예요", "One-line description — how sessions pick this index"), draft)
                .on_input(Message::DescriptionDraftChanged)
                .on_submit(Message::ConfirmDescribeIndex)
                .padding(10)
                .size(14)
                .style(theme::input),
            row![space::horizontal(), ghost_button(t("취소", "Cancel"), Message::CancelDescribeIndex), weak_button(t("저장", "Save"), Message::ConfirmDescribeIndex)]
                .spacing(4)
                .align_y(Center),
        ]
        .spacing(8)
        .into();
    }
    if let Some(draft) = &app.rename_draft {
        return column![
            text_input(t("새 이름", "New name"), draft)
                .on_input(Message::RenameDraftChanged)
                .on_submit(Message::ConfirmRenameIndex)
                .padding(10)
                .size(14)
                .style(theme::input),
            row![space::horizontal(), ghost_button(t("취소", "Cancel"), Message::CancelRenameIndex), weak_button(t("저장", "Save"), Message::ConfirmRenameIndex)]
                .spacing(4)
                .align_y(Center),
        ]
        .spacing(8)
        .into();
    }

    let mut bar = row![ghost_button(t("✎ 이름 바꾸기", "✎ Rename"), Message::StartRenameIndex)].spacing(4).align_y(Center);
    bar = bar.push(ghost_button(t("✎ 설명", "✎ Description"), Message::StartDescribeIndex));
    if let Some(name) = &app.selected_index {
        bar = bar.push(ghost_button(t("⇪ 내보내기", "⇪ Export"), Message::OpenExport(Some(name.clone()))));
    }
    bar = bar.push(space::horizontal());
    bar = if app.confirm_index_delete {
        bar.push(ghost_button(t("취소", "Cancel"), Message::CancelDeleteIndex)).push(danger_button(t("삭제", "Delete"), Message::ConfirmDeleteIndex))
    } else {
        bar.push(ghost_button(t("삭제", "Delete"), Message::AskDeleteIndex))
    };
    bar.into()
}

/// 담긴 컨텍스트. 위에서부터 목차 순서.
fn order<'a>(app: &'a App, index: &'a Index) -> Element<'a, Message> {
    if index.contexts.is_empty() {
        return container(caption(t("비어 있어요 · ‹ 컨텍스트에서 '담기'", "Empty · use 'Add' in ‹ Contexts"))).padding(Padding::from([8, 4])).into();
    }
    let mut order = column![].spacing(2);
    for (position, id) in index.contexts.iter().enumerate() {
        let last = position + 1 == index.contexts.len();
        let (badge, name): (Element<'_, Message>, &str) = match app.context_of(id) {
            Some(context) => (kind_badge(context.source.kind()), context.title.as_str()),
            None => (error(t("없음", "Missing")), id.as_str()),
        };
        order = order.push(
            row![
                text((position + 1).to_string()).size(13).font(BOLD).color(theme::GREY500).width(18),
                badge,
                text(name).size(14).color(theme::GREY900).width(Fill),
                icon_button("↑", (position > 0).then_some(Message::MoveInIndex(position, -1))),
                icon_button("↓", (!last).then_some(Message::MoveInIndex(position, 1))),
                icon_button("✕", Some(Message::ToggleInIndex(id.clone()))),
            ]
            .spacing(8)
            .align_y(Center)
            .padding(Padding::from([4, 4])),
        );
    }
    order.into()
}
