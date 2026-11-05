//! App icon (L-shaped chart axis + rising zigzag line, black on a white tile),
//! drawn with GDI at runtime. Drawn 4x larger and scaled down with HALFTONE for smooth edges.

use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

/// Creates an icon of `size` x `size` pixels.
pub fn make(size: i32) -> HICON {
    unsafe {
        let big = size * 4;
        let f = |v: i32| v * big / 1000; // design grid is 1000 x 1000
        let screen = GetDC(null_mut());
        let dc_big = CreateCompatibleDC(screen);
        let bmp_big = CreateCompatibleBitmap(screen, big, big);
        let old_big = SelectObject(dc_big, bmp_big);

        // black outside the tile (transparent through the mask), white rounded tile
        PatBlt(dc_big, 0, 0, big, big, BLACKNESS);
        let white = GetStockObject(WHITE_BRUSH);
        let null_pen = GetStockObject(NULL_PEN);
        SelectObject(dc_big, white);
        SelectObject(dc_big, null_pen);
        RoundRect(dc_big, 0, 0, big + 1, big + 1, f(220), f(220));

        // thick black strokes with round caps and joins
        let lb = LOGBRUSH { lbStyle: BS_SOLID, lbColor: 0, lbHatch: 0 };
        let style = (PS_GEOMETRIC | PS_SOLID | PS_ENDCAP_ROUND | PS_JOIN_ROUND) as u32;
        let pen = ExtCreatePen(style, f(80) as u32, &lb, 0, null());
        let old_pen = SelectObject(dc_big, pen);
        let axis = [
            POINT { x: f(200), y: f(150) },
            POINT { x: f(200), y: f(800) },
            POINT { x: f(850), y: f(800) },
        ];
        Polyline(dc_big, axis.as_ptr(), axis.len() as i32);
        let zigzag = [
            POINT { x: f(200), y: f(610) },
            POINT { x: f(340), y: f(470) },
            POINT { x: f(440), y: f(580) },
            POINT { x: f(545), y: f(465) },
            POINT { x: f(655), y: f(580) },
            POINT { x: f(820), y: f(345) },
        ];
        Polyline(dc_big, zigzag.as_ptr(), zigzag.len() as i32);
        SelectObject(dc_big, old_pen);
        DeleteObject(pen);

        // scale down
        let dc = CreateCompatibleDC(screen);
        let color = CreateCompatibleBitmap(screen, size, size);
        let old = SelectObject(dc, color);
        SetStretchBltMode(dc, HALFTONE);
        SetBrushOrgEx(dc, 0, 0, null_mut());
        StretchBlt(dc, 0, 0, size, size, dc_big, 0, 0, big, big, SRCCOPY);
        SelectObject(dc, old);

        // mask: white = transparent (outside the tile), black = opaque
        let mask = CreateBitmap(size, size, 1, 1, null());
        let old = SelectObject(dc, mask);
        PatBlt(dc, 0, 0, size, size, WHITENESS);
        SelectObject(dc, GetStockObject(BLACK_BRUSH));
        SelectObject(dc, null_pen);
        let r = (size * 22 / 100).max(3);
        RoundRect(dc, 0, 0, size + 1, size + 1, r, r);
        SelectObject(dc, old);

        let info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info);

        SelectObject(dc_big, old_big);
        DeleteObject(bmp_big);
        DeleteDC(dc_big);
        DeleteObject(color);
        DeleteObject(mask);
        DeleteDC(dc);
        ReleaseDC(null_mut(), screen);
        icon
    }
}
