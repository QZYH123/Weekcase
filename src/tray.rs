use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
#[cfg(not(windows))]
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::candidate::Candidate;
use crate::config::Config;
use crate::execute::ExecCmd;
use crate::known_folders::{deny_destination, DenyReason, KnownFolders};
use crate::paths::Paths;
use crate::state::AppState;
use crate::undo::UndoError;
use crate::watch::WatchCmd;

pub const KF_DELAY_MS: u32 = 15_000;

pub struct App {
    pub paths: Paths,
    pub folders: Arc<Mutex<KnownFolders>>,
    pub cfg: Arc<Mutex<Config>>,
    pub state: Arc<Mutex<AppState>>,
    pub candidates: Arc<Mutex<HashMap<PathBuf, Candidate>>>,
    pub paused: Arc<AtomicBool>,
    pub shutdown: Arc<AtomicBool>,
    pub archived_today: Arc<AtomicU32>,
    pub watch: Option<JoinHandle<()>>,
    pub watch_tx: Sender<WatchCmd>,
    pub pending_watch_rx: Option<Receiver<WatchCmd>>,
    pub stab: Option<JoinHandle<()>>,
    pub exec_tx: Sender<ExecCmd>,
    pub pending_exec_rx: Option<Receiver<ExecCmd>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveRoot {
    Accepted,
    OneDrive,
}

pub fn run(app: App) -> io::Result<ExitCode> {
    #[cfg(windows)]
    {
        win::run(app)
    }
    #[cfg(not(windows))]
    {
        run_stub(app)
    }
}

pub fn show_error(text: &str) {
    #[cfg(windows)]
    {
        win::message_box(None, text, false);
    }
    #[cfg(not(windows))]
    {
        eprintln!("weekcase: {text}");
    }
}

pub fn enabled_source_paths(cfg: &Config, folders: &KnownFolders) -> Vec<PathBuf> {
    cfg.sources
        .iter()
        .filter(|s| s.enabled)
        .filter_map(|s| s.resolved_path(folders))
        .collect()
}

pub fn check_archive_root(
    dest: &Path,
    sources: &[PathBuf],
    folders: &KnownFolders,
) -> Result<ArchiveRoot, DenyReason> {
    if let Some(reason) = deny_destination(dest, sources, folders) {
        return Err(reason);
    }
    if folders.is_onedrive_path(dest) {
        Ok(ArchiveRoot::OneDrive)
    } else {
        Ok(ArchiveRoot::Accepted)
    }
}

pub fn deny_text(reason: DenyReason) -> &'static str {
    match reason {
        DenyReason::Unc => "不能使用网络路径",
        DenyReason::DriveRoot => "不能使用盘符根目录",
        DenyReason::RemoteDrive => "不能使用网络驱动器",
        DenyReason::NoRootDir => "路径无效",
        DenyReason::ProfileRoot => "不能使用用户配置根目录",
        DenyReason::Windows => "不能使用 Windows 目录",
        DenyReason::ProgramFiles => "不能使用 Program Files",
        DenyReason::ProgramData => "不能使用 ProgramData",
        DenyReason::DestInsideSource => "归档目录不能落在监视源里面",
    }
}

pub fn tooltip_text(paused: bool, archived_today: u32, overflow: bool) -> String {
    if overflow {
        "有文件没进队，整理未完成".into()
    } else if paused {
        format!("已暂停 · 今日已归档 {archived_today}")
    } else {
        format!("监视中 · 今日已归档 {archived_today}")
    }
}

pub fn undo_error_text(err: &UndoError) -> String {
    match err {
        UndoError::Empty => "没有可撤销的记录".into(),
        UndoError::SourceExists => "原路径已有文件，无法撤销".into(),
        UndoError::SplitCopy => "跨盘复制不完整，无法撤销".into(),
        UndoError::Io(e) => format!("撤销失败：{e}"),
    }
}

pub fn autostart_command(exe: &Path) -> String {
    format!("\"{}\"", display_exe(exe))
}

fn display_exe(exe: &Path) -> String {
    let s = exe.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if rest.len() >= 2 && rest.as_bytes().get(1) == Some(&b':') {
            return rest.to_string();
        }
    }
    s.into_owned()
}

#[cfg(not(windows))]
fn run_stub(app: App) -> io::Result<ExitCode> {
    let App {
        watch,
        shutdown,
        stab,
        ..
    } = app;
    if let Some(watch) = watch {
        let _ = watch.join();
    }
    shutdown.store(true, Ordering::Relaxed);
    if let Some(stab) = stab {
        let _ = stab.join();
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(windows)]
fn cfg_lock(cfg: &Mutex<Config>) -> std::sync::MutexGuard<'_, Config> {
    cfg.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(windows)]
fn state_lock(state: &Mutex<AppState>) -> std::sync::MutexGuard<'_, AppState> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(windows)]
fn folders_lock(folders: &Mutex<KnownFolders>) -> std::sync::MutexGuard<'_, KnownFolders> {
    folders.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(windows)]
pub use win::apply_autostart;

#[cfg(windows)]
pub use win::pick_directory as pick_archive_folder;

#[cfg(windows)]
mod win {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::mpsc::Sender;
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::Graphics::Gdi::HBRUSH;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IBindCtx,
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileDialog, IShellItem, SHCreateItemFromParsingName, ShellExecuteW,
        Shell_NotifyIconW, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, NIF_ICON, NIF_MESSAGE,
        NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, SIGDN_FILESYSPATH,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
        DispatchMessageW, GetCursorPos, GetMessageW, GetWindowLongPtrW, KillTimer, LoadIconW,
        MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW,
        SetForegroundWindow, SetTimer, SetWindowLongPtrW, TrackPopupMenu, TranslateMessage,
        CW_USEDEFAULT, GWLP_USERDATA, HICON, IDI_APPLICATION, IDYES, MB_ICONERROR, MB_ICONWARNING,
        MB_OK, MB_YESNO, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, SW_SHOWNORMAL,
        TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_DESTROY, WM_LBUTTONUP, WM_NULL,
        WM_RBUTTONUP, WM_TIMER, WNDCLASSEXW, WNDPROC, WS_EX_TOOLWINDOW, WS_POPUP,
    };

    use super::{
        cfg_lock, check_archive_root, deny_text, enabled_source_paths, folders_lock, state_lock,
        tooltip_text, undo_error_text, App, ArchiveRoot, KF_DELAY_MS,
    };
    use crate::candidate::Candidate;
    use crate::config::Config;
    use crate::known_folders::KnownFolders;
    use crate::paths::Paths;
    use crate::state::AppState;
    use crate::watch::WatchCmd;

    const WM_TRAY: u32 = WM_APP + 1;
    const TRAY_ID: u32 = 1;
    const TIMER_TIP: usize = 1;
    const TIMER_BOOT: usize = 2;
    const ID_PAUSE: usize = 1001;
    const ID_UNDO: usize = 1002;
    const ID_SWEEP: usize = 1003;
    const ID_PICK: usize = 1004;
    const ID_OPEN_ROOT: usize = 1005;
    const ID_OPEN_LOG: usize = 1006;
    const ID_RELOAD: usize = 1007;
    const ID_AUTOSTART: usize = 1008;
    const ID_EXIT: usize = 1009;
    /// Resource id from `assets/weekcase.rc`.
    const IDI_WEEKCASE: PCWSTR = PCWSTR(1 as *const u16);

    fn load_app_icon(hinstance: HINSTANCE) -> HICON {
        unsafe { LoadIconW(Some(hinstance), IDI_WEEKCASE) }
            .or_else(|_| unsafe { LoadIconW(None, IDI_APPLICATION) })
            .unwrap_or_default()
    }

    struct Host {
        inner: RefCell<Inner>,
    }

    struct Inner {
        paths: Paths,
        folders: Arc<Mutex<KnownFolders>>,
        cfg: Arc<Mutex<Config>>,
        state: Arc<Mutex<AppState>>,
        candidates: Arc<Mutex<HashMap<PathBuf, Candidate>>>,
        paused: Arc<AtomicBool>,
        shutdown: Arc<AtomicBool>,
        archived_today: Arc<AtomicU32>,
        watch: Option<JoinHandle<()>>,
        watch_tx: Sender<WatchCmd>,
        pending_watch_rx: Option<std::sync::mpsc::Receiver<WatchCmd>>,
        stab: Option<JoinHandle<()>>,
        exec_tx: Sender<crate::execute::ExecCmd>,
        pending_exec_rx: Option<std::sync::mpsc::Receiver<crate::execute::ExecCmd>>,
        hwnd: HWND,
        icon: HICON,
        icon_added: bool,
        quitting: bool,
        taskbar_created: u32,
    }

    pub fn run(app: App) -> io::Result<ExitCode> {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let com_owned = hr.0 == 0;

        let hinstance = unsafe { GetModuleHandleW(None) }
            .map(Into::into)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let icon = load_app_icon(hinstance);
        let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };

        let class = WNDCLASSEXW {
            cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: WNDPROC::Some(wndproc),
            hInstance: hinstance,
            hIcon: icon,
            hIconSm: icon,
            hbrBackground: HBRUSH::default(),
            lpszClassName: w!("WeekcaseTray"),
            ..Default::default()
        };
        if unsafe { RegisterClassExW(&class) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "RegisterClassExW failed",
            ));
        }

        let mut host = Box::new(Host {
            inner: RefCell::new(Inner {
                paths: app.paths,
                folders: app.folders,
                cfg: app.cfg,
                state: app.state,
                candidates: app.candidates,
                paused: app.paused,
                shutdown: app.shutdown,
                archived_today: app.archived_today,
                watch: app.watch,
                watch_tx: app.watch_tx,
                pending_watch_rx: app.pending_watch_rx,
                stab: app.stab,
                exec_tx: app.exec_tx,
                pending_exec_rx: app.pending_exec_rx,
                hwnd: HWND::default(),
                icon,
                icon_added: false,
                quitting: false,
                taskbar_created,
            }),
        });
        let host_ptr = host.as_mut() as *mut Host;

        // Top-level (not HWND_MESSAGE): TaskbarCreated is HWND_BROADCAST.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("WeekcaseTray"),
                w!("Weekcase"),
                WS_POPUP,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                0,
                0,
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
        host.inner.borrow_mut().hwnd = hwnd;
        host.inner.borrow_mut().sync_icon();
        unsafe {
            SetTimer(Some(hwnd), TIMER_TIP, 2000, None);
            SetTimer(Some(hwnd), TIMER_BOOT, KF_DELAY_MS, None);
        }

        let mut msg = MSG::default();
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        {
            let mut inner = host.inner.borrow_mut();
            inner.quitting = true;
            inner.remove_icon();
        }
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_TIP);
            let _ = KillTimer(Some(hwnd), TIMER_BOOT);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(hwnd);
        }

        let Host { inner } = *host;
        let mut inner = inner.into_inner();
        let _ = inner.watch_tx.send(WatchCmd::Shutdown);
        inner.shutdown.store(true, Ordering::Relaxed);
        if let Some(watch) = inner.watch.take() {
            let _ = watch.join();
        }
        if let Some(stab) = inner.stab.take() {
            let _ = stab.join();
        }
        if com_owned {
            unsafe {
                CoUninitialize();
            }
        }
        Ok(ExitCode::SUCCESS)
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
        // SAFETY: `run` owns Host until it clears GWLP_USERDATA.
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
            if msg == self.taskbar_created {
                self.icon_added = false;
                self.sync_icon();
                return LRESULT(0);
            }
            match msg {
                WM_TIMER => {
                    if wparam.0 == TIMER_BOOT {
                        let _ = unsafe { KillTimer(Some(hwnd), TIMER_BOOT) };
                        self.boot_pipeline();
                    } else {
                        self.sync_icon();
                    }
                    LRESULT(0)
                }
                m if m == WM_TRAY => {
                    let event = lparam.0 as u32;
                    if event == WM_RBUTTONUP || event == WM_LBUTTONUP {
                        self.popup_menu();
                    }
                    LRESULT(0)
                }
                WM_DESTROY => LRESULT(0),
                _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
            }
        }

        fn popup_menu(&mut self) {
            let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
                return;
            };
            let paused = self.paused.load(Ordering::Relaxed);
            let pause_label = if paused {
                w!("恢复")
            } else {
                w!("暂停归档")
            };
            let gray_undo = match crate::undo::read_records(&self.paths.undo_file()) {
                Ok(recs) => crate::undo::last_undoable(&recs).is_none(),
                Err(_) => false,
            };
            let autostart = cfg_lock(&self.cfg).general.start_with_windows;
            let undo_flags = if gray_undo {
                MF_STRING | MF_GRAYED
            } else {
                MF_STRING
            };
            let autostart_flags = if autostart {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };

            unsafe {
                let _ = AppendMenuW(menu, MF_STRING, ID_PAUSE, pause_label);
                let _ = AppendMenuW(menu, undo_flags, ID_UNDO, w!("撤销上一次"));
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                let _ = AppendMenuW(menu, MF_STRING, ID_SWEEP, w!("整理现有文件"));
                let _ = AppendMenuW(menu, MF_STRING, ID_PICK, w!("选择归档文件夹"));
                let _ = AppendMenuW(menu, MF_STRING, ID_OPEN_ROOT, w!("打开归档文件夹"));
                let _ = AppendMenuW(menu, MF_STRING, ID_OPEN_LOG, w!("打开日志"));
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                let _ = AppendMenuW(menu, MF_STRING, ID_RELOAD, w!("重新加载配置"));
                let _ = AppendMenuW(menu, autostart_flags, ID_AUTOSTART, w!("开机启动"));
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("退出"));
            }

            let mut pt = POINT::default();
            let _ = unsafe { GetCursorPos(&mut pt) };
            unsafe {
                let _ = SetForegroundWindow(self.hwnd);
            }
            let cmd = unsafe {
                TrackPopupMenu(
                    menu,
                    TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_RETURNCMD,
                    pt.x,
                    pt.y,
                    None,
                    self.hwnd,
                    None,
                )
            };
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
                let _ = DestroyMenu(menu);
            }

            match cmd.0 as usize {
                ID_PAUSE => self.toggle_pause(),
                ID_UNDO => self.undo_last(),
                ID_SWEEP => self.sweep_existing(),
                ID_PICK => self.pick_root(),
                ID_OPEN_ROOT => self.open_root(),
                ID_OPEN_LOG => self.open_path(&self.paths.log_file()),
                ID_RELOAD => self.reload_config(),
                ID_AUTOSTART => self.toggle_autostart(),
                ID_EXIT => {
                    self.request_quit();
                    return;
                }
                _ => {}
            }
            self.sync_icon();
        }

        fn toggle_pause(&mut self) {
            let next = !self.paused.load(Ordering::Relaxed);
            self.paused.store(next, Ordering::Relaxed);
            cfg_lock(&self.cfg).general.paused = next;
            if let Err(err) =
                crate::config::persist_bool(&self.paths.config_file(), "general", "paused", next)
            {
                tracing::error!(%err, "persist paused failed");
            }
        }

        fn undo_last(&mut self) {
            if self.pending_exec_rx.is_some() {
                match crate::execute::undo_last(&self.paths.undo_file()) {
                    Ok(_) => {}
                    Err(err) => self.error(&undo_error_text(&err)),
                }
                return;
            }
            let (reply, rx) = std::sync::mpsc::channel();
            if self
                .exec_tx
                .send(crate::execute::ExecCmd::UndoLast { reply })
                .is_err()
            {
                self.error("归档线程已停止");
                return;
            }
            match rx.recv() {
                Ok(Ok(_)) => {}
                Ok(Err(err)) => self.error(&undo_error_text(&err)),
                Err(_) => self.error("撤销未完成"),
            }
        }

        fn sweep_existing(&mut self) {
            use std::time::Duration;

            if !self.confirm(
                "将按当前规则移动两个源目录顶层的已有文件（含未满 7 天的下载），可随后撤销。继续？",
            ) {
                return;
            }
            state_lock(&self.state).overflow_unacked = false;
            if self
                .watch_tx
                .send(WatchCmd::Rescan {
                    source: None,
                    include_existing: true,
                    min_age_override: Some(Duration::ZERO),
                })
                .is_err()
            {
                self.error("监视线程已停止");
            }
        }

        fn pick_root(&mut self) {
            let folders = live_folders(&self.folders);
            let current = cfg_lock(&self.cfg).resolved_root(&folders);
            let picked = match pick_directory(self.hwnd, current.as_deref()) {
                Ok(Some(path)) => path,
                Ok(None) => return,
                Err(err) => {
                    self.error(&format!("无法选择文件夹：{err}"));
                    return;
                }
            };
            let sources = enabled_source_paths(&cfg_lock(&self.cfg), &folders);
            match check_archive_root(&picked, &sources, &folders) {
                Err(reason) => {
                    self.error(deny_text(reason));
                    return;
                }
                Ok(ArchiveRoot::OneDrive) => {
                    if !self.confirm(
                        "该路径由 OneDrive 同步。归档会上传到云端。建议改到本地磁盘。继续？",
                    ) {
                        return;
                    }
                }
                Ok(ArchiveRoot::Accepted) => {}
            }
            if let Err(err) = std::fs::create_dir_all(&picked) {
                self.error(&format!("无法创建归档文件夹：{err}"));
                return;
            }
            let root = picked.to_string_lossy().into_owned();
            cfg_lock(&self.cfg).destination.root = Some(root.clone());
            if let Err(err) = crate::config::persist_string(
                &self.paths.config_file(),
                "destination",
                "root",
                &root,
            ) {
                tracing::error!(%err, "persist destination.root failed");
            }
            self.unpoison_and_rescan();
        }

        fn open_root(&mut self) {
            let folders = live_folders(&self.folders);
            let Some(root) = cfg_lock(&self.cfg).resolved_root(&folders) else {
                self.error("无法解析归档目录");
                return;
            };
            if let Err(err) = std::fs::create_dir_all(&root) {
                self.error(&format!("无法创建归档文件夹：{err}"));
                return;
            }
            self.open_path(&root);
        }

        fn open_path(&self, path: &Path) {
            let wide: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(core::iter::once(0))
                .collect();
            let inst = unsafe {
                ShellExecuteW(
                    Some(self.hwnd),
                    w!("open"),
                    PCWSTR(wide.as_ptr()),
                    None,
                    None,
                    SW_SHOWNORMAL,
                )
            };
            if inst.0 as isize <= 32 {
                tracing::error!(path = %path.display(), "ShellExecuteW failed");
            }
        }

        fn reload_config(&mut self) {
            match crate::config::load_or_default(&self.paths.config_file()) {
                Ok(new) => {
                    self.paused.store(new.general.paused, Ordering::Relaxed);
                    if let Err(err) = apply_autostart(new.general.start_with_windows) {
                        tracing::error!(%err, "autostart apply failed");
                    }
                    *cfg_lock(&self.cfg) = new;
                    self.rebuild_watch();
                    self.unpoison_and_rescan();
                }
                Err(err) => self.error(&format!("无法加载配置：{err}")),
            }
        }

        fn toggle_autostart(&mut self) {
            let next = !cfg_lock(&self.cfg).general.start_with_windows;
            if let Err(err) = apply_autostart(next) {
                self.error(&format!("无法更新开机启动：{err}"));
                return;
            }
            cfg_lock(&self.cfg).general.start_with_windows = next;
            if let Err(err) = crate::config::persist_bool(
                &self.paths.config_file(),
                "general",
                "start_with_windows",
                next,
            ) {
                tracing::error!(%err, "persist start_with_windows failed");
            }
        }

        fn rebuild_watch(&mut self) {
            *folders_lock(&self.folders) = KnownFolders::resolve();
            if self.watch.is_none() {
                return;
            }
            let _ = self.watch_tx.send(WatchCmd::Shutdown);
            if let Some(watch) = self.watch.take() {
                let _ = watch.join();
            }
            let (tx, rx) = std::sync::mpsc::channel();
            self.watch_tx = tx;
            let cfg = Arc::new(cfg_lock(&self.cfg).clone());
            self.watch = Some(crate::watch::start_watch(
                cfg,
                Arc::clone(&self.state),
                Arc::clone(&self.candidates),
                Arc::clone(&self.folders),
                rx,
            ));
        }

        fn unpoison_and_rescan(&mut self) {
            {
                let mut table = self.candidates.lock().unwrap_or_else(|e| e.into_inner());
                crate::candidate::clear_poisoned(&mut table, None);
            }
            let _ = self.watch_tx.send(WatchCmd::Rescan {
                source: None,
                include_existing: false,
                min_age_override: None,
            });
        }

        fn boot_pipeline(&mut self) {
            if self.watch.is_some() || self.quitting {
                return;
            }
            let Some(cmd_rx) = self.pending_watch_rx.take() else {
                return;
            };
            let Some(exec_rx) = self.pending_exec_rx.take() else {
                return;
            };
            *folders_lock(&self.folders) = KnownFolders::resolve();
            let cfg = Arc::new(cfg_lock(&self.cfg).clone());
            self.watch = Some(crate::watch::start_watch(
                cfg,
                Arc::clone(&self.state),
                Arc::clone(&self.candidates),
                Arc::clone(&self.folders),
                cmd_rx,
            ));
            self.stab = Some(crate::stabilize::start_stabilize(
                Arc::clone(&self.cfg),
                Arc::clone(&self.candidates),
                Arc::clone(&self.paused),
                Arc::clone(&self.shutdown),
                Arc::clone(&self.folders),
                self.paths.undo_file(),
                self.paths.state_file(),
                Arc::clone(&self.state),
                Arc::clone(&self.archived_today),
                exec_rx,
            ));
        }

        fn request_quit(&mut self) {
            self.quitting = true;
            self.remove_icon();
            let _ = self.watch_tx.send(WatchCmd::Shutdown);
            self.shutdown.store(true, Ordering::Relaxed);
            unsafe {
                PostQuitMessage(0);
            }
        }

        fn tooltip(&self) -> String {
            tooltip_text(
                self.paused.load(Ordering::Relaxed),
                self.archived_today.load(Ordering::Relaxed),
                state_lock(&self.state).overflow_unacked,
            )
        }

        fn sync_icon(&mut self) {
            if self.quitting {
                return;
            }
            let mut nid = NOTIFYICONDATAW {
                cbSize: core::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd,
                uID: TRAY_ID,
                uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
                uCallbackMessage: WM_TRAY,
                hIcon: self.icon,
                ..Default::default()
            };
            copy_tip(&mut nid.szTip, &self.tooltip());
            if self.icon_added {
                if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) }.as_bool() {
                    self.icon_added = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) }.as_bool();
                }
            } else {
                self.icon_added = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) }.as_bool();
            }
        }

        fn remove_icon(&mut self) {
            if !self.icon_added {
                return;
            }
            let nid = NOTIFYICONDATAW {
                cbSize: core::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd,
                uID: TRAY_ID,
                ..Default::default()
            };
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            }
            self.icon_added = false;
        }

        fn error(&self, text: &str) {
            message_box(Some(self.hwnd), text, false);
        }

        fn confirm(&self, text: &str) -> bool {
            message_box(Some(self.hwnd), text, true)
        }
    }

    fn live_folders(shared: &Mutex<KnownFolders>) -> KnownFolders {
        let folders = KnownFolders::resolve();
        *folders_lock(shared) = folders.clone();
        folders
    }

    fn copy_tip(dst: &mut [u16; 128], text: &str) {
        dst.fill(0);
        for (i, unit) in text.encode_utf16().take(127).enumerate() {
            dst[i] = unit;
        }
    }

    pub fn message_box(hwnd: Option<HWND>, text: &str, confirm: bool) -> bool {
        let wide: Vec<u16> = text.encode_utf16().chain(core::iter::once(0)).collect();
        let flags = if confirm {
            MB_YESNO | MB_ICONWARNING
        } else {
            MB_OK | MB_ICONERROR
        };
        let rc = unsafe { MessageBoxW(hwnd, PCWSTR(wide.as_ptr()), w!("Weekcase"), flags) };
        !confirm || rc == IDYES
    }

    pub fn pick_directory(hwnd: HWND, start: Option<&Path>) -> io::Result<Option<PathBuf>> {
        let dlg: IFileDialog =
            unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        unsafe {
            dlg.SetOptions(FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            let _ = dlg.SetTitle(w!("选择归档文件夹"));
        }
        if let Some(start) = start {
            let wide: Vec<u16> = start
                .as_os_str()
                .encode_wide()
                .chain(core::iter::once(0))
                .collect();
            if let Ok(item) = unsafe {
                SHCreateItemFromParsingName::<_, _, IShellItem>(
                    PCWSTR(wide.as_ptr()),
                    None::<&IBindCtx>,
                )
            } {
                let _ = unsafe { dlg.SetFolder(&item) };
            }
        }
        if unsafe { dlg.Show(Some(hwnd)) }.is_err() {
            return Ok(None);
        }
        let item =
            unsafe { dlg.GetResult() }.map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let psz = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let path = unsafe { psz.to_string() }.ok();
        unsafe {
            CoTaskMemFree(Some(psz.0 as *const core::ffi::c_void));
        }
        Ok(path.filter(|s| !s.is_empty()).map(PathBuf::from))
    }

    pub fn apply_autostart(enable: bool) -> io::Result<()> {
        use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
        use windows::Win32::System::Registry::{
            RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
            KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
        };

        let exe = std::env::current_exe()?;
        let cmd = super::autostart_command(&exe);
        let mut key = HKEY::default();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("RegCreateKeyExW {status:?}"),
            ));
        }
        let result = if enable {
            let wide: Vec<u16> = cmd.encode_utf16().chain(core::iter::once(0)).collect();
            // SAFETY: `bytes` aliases `wide` only for this call.
            let bytes =
                unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
            let st = unsafe { RegSetValueExW(key, w!("Weekcase"), None, REG_SZ, Some(bytes)) };
            if st == ERROR_SUCCESS {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("RegSetValueExW {st:?}"),
                ))
            }
        } else {
            let st = unsafe { RegDeleteValueW(key, w!("Weekcase")) };
            if st == ERROR_SUCCESS || st == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("RegDeleteValueW {st:?}"),
                ))
            }
        };
        unsafe {
            let _ = RegCloseKey(key);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn tooltip_empty_paused_and_overflow() {
        assert_eq!(tooltip_text(false, 0, false), "监视中 · 今日已归档 0");
        assert_eq!(tooltip_text(true, 3, false), "已暂停 · 今日已归档 3");
        assert_eq!(tooltip_text(false, 1, true), "有文件没进队，整理未完成");
    }

    #[test]
    fn archive_root_denylist_and_onedrive() {
        let f = folders();
        let sources = vec![PathBuf::from(r"C:\Users\a\Downloads")];
        assert_eq!(
            check_archive_root(Path::new(r"C:\Users\a\Downloads\Weekcase"), &sources, &f),
            Err(DenyReason::DestInsideSource)
        );
        assert_eq!(
            check_archive_root(Path::new(r"C:\Users\a"), &sources, &f),
            Err(DenyReason::ProfileRoot)
        );
        assert_eq!(
            check_archive_root(Path::new(r"C:\Users\a\OneDrive\Weekcase"), &sources, &f),
            Ok(ArchiveRoot::OneDrive)
        );
        assert_eq!(
            check_archive_root(Path::new(r"C:\Users\a\Documents\Weekcase"), &sources, &f),
            Ok(ArchiveRoot::Accepted)
        );
        assert_eq!(deny_text(DenyReason::Unc), "不能使用网络路径");
    }

    #[test]
    fn autostart_command_quotes_and_strips_verbatim() {
        assert_eq!(
            autostart_command(Path::new(r"C:\Program Files\Weekcase\weekcase.exe")),
            r#""C:\Program Files\Weekcase\weekcase.exe""#
        );
        assert_eq!(
            autostart_command(Path::new(r"\\?\C:\Weekcase\weekcase.exe")),
            r#""C:\Weekcase\weekcase.exe""#
        );
    }

    #[test]
    fn undo_error_is_one_line() {
        assert_eq!(undo_error_text(&UndoError::Empty), "没有可撤销的记录");
        assert!(!undo_error_text(&UndoError::SourceExists).contains('\n'));
    }

    #[test]
    fn known_folder_delay_is_fifteen_seconds() {
        assert_eq!(KF_DELAY_MS, 15_000);
    }
}
