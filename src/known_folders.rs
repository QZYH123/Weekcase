use std::path::{Path, PathBuf};

pub const DRIVE_NO_ROOT_DIR: u32 = 1;
pub const DRIVE_REMOTE: u32 = 4;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnownFolders {
    pub downloads: Option<PathBuf>,
    pub screenshots: Option<PathBuf>,
    pub documents: Option<PathBuf>,
    pub onedrive: Option<PathBuf>,
    pub profile: Option<PathBuf>,
    pub windows: Option<PathBuf>,
    pub program_files: Vec<PathBuf>,
    pub program_data: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    Unc,
    DriveRoot,
    RemoteDrive,
    NoRootDir,
    ProfileRoot,
    Windows,
    ProgramFiles,
    ProgramData,
    DestInsideSource,
}

impl KnownFolders {
    pub fn resolve() -> Self {
        #[cfg(windows)]
        {
            resolve_windows()
        }
        #[cfg(not(windows))]
        {
            Self::default()
        }
    }

    pub fn default_root(&self) -> Option<PathBuf> {
        self.documents.as_ref().map(|dir| dir.join("Weekcase"))
    }

    pub fn is_onedrive_path(&self, path: &Path) -> bool {
        if let Some(od) = &self.onedrive {
            if is_same_or_inside(path, od) {
                return true;
            }
        }
        let key = canonical_key(path);
        key.contains("\\onedrive\\") || key.ends_with("\\onedrive")
    }
}

pub fn dest_inside_any_source(dest: &Path, sources: &[PathBuf]) -> bool {
    sources.iter().any(|src| is_same_or_inside(dest, src))
}

pub fn deny_source(path: &Path, folders: &KnownFolders) -> Option<DenyReason> {
    deny_source_with(path, folders, queried_drive_type(path))
}

pub fn deny_source_with(
    path: &Path,
    folders: &KnownFolders,
    drive_type: Option<u32>,
) -> Option<DenyReason> {
    let key = canonical_key(path);
    if is_unc_key(&key) {
        return Some(DenyReason::Unc);
    }
    if is_drive_root_key(&key) {
        return Some(DenyReason::DriveRoot);
    }
    match drive_type {
        Some(DRIVE_REMOTE) => return Some(DenyReason::RemoteDrive),
        Some(DRIVE_NO_ROOT_DIR) => return Some(DenyReason::NoRootDir),
        _ => {}
    }
    // Profile root itself is denied; children such as Downloads are valid sources.
    if folders
        .profile
        .as_ref()
        .is_some_and(|p| canonical_key(p) == key)
    {
        return Some(DenyReason::ProfileRoot);
    }
    if folders
        .windows
        .as_ref()
        .is_some_and(|p| is_same_or_inside(path, p))
    {
        return Some(DenyReason::Windows);
    }
    if folders
        .program_files
        .iter()
        .any(|p| is_same_or_inside(path, p))
    {
        return Some(DenyReason::ProgramFiles);
    }
    if folders
        .program_data
        .as_ref()
        .is_some_and(|p| is_same_or_inside(path, p))
    {
        return Some(DenyReason::ProgramData);
    }
    None
}

pub fn deny_destination(
    dest: &Path,
    sources: &[PathBuf],
    folders: &KnownFolders,
) -> Option<DenyReason> {
    if let Some(reason) = deny_source(dest, folders) {
        return Some(reason);
    }
    if dest_inside_any_source(dest, sources) {
        return Some(DenyReason::DestInsideSource);
    }
    None
}

pub fn deny_destination_with(
    dest: &Path,
    sources: &[PathBuf],
    folders: &KnownFolders,
    drive_type: Option<u32>,
) -> Option<DenyReason> {
    if let Some(reason) = deny_source_with(dest, folders, drive_type) {
        return Some(reason);
    }
    if dest_inside_any_source(dest, sources) {
        return Some(DenyReason::DestInsideSource);
    }
    None
}

pub(crate) fn is_same_or_inside(inner: &Path, outer: &Path) -> bool {
    let inner = canonical_key(inner);
    let outer = canonical_key(outer);
    if inner.is_empty() || outer.is_empty() {
        return false;
    }
    inner == outer || inner.starts_with(&format!("{outer}\\"))
}

pub(crate) fn canonical_key(path: &Path) -> String {
    let mut s: String = path
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c == '/' {
                '\\'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    if let Some(rest) = s.strip_prefix("\\\\?\\unc\\") {
        s = format!("\\\\{rest}");
    } else if let Some(rest) = s.strip_prefix("\\\\?\\") {
        s = rest.to_string();
    }
    while s.ends_with('\\') && s != "\\" {
        s.pop();
    }
    s
}

fn is_unc_key(key: &str) -> bool {
    key.starts_with("\\\\")
}

fn is_drive_root_key(key: &str) -> bool {
    key == "\\" || is_win_drive_root_key(key)
}

fn is_win_drive_root_key(key: &str) -> bool {
    let b = key.as_bytes();
    b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

#[cfg(windows)]
fn queried_drive_type(path: &Path) -> Option<u32> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;

    match volume_root_wide(path) {
        Some(root) => {
            // SAFETY: `root` is NUL-terminated UTF-16 volume root (`C:\`).
            Some(unsafe { GetDriveTypeW(PCWSTR::from_raw(root.as_ptr())) })
        }
        None => Some(DRIVE_NO_ROOT_DIR),
    }
}

#[cfg(not(windows))]
fn queried_drive_type(_path: &Path) -> Option<u32> {
    None
}

#[cfg(windows)]
fn volume_root_wide(path: &Path) -> Option<Vec<u16>> {
    let s = path.to_string_lossy().replace('/', "\\");
    let rest = s.strip_prefix("\\\\?\\").unwrap_or(s.as_str());
    if rest.len() >= 2 {
        let mut chars = rest.chars();
        let letter = chars.next()?;
        if letter.is_ascii_alphabetic() && chars.next() == Some(':') {
            return Some(
                format!("{}:\\", letter.to_ascii_uppercase())
                    .encode_utf16()
                    .chain(core::iter::once(0))
                    .collect(),
            );
        }
    }
    None
}

#[cfg(windows)]
fn resolve_windows() -> KnownFolders {
    use windows::Win32::UI::Shell::{
        FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_OneDrive, FOLDERID_Profile,
        FOLDERID_ProgramData, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX64,
        FOLDERID_ProgramFilesX86, FOLDERID_Screenshots, FOLDERID_Windows,
    };

    let mut program_files: Vec<PathBuf> = Vec::new();
    for id in [
        FOLDERID_ProgramFiles,
        FOLDERID_ProgramFilesX86,
        FOLDERID_ProgramFilesX64,
    ] {
        if let Some(path) = known_folder(id) {
            if !program_files
                .iter()
                .any(|existing| canonical_key(existing) == canonical_key(&path))
            {
                program_files.push(path);
            }
        }
    }

    KnownFolders {
        downloads: known_folder(FOLDERID_Downloads),
        screenshots: known_folder(FOLDERID_Screenshots),
        documents: known_folder(FOLDERID_Documents),
        onedrive: known_folder(FOLDERID_OneDrive),
        profile: known_folder(FOLDERID_Profile),
        windows: known_folder(FOLDERID_Windows),
        program_files,
        program_data: known_folder(FOLDERID_ProgramData),
    }
}

#[cfg(windows)]
fn known_folder(id: windows::core::GUID) -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DONT_VERIFY};

    // DONT_VERIFY returns the path even if Screenshots is missing; it does not create it.
    // SAFETY: `id` is a live FOLDERID GUID; None is the current-user token.
    let pwstr = unsafe { SHGetKnownFolderPath(&id, KF_FLAG_DONT_VERIFY, None) }.ok()?;
    // SAFETY: a successful call returns a CoTaskMemAlloc'd NUL-terminated PWSTR.
    let path = unsafe { pwstr.to_string() }.ok();
    // SAFETY: `pwstr` came from that SHGetKnownFolderPath and is freed once.
    unsafe {
        CoTaskMemFree(Some(pwstr.0 as *const core::ffi::c_void));
    }
    path.filter(|s| !s.is_empty()).map(PathBuf::from)
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
            program_files: vec![
                PathBuf::from(r"C:\Program Files"),
                PathBuf::from(r"C:\Program Files (x86)"),
            ],
            program_data: Some(PathBuf::from(r"C:\ProgramData")),
        }
    }

    #[test]
    fn dest_inside_source_is_separator_and_case_insensitive() {
        let sources = [PathBuf::from(r"C:\Users\a\Downloads")];
        assert!(dest_inside_any_source(
            Path::new(r"C:\Users\a\Downloads\Weekcase"),
            &sources
        ));
        assert!(dest_inside_any_source(
            Path::new("C:/Users/a/Downloads"),
            &sources
        ));
        assert!(!dest_inside_any_source(
            Path::new(r"C:\Users\a\Documents\Weekcase"),
            &sources
        ));
        assert!(!dest_inside_any_source(
            Path::new(r"C:\Users\a\Downloads2\x"),
            &sources
        ));
    }

    #[test]
    fn prefix_is_not_a_path_component() {
        assert!(!dest_inside_any_source(
            Path::new(r"C:\Documents\Weekcase"),
            &[PathBuf::from(r"C:\Doc")]
        ));
    }

    #[test]
    fn denylist_profile_is_exact_windows_is_tree() {
        let f = folders();
        assert_eq!(
            deny_source(Path::new(r"C:\Users\a"), &f),
            Some(DenyReason::ProfileRoot)
        );
        assert_eq!(deny_source(Path::new(r"C:\Users\a\Downloads"), &f), None);
        assert_eq!(
            deny_source(Path::new(r"C:\Windows\System32"), &f),
            Some(DenyReason::Windows)
        );
        assert_eq!(
            deny_source(Path::new(r"C:\Program Files\Foo"), &f),
            Some(DenyReason::ProgramFiles)
        );
        assert_eq!(
            deny_source(Path::new(r"C:\ProgramData\Bar"), &f),
            Some(DenyReason::ProgramData)
        );
        assert_eq!(
            deny_source(Path::new(r"C:\"), &f),
            Some(DenyReason::DriveRoot)
        );
        assert_eq!(
            deny_source(Path::new(r"\\nas\share"), &f),
            Some(DenyReason::Unc)
        );
        assert_eq!(
            deny_source(Path::new("//nas/share/folder"), &f),
            Some(DenyReason::Unc)
        );
    }

    #[test]
    fn mapped_drive_denied_when_drive_type_is_remote() {
        let f = folders();
        assert_eq!(
            deny_source_with(Path::new(r"Z:\Downloads"), &f, Some(DRIVE_REMOTE)),
            Some(DenyReason::RemoteDrive)
        );
        assert_eq!(
            deny_source_with(Path::new(r"Z:\Downloads"), &f, Some(DRIVE_NO_ROOT_DIR)),
            Some(DenyReason::NoRootDir)
        );
    }

    #[test]
    fn dest_inside_source_is_denied() {
        let f = folders();
        let sources = vec![PathBuf::from(r"C:\Users\a\Downloads")];
        assert_eq!(
            deny_destination_with(
                Path::new(r"C:\Users\a\Downloads\Weekcase"),
                &sources,
                &f,
                None
            ),
            Some(DenyReason::DestInsideSource)
        );
        assert_eq!(
            deny_destination_with(
                Path::new(r"C:\Users\a\Documents\Weekcase"),
                &sources,
                &f,
                None
            ),
            None
        );
    }

    #[test]
    fn default_root_and_onedrive() {
        let f = folders();
        assert_eq!(
            canonical_key(&f.default_root().unwrap()),
            canonical_key(Path::new(r"C:\Users\a\Documents\Weekcase"))
        );
        assert!(f.is_onedrive_path(Path::new(r"C:\Users\a\OneDrive\Weekcase")));
        assert!(f.is_onedrive_path(Path::new(r"D:\Work\OneDrive\Files")));
        assert!(!f.is_onedrive_path(Path::new(r"C:\Users\a\Documents\Weekcase")));
    }
}
