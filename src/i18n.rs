//! 한국어/영어 표시. 언어는 프로세스 전체에서 하나라 GUI·트레이·MCP가 같은 값을 본다.
//!
//! 문장은 쓰는 자리에서 두 언어를 나란히 적는다:
//! - 고정 문장: `t("저장했어요", "Saved")`
//! - 값이 들어가는 문장: `tr!("'{name}' 인덱스를 만들었어요", "Created index '{name}'")`

use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lang {
    #[default]
    Ko,
    En,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::Ko, Lang::En];

    /// 언어 고르기 버튼 라벨. 늘 그 언어 자신의 이름으로 보인다.
    pub fn label(self) -> &'static str {
        match self {
            Lang::Ko => "한국어",
            Lang::En => "English",
        }
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn set(lang: Lang) {
    LANG.store(lang as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if LANG.load(Ordering::Relaxed) == Lang::En as u8 { Lang::En } else { Lang::Ko }
}

pub fn is_en() -> bool {
    lang() == Lang::En
}

/// 지금 언어의 고정 문장
pub fn t(ko: &'static str, en: &'static str) -> &'static str {
    if is_en() { en } else { ko }
}

/// 지금 언어로 `format!`. 두 문장 모두 같은 값을 이름으로 쓸 수 있다.
#[macro_export]
macro_rules! tr {
    ($ko:literal, $en:literal $(, $($arg:tt)*)?) => {
        if $crate::i18n::is_en() { format!($en $(, $($arg)*)?) } else { format!($ko $(, $($arg)*)?) }
    };
}
