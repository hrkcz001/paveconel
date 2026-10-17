//! Tray-state icons.
//!
//! Three emoji (snowflake / steam / fire) are pre-rendered by `tools/gen_icons.py` into raw
//! top-down RGBA and embedded here, then turned into `HICON`s once at first use. The tray
//! picks a band from the hottest sensor; see [`Band`].

use std::sync::OnceLock;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, GetDC, ReleaseDC, BITMAPINFO, BI_RGB,
    DIB_RGB_COLORS, HGDIOBJ,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

/// Side length of the pre-rendered emoji, in pixels.
pub const SIZE: i32 = 32;

const COLD: &[u8] = include_bytes!("../assets/icon_cold.rgba");
const WARM: &[u8] = include_bytes!("../assets/icon_warm.rgba");
const HOT: &[u8] = include_bytes!("../assets/icon_hot.rgba");
const APP: &[u8] = include_bytes!("../assets/app.rgba");

/// Which emoji to show, chosen from the hottest sensor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Band {
    Cold,
    Warm,
    Hot,
}

// Thresholds on the hottest of CPU/GPU, in Celsius. Tune here; they are the only knobs.
const WARM_AT: u8 = 55;
const HOT_AT: u8 = 72;

impl Band {
    /// Band for the hottest sensor reading. `gpu` may be 0 when the dGPU is asleep.
    pub fn for_temps(cpu: u8, gpu: u8) -> Band {
        let hotspot = cpu.max(gpu);
        if hotspot >= HOT_AT {
            Band::Hot
        } else if hotspot >= WARM_AT {
            Band::Warm
        } else {
            Band::Cold
        }
    }
}

struct Icons([HICON; 3]);

// `HICON` is a raw pointer, so the set is neither `Send` nor `Sync` by default. Icon handles
// are process-global and only ever read after creation, so sharing them is fine.
unsafe impl Send for Icons {}
unsafe impl Sync for Icons {}

static ICONS: OnceLock<Icons> = OnceLock::new();

struct AppIcon(HICON);
unsafe impl Send for AppIcon {}
unsafe impl Sync for AppIcon {}

static APP_ICON: OnceLock<AppIcon> = OnceLock::new();

/// The flat fan tile used for the window and the taskbar entry.
pub fn app_icon() -> HICON {
    APP_ICON.get_or_init(|| AppIcon(hicon_from_rgba(APP))).0
}

/// The `HICON` for `band`, creating all three on first call.
pub fn icon(band: Band) -> HICON {
    let set = ICONS.get_or_init(|| {
        Icons([
            hicon_from_rgba(COLD),
            hicon_from_rgba(WARM),
            hicon_from_rgba(HOT),
        ])
    });
    set.0[band as usize]
}

/// Free the icon handles. Called on the way out; the process is exiting anyway.
#[allow(dead_code)]
pub fn destroy() {
    if let Some(set) = ICONS.get() {
        for h in set.0 {
            if !h.is_null() {
                unsafe { windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon(h) };
            }
        }
    }
}

/// Build a 32-bit `HICON` from a top-down RGBA buffer.
///
/// The colour bitmap is a top-down 32bpp DIB (negative height) so the rows need no flipping,
/// and its alpha channel carries the transparency. The icon still needs a mask bitmap; a
/// zeroed 1bpp mask plus a real alpha channel is the combination the shell renders correctly.
fn hicon_from_rgba(rgba: &[u8]) -> HICON {
    let expected = (SIZE * SIZE * 4) as usize;
    if rgba.len() != expected {
        return std::ptr::null_mut();
    }
    unsafe {
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of_val(&bi.bmiHeader) as u32;
        bi.bmiHeader.biWidth = SIZE;
        bi.bmiHeader.biHeight = -SIZE; // top-down
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB as u32;

        let screen = GetDC(std::ptr::null_mut());
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(
            screen,
            &bi,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut() as HANDLE,
            0,
        );
        ReleaseDC(std::ptr::null_mut(), screen);
        if color.is_null() || bits.is_null() {
            return std::ptr::null_mut();
        }

        // Source is RGBA; a 32bpp DIB is BGRA.
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, expected);
        for px in 0..(SIZE * SIZE) as usize {
            let s = px * 4;
            dst[s] = rgba[s + 2];
            dst[s + 1] = rgba[s + 1];
            dst[s + 2] = rgba[s];
            dst[s + 3] = rgba[s + 3];
        }

        // 1bpp mask, rows padded to a WORD: 4 bytes per row for a 32px icon.
        let mask_bits = vec![0u8; (SIZE * 4) as usize];
        let mask = CreateBitmap(SIZE, SIZE, 1, 1, mask_bits.as_ptr() as *const core::ffi::c_void);

        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info);

        DeleteObject(mask as HGDIOBJ);
        DeleteObject(color as HGDIOBJ);
        icon
    }
}
