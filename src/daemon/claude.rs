//! Claude Code에 Octo MCP를 사용자 범위(모든 프로젝트)로 한 번 등록한다.
//! `claude` CLI가 설정 파일을 직접 관리하므로 우리는 명령만 부른다.

use std::process::{Command, Output};
#[cfg(not(windows))]
use std::process::Stdio;
#[cfg(not(windows))]
use std::sync::OnceLock;
#[cfg(not(windows))]
use std::time::{Duration, Instant};

use crate::mcp;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Connected,
    NotConnected,
    /// claude CLI를 찾을 수 없음
    Unavailable,
}

pub fn status() -> Status {
    match run(&["mcp", "get", mcp::SERVER_NAME]) {
        Ok(output) if output.status.success() => Status::Connected,
        Ok(_) => Status::NotConnected,
        Err(_) => Status::Unavailable,
    }
}

pub fn connect(port: u16) -> Result<(), String> {
    // 예전 주소나 예전 이름(octopuser)으로 등록돼 있으면 새로 등록하기 위해 먼저 지운다
    let _ = run(&["mcp", "remove", "--scope", "user", mcp::SERVER_NAME]);
    let _ = run(&["mcp", "remove", "--scope", "user", mcp::LEGACY_SERVER_NAME]);
    let url = mcp::url(port);
    let output = run(&["mcp", "add", "--transport", "http", "--scope", "user", mcp::SERVER_NAME, &url])
        .map_err(|err| crate::tr!("claude CLI를 실행하지 못했어요: {err}", "Couldn't run the claude CLI: {err}"))?;
    ok_or_stderr(output)
}

pub fn disconnect() -> Result<(), String> {
    let output = run(&["mcp", "remove", "--scope", "user", mcp::SERVER_NAME])
        .map_err(|err| crate::tr!("claude CLI를 실행하지 못했어요: {err}", "Couldn't run the claude CLI: {err}"))?;
    ok_or_stderr(output)
}

fn ok_or_stderr(output: Output) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(if stderr.trim().is_empty() { stdout } else { stderr }.trim().to_owned())
    }
}

/// Windows의 `claude`는 npm이 만든 `.cmd`라서 cmd를 거쳐 실행하고, 콘솔 창은 띄우지 않는다.
fn run(args: &[&str]) -> std::io::Result<Output> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        Command::new("cmd").arg("/C").arg("claude").args(args).creation_flags(CREATE_NO_WINDOW).output()
    }
    #[cfg(not(windows))]
    {
        Command::new("claude").args(args).env("PATH", shell_path()).output()
    }
}

/// Dock·Finder로 연 .app은 launchd의 기본 PATH(/usr/bin:/bin:/usr/sbin:/sbin)만 받아서
/// ~/.local/bin, Homebrew 등에 있는 claude(와 claude가 부르는 node)를 못 찾는다.
/// 로그인 셸의 PATH를 한 번 읽어 두고, 흔한 설치 경로도 덧붙인다.
#[cfg(not(windows))]
fn shell_path() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        let mut dirs: Vec<String> = Vec::new();
        dirs.extend(login_shell_path().iter().flat_map(|p| p.split(':')).map(str::to_owned));
        dirs.extend(std::env::var("PATH").iter().flat_map(|p| p.split(':')).map(str::to_owned));
        if let Some(home) = dirs::home_dir() {
            for dir in [".local/bin", ".claude/local"] {
                dirs.push(home.join(dir).to_string_lossy().into_owned());
            }
        }
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(str::to_owned));

        let mut seen = std::collections::HashSet::new();
        dirs.retain(|d| !d.is_empty() && seen.insert(d.clone()));
        dirs.join(":")
    })
}

/// `$SHELL -ilc`로 .zshrc 등까지 읽은 PATH. 셸 설정이 멈추면 3초 뒤 포기한다.
#[cfg(not(windows))]
fn login_shell_path() -> Option<String> {
    const MARK: &str = "__OCTO_PATH__";
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
    let mut child = Command::new(shell)
        .arg("-ilc")
        .arg(format!("printf '{MARK}%s{MARK}' \"$PATH\""))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + Duration::from_secs(3);
    while child.try_wait().ok()?.is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().ok()?;
    // 셸 설정이 출력하는 인사말 등은 표식 밖에 있으니 버린다
    String::from_utf8_lossy(&output.stdout).split(MARK).nth(1).map(str::to_owned)
}
