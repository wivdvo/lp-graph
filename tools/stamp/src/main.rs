//! Build helper: writes the app icon and version info (name shown by Windows in Explorer,
//! Task Manager and notifications) into the built .exe files.
//! Usage: stamp <exe> <description> [<exe> <description> ...]
//!
//! Uses Windows' own UpdateResource API, so no resource compiler is needed.

use std::ffi::c_void;
use std::ptr::null_mut;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::{BeginUpdateResourceW, EndUpdateResourceW, UpdateResourceW};
use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO, RT_GROUP_ICON, RT_ICON, RT_VERSION};

const SIZES: [i32; 7] = [16, 20, 24, 32, 40, 48, 64];
const LANG_EN_US: u16 = 0x0409;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 || args.len() % 2 != 0 {
        eprintln!("usage: stamp <exe> <description> [<exe> <description> ...]");
        std::process::exit(2);
    }
    let images: Vec<(i32, Vec<u8>)> = SIZES.iter().map(|&s| (s, icon_image(s))).collect();
    for pair in args.chunks(2) {
        match stamp(&pair[0], &pair[1], &images) {
            Ok(()) => println!("stamped {} ({})", pair[0], pair[1]),
            Err(e) => {
                eprintln!("failed to stamp {}: {e}", pair[0]);
                std::process::exit(1);
            }
        }
    }
}

/// One RT_ICON entry: BITMAPINFOHEADER + 32-bit BGRA pixels (bottom-up) + 1-bit AND mask.
fn icon_image(size: i32) -> Vec<u8> {
    unsafe {
        let icon = common::icon::make(size);
        let mut info: ICONINFO = std::mem::zeroed();
        GetIconInfo(icon, &mut info);
        let color = dib_pixels(info.hbmColor, size);
        let mask = dib_pixels(info.hbmMask, size); // mask: white = transparent
        DeleteObject(info.hbmColor);
        DeleteObject(info.hbmMask);
        DestroyIcon(icon);

        let mut out = Vec::new();
        let header = BITMAPINFOHEADER {
            biSize: 40,
            biWidth: size,
            biHeight: size * 2, // XOR + AND
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };
        out.extend_from_slice(std::slice::from_raw_parts(&header as *const _ as *const u8, 40));
        // pixels, bottom-up (GetDIBits already returned bottom-up rows), alpha from the mask
        for i in 0..(size * size) as usize {
            let opaque = mask[i * 4] < 128;
            out.extend_from_slice(&[color[i * 4], color[i * 4 + 1], color[i * 4 + 2], if opaque { 255 } else { 0 }]);
        }
        // AND mask: 1 bit per pixel, rows padded to 4 bytes; all 0 (alpha channel decides)
        let row = ((size + 31) / 32 * 4) as usize;
        out.extend(std::iter::repeat(0u8).take(row * size as usize));
        out
    }
}

/// Reads a bitmap as 32-bit bottom-up BGRA.
unsafe fn dib_pixels(bmp: HBITMAP, size: i32) -> Vec<u8> {
    let mut bi: BITMAPINFO = std::mem::zeroed();
    bi.bmiHeader.biSize = 40;
    bi.bmiHeader.biWidth = size;
    bi.bmiHeader.biHeight = size; // positive = bottom-up
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = BI_RGB;
    let mut buf = vec![0u8; (size * size * 4) as usize];
    let dc = GetDC(null_mut());
    GetDIBits(dc, bmp, 0, size as u32, buf.as_mut_ptr() as *mut c_void, &mut bi, DIB_RGB_COLORS);
    ReleaseDC(null_mut(), dc);
    buf
}

fn stamp(exe: &str, description: &str, images: &[(i32, Vec<u8>)]) -> Result<(), String> {
    let path: Vec<u16> = exe.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let h = BeginUpdateResourceW(path.as_ptr(), 0);
        if h.is_null() {
            return Err(format!("BeginUpdateResource failed ({})", std::io::Error::last_os_error()));
        }
        let mut ok = true;
        // icon images (ids 1..n) and the icon group (id 1) that Windows uses for the exe
        let mut group = Vec::new();
        group.extend_from_slice(&0u16.to_le_bytes());
        group.extend_from_slice(&1u16.to_le_bytes());
        group.extend_from_slice(&(images.len() as u16).to_le_bytes());
        for (i, (size, data)) in images.iter().enumerate() {
            let id = (i + 1) as u16;
            ok &= UpdateResourceW(h, RT_ICON, id as usize as *const u16, LANG_EN_US, data.as_ptr() as *const c_void, data.len() as u32) != 0;
            group.push(*size as u8);
            group.push(*size as u8);
            group.push(0); // colors
            group.push(0); // reserved
            group.extend_from_slice(&1u16.to_le_bytes()); // planes
            group.extend_from_slice(&32u16.to_le_bytes()); // bit count
            group.extend_from_slice(&(data.len() as u32).to_le_bytes());
            group.extend_from_slice(&id.to_le_bytes());
        }
        ok &= UpdateResourceW(h, RT_GROUP_ICON, 1usize as *const u16, LANG_EN_US, group.as_ptr() as *const c_void, group.len() as u32) != 0;
        let file_name = std::path::Path::new(exe).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let ver = version_info(description, &file_name, env!("CARGO_PKG_VERSION"));
        ok &= UpdateResourceW(h, RT_VERSION, 1usize as *const u16, LANG_EN_US, ver.as_ptr() as *const c_void, ver.len() as u32) != 0;
        if EndUpdateResourceW(h, if ok { 0 } else { 1 }) == 0 || !ok {
            return Err(format!("UpdateResource failed ({})", std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

// ---------- VS_VERSIONINFO ----------

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16().chain(Some(0)).flat_map(|c| c.to_le_bytes()).collect()
}

fn pad4(v: &mut Vec<u8>) {
    while v.len() % 4 != 0 {
        v.push(0);
    }
}

/// One version-resource block: wLength, wValueLength, wType, key, value, children.
fn block(key: &str, value: &[u8], value_len: u16, text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0u8; 6];
    b.extend_from_slice(&utf16z(key));
    pad4(&mut b);
    b.extend_from_slice(value);
    for c in children {
        pad4(&mut b);
        b.extend_from_slice(c);
    }
    let len = b.len() as u16;
    b[0..2].copy_from_slice(&len.to_le_bytes());
    b[2..4].copy_from_slice(&value_len.to_le_bytes());
    b[4..6].copy_from_slice(&(text as u16).to_le_bytes());
    b
}

fn string(key: &str, value: &str) -> Vec<u8> {
    let v = utf16z(value);
    block(key, &v, (v.len() / 2) as u16, true, &[])
}

fn version_info(description: &str, file_name: &str, version: &str) -> Vec<u8> {
    let nums: Vec<u16> = version.split('.').map(|p| p.parse().unwrap_or(0)).chain([0, 0, 0, 0]).take(4).collect();
    let ms = (nums[0] as u32) << 16 | nums[1] as u32;
    let ls = (nums[2] as u32) << 16 | nums[3] as u32;
    let mut fixed = Vec::new();
    for v in [0xFEEF04BDu32, 0x0001_0000, ms, ls, ms, ls, 0x3F, 0, 0x0004_0004, 1, 0, 0, 0] {
        fixed.extend_from_slice(&v.to_le_bytes());
    }
    let table = block(
        "040904B0",
        &[],
        0,
        true,
        &[
            string("FileDescription", description),
            string("ProductName", "LP Graph"),
            string("FileVersion", version),
            string("ProductVersion", version),
            string("OriginalFilename", file_name),
            string("InternalName", file_name.trim_end_matches(".exe")),
            string("CompanyName", "github.com/wivdvo/lp-graph"),
        ],
    );
    let strings = block("StringFileInfo", &[], 0, true, &[table]);
    let translation = block("Translation", &0x04B0_0409u32.to_le_bytes(), 4, false, &[]);
    let vars = block("VarFileInfo", &[], 0, true, &[translation]);
    block("VS_VERSION_INFO", &fixed, fixed.len() as u16, false, &[strings, vars])
}
