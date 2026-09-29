//! 내보내기·가져오기 시트. 내보내기는 큰 첨부와 비밀번호를 확인받고, 가져오기는 요약을 보여준 뒤 진행하고 되돌릴 수 있다.

use iced::widget::{checkbox, column, container, row, space, text};
use iced::{Center, Element, Fill};

use crate::core::transfer::{self, Scope};
use crate::i18n::t;
use crate::tr;
use crate::ui::app::{ExportSheet, ImportSheet, Message};
use crate::ui::theme::{self, BOLD};
use crate::ui::widgets::{caption, error, ghost_button, heading, icon_button, primary_button, title};

pub fn export_view(sheet: &ExportSheet) -> Element<'_, Message> {
    let heading_text = match &sheet.scope {
        Scope::All => t("전체 내보내기", "Export everything").to_owned(),
        Scope::Indexes(names) => tr!("'{}' 내보내기", "Export '{}'", names.join(", ")),
    };
    let what = match &sheet.scope {
        Scope::All => t(
            "모든 인덱스와 컨텍스트를 담아요. 다른 PC로 옮기거나 백업할 때 써요.",
            "Includes every index and context. Use it to move to another PC or back up.",
        ),
        Scope::Indexes(_) => t(
            "목차의 컨텍스트, 문서 본문, 경로가 가리키는 파일·폴더를 담아요. 팀원에게 보내 공유할 때 써요.",
            "Includes the listed contexts, document text, and the files and folders paths point to. Use it to share with teammates.",
        ),
    };

    let mut body = column![row![title(heading_text), space::horizontal(), icon_button("✕", Some(Message::CloseSheet))].align_y(Center), caption(what)]
        .spacing(16);

    match &sheet.large {
        None => body = body.push(caption(t("첨부 크기를 확인하는 중…", "Checking attachment sizes…"))),
        Some(large) if large.is_empty() => {}
        Some(large) => {
            let mut list = column![
                text(tr!("첨부가 {}를 넘는 컨텍스트", "Contexts with attachments over {}", transfer::human_bytes(transfer::LARGE_BYTES)))
                    .size(14)
                    .font(BOLD)
                    .color(theme::GREY900)
            ]
            .spacing(6);
            for item in large {
                list = list.push(caption(format!("· {} — {}", item.title, transfer::human_bytes(item.bytes))));
            }
            list = list.push(
                checkbox(sheet.attach_large)
                    .label(t("그래도 파일째 넣기 (끄면 경로만 담아요)", "Include the files anyway (off: path only)"))
                    .on_toggle(Message::ExportAttachLargeToggled)
                    .size(18)
                    .text_size(14),
            );
            body = body.push(container(list).padding(14).width(Fill).style(theme::tinted(theme::GREY50, 12.0)));
        }
    }

    if sheet.scope == Scope::All {
        let mut secrets = column![
            checkbox(sheet.include_secrets)
                .label(t("그래프 연결 비밀번호도 넣기", "Include graph connection passwords"))
                .on_toggle(Message::ExportSecretsToggled)
                .size(18)
                .text_size(14)
        ]
        .spacing(6);
        if sheet.include_secrets {
            secrets = secrets.push(error(t(
                "⚠ 비밀번호가 암호화 없이 평문으로 들어가요. 이 파일은 남에게 보내지 말고 안전한 곳에만 두세요.",
                "⚠ Passwords go in as plain text, unencrypted. Don't send this file to anyone; keep it somewhere safe.",
            )));
        } else {
            secrets = secrets.push(caption(t("끄면 가져온 뒤 연결 관리에서 비밀번호만 다시 입력하면 돼요.", "If off, re-enter just the passwords in Connections after importing.")));
        }
        body = body.push(secrets);
    }

    let ready = sheet.large.is_some() && !sheet.working;
    let save = if sheet.working { t("내보내는 중…", "Exporting…") } else { t("저장 위치 고르기…", "Choose where to save…") };
    let mut actions = row![space::horizontal(), ghost_button(t("취소", "Cancel"), Message::CloseSheet)].spacing(8).align_y(Center);
    actions = actions.push(primary_button(save, Message::ChooseExportPath).on_press_maybe(ready.then_some(Message::ChooseExportPath)));
    body = body.push(space::vertical()).push(actions);

    container(body).padding(28).width(560).height(460).style(theme::sheet).into()
}

pub fn import_view(sheet: &ImportSheet) -> Element<'_, Message> {
    let file = sheet.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let done = sheet.done.is_some();
    let heading_text = if done { t("가져왔어요", "Imported") } else { t("가져오기", "Import") };
    let plan = sheet.done.as_ref().map_or(&sheet.plan, |d| &d.plan);

    let mut body = column![
        row![title(heading_text), space::horizontal(), icon_button("✕", Some(Message::CloseSheet))].align_y(Center),
        caption(file),
        summary(plan),
    ]
    .spacing(16);

    if plan.secrets {
        body = body.push(error(t("⚠ 이 파일에는 그래프 연결 비밀번호가 평문으로 들어 있어요. 가져오면 키체인에 저장돼요.", "⚠ This file has graph connection passwords in plain text. Importing saves them to your keychain.")));
    }
    body = body.push(caption(if done {
        t("잘못 가져왔다면 되돌리기로 가져오기 직전 상태로 돌아가요.", "If this was a mistake, Undo returns to the state just before the import.")
    } else {
        t(
            "가져온 항목은 내 것이 돼요(에이전트가 고치거나 지울 수 없어요). 내 인덱스는 덮지 않고, 가져오기 직전 상태를 스냅샷으로 남겨요.",
            "Imported items become yours (agents can't edit or delete them). Your indexes aren't overwritten, and the state just before is kept as a snapshot.",
        )
    }));

    let mut actions = row![space::horizontal()].spacing(8).align_y(Center);
    if done {
        let undo = if sheet.working { t("되돌리는 중…", "Restoring…") } else { t("되돌리기", "Undo") };
        actions = actions
            .push(ghost_button(undo, Message::UndoImport).on_press_maybe((!sheet.working).then_some(Message::UndoImport)))
            .push(primary_button(t("닫기", "Close"), Message::CloseSheet));
    } else {
        let go = if sheet.working { t("가져오는 중…", "Importing…") } else { t("가져오기", "Import") };
        actions = actions
            .push(ghost_button(t("취소", "Cancel"), Message::CloseSheet))
            .push(primary_button(go, Message::ConfirmImport).on_press_maybe((!sheet.working).then_some(Message::ConfirmImport)));
    }
    body = body.push(space::vertical()).push(actions);

    container(body).padding(28).width(560).height(460).style(theme::sheet).into()
}

fn summary(plan: &transfer::Plan) -> Element<'_, Message> {
    let mut list = column![heading(t("요약", "Summary"))].spacing(8);
    for (from, to) in &plan.indexes {
        let line = if from == to {
            tr!("인덱스 {from}", "Index {from}")
        } else {
            tr!("인덱스 {from} → {to} (같은 이름이 있어 새 이름으로)", "Index {from} → {to} (name taken, so renamed)")
        };
        list = list.push(text(line).size(14).color(theme::GREY700));
    }
    list = list
        .push(text(tr!(
            "컨텍스트: 새로 {} · 갱신 {} · 그대로 {}",
            "Contexts: {} new · {} updated · {} unchanged",
            plan.new,
            plan.updated,
            plan.unchanged
        ))
        .size(14)
        .color(theme::GREY700));
    if plan.connections > 0 {
        list = list.push(text(tr!("그래프 연결 {}개", "{} graph connections", plan.connections)).size(14).color(theme::GREY700));
    }
    if plan.attach_bytes > 0 {
        list = list.push(caption(tr!(
            "원본이 없는 경로는 첨부본 {}를 Octo 폴더에 풀어 가리켜요",
            "Paths without an original point to {} of attachments extracted into the Octo folder",
            transfer::human_bytes(plan.attach_bytes)
        )));
    }
    container(list).padding(14).width(Fill).style(theme::tinted(theme::GREY50, 12.0)).into()
}
