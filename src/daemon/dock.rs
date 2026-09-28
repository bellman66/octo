//! macOS Dock 아이콘 클릭 → 창 띄우기.
//! winit은 `applicationShouldHandleReopen:hasVisibleWindows:`를 처리하지 않아서,
//! 창을 닫은 뒤 Dock을 눌러도 아무 일이 없다. 앱 델리게이트 클래스에 그 메서드를 직접 붙인다.

use std::sync::Mutex;

use futures::channel::mpsc;
use futures::Stream;
use iced::Subscription;

static TX: Mutex<Option<mpsc::UnboundedSender<()>>> = Mutex::new(None);

/// Dock 아이콘을 누를 때마다 이벤트를 낸다.
pub fn subscription() -> Subscription<()> {
    Subscription::run(events)
}

fn events() -> impl Stream<Item = ()> {
    let (tx, rx) = mpsc::unbounded();
    *TX.lock().unwrap() = Some(tx);
    rx
}

/// 메인 스레드에서, 이벤트 루프가 델리게이트를 설정한 뒤(첫 창이 열린 시점) 호출한다.
#[cfg(target_os = "macos")]
pub fn install() {
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::{class, msg_send, sel, Encode};

    unsafe extern "C-unwind" fn reopen(_this: &AnyObject, _cmd: Sel, _app: *mut AnyObject, _visible: Bool) -> Bool {
        if let Some(tx) = TX.lock().ok().and_then(|tx| tx.clone()) {
            let _ = tx.unbounded_send(());
        }
        // 최소화된 창 복원 등 기본 동작은 AppKit이 이어서 한다
        Bool::YES
    }

    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let delegate: *mut AnyObject = msg_send![app, delegate];
        let Some(delegate) = delegate.as_ref() else {
            return;
        };
        let cls = delegate.class() as *const AnyClass as *mut AnyClass;
        let types = format!("{}@:@{}\0", Bool::ENCODING, Bool::ENCODING);
        let imp: Imp = std::mem::transmute(
            reopen as unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject, Bool) -> Bool,
        );
        // 이미 있으면(두 번째 호출 등) NO를 돌려주고 아무것도 바꾸지 않는다
        objc2::ffi::class_addMethod(cls, sel!(applicationShouldHandleReopen:hasVisibleWindows:), imp, types.as_ptr().cast());
    }
}

#[cfg(not(target_os = "macos"))]
pub fn install() {}
