//! 아이콘: assets/octo.svg 를 크기별 RGBA로 렌더해 OUT_DIR에 두고(src/icon.rs가 포함),
//! Windows에서는 .ico로 만들어 exe 리소스에도 넣는다.

use std::path::{Path, PathBuf};

use resvg::{tiny_skia, usvg};

const SVG: &str = "assets/octo.svg";
/// src/icon.rs가 쓰는 크기
const SIZES: [u32; 4] = [32, 64, 96, 256];
const ICO_SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];

fn main() {
    println!("cargo:rerun-if-changed={SVG}");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let svg = std::fs::read(SVG).expect("read icon svg");
    let tree = usvg::Tree::from_data(&svg, &usvg::Options::default()).expect("parse icon svg");

    for size in SIZES {
        std::fs::write(out.join(format!("icon-{size}.rgba")), render(&tree, size)).unwrap();
    }
    // 눈으로 확인하기 위한 미리보기
    let preview = render_pixmap(&tree, 256);
    let _ = preview.save_png(out.join("icon-preview.png"));

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_exe_icon(&tree, &out);
    }
}

fn render_pixmap(tree: &usvg::Tree, size: u32) -> tiny_skia::Pixmap {
    let mut pixmap = tiny_skia::Pixmap::new(size, size).unwrap();
    let scale = size as f32 / tree.size().width();
    resvg::render(tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap
}

/// 곱해진 알파를 풀어 일반 RGBA로
fn render(tree: &usvg::Tree, size: u32) -> Vec<u8> {
    render_pixmap(tree, size)
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

fn embed_exe_icon(tree: &usvg::Tree, out: &Path) {
    let path = out.join("octo.ico");
    std::fs::write(&path, ico(tree)).unwrap();

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon(path.to_str().unwrap());
    if let Err(err) = resource.compile() {
        // SDK(rc.exe)가 없으면 아이콘 없이 빌드는 계속한다
        println!("cargo:warning=exe icon skipped: {err}");
    }
}

/// 32bit BMP 항목으로 된 ICO 파일
fn ico(tree: &usvg::Tree) -> Vec<u8> {
    let images: Vec<Vec<u8>> = ICO_SIZES.iter().map(|&size| dib(&render(tree, size), size)).collect();

    let mut out = Vec::new();
    out.extend(0u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend((ICO_SIZES.len() as u16).to_le_bytes());

    let mut offset = 6 + 16 * ICO_SIZES.len() as u32;
    for (&size, image) in ICO_SIZES.iter().zip(&images) {
        let dim = if size >= 256 { 0 } else { size as u8 };
        out.extend([dim, dim, 0, 0]);
        out.extend(1u16.to_le_bytes());
        out.extend(32u16.to_le_bytes());
        out.extend((image.len() as u32).to_le_bytes());
        out.extend(offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for image in images {
        out.extend(image);
    }
    out
}

fn dib(pixels: &[u8], size: u32) -> Vec<u8> {
    let mask_row = size.div_ceil(32) * 4;
    let mut out = Vec::new();
    out.extend(40u32.to_le_bytes());
    out.extend((size as i32).to_le_bytes());
    out.extend((size as i32 * 2).to_le_bytes()); // 색 + 마스크
    out.extend(1u16.to_le_bytes());
    out.extend(32u16.to_le_bytes());
    out.extend([0u8; 24]);
    // BGRA, 아래 줄부터
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            out.extend([pixels[i + 2], pixels[i + 1], pixels[i], pixels[i + 3]]);
        }
    }
    // 알파를 쓰므로 AND 마스크는 전부 0
    out.extend(vec![0u8; (mask_row * size) as usize]);
    out
}
