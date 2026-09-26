//! 화면 곳곳에서 반복되는 작은 조각. 모양은 theme.rs, 조립은 여기서.

use iced::widget::{Column, button, column, container, row, scrollable, text};
use iced::{Center, Element, Fill, Length, Padding};

use super::theme::{self, BOLD};
use crate::core::store::Kind;

pub fn title<'a, M: 'a>(value: impl text::IntoFragment<'a>) -> Element<'a, M> {
    text(value).size(22).font(BOLD).color(theme::GREY900).into()
}

pub fn heading<'a, M: 'a>(value: impl text::IntoFragment<'a>) -> Element<'a, M> {
    text(value).size(17).font(BOLD).color(theme::GREY900).into()
}

pub fn caption<'a, M: 'a>(value: impl text::IntoFragment<'a>) -> Element<'a, M> {
    text(value).size(13).color(theme::GREY600).into()
}

pub fn error<'a, M: 'a>(value: impl text::IntoFragment<'a>) -> Element<'a, M> {
    text(value).size(13).color(theme::RED).into()
}

/// 흰 카드 영역
pub fn card<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> container::Container<'a, M> {
    container(content).padding(20).style(theme::card)
}

pub fn scroll<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    scrollable(content).style(theme::scroll).spacing(4).height(Fill).into()
}

/// 라벨 + 입력 + 도움말
pub fn field<'a, M: 'a>(label: &'a str, help: Option<&'a str>, input: impl Into<Element<'a, M>>) -> Element<'a, M> {
    let mut col = Column::new()
        .push(text(label).size(13).font(BOLD).color(theme::GREY700))
        .push(input)
        .spacing(6);
    if let Some(help) = help {
        col = col.push(caption(help));
    }
    col.width(Fill).into()
}

pub fn kind_badge<'a, M: 'a>(kind: Kind) -> Element<'a, M> {
    let (fg, bg) = match kind {
        Kind::Document => (theme::BLUE, theme::BLUE_LIGHT),
        Kind::Path => (theme::GREEN, theme::GREEN_LIGHT),
        Kind::Graph => (theme::PURPLE, theme::PURPLE_LIGHT),
    };
    // 폭을 고정해 목록에서 제목이 한 줄로 정렬되게 한다
    container(text(kind.label()).size(12).font(BOLD).color(fg))
        .center_x(Length::Fixed(52.0))
        .padding(Padding::from([3, 0]))
        .style(theme::tinted(bg, 6.0))
        .into()
}

/// 작은 아이콘형 버튼. 메시지가 없으면 비활성.
pub fn icon_button<'a, M: Clone + 'a>(glyph: &'a str, message: Option<M>) -> Element<'a, M> {
    button(container(text(glyph).size(14)).center(Length::Fixed(28.0)))
        .padding(0)
        .style(theme::ghost)
        .on_press_maybe(message)
        .into()
}

pub fn primary_button<'a, M: Clone + 'a>(label: &'a str, message: M) -> button::Button<'a, M> {
    button(text(label).size(15).font(BOLD)).padding(Padding::from([10, 18])).style(theme::primary).on_press(message)
}

pub fn weak_button<'a, M: Clone + 'a>(label: &'a str, message: M) -> button::Button<'a, M> {
    button(text(label).size(14).font(BOLD)).padding(Padding::from([8, 14])).style(theme::weak).on_press(message)
}

pub fn ghost_button<'a, M: Clone + 'a>(label: &'a str, message: M) -> button::Button<'a, M> {
    button(text(label).size(14)).padding(Padding::from([8, 14])).style(theme::ghost).on_press(message)
}

pub fn danger_button<'a, M: Clone + 'a>(label: &'a str, message: M) -> button::Button<'a, M> {
    button(text(label).size(14).font(BOLD)).padding(Padding::from([8, 14])).style(theme::danger).on_press(message)
}

pub fn chip<'a, M: Clone + 'a>(label: &'a str, selected: bool, message: M) -> Element<'a, M> {
    button(text(label).size(13).font(BOLD))
        .padding(Padding::from([6, 12]))
        .style(theme::chip(selected))
        .on_press(message)
        .into()
}

/// 비어 있을 때 다음 행동을 알려주는 안내
pub fn empty_state<'a, M: 'a>(headline: &'a str, description: &'a str) -> Element<'a, M> {
    container(
        column![
            text(headline).size(15).font(BOLD).color(theme::GREY700),
            text(description).size(13).color(theme::GREY500).align_x(Center),
        ]
        .spacing(6)
        .align_x(Center),
    )
    .center(Fill)
    .padding(24)
    .into()
}

/// 흐름 단계 머리: ① 제목 개수 (?) ……… 오른쪽 액션
pub fn step_header<'a, M: Clone + 'a>(
    step: &'a str,
    label: &'a str,
    count: Option<usize>,
    help_open: bool,
    on_help: M,
    action: Option<Element<'a, M>>,
) -> Element<'a, M> {
    let number = container(text(step).size(12).font(BOLD).color(theme::WHITE))
        .center(Length::Fixed(22.0))
        .style(theme::tinted(theme::BLUE, 999.0));
    let help = button(container(text("?").size(12).font(BOLD)).center(Length::Fixed(20.0)))
        .padding(0)
        .style(theme::help(help_open))
        .on_press(on_help);

    let mut header = row![number, heading(label)].spacing(8).align_y(Center);
    if let Some(count) = count {
        header = header.push(text(count.to_string()).size(17).font(BOLD).color(theme::BLUE));
    }
    header = header.push(help).push(iced::widget::space::horizontal());
    if let Some(action) = action {
        header = header.push(action);
    }
    header.into()
}

/// ? 를 눌렀을 때 펼쳐지는 설명
pub fn help_box<'a, M: 'a>(lines: &'a [&'a str]) -> Element<'a, M> {
    let mut col = column![].spacing(4);
    for line in lines {
        col = col.push(text(*line).size(13).color(theme::GREY700));
    }
    container(col).padding(14).width(Fill).style(theme::tinted(theme::BLUE_LIGHT, 12.0)).into()
}

/// 단계 사이 화살표
pub fn flow_arrow<'a, M: 'a>() -> Element<'a, M> {
    container(text("›").size(26).font(BOLD).color(theme::GREY400)).center_y(Fill).into()
}
