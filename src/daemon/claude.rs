//! Claude Code에 Octo MCP를 사용자 범위(모든 프로젝트)로 한 번 등록한다.
//! `claude` CLI가 설정 파일을 직접 관리하므로 우리는 명령만 부른다.

use std::process::{Command, Output};

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
        Command::new("claude").args(args).output()
    }
}
