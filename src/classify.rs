use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{Config, SourceKind};
use crate::known_folders::{deny_destination_with, KnownFolders};

pub use crate::candidate::FileSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    Images,
    Documents,
    Archives,
    Audio,
    Video,
    Installers,
    Other,
    Screenshots,
}

impl Bucket {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Images => "Images",
            Self::Documents => "Documents",
            Self::Archives => "Archives",
            Self::Audio => "Audio",
            Self::Video => "Video",
            Self::Installers => "Installers",
            Self::Other => "Other",
            Self::Screenshots => "Screenshots",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub dest_dir: PathBuf,
    pub dest_name: OsString,
    pub bucket: Bucket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassifyError {
    BadTemplate,
    DestInsideSource,
    IllegalName,
}

pub fn classify(
    cfg: &Config,
    snap: &FileSnapshot,
    folders: &KnownFolders,
) -> Result<Placement, ClassifyError> {
    let dest_name = file_name(&snap.path);
    if is_illegal_name(&dest_name) {
        return Err(ClassifyError::IllegalName);
    }

    let root = cfg
        .resolved_root(folders)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or(ClassifyError::BadTemplate)?;
    let root = root.to_string_lossy();

    let (bucket, template) = match snap.source_kind {
        SourceKind::Screenshots => (
            Bucket::Screenshots,
            cfg.destination.screenshots_template.as_str(),
        ),
        SourceKind::Downloads => (
            bucket_for_name(&dest_name.to_string_lossy()),
            cfg.destination.downloads_template.as_str(),
        ),
    };

    let (year, month) = year_month(snap.created);
    let yyyy = format!("{year:04}");
    let mm = format!("{month:02}");
    let expanded = expand_template(template, &root, bucket.as_str(), &yyyy, &mm)?;
    let dest = normalize_dest(&expanded);
    if !is_absolute_dest(&dest) {
        return Err(ClassifyError::BadTemplate);
    }
    let dest_dir = PathBuf::from(&dest);

    let sources: Vec<PathBuf> = cfg
        .sources
        .iter()
        .filter_map(|s| s.resolved_path(folders))
        .collect();
    // Path-string denylist only; do not query drive type (no IO).
    if deny_destination_with(&dest_dir, &sources, folders, None).is_some() {
        return Err(ClassifyError::DestInsideSource);
    }

    Ok(Placement {
        dest_dir,
        dest_name,
        bucket,
    })
}

fn file_name(path: &Path) -> OsString {
    if let Some(name) = path.file_name() {
        let lossy = name.to_string_lossy();
        if !lossy.contains('\\') {
            return name.to_os_string();
        }
    }
    // Unix PathBuf keeps `\` inside a single component.
    path.as_os_str()
        .to_string_lossy()
        .rsplit(['\\', '/'])
        .next()
        .map(OsString::from)
        .unwrap_or_default()
}

fn is_illegal_name(name: &OsString) -> bool {
    let name = name.to_string_lossy();
    name.is_empty() || name == "." || name == ".." || name.contains(['\\', '/'])
}

fn bucket_for_name(name: &str) -> Bucket {
    let Some((_, ext)) = name.rsplit_once('.') else {
        return Bucket::Other;
    };
    if ext.is_empty() {
        return Bucket::Other;
    }
    match ext.to_ascii_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "heic" | "heif"
        | "svg" => Bucket::Images,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "csv" | "rtf"
        | "odt" | "ods" | "odp" | "epub" => Bucket::Documents,
        "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "iso" => Bucket::Archives,
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" | "wma" => Bucket::Audio,
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "mpeg" | "mpg" | "wmv" => Bucket::Video,
        "exe" | "msi" | "msix" | "appx" | "cab" => Bucket::Installers,
        _ => Bucket::Other,
    }
}

fn expand_template(
    template: &str,
    root: &str,
    bucket: &str,
    yyyy: &str,
    mm: &str,
) -> Result<String, ClassifyError> {
    let mut out = String::with_capacity(template.len() + root.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        let Some(end) = rest.find('}') else {
            return Err(ClassifyError::BadTemplate);
        };
        let value = match &rest[..end] {
            "root" => root,
            "bucket" => bucket,
            "yyyy" => yyyy,
            "mm" => mm,
            _ => return Err(ClassifyError::BadTemplate),
        };
        out.push_str(value);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn normalize_dest(raw: &str) -> String {
    let mut s = raw.replace('/', "\\");
    if let Some(rest) = s.strip_prefix("\\\\?\\") {
        if let Some(unc) = rest
            .strip_prefix("UNC\\")
            .or_else(|| rest.strip_prefix("unc\\"))
        {
            s = format!("\\\\{unc}");
        } else {
            s = rest.to_string();
        }
    }

    if s.starts_with("\\\\") {
        while s.ends_with('\\') && s.len() > 2 {
            s.pop();
        }
        return s;
    }

    let (prefix, body) = drive_prefix(&s);
    let mut stack: Vec<&str> = Vec::new();
    for part in body.split('\\') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            stack.pop();
            continue;
        }
        stack.push(part);
    }
    if prefix.is_empty() {
        return stack.join("\\");
    }
    if stack.is_empty() {
        format!("{prefix}\\")
    } else {
        format!("{prefix}\\{}", stack.join("\\"))
    }
}

fn drive_prefix(s: &str) -> (&str, &str) {
    let b = s.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        (&s[..2], &s[2..])
    } else {
        ("", s)
    }
}

fn is_absolute_dest(s: &str) -> bool {
    let n = s.replace('/', "\\");
    let b = n.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\')
        || n.starts_with("\\\\")
        || s.starts_with('/')
}

fn year_month(created: SystemTime) -> (i32, u32) {
    #[cfg(windows)]
    {
        if let Some(ym) = local_year_month(created) {
            return ym;
        }
    }
    utc_year_month(created)
}

fn utc_year_month(created: SystemTime) -> (i32, u32) {
    let secs = match created.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    let days = secs.div_euclid(86_400);
    let (y, m, _) = civil_from_days(days);
    (y, m)
}

/// Howard Hinnant civil-from-days; `z` is days since 1970-01-01.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + i64::from(m <= 2);
    (y as i32, m, d)
}

#[cfg(windows)]
fn local_year_month(created: SystemTime) -> Option<(i32, u32)> {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
    use windows::Win32::System::Time::FileTimeToSystemTime;

    let ft = system_time_to_filetime(created)?;
    let mut local = FILETIME::default();
    // SAFETY: `ft` and `local` are our stack FILETIME values for this call only.
    unsafe { FileTimeToLocalFileTime(&ft, &mut local) }.ok()?;
    let mut st = SYSTEMTIME::default();
    // SAFETY: `local` and `st` are our stack slots for this call only.
    unsafe { FileTimeToSystemTime(&local, &mut st) }.ok()?;
    if st.wYear == 0 || !(1..=12).contains(&st.wMonth) {
        return None;
    }
    Some((i32::from(st.wYear), u32::from(st.wMonth)))
}

#[cfg(windows)]
fn system_time_to_filetime(created: SystemTime) -> Option<windows::Win32::Foundation::FILETIME> {
    const HNS_PER_SEC: u64 = 10_000_000;
    const UNIX_OFFSET: u64 = 11_644_473_600 * HNS_PER_SEC;
    let d = created.duration_since(UNIX_EPOCH).ok()?;
    let hns = d
        .as_secs()
        .checked_mul(HNS_PER_SEC)?
        .checked_add(u64::from(d.subsec_nanos()) / 100)?
        .checked_add(UNIX_OFFSET)?;
    Some(windows::Win32::Foundation::FILETIME {
        dwLowDateTime: hns as u32,
        dwHighDateTime: (hns >> 32) as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn injected_created_is_calendar_year_month() {
        let t = |secs| UNIX_EPOCH + Duration::from_secs(secs);
        assert_eq!(utc_year_month(t(1_788_048_000)), (2026, 8));
        assert_eq!(utc_year_month(t(1_767_225_600)), (2026, 1));
        assert_eq!(utc_year_month(t(1_767_139_200)), (2025, 12));
        assert_eq!(utc_year_month(t(1_798_675_200)), (2026, 12));
    }

    #[test]
    fn last_dot_extension_selects_bucket() {
        assert_eq!(bucket_for_name("a.PDF"), Bucket::Documents);
        assert_eq!(bucket_for_name("File.PDF"), Bucket::Documents);
        assert_eq!(bucket_for_name("archive.tar.gz"), Bucket::Archives);
        assert_eq!(bucket_for_name("LICENSE"), Bucket::Other);
        assert_eq!(bucket_for_name("photo.heic"), Bucket::Images);
        assert_eq!(bucket_for_name("setup.exe"), Bucket::Installers);
        assert_eq!(bucket_for_name("clip.webm"), Bucket::Video);
        assert_eq!(bucket_for_name("track.m4a"), Bucket::Audio);
    }
}
