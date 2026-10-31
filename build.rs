//! Nhúng assets/icon.ico vào file .exe.
//!
//! Tự tạo file object COFF chứa tài nguyên icon thay vì dùng `windres`/`rc`,
//! vì bộ Rust bản GNU không kèm các công cụ đó.
use std::{env, fs, path::Path};

const ICON_PATH: &str = "assets/icon.ico";
const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
const LANG_EN_US: u32 = 0x0409;
const DIR_SIZE: usize = 16;
const DIR_ENTRY_SIZE: usize = 8;
const ONE_ENTRY_DIR: usize = DIR_SIZE + DIR_ENTRY_SIZE;
const DATA_ENTRY_SIZE: usize = 16;
const SUBDIR_FLAG: u32 = 0x8000_0000;
const COFF_HEADER_SIZE: u32 = 20;
const SECTION_HEADER_SIZE: u32 = 40;
const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
const IMAGE_REL_AMD64_ADDR32NB: u16 = 3;
// Dữ liệu đã khởi tạo, chỉ đọc, căn lề 4 byte
const RSRC_CHARACTERISTICS: u32 = 0x4030_0040;
const IMAGE_SYM_CLASS_STATIC: u8 = 3;

fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_dir(out: &mut Vec<u8>, entries: &[(u32, u32)]) {
    out.extend_from_slice(&[0; 12]); // characteristics, timestamp, phiên bản
    push_u16(out, 0); // số mục đặt theo tên
    push_u16(out, entries.len() as u16);
    for (id, offset) in entries {
        push_u32(out, *id);
        push_u32(out, *offset);
    }
}

/// Dựng section .rsrc: thư mục loại -> ID -> ngôn ngữ -> dữ liệu.
/// Trả về nội dung section và vị trí các trường RVA cần sửa khi liên kết.
fn build_rsrc(ico: &[u8]) -> (Vec<u8>, Vec<u32>) {
    let count = u16_at(ico, 4) as usize;
    let entry = |i: usize| 6 + 16 * i;
    let image = |i: usize| {
        let (size, offset) = (u32_at(ico, entry(i) + 8) as usize, u32_at(ico, entry(i) + 12) as usize);
        &ico[offset..offset + size]
    };

    // RT_GROUP_ICON giống thư mục của ICO nhưng thay vị trí ảnh bằng ID tài nguyên
    let mut group = vec![0, 0, 1, 0];
    push_u16(&mut group, count as u16);
    for i in 0..count {
        group.extend_from_slice(&ico[entry(i)..entry(i) + 12]);
        push_u16(&mut group, i as u16 + 1);
    }

    let icon_dir = DIR_SIZE + 2 * DIR_ENTRY_SIZE;
    let group_dir = icon_dir + DIR_SIZE + count * DIR_ENTRY_SIZE;
    let lang_dirs = group_dir + ONE_ENTRY_DIR;
    let data_entries = lang_dirs + ONE_ENTRY_DIR * (count + 1);
    let mut data_offset = data_entries + DATA_ENTRY_SIZE * (count + 1);

    let mut rsrc = Vec::new();
    push_dir(
        &mut rsrc,
        &[
            (RT_ICON, icon_dir as u32 | SUBDIR_FLAG),
            (RT_GROUP_ICON, group_dir as u32 | SUBDIR_FLAG),
        ],
    );
    let icon_entries: Vec<(u32, u32)> = (0..count)
        .map(|i| (i as u32 + 1, (lang_dirs + ONE_ENTRY_DIR * i) as u32 | SUBDIR_FLAG))
        .collect();
    push_dir(&mut rsrc, &icon_entries);
    push_dir(&mut rsrc, &[(1, (lang_dirs + ONE_ENTRY_DIR * count) as u32 | SUBDIR_FLAG)]);
    for i in 0..=count {
        push_dir(&mut rsrc, &[(LANG_EN_US, (data_entries + DATA_ENTRY_SIZE * i) as u32)]);
    }

    let blobs: Vec<&[u8]> = (0..count).map(image).chain([group.as_slice()]).collect();
    let mut relocations = Vec::new();
    for blob in &blobs {
        relocations.push(rsrc.len() as u32);
        push_u32(&mut rsrc, data_offset as u32);
        push_u32(&mut rsrc, blob.len() as u32);
        push_u32(&mut rsrc, 0);
        push_u32(&mut rsrc, 0);
        data_offset += blob.len().next_multiple_of(4);
    }
    for blob in &blobs {
        rsrc.extend_from_slice(blob);
        rsrc.resize(rsrc.len().next_multiple_of(4), 0);
    }
    (rsrc, relocations)
}

fn coff_object(rsrc: &[u8], relocations: &[u32]) -> Vec<u8> {
    let section_start = COFF_HEADER_SIZE + SECTION_HEADER_SIZE;
    let relocations_start = section_start + rsrc.len() as u32;
    let symbols_start = relocations_start + relocations.len() as u32 * 10;

    let mut out = Vec::new();
    push_u16(&mut out, IMAGE_FILE_MACHINE_AMD64);
    push_u16(&mut out, 1); // số section
    push_u32(&mut out, 0); // thời gian
    push_u32(&mut out, symbols_start);
    push_u32(&mut out, 1); // số symbol
    push_u16(&mut out, 0); // optional header
    push_u16(&mut out, 0); // characteristics

    out.extend_from_slice(b".rsrc\0\0\0");
    push_u32(&mut out, 0); // virtual size
    push_u32(&mut out, 0); // virtual address
    push_u32(&mut out, rsrc.len() as u32);
    push_u32(&mut out, section_start);
    push_u32(&mut out, relocations_start);
    push_u32(&mut out, 0); // line numbers
    push_u16(&mut out, relocations.len() as u16);
    push_u16(&mut out, 0);
    push_u32(&mut out, RSRC_CHARACTERISTICS);

    out.extend_from_slice(rsrc);
    for offset in relocations {
        push_u32(&mut out, *offset);
        push_u32(&mut out, 0); // symbol của section
        push_u16(&mut out, IMAGE_REL_AMD64_ADDR32NB);
    }

    out.extend_from_slice(b".rsrc\0\0\0");
    push_u32(&mut out, 0); // value
    push_u16(&mut out, 1); // section
    push_u16(&mut out, 0); // type
    out.push(IMAGE_SYM_CLASS_STATIC);
    out.push(0); // số symbol phụ
    push_u32(&mut out, 4); // bảng chuỗi rỗng
    out
}

fn main() {
    println!("cargo:rerun-if-changed={ICON_PATH}");
    println!("cargo:rerun-if-changed=build.rs");

    let ico = fs::read(ICON_PATH).expect("Không đọc được assets/icon.ico");
    let (rsrc, relocations) = build_rsrc(&ico);
    let out_dir = env::var("OUT_DIR").expect("Cargo luôn đặt OUT_DIR");
    let object = Path::new(&out_dir).join("icon.o");
    fs::write(&object, coff_object(&rsrc, &relocations)).expect("Không ghi được icon.o");
    println!("cargo:rustc-link-arg-bins={}", object.display());
}
