//! 토스 느낌의 흰색·파란색 디자인 토큰과 위젯 스타일. 화면 코드는 여기 있는 것만 쓴다.

use iced::border::Radius;
use iced::theme::Palette;
use iced::widget::{button, container, scrollable, text_editor, text_input};
use iced::{Background, Border, Color, Font, Shadow, Theme, Vector, color, font};

// ── 색 ──
pub const BLUE: Color = color!(0x3182F6);
pub const BLUE_DARK: Color = color!(0x1B64DA);
pub const BLUE_LIGHT: Color = color!(0xE8F3FF);
pub const WHITE: Color = Color::WHITE;
pub const GREY50: Color = color!(0xF9FAFB);
pub const GREY100: Color = color!(0xF2F4F6);
pub const GREY200: Color = color!(0xE5E8EB);
pub const GREY400: Color = color!(0xB0B8C1);
pub const GREY500: Color = color!(0x8B95A1);
pub const GREY600: Color = color!(0x6B7684);
pub const GREY700: Color = color!(0x4E5968);
pub const GREY900: Color = color!(0x191F28);
pub const RED: Color = color!(0xF04452);
pub const RED_LIGHT: Color = color!(0xFFEEEE);
pub const GREEN: Color = color!(0x03B26C);
pub const GREEN_LIGHT: Color = color!(0xE5F8EF);
pub const PURPLE: Color = color!(0x8B5CF6);
pub const PURPLE_LIGHT: Color = color!(0xF1EBFF);

// ── 글꼴 ──
#[cfg(target_os = "macos")]
const FAMILY: &str = "Apple SD Gothic Neo";
#[cfg(not(target_os = "macos"))]
const FAMILY: &str = "Malgun Gothic";

pub const FONT: Font = Font::with_name(FAMILY);
pub const BOLD: Font = Font { weight: font::Weight::Bold, ..FONT };
#[cfg(target_os = "macos")]
pub const MONO: Font = Font::with_name("Menlo");
#[cfg(not(target_os = "macos"))]
pub const MONO: Font = Font::with_name("Consolas");

// ── 크기 ──
pub const RADIUS_CARD: f32 = 20.0;
pub const RADIUS_CONTROL: f32 = 12.0;

pub fn theme() -> Theme {
    Theme::custom(
        "Toss",
        Palette {
            background: GREY100,
            text: GREY900,
            primary: BLUE,
            success: GREEN,
            warning: color!(0xFFB331),
            danger: RED,
        },
    )
}

fn border(radius: f32) -> Border {
    Border { radius: Radius::from(radius), ..Border::default() }
}

fn button_style(background: Option<Color>, text: Color, radius: f32) -> button::Style {
    button::Style {
        background: background.map(Background::Color),
        text_color: text,
        border: border(radius),
        shadow: Shadow::default(),
        snap: true,
    }
}

// ── 버튼 ──

/// 주요 행동 하나에만 쓰는 파란 버튼
pub fn primary(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered | button::Status::Pressed => button_style(Some(BLUE_DARK), WHITE, RADIUS_CONTROL),
        button::Status::Disabled => button_style(Some(color!(0xC9E2FF)), WHITE, RADIUS_CONTROL),
        button::Status::Active => button_style(Some(BLUE), WHITE, RADIUS_CONTROL),
    }
}

/// 보조 행동: 연한 파랑 바탕
pub fn weak(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered | button::Status::Pressed => button_style(Some(color!(0xD6E9FF)), BLUE, RADIUS_CONTROL),
        button::Status::Disabled => button_style(Some(GREY100), GREY400, RADIUS_CONTROL),
        button::Status::Active => button_style(Some(BLUE_LIGHT), BLUE, RADIUS_CONTROL),
    }
}

/// 배경 없는 버튼
pub fn ghost(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered | button::Status::Pressed => button_style(Some(GREY100), GREY700, RADIUS_CONTROL),
        button::Status::Disabled => button_style(None, GREY400, RADIUS_CONTROL),
        button::Status::Active => button_style(None, GREY700, RADIUS_CONTROL),
    }
}

pub fn danger(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered | button::Status::Pressed => button_style(Some(color!(0xFFDCDF)), RED, RADIUS_CONTROL),
        _ => button_style(Some(RED_LIGHT), RED, RADIUS_CONTROL),
    }
}

/// 필터·선택 칩
pub fn chip(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        match (selected, hovered) {
            (true, _) => button_style(Some(GREY900), WHITE, 999.0),
            (false, true) => button_style(Some(GREY200), GREY700, 999.0),
            (false, false) => button_style(Some(GREY100), GREY700, 999.0),
        }
    }
}

/// ? 버튼: 펼치면 파랗게
pub fn help(open: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        match (open, hovered) {
            (true, _) => button_style(Some(BLUE), WHITE, 999.0),
            (false, true) => button_style(Some(GREY200), GREY700, 999.0),
            (false, false) => button_style(Some(GREY100), GREY500, 999.0),
        }
    }
}

/// 목록 한 줄 (카드 안의 행)
pub fn list_item(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let background = match (selected, hovered) {
            (true, _) => BLUE_LIGHT,
            (false, true) => GREY50,
            (false, false) => WHITE,
        };
        button_style(Some(background), GREY900, 14.0)
    }
}

// ── 컨테이너 ──

pub fn card(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(WHITE)),
        border: border(RADIUS_CARD),
        shadow: Shadow {
            color: Color { a: 0.04, ..Color::BLACK },
            offset: Vector::new(0.0, 2.0),
            blur_radius: 12.0,
        },
        ..container::Style::default()
    }
}

pub fn sheet(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(WHITE)),
        border: border(24.0),
        shadow: Shadow {
            color: Color { a: 0.18, ..Color::BLACK },
            offset: Vector::new(0.0, 12.0),
            blur_radius: 40.0,
        },
        ..container::Style::default()
    }
}

pub fn backdrop(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color { a: 0.45, ..GREY900 })),
        ..container::Style::default()
    }
}

/// 회색 바탕 상자 (미리보기, 안내)
pub fn well(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(GREY50)),
        border: Border { radius: Radius::from(RADIUS_CONTROL), color: GREY200, width: 1.0 },
        ..container::Style::default()
    }
}

pub fn tinted(background: Color, radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(background)),
        border: border(radius),
        ..container::Style::default()
    }
}

pub fn toast(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color { a: 0.92, ..GREY900 })),
        text_color: Some(WHITE),
        border: border(999.0),
        ..container::Style::default()
    }
}

// ── 입력 ──

fn field_border(status_focused: bool) -> Border {
    Border {
        radius: Radius::from(RADIUS_CONTROL),
        color: if status_focused { BLUE } else { GREY100 },
        width: if status_focused { 1.5 } else { 1.0 },
    }
}

pub fn input(_: &Theme, status: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: Background::Color(GREY100),
        border: field_border(matches!(status, text_input::Status::Focused { .. })),
        icon: GREY500,
        placeholder: GREY500,
        value: GREY900,
        selection: color!(0xC9E2FF),
    }
}

pub fn editor(_: &Theme, status: text_editor::Status) -> text_editor::Style {
    text_editor::Style {
        background: Background::Color(GREY100),
        border: field_border(matches!(status, text_editor::Status::Focused { .. })),
        placeholder: GREY500,
        value: GREY900,
        selection: color!(0xC9E2FF),
    }
}

pub fn scroll(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let mut style = scrollable::default(theme, status);
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(GREY200),
            border: border(999.0),
        },
    };
    style.vertical_rail = rail;
    style.horizontal_rail = rail;
    style
}
