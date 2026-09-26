use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use futures::channel::mpsc;
use futures::Stream;
use iced::Subscription;

/// 실행 중인 인스턴스가 대기하는 로컬 포트. 127.0.0.1에만 바인딩한다.
const PORT: u16 = 47_613;
const REQUEST: &str = "octo show";
const REPLY: &str = "octo ok";

static LISTENER: OnceLock<TcpListener> = OnceLock::new();

/// 이 프로세스가 계속 실행돼야 하면 `true`를 반환한다.
/// 이미 다른 인스턴스가 떠 있으면 그쪽에 창을 띄우라고 알리고 `false`를 반환한다.
pub fn acquire() -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, PORT));

    match TcpListener::bind(addr) {
        Ok(listener) => {
            let _ = LISTENER.set(listener);
            true
        }
        Err(_) if notify_existing(addr) => false,
        // 포트를 다른 프로그램이 쓰고 있는 경우: 중복 방지 없이 그냥 실행한다
        Err(err) => {
            eprintln!("single-instance check unavailable: {err}");
            true
        }
    }
}

/// 다른 인스턴스가 "창 열기"를 요청할 때마다 이벤트를 낸다.
pub fn subscription() -> Subscription<()> {
    Subscription::run(requests)
}

fn notify_existing(addr: SocketAddr) -> bool {
    let timeout = Duration::from_millis(500);
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(timeout));

    if writeln!(stream, "{REQUEST}").is_err() {
        return false;
    }
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).is_ok() && reply.trim() == REPLY
}

fn requests() -> impl Stream<Item = ()> {
    let (tx, rx) = mpsc::unbounded();

    if let Some(listener) = LISTENER.get().and_then(|l| l.try_clone().ok()) {
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                if handle(stream) && tx.unbounded_send(()).is_err() {
                    break;
                }
            }
        });
    }

    rx
}

fn handle(mut stream: TcpStream) -> bool {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));

    let mut line = String::new();
    let Ok(reader) = stream.try_clone() else {
        return false;
    };
    if BufReader::new(reader).read_line(&mut line).is_err() || line.trim() != REQUEST {
        return false;
    }
    writeln!(stream, "{REPLY}").is_ok()
}
