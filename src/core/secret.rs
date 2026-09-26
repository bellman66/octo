//! 연결 비밀번호 보관. OS 키체인(Windows 자격 증명 관리자 / macOS Keychain)을 먼저 쓰고,
//! 키체인을 쓸 수 없으면 저장소의 `secrets.json`에 둔다.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const SERVICE: &str = "octo";
/// 이름을 바꾸기 전에 저장한 비밀번호는 여기서 찾는다
const LEGACY_SERVICE: &str = "octopuser";

pub fn set(root: &Path, id: &str, password: &str) {
    let stored = keyring::Entry::new(SERVICE, id)
        .and_then(|entry| entry.set_password(password))
        .is_ok();

    let mut file = read_file(root);
    if stored {
        file.remove(id);
    } else {
        file.insert(id.to_owned(), password.to_owned());
    }
    write_file(root, &file);
}

pub fn get(root: &Path, id: &str) -> Option<String> {
    keyring::Entry::new(SERVICE, id)
        .and_then(|entry| entry.get_password())
        .or_else(|_| keyring::Entry::new(LEGACY_SERVICE, id).and_then(|entry| entry.get_password()))
        .ok()
        .or_else(|| read_file(root).remove(id))
}

pub fn delete(root: &Path, id: &str) {
    if let Ok(entry) = keyring::Entry::new(SERVICE, id) {
        let _ = entry.delete_credential();
    }
    let mut file = read_file(root);
    if file.remove(id).is_some() {
        write_file(root, &file);
    }
}

fn read_file(root: &Path) -> BTreeMap<String, String> {
    fs::read(root.join("secrets.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_file(root: &Path, secrets: &BTreeMap<String, String>) {
    let path = root.join("secrets.json");
    if secrets.is_empty() {
        let _ = fs::remove_file(path);
    } else if let Ok(bytes) = serde_json::to_vec_pretty(secrets) {
        let _ = fs::write(path, bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_round_trip_and_delete() {
        let root = std::env::temp_dir().join(format!("octo-secret-{}", crate::core::store::new_id()));
        fs::create_dir_all(&root).unwrap();
        let id = format!("test-{}", crate::core::store::new_id());

        set(&root, &id, "pw-123");
        assert_eq!(get(&root, &id).as_deref(), Some("pw-123"));
        // 키체인에 들어갔다면 평문 파일은 남지 않는다
        let in_keychain = keyring::Entry::new(SERVICE, &id).and_then(|e| e.get_password()).is_ok();
        assert_eq!(root.join("secrets.json").exists(), !in_keychain);

        delete(&root, &id);
        assert_eq!(get(&root, &id), None);
    }
}
