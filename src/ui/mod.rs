//! GUI. 상태·로직(app) / 화면(view) / 디자인(theme, widgets)으로 나눈다.

mod app;
mod theme;
mod view;
mod widgets;

pub use app::App;
pub use theme::{FONT, theme};
pub use view::view;
