//! A Windows program's icon as RGBA pixels, through the shell and GDI.
//! Kept free of the rest of the app so it can be tested on its own under
//! Wine.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, ReleaseDC,
};
use windows_sys::Win32::UI::Shell::SHDefExtractIconW;
use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

const SIZE: i32 = 64;

/// The program's main icon at 64×64: `(width, height, RGBA pixels)`.
pub fn extract(exe: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let wide: Vec<u16> = exe.as_os_str().encode_wide().chain([0]).collect();
    let mut icon: HICON = std::ptr::null_mut();
    // SAFETY: `wide` is NUL-terminated; the icon is destroyed below.
    let result = unsafe {
        SHDefExtractIconW(
            wide.as_ptr(),
            0,
            0,
            &mut icon,
            std::ptr::null_mut(),
            SIZE as u32,
        )
    };
    if result != 0 || icon.is_null() {
        return None;
    }
    let rgba = pixels_of(icon);
    // SAFETY: `icon` came from SHDefExtractIconW and isn't used again.
    unsafe { DestroyIcon(icon) };
    rgba.map(|rgba| (SIZE as u32, SIZE as u32, rgba))
}

fn pixels_of(icon: HICON) -> Option<Vec<u8>> {
    // SAFETY: plain GDI calls on handles owned here; both bitmaps that
    // GetIconInfo returns are deleted before returning.
    unsafe {
        let mut info: ICONINFO = std::mem::zeroed();
        if GetIconInfo(icon, &mut info) == 0 {
            return None;
        }
        let dc = GetDC(std::ptr::null_mut());
        let mut header: BITMAPINFO = std::mem::zeroed();
        header.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: SIZE,
            // Negative: rows top to bottom.
            biHeight: -SIZE,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let len = (SIZE * SIZE * 4) as usize;
        let mut bgra = vec![0u8; len];
        let rows = if info.hbmColor.is_null() {
            0
        } else {
            GetDIBits(
                dc,
                info.hbmColor,
                0,
                SIZE as u32,
                bgra.as_mut_ptr().cast(),
                &mut header,
                DIB_RGB_COLORS,
            )
        };
        // Old icons have no alpha channel; a mask marks what's transparent.
        let has_alpha = bgra.chunks(4).any(|p| p[3] != 0);
        let mut mask = vec![0u8; len];
        if rows > 0 && !has_alpha && !info.hbmMask.is_null() {
            GetDIBits(
                dc,
                info.hbmMask,
                0,
                SIZE as u32,
                mask.as_mut_ptr().cast(),
                &mut header,
                DIB_RGB_COLORS,
            );
        }
        ReleaseDC(std::ptr::null_mut(), dc);
        if !info.hbmColor.is_null() {
            DeleteObject(info.hbmColor);
        }
        if !info.hbmMask.is_null() {
            DeleteObject(info.hbmMask);
        }
        if rows == 0 {
            return None;
        }
        Some(
            bgra.chunks(4)
                .zip(mask.chunks(4))
                .flat_map(|(p, m)| {
                    let alpha = if has_alpha {
                        p[3]
                    } else if m[0] == 0 {
                        255
                    } else {
                        0
                    };
                    [p[2], p[1], p[0], alpha]
                })
                .collect(),
        )
    }
}
