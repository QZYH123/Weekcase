use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config;
use crate::known_folders::{canonical_key, KnownFolders};
use crate::paths::Paths;
use crate::state::AppState;
use crate::tray::enabled_source_paths;

pub const SKIP_FIRST_RUN_ENV: &str = "WEEKCASE_SKIP_FIRST_RUN";

pub const MISSING_SOURCE: &str = "尚未创建，出现后自动监视";

pub const ONEDRIVE_WARNING: &str =
    "警告：该路径由 OneDrive 同步。归档会上传到云端。建议改到本地磁盘。";

pub const FIRST_RUN_TITLE: &str = "归档下载和截图";

pub const FIRST_RUN_SUBTITLE: &str =
    "文件写完并过了冷静期后才会移动。默认不会收拾已经在文件夹里的旧文件。";

pub const FIRST_RUN_LINES: &[&str] = &[
    "下载会在原文件夹里大约留 7 天再归档（刚下的还能打开）。",
    "截图大约几十秒后归档。",
    "之后可以从托盘选择「整理现有文件」来立刻收旧文件。",
    "本程序会随 Windows 登录启动（可在托盘关掉）。",
];

pub const TRAY_HINT: &str = "开始后，请到任务栏右下角（隐藏图标）找到 Weekcase。";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirstRun {
    Start { root: Option<PathBuf> },
    Exit,
}

pub fn needs_first_run(config_path: &Path) -> bool {
    !config_path.is_file()
}

pub fn skip_from_env(value: Option<&str>) -> bool {
    match value {
        None | Some("") | Some("0") => false,
        Some(_) => true,
    }
}

pub fn skip_first_run_dialog() -> bool {
    #[cfg(not(windows))]
    {
        true
    }
    #[cfg(windows)]
    {
        skip_from_env(std::env::var(SKIP_FIRST_RUN_ENV).ok().as_deref())
    }
}

pub fn source_watch_line(kind: &str, path: Option<&Path>) -> String {
    match path {
        Some(p) => format!("{kind}：{}", p.display()),
        None => format!("{kind}：{MISSING_SOURCE}"),
    }
}

pub fn needs_onedrive_warning(root: &Path, sources: &[PathBuf], folders: &KnownFolders) -> bool {
    folders.is_onedrive_path(root) || sources.iter().any(|s| folders.is_onedrive_path(s))
}

pub fn watch_sources(folders: &KnownFolders) -> Vec<PathBuf> {
    enabled_source_paths(&config::Config::default(), folders)
}

pub fn prompt_first_run(folders: &KnownFolders) -> io::Result<FirstRun> {
    #[cfg(windows)]
    {
        if skip_first_run_dialog() {
            Ok(FirstRun::Start {
                root: folders.default_root(),
            })
        } else {
            win::run(folders)
        }
    }
    #[cfg(not(windows))]
    {
        Ok(FirstRun::Start {
            root: folders.default_root(),
        })
    }
}

pub fn commit_first_run(
    paths: &Paths,
    root: Option<&Path>,
    default_root: Option<&Path>,
    now: SystemTime,
) -> io::Result<config::Config> {
    if let Some(root) = root {
        fs::create_dir_all(root)?;
    }
    let cfg_path = paths.config_file();
    config::write_default_if_missing(&cfg_path)?;
    if let Some(root) = root {
        let persist = match default_root {
            Some(def) => canonical_key(root) != canonical_key(def),
            None => true,
        };
        if persist {
            config::persist_string(&cfg_path, "destination", "root", &root.to_string_lossy())?;
        }
    }
    let mut state = AppState::load(&paths.state_file())?;
    state.stamp_first_run(now);
    state.save(&paths.state_file())?;
    config::load(&cfg_path)
}

#[cfg(windows)]
mod win {
    use std::cell::RefCell;
    use std::io;
    use std::path::PathBuf;

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{
        GetLastError, COLORREF, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{
        GetSysColor, GetSysColorBrush, SetBkColor, SetTextColor, COLOR_GRAYTEXT, COLOR_INFOBK,
        COLOR_INFOTEXT, COLOR_WINDOW, HDC,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        GetWindowLongPtrW, IsDialogMessageW, LoadCursorW, MessageBoxW, PostQuitMessage,
        RegisterClassExW, SetForegroundWindow, SetWindowLongPtrW, SetWindowTextW, ShowWindow,
        TranslateMessage, BS_DEFPUSHBUTTON, ES_AUTOHSCROLL, ES_READONLY, GWLP_USERDATA, HMENU,
        IDC_ARROW, MB_ICONERROR, MB_OK, MSG, SW_HIDE, SW_SHOWNORMAL, WINDOW_EX_STYLE, WINDOW_STYLE,
        WM_CLOSE, WM_COMMAND, WM_CTLCOLORSTATIC, WM_DESTROY, WNDCLASSEXW, WNDPROC, WS_CAPTION,
        WS_CHILD, WS_EX_CLIENTEDGE, WS_EX_CONTROLPARENT, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP,
        WS_VISIBLE,
    };

    use super::{
        needs_onedrive_warning, source_watch_line, watch_sources, FirstRun, FIRST_RUN_LINES,
        FIRST_RUN_SUBTITLE, FIRST_RUN_TITLE, ONEDRIVE_WARNING, TRAY_HINT,
    };
    use crate::known_folders::KnownFolders;
    use crate::tray::{check_archive_root, deny_text, pick_archive_folder};
    use crate::win32::{
        apply_font, center_in, cursor_work_area_dpi, enable_dark_titlebar, init_common_controls,
        load_app_icon, scale, window_size_for_client, Fonts,
    };

    const ID_START: u32 = 1;
    const ID_EXIT: u32 = 2;
    const ID_BROWSE: u32 = 100;
    const SS_NOPREFIX: u32 = 0x80;
    const SS_ENDELLIPSIS: u32 = 0x4000;

    struct Host {
        inner: RefCell<Inner>,
    }

    struct Inner {
        folders: KnownFolders,
        sources: Vec<PathBuf>,
        root: PathBuf,
        result: Option<FirstRun>,
        hwnd: HWND,
        edit: HWND,
        subtitle: HWND,
        footer: HWND,
        warn: bool,
        fonts: Option<Fonts>,
    }

    struct Layout {
        dpi: u32,
        client_w: i32,
        margin: i32,
        line: i32,
        edit_h: i32,
        btn_w: i32,
        btn_h: i32,
        browse_w: i32,
        footer_h: i32,
        client_h: i32,
    }

    impl Layout {
        fn new(dpi: u32) -> Self {
            let s = |px| scale(dpi, px);
            let margin = s(24);
            let line = s(22);
            let edit_h = s(26);
            let btn_h = s(32);
            let footer_h = s(40);
            let notes = FIRST_RUN_LINES.len() as i32;
            let client_h = s(24)
                + s(28)
                + s(6)
                + s(40)
                + s(18)
                + s(20)
                + line * 2
                + s(16)
                + s(20)
                + edit_h
                + s(16)
                + line * notes
                + s(12)
                + footer_h
                + s(16)
                + btn_h
                + s(20);
            Self {
                dpi,
                client_w: s(500),
                margin,
                line,
                edit_h,
                btn_w: s(96),
                btn_h,
                browse_w: s(88),
                footer_h,
                client_h,
            }
        }
    }

    pub fn run(folders: &KnownFolders) -> io::Result<FirstRun> {
        init_common_controls();
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let com_owned = hr.0 == 0;
        let result = run_dialog(folders);
        if com_owned {
            unsafe {
                CoUninitialize();
            }
        }
        result
    }

    fn run_dialog(folders: &KnownFolders) -> io::Result<FirstRun> {
        let (hinstance, icon) = load_app_icon()?;
        let class = WNDCLASSEXW {
            cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: WNDPROC::Some(wndproc),
            hInstance: hinstance,
            hIcon: icon,
            hIconSm: icon,
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            hbrBackground: unsafe { GetSysColorBrush(COLOR_WINDOW) },
            lpszClassName: w!("WeekcaseFirstRun"),
            ..Default::default()
        };
        if unsafe { RegisterClassExW(&class) } == 0
            && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
        {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "RegisterClassExW failed",
            ));
        }

        let (work, dpi) = cursor_work_area_dpi();
        let layout = Layout::new(dpi);
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
        let ex = WS_EX_CONTROLPARENT;
        let (win_w, win_h) =
            window_size_for_client(layout.client_w, layout.client_h, style, ex, dpi);
        let pos = center_in(work, win_w, win_h);

        let mut host = Box::new(Host {
            inner: RefCell::new(Inner {
                folders: folders.clone(),
                sources: watch_sources(folders),
                root: folders.default_root().unwrap_or_default(),
                result: None,
                hwnd: HWND::default(),
                edit: HWND::default(),
                subtitle: HWND::default(),
                footer: HWND::default(),
                warn: false,
                fonts: Some(Fonts::new(dpi)),
            }),
        });
        let host_ptr = host.as_mut() as *mut Host;
        let hwnd = unsafe {
            CreateWindowExW(
                ex,
                w!("WeekcaseFirstRun"),
                w!("Weekcase"),
                style,
                pos.x,
                pos.y,
                win_w,
                win_h,
                None,
                None,
                Some(hinstance),
                None,
            )
        }
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, host_ptr as isize);
        }
        enable_dark_titlebar(hwnd);

        let downloads = source_watch_line("下载", folders.downloads.as_deref());
        let screenshots = source_watch_line("截图", folders.screenshots.as_deref());
        let fonts_body;
        let fonts_title;
        {
            let inner = host.inner.borrow();
            let fonts = inner.fonts.as_ref().expect("fonts");
            fonts_body = fonts.body;
            fonts_title = fonts.title;
        }
        let widgets = match create_controls(
            hwnd,
            hinstance,
            &layout,
            fonts_title,
            fonts_body,
            &downloads,
            &screenshots,
        ) {
            Ok(w) => w,
            Err(err) => {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    let _ = DestroyWindow(hwnd);
                }
                return Err(err);
            }
        };
        {
            let mut inner = host.inner.borrow_mut();
            inner.hwnd = hwnd;
            inner.edit = widgets.edit;
            inner.subtitle = widgets.subtitle;
            inner.footer = widgets.footer;
            inner.sync_widgets();
        }
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
        }

        let mut msg = MSG::default();
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
            if !unsafe { IsDialogMessageW(hwnd, &msg) }.as_bool() {
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(hwnd);
        }
        let Host { inner } = *host;
        Ok(inner.into_inner().result.unwrap_or(FirstRun::Exit))
    }

    struct Widgets {
        edit: HWND,
        subtitle: HWND,
        footer: HWND,
    }

    fn create_controls(
        hwnd: HWND,
        hinstance: windows::Win32::Foundation::HINSTANCE,
        layout: &Layout,
        title_font: windows::Win32::Graphics::Gdi::HFONT,
        body_font: windows::Win32::Graphics::Gdi::HFONT,
        downloads: &str,
        screenshots: &str,
    ) -> io::Result<Widgets> {
        let s = |px| scale(layout.dpi, px);
        let text_w = layout.client_w - layout.margin * 2;
        let mut y = s(24);
        child(
            hwnd,
            hinstance,
            title_font,
            w!("STATIC"),
            FIRST_RUN_TITLE,
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(SS_NOPREFIX),
            WINDOW_EX_STYLE(0),
            layout.margin,
            y,
            text_w,
            s(28),
            0,
        )?;
        y += s(28) + s(6);
        let subtitle = child(
            hwnd,
            hinstance,
            body_font,
            w!("STATIC"),
            FIRST_RUN_SUBTITLE,
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(SS_NOPREFIX),
            WINDOW_EX_STYLE(0),
            layout.margin,
            y,
            text_w,
            s(40),
            0,
        )?;
        y += s(40) + s(18);
        child(
            hwnd,
            hinstance,
            body_font,
            w!("STATIC"),
            "监视",
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(SS_NOPREFIX),
            WINDOW_EX_STYLE(0),
            layout.margin,
            y,
            text_w,
            s(20),
            0,
        )?;
        y += s(20);
        child(
            hwnd,
            hinstance,
            body_font,
            w!("STATIC"),
            &downloads,
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(SS_NOPREFIX | SS_ENDELLIPSIS),
            WINDOW_EX_STYLE(0),
            layout.margin,
            y,
            text_w,
            layout.line,
            0,
        )?;
        y += layout.line;
        child(
            hwnd,
            hinstance,
            body_font,
            w!("STATIC"),
            screenshots,
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(SS_NOPREFIX | SS_ENDELLIPSIS),
            WINDOW_EX_STYLE(0),
            layout.margin,
            y,
            text_w,
            layout.line,
            0,
        )?;
        y += layout.line + s(16);
        child(
            hwnd,
            hinstance,
            body_font,
            w!("STATIC"),
            "归档到",
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(SS_NOPREFIX),
            WINDOW_EX_STYLE(0),
            layout.margin,
            y,
            text_w,
            s(20),
            0,
        )?;
        y += s(20);
        let edit = child(
            hwnd,
            hinstance,
            body_font,
            w!("EDIT"),
            "",
            WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | WINDOW_STYLE((ES_READONLY | ES_AUTOHSCROLL) as u32),
            WS_EX_CLIENTEDGE,
            layout.margin,
            y,
            text_w - layout.browse_w - s(8),
            layout.edit_h,
            0,
        )?;
        child(
            hwnd,
            hinstance,
            body_font,
            w!("BUTTON"),
            "浏览(&B)…",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            WINDOW_EX_STYLE(0),
            layout.client_w - layout.margin - layout.browse_w,
            y,
            layout.browse_w,
            layout.edit_h,
            ID_BROWSE as i32,
        )?;
        y += layout.edit_h + s(16);
        for line in FIRST_RUN_LINES {
            child(
                hwnd,
                hinstance,
                body_font,
                w!("STATIC"),
                line,
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(SS_NOPREFIX | SS_ENDELLIPSIS),
                WINDOW_EX_STYLE(0),
                layout.margin,
                y,
                text_w,
                layout.line,
                0,
            )?;
            y += layout.line;
        }
        y += s(12);
        let footer = child(
            hwnd,
            hinstance,
            body_font,
            w!("STATIC"),
            TRAY_HINT,
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(SS_NOPREFIX),
            WINDOW_EX_STYLE(0),
            layout.margin,
            y,
            text_w,
            layout.footer_h,
            0,
        )?;
        y += layout.footer_h + s(16);
        child(
            hwnd,
            hinstance,
            body_font,
            w!("BUTTON"),
            "开始(&S)",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            WINDOW_EX_STYLE(0),
            layout.client_w - layout.margin - layout.btn_w * 2 - s(12),
            y,
            layout.btn_w,
            layout.btn_h,
            ID_START as i32,
        )?;
        child(
            hwnd,
            hinstance,
            body_font,
            w!("BUTTON"),
            "退出(&X)",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            WINDOW_EX_STYLE(0),
            layout.client_w - layout.margin - layout.btn_w,
            y,
            layout.btn_w,
            layout.btn_h,
            ID_EXIT as i32,
        )?;
        Ok(Widgets {
            edit,
            subtitle,
            footer,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn child(
        parent: HWND,
        hinstance: windows::Win32::Foundation::HINSTANCE,
        font: windows::Win32::Graphics::Gdi::HFONT,
        class: PCWSTR,
        text: &str,
        style: WINDOW_STYLE,
        ex: WINDOW_EX_STYLE,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        id: i32,
    ) -> io::Result<HWND> {
        let wide: Vec<u16> = text.encode_utf16().chain(core::iter::once(0)).collect();
        let hwnd = unsafe {
            CreateWindowExW(
                ex,
                class,
                PCWSTR(wide.as_ptr()),
                style,
                x,
                y,
                w,
                h,
                Some(parent),
                Some(HMENU(id as usize as *mut core::ffi::c_void)),
                Some(hinstance),
                None,
            )
        }
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        apply_font(hwnd, font);
        Ok(hwnd)
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Host;
        if ptr.is_null() {
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        // SAFETY: `run_dialog` owns Host until it clears GWLP_USERDATA.
        unsafe { (*ptr).dispatch(hwnd, msg, wparam, lparam) }
    }

    impl Host {
        fn dispatch(&self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
            match self.inner.try_borrow_mut() {
                Ok(mut inner) => inner.handle(hwnd, msg, wparam, lparam),
                Err(_) => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
            }
        }
    }

    impl Inner {
        fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
            match msg {
                WM_COMMAND => {
                    match (wparam.0 as u32) & 0xFFFF {
                        ID_START => self.accept(),
                        ID_EXIT => self.dismiss(),
                        ID_BROWSE => self.browse(),
                        _ => {}
                    }
                    LRESULT(0)
                }
                WM_CTLCOLORSTATIC => self.color_static(wparam, lparam),
                WM_CLOSE => {
                    self.dismiss();
                    LRESULT(0)
                }
                WM_DESTROY => {
                    self.fonts = None;
                    LRESULT(0)
                }
                _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
            }
        }

        fn color_static(&self, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
            let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
            let child = HWND(lparam.0 as *mut core::ffi::c_void);
            let (text, back, brush) = if child.0 == self.footer.0 && self.warn {
                (COLOR_INFOTEXT, COLOR_INFOBK, unsafe {
                    GetSysColorBrush(COLOR_INFOBK)
                })
            } else if child.0 == self.subtitle.0 || child.0 == self.footer.0 {
                (COLOR_GRAYTEXT, COLOR_WINDOW, unsafe {
                    GetSysColorBrush(COLOR_WINDOW)
                })
            } else {
                return unsafe { DefWindowProcW(self.hwnd, WM_CTLCOLORSTATIC, wparam, lparam) };
            };
            unsafe {
                SetTextColor(hdc, COLORREF(GetSysColor(text)));
                SetBkColor(hdc, COLORREF(GetSysColor(back)));
            }
            LRESULT(brush.0 as isize)
        }

        fn accept(&mut self) {
            if self.root.as_os_str().is_empty() {
                self.error("请选择归档文件夹");
                return;
            }
            if let Err(reason) = check_archive_root(&self.root, &self.sources, &self.folders) {
                self.error(deny_text(reason));
                return;
            }
            if let Err(err) = std::fs::create_dir_all(&self.root) {
                self.error(&format!("无法创建归档文件夹：{err}"));
                return;
            }
            self.finish(FirstRun::Start {
                root: Some(self.root.clone()),
            });
        }

        fn dismiss(&mut self) {
            self.finish(FirstRun::Exit);
        }

        fn finish(&mut self, result: FirstRun) {
            self.result = Some(result);
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                PostQuitMessage(0);
            }
        }

        fn browse(&mut self) {
            let start = (!self.root.as_os_str().is_empty()).then_some(self.root.as_path());
            match pick_archive_folder(self.hwnd, start) {
                Ok(Some(path)) => {
                    if let Err(reason) = check_archive_root(&path, &self.sources, &self.folders) {
                        self.error(deny_text(reason));
                    } else {
                        self.root = path;
                        self.sync_widgets();
                    }
                }
                Ok(None) => {}
                Err(err) => self.error(&format!("无法选择文件夹：{err}")),
            }
        }

        fn sync_widgets(&mut self) {
            set_text(self.edit, &self.root.to_string_lossy());
            self.warn = !self.root.as_os_str().is_empty()
                && needs_onedrive_warning(&self.root, &self.sources, &self.folders);
            set_text(
                self.footer,
                if self.warn {
                    ONEDRIVE_WARNING
                } else {
                    TRAY_HINT
                },
            );
        }

        fn error(&self, text: &str) {
            let wide: Vec<u16> = text.encode_utf16().chain(core::iter::once(0)).collect();
            unsafe {
                MessageBoxW(
                    Some(self.hwnd),
                    PCWSTR(wide.as_ptr()),
                    w!("Weekcase"),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
    }

    fn set_text(hwnd: HWND, text: &str) {
        if hwnd.is_invalid() {
            return;
        }
        let wide: Vec<u16> = text.encode_utf16().chain(core::iter::once(0)).collect();
        let _ = unsafe { SetWindowTextW(hwnd, PCWSTR(wide.as_ptr())) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::known_folders::DenyReason;
    use crate::tray::check_archive_root;
    use std::time::{Duration, UNIX_EPOCH};

    fn folders() -> KnownFolders {
        KnownFolders {
            downloads: Some(PathBuf::from(r"C:\Users\a\Downloads")),
            screenshots: Some(PathBuf::from(r"C:\Users\a\Pictures\Screenshots")),
            documents: Some(PathBuf::from(r"C:\Users\a\Documents")),
            onedrive: Some(PathBuf::from(r"C:\Users\a\OneDrive")),
            profile: Some(PathBuf::from(r"C:\Users\a")),
            windows: Some(PathBuf::from(r"C:\Windows")),
            program_files: vec![PathBuf::from(r"C:\Program Files")],
            program_data: Some(PathBuf::from(r"C:\ProgramData")),
        }
    }

    fn temp_paths() -> (Paths, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "weekcase-first-run-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        (
            Paths {
                config_dir: root.join("cfg"),
                local_dir: root.join("local"),
                portable: false,
            },
            root,
        )
    }

    #[test]
    fn missing_config_is_first_run() {
        assert!(needs_first_run(Path::new("/no/such/weekcase/config.toml")));
    }

    #[test]
    fn skip_env_only_accepts_nonzero() {
        assert!(!skip_from_env(None));
        assert!(!skip_from_env(Some("")));
        assert!(!skip_from_env(Some("0")));
        assert!(skip_from_env(Some("1")));
        assert!(skip_from_env(Some("true")));
    }

    #[cfg(not(windows))]
    #[test]
    fn linux_skips_dialog_and_continues() {
        assert!(skip_first_run_dialog());
        match prompt_first_run(&KnownFolders::default()).unwrap() {
            FirstRun::Start { root } => assert!(root.is_none()),
            FirstRun::Exit => panic!("skip must continue"),
        }
    }

    #[test]
    fn source_lines_use_resolved_or_missing() {
        let f = folders();
        assert_eq!(
            source_watch_line("下载", f.downloads.as_deref()),
            r"下载：C:\Users\a\Downloads"
        );
        assert_eq!(
            source_watch_line("截图", None),
            format!("截图：{MISSING_SOURCE}")
        );
    }

    #[test]
    fn copy_covers_existing_files_and_autostart() {
        let text = format!(
            "{}{}{}",
            FIRST_RUN_SUBTITLE,
            FIRST_RUN_LINES.join(""),
            TRAY_HINT
        );
        assert!(text.contains("默认不会收拾已经在文件夹里的旧文件"));
        assert!(text.contains("整理现有文件"));
        assert!(text.contains("随 Windows 登录启动"));
        assert!(text.contains("任务栏右下角"));
        assert!(FIRST_RUN_TITLE.contains("归档"));
        assert!(ONEDRIVE_WARNING.contains("OneDrive"));
    }

    #[test]
    fn onedrive_warning_if_root_or_source() {
        let f = folders();
        let local_root = Path::new(r"C:\Users\a\Documents\Weekcase");
        let local_src = vec![PathBuf::from(r"C:\Users\a\Downloads")];
        assert!(!needs_onedrive_warning(local_root, &local_src, &f));
        assert!(needs_onedrive_warning(
            Path::new(r"C:\Users\a\OneDrive\Documents\Weekcase"),
            &local_src,
            &f
        ));
        assert!(needs_onedrive_warning(
            local_root,
            &[PathBuf::from(r"C:\Users\a\OneDrive\Downloads")],
            &f
        ));
    }

    #[test]
    fn dest_inside_source_is_rejected() {
        let f = folders();
        let sources = watch_sources(&f);
        assert_eq!(
            check_archive_root(Path::new(r"C:\Users\a\Downloads\Weekcase"), &sources, &f),
            Err(DenyReason::DestInsideSource)
        );
        assert!(
            check_archive_root(Path::new(r"C:\Users\a\Documents\Weekcase"), &sources, &f).is_ok()
        );
    }

    #[test]
    fn commit_writes_config_and_first_run_at() {
        let (paths, dir) = temp_paths();
        let def = dir.join("Weekcase");
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let cfg = commit_first_run(&paths, Some(&def), Some(&def), now).unwrap();
        assert!(paths.config_file().is_file());
        assert!(cfg.destination.root.is_none());
        assert!(def.is_dir());
        let state = AppState::load(&paths.state_file()).unwrap();
        assert_eq!(state.first_run_unix(), Some(1_700_000_000));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_persists_custom_root() {
        let (paths, dir) = temp_paths();
        let def = dir.join("Weekcase");
        let custom = dir.join("Archive");
        let cfg = commit_first_run(&paths, Some(&custom), Some(&def), UNIX_EPOCH).unwrap();
        assert_eq!(
            cfg.destination.root.as_deref(),
            Some(custom.to_str().unwrap())
        );
        assert!(custom.is_dir());
        assert!(!def.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_without_root_writes_default_only() {
        let (paths, dir) = temp_paths();
        let cfg = commit_first_run(&paths, None, None, UNIX_EPOCH).unwrap();
        assert_eq!(cfg, config::Config::default());
        assert!(paths.config_file().is_file());
        assert!(AppState::load(&paths.state_file())
            .unwrap()
            .first_run_at
            .is_some());
        let _ = fs::remove_dir_all(&dir);
    }
}
