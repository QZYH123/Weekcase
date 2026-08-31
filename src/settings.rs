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

pub const FIRST_RUN_LINES: &[&str] = &[
    "默认不会移动已经在文件夹里的旧文件。",
    "下载会在原文件夹里大约留 7 天再归档（刚下的还能打开）。",
    "截图大约几十秒后归档。",
    "之后可以从托盘选择「整理现有文件」来立刻收旧文件。",
    "本程序会随 Windows 登录启动（可在托盘关掉）。",
];

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
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        GetStockObject, GetSysColorBrush, COLOR_WINDOW, DEFAULT_GUI_FONT,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        GetMessageW, GetWindowLongPtrW, IsDialogMessageW, LoadCursorW, MessageBoxW,
        PostQuitMessage, RegisterClassExW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
        SetWindowTextW, ShowWindow, TranslateMessage, BS_DEFPUSHBUTTON, CW_USEDEFAULT,
        ES_AUTOHSCROLL, ES_READONLY, GWLP_USERDATA, HMENU, IDC_ARROW, MB_ICONERROR, MB_OK, MSG,
        SW_HIDE, SW_SHOWNORMAL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY,
        WM_SETFONT, WNDCLASSEXW, WNDPROC, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE,
        WS_EX_CONTROLPARENT, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
    };

    use super::{
        needs_onedrive_warning, source_watch_line, watch_sources, FirstRun, FIRST_RUN_LINES,
        ONEDRIVE_WARNING,
    };
    use crate::known_folders::KnownFolders;
    use crate::tray::{check_archive_root, deny_text, pick_archive_folder};

    const ID_START: u32 = 1;
    const ID_EXIT: u32 = 2;
    const ID_BROWSE: u32 = 100;
    const CLIENT_W: i32 = 520;
    const MARGIN: i32 = 20;
    const LINE: i32 = 22;
    const EDIT_H: i32 = 24;
    const BTN_W: i32 = 88;
    const BTN_H: i32 = 28;
    const BROWSE_W: i32 = 80;
    const WARN_H: i32 = 40;

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
        warn: HWND,
    }

    pub fn run(folders: &KnownFolders) -> io::Result<FirstRun> {
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
        let hinstance = unsafe { GetModuleHandleW(None) }
            .map(Into::into)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let class = WNDCLASSEXW {
            cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: WNDPROC::Some(wndproc),
            hInstance: hinstance,
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            hbrBackground: unsafe { GetSysColorBrush(COLOR_WINDOW) },
            lpszClassName: w!("WeekcaseFirstRun"),
            ..Default::default()
        };
        if unsafe { RegisterClassExW(&class) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "RegisterClassExW failed",
            ));
        }

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
        let ex = WS_EX_CONTROLPARENT;
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: CLIENT_W,
            bottom: dialog_height(),
        };
        unsafe {
            let _ = AdjustWindowRectEx(&mut rc, style, false, ex);
        }

        let mut host = Box::new(Host {
            inner: RefCell::new(Inner {
                folders: folders.clone(),
                sources: watch_sources(folders),
                root: folders.default_root().unwrap_or_default(),
                result: None,
                hwnd: HWND::default(),
                edit: HWND::default(),
                warn: HWND::default(),
            }),
        });
        let host_ptr = host.as_mut() as *mut Host;
        let hwnd = unsafe {
            CreateWindowExW(
                ex,
                w!("WeekcaseFirstRun"),
                w!("Weekcase"),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                rc.right - rc.left,
                rc.bottom - rc.top,
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

        let downloads = source_watch_line("下载", folders.downloads.as_deref());
        let screenshots = source_watch_line("截图", folders.screenshots.as_deref());
        let (edit, warn) = match create_controls(hwnd, hinstance, &downloads, &screenshots) {
            Ok(pair) => pair,
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
            inner.edit = edit;
            inner.warn = warn;
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

    fn dialog_height() -> i32 {
        16 + LINE * 3
            + 8
            + LINE
            + EDIT_H
            + 16
            + LINE * FIRST_RUN_LINES.len() as i32
            + 8
            + WARN_H
            + 16
            + BTN_H
            + 20
    }

    fn create_controls(
        hwnd: HWND,
        hinstance: windows::Win32::Foundation::HINSTANCE,
        downloads: &str,
        screenshots: &str,
    ) -> io::Result<(HWND, HWND)> {
        let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) }.0 as isize;
        let mut y = 16;
        let text_w = CLIENT_W - MARGIN * 2;
        child(
            hwnd,
            hinstance,
            font,
            w!("STATIC"),
            "Weekcase 将监视：",
            WS_CHILD | WS_VISIBLE,
            WINDOW_EX_STYLE(0),
            MARGIN,
            y,
            text_w,
            LINE,
            0,
        )?;
        y += LINE;
        child(
            hwnd,
            hinstance,
            font,
            w!("STATIC"),
            downloads,
            WS_CHILD | WS_VISIBLE,
            WINDOW_EX_STYLE(0),
            MARGIN,
            y,
            text_w,
            LINE,
            0,
        )?;
        y += LINE;
        child(
            hwnd,
            hinstance,
            font,
            w!("STATIC"),
            screenshots,
            WS_CHILD | WS_VISIBLE,
            WINDOW_EX_STYLE(0),
            MARGIN,
            y,
            text_w,
            LINE,
            0,
        )?;
        y += LINE + 8;
        child(
            hwnd,
            hinstance,
            font,
            w!("STATIC"),
            "归档到：",
            WS_CHILD | WS_VISIBLE,
            WINDOW_EX_STYLE(0),
            MARGIN,
            y,
            text_w,
            LINE,
            0,
        )?;
        y += LINE;
        let edit = child(
            hwnd,
            hinstance,
            font,
            w!("EDIT"),
            "",
            WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | WINDOW_STYLE((ES_READONLY | ES_AUTOHSCROLL) as u32),
            WS_EX_CLIENTEDGE,
            MARGIN,
            y,
            text_w - BROWSE_W - 8,
            EDIT_H,
            0,
        )?;
        child(
            hwnd,
            hinstance,
            font,
            w!("BUTTON"),
            "浏览…",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            WINDOW_EX_STYLE(0),
            CLIENT_W - MARGIN - BROWSE_W,
            y,
            BROWSE_W,
            EDIT_H,
            ID_BROWSE as i32,
        )?;
        y += EDIT_H + 16;
        for line in FIRST_RUN_LINES {
            child(
                hwnd,
                hinstance,
                font,
                w!("STATIC"),
                line,
                WS_CHILD | WS_VISIBLE,
                WINDOW_EX_STYLE(0),
                MARGIN,
                y,
                text_w,
                LINE,
                0,
            )?;
            y += LINE;
        }
        y += 8;
        let warn = child(
            hwnd,
            hinstance,
            font,
            w!("STATIC"),
            "",
            WS_CHILD | WS_VISIBLE,
            WINDOW_EX_STYLE(0),
            MARGIN,
            y,
            text_w,
            WARN_H,
            0,
        )?;
        y += WARN_H + 16;
        child(
            hwnd,
            hinstance,
            font,
            w!("BUTTON"),
            "开始",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            WINDOW_EX_STYLE(0),
            CLIENT_W - MARGIN - BTN_W * 2 - 12,
            y,
            BTN_W,
            BTN_H,
            ID_START as i32,
        )?;
        child(
            hwnd,
            hinstance,
            font,
            w!("BUTTON"),
            "退出",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            WINDOW_EX_STYLE(0),
            CLIENT_W - MARGIN - BTN_W,
            y,
            BTN_W,
            BTN_H,
            ID_EXIT as i32,
        )?;
        Ok((edit, warn))
    }

    #[allow(clippy::too_many_arguments)]
    fn child(
        parent: HWND,
        hinstance: windows::Win32::Foundation::HINSTANCE,
        font: isize,
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
        unsafe {
            SendMessageW(
                hwnd,
                WM_SETFONT,
                Some(WPARAM(font as usize)),
                Some(LPARAM(1)),
            );
        }
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
                WM_CLOSE => {
                    self.dismiss();
                    LRESULT(0)
                }
                WM_DESTROY => LRESULT(0),
                _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
            }
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

        fn sync_widgets(&self) {
            set_text(self.edit, &self.root.to_string_lossy());
            let warn = if !self.root.as_os_str().is_empty()
                && needs_onedrive_warning(&self.root, &self.sources, &self.folders)
            {
                ONEDRIVE_WARNING
            } else {
                ""
            };
            set_text(self.warn, warn);
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
        let text = FIRST_RUN_LINES.join("");
        assert!(text.contains("默认不会移动已经在文件夹里的旧文件"));
        assert!(text.contains("整理现有文件"));
        assert!(text.contains("随 Windows 登录启动"));
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
