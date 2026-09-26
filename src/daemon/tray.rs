use futures::channel::mpsc;
use futures::Stream;
use iced::Subscription;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use crate::i18n::t;
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const SHOW_ID: &str = "show";
const QUIT_ID: &str = "quit";

#[derive(Debug, Clone, Copy)]
pub enum TrayAction {
    Show,
    Quit,
}

/// 트레이 아이콘을 만든다. 반환값이 drop되면 아이콘이 사라지므로 상태에 보관해야 한다.
pub fn create() -> TrayIcon {
    let menu = Menu::new();
    menu.append_items(&[
        &MenuItem::with_id(SHOW_ID, t("열기", "Open"), true, None),
        &MenuItem::with_id(QUIT_ID, t("종료", "Quit"), true, None),
    ])
    .expect("tray menu");

    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Octo")
        .with_icon(icon())
        .build()
        .expect("tray icon")
}

pub fn subscription() -> Subscription<TrayAction> {
    Subscription::run(events)
}

fn events() -> impl Stream<Item = TrayAction> {
    let (tx, rx) = mpsc::unbounded();

    let menu_tx = tx.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let action = if event.id == SHOW_ID {
            TrayAction::Show
        } else if event.id == QUIT_ID {
            TrayAction::Quit
        } else {
            return;
        };
        let _ = menu_tx.unbounded_send(action);
    }));

    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        let show = matches!(
            event,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } | TrayIconEvent::DoubleClick { .. }
        );
        if show {
            let _ = tx.unbounded_send(TrayAction::Show);
        }
    }));

    rx
}

fn icon() -> Icon {
    const SIZE: u32 = 32;
    Icon::from_rgba(crate::icon::rgba(SIZE), SIZE, SIZE).expect("tray icon image")
}
