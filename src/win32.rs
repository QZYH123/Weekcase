//! Shared Win32 chrome: visual styles, DPI, app icon, title bar.

use std::io;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, GetStockObject, CLEARTYPE_QUALITY, DEFAULT_CHARSET,
    DEFAULT_GUI_FONT, FW_NORMAL, FW_SEMIBOLD, HFONT, LOGFONTW, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, LoadIconW, SystemParametersInfoW, HICON, IDI_APPLICATION, NONCLIENTMETRICSW,
    SPI_GETNONCLIENTMETRICS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

/// Resource id from `assets/weekcase.rc`.
pub const IDI_WEEKCASE: PCWSTR = PCWSTR(1 as *const u16);

pub fn init_common_controls() {
    use windows::Win32::UI::Controls::{
        InitCommonControlsEx, ICC_STANDARD_CLASSES, ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX,
    };

    let icc = INITCOMMONCONTROLSEX {
        dwSize: core::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_STANDARD_CLASSES | ICC_WIN95_CLASSES,
    };
    unsafe {
        let _ = InitCommonControlsEx(&icc);
    }
}

pub fn load_app_icon() -> io::Result<(windows::Win32::Foundation::HINSTANCE, HICON)> {
    let hinstance = unsafe { GetModuleHandleW(None) }
        .map(Into::into)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let icon = unsafe { LoadIconW(Some(hinstance), IDI_WEEKCASE) }
        .or_else(|_| unsafe { LoadIconW(None, IDI_APPLICATION) })
        .unwrap_or_default();
    Ok((hinstance, icon))
}

pub fn enable_dark_titlebar(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};

    let enabled: i32 = 1;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &enabled as *const i32 as *const core::ffi::c_void,
            core::mem::size_of::<i32>() as u32,
        );
    }
}

pub fn scale(dpi: u32, px: i32) -> i32 {
    let dpi = dpi.max(96);
    (i64::from(px) * i64::from(dpi) / 96) as i32
}

pub fn cursor_work_area_dpi() -> (RECT, u32) {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint};
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

    let mut pt = POINT::default();
    let _ = unsafe { GetCursorPos(&mut pt) };
    let monitor = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: core::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = unsafe { GetMonitorInfoW(monitor, &mut info) };
    let mut dx = 0u32;
    let mut dy = 0u32;
    let dpi = if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) }.is_ok()
        && dx > 0
    {
        dx
    } else {
        96
    };
    (info.rcWork, dpi)
}

pub fn center_in(work: RECT, width: i32, height: i32) -> POINT {
    POINT {
        x: work.left + (work.right - work.left - width).max(0) / 2,
        y: work.top + (work.bottom - work.top - height).max(0) / 2,
    }
}

pub fn window_size_for_client(
    client_w: i32,
    client_h: i32,
    style: windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE,
    ex: windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE,
    dpi: u32,
) -> (i32, i32) {
    use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;

    let mut rc = RECT {
        left: 0,
        top: 0,
        right: client_w,
        bottom: client_h,
    };
    let _ = unsafe { AdjustWindowRectExForDpi(&mut rc, style, false, ex, dpi) };
    (rc.right - rc.left, rc.bottom - rc.top)
}

pub struct Fonts {
    pub title: HFONT,
    pub body: HFONT,
    owned: bool,
}

impl Fonts {
    pub fn new(dpi: u32) -> Self {
        let body = create_ui_font(dpi, 12, FW_NORMAL.0 as i32);
        let title = create_ui_font(dpi, 16, FW_SEMIBOLD.0 as i32);
        match (title, body) {
            (Some(title), Some(body)) => Self {
                title,
                body,
                owned: true,
            },
            _ => {
                let stock = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
                let h = HFONT(stock.0);
                Self {
                    title: h,
                    body: h,
                    owned: false,
                }
            }
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        if !self.owned {
            return;
        }
        unsafe {
            if !self.title.is_invalid() {
                let _ = DeleteObject(self.title.into());
            }
            if !self.body.is_invalid() && self.body.0 != self.title.0 {
                let _ = DeleteObject(self.body.into());
            }
        }
    }
}

fn create_ui_font(dpi: u32, point: i32, weight: i32) -> Option<HFONT> {
    let mut lf = message_logfont().unwrap_or_else(segoe_logfont);
    lf.lfHeight = -scale(dpi, (point * 96 + 36) / 72);
    lf.lfWeight = weight;
    lf.lfQuality = CLEARTYPE_QUALITY.0;
    lf.lfCharSet = DEFAULT_CHARSET.0;
    let font = unsafe { CreateFontIndirectW(&lf) };
    if font.is_invalid() {
        None
    } else {
        Some(font)
    }
}

fn message_logfont() -> Option<LOGFONTW> {
    let mut ncm = NONCLIENTMETRICSW {
        cbSize: core::mem::size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    unsafe {
        SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            ncm.cbSize,
            Some((&mut ncm as *mut NONCLIENTMETRICSW).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .ok()?;
    Some(ncm.lfMessageFont)
}

fn segoe_logfont() -> LOGFONTW {
    let mut lf = LOGFONTW {
        lfQuality: CLEARTYPE_QUALITY.0,
        lfCharSet: DEFAULT_CHARSET.0,
        lfWeight: FW_NORMAL.0 as i32,
        ..Default::default()
    };
    let name: Vec<u16> = "Segoe UI"
        .encode_utf16()
        .chain(core::iter::once(0))
        .collect();
    let n = name.len().min(lf.lfFaceName.len());
    lf.lfFaceName[..n].copy_from_slice(&name[..n]);
    lf
}

pub fn apply_font(hwnd: HWND, font: HFONT) {
    use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_SETFONT};

    unsafe {
        SendMessageW(
            hwnd,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
}
