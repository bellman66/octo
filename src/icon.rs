//! Octo 아이콘. 원본은 assets/octo.svg이고, build.rs가 크기별 RGBA로 렌더해 둔다.
//! 창·트레이·헤더 로고가 모두 이 그림을 쓴다.

/// `size`×`size` RGBA 픽셀. 렌더해 둔 크기(32, 64, 96, 256)만 쓸 수 있다.
pub fn rgba(size: u32) -> Vec<u8> {
    let bytes: &[u8] = match size {
        32 => include_bytes!(concat!(env!("OUT_DIR"), "/icon-32.rgba")),
        64 => include_bytes!(concat!(env!("OUT_DIR"), "/icon-64.rgba")),
        96 => include_bytes!(concat!(env!("OUT_DIR"), "/icon-96.rgba")),
        256 => include_bytes!(concat!(env!("OUT_DIR"), "/icon-256.rgba")),
        _ => panic!("icon size {size} is not rendered by build.rs"),
    };
    bytes.to_vec()
}
