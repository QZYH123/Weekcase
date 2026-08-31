use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::known_folders::KnownFolders;

pub const V1_TEMPLATE_TOKENS: &[&str] = &["root", "bucket", "yyyy", "mm"];

pub const DOWNLOADS_MIN_AGE_SECS: u64 = 604_800;
pub const DOWNLOADS_SETTLE_SECS: u64 = 15;
pub const DOWNLOADS_SCAN_INTERVAL_SECS: u64 = 60;
pub const SCREENSHOTS_MIN_AGE_SECS: u64 = 20;
pub const SCREENSHOTS_SETTLE_SECS: u64 = 8;
pub const SCREENSHOTS_SCAN_INTERVAL_SECS: u64 = 10;

pub const DEFAULT_DOWNLOADS_TEMPLATE: &str = "{root}/Downloads/{bucket}";
pub const DEFAULT_SCREENSHOTS_TEMPLATE: &str = "{root}/Screenshots/{yyyy}-{mm}";

pub const MAX_PENDING: u32 = 256;
pub const MAX_SOURCES: u32 = 8;
pub const RDC_BUFFER_BYTES: u32 = 64 * 1024;
pub const TICK_MS: u64 = 1_000;
pub const DEBOUNCE_MS: u64 = 500;

/// On-disk template so comments survive; not serde-emitted.
pub const DEFAULT_TOML: &str = r#"# Weekcase 默认配置。未知字段会被忽略。
# v1 模板只认 {root} {bucket} {yyyy} {mm}，没有 {ww}。

[general]
start_with_windows = true
paused = false

[destination]
# 空 = 文档\Weekcase
root = ""
downloads_template = "{root}/Downloads/{bucket}"
screenshots_template = "{root}/Screenshots/{yyyy}-{mm}"
collision = "suffix"

[watch]
max_pending = 256
max_sources = 8
buffer_bytes = 65536
tick_ms = 1000
debounce_ms = 500

[[sources]]
id = "downloads"
kind = "downloads"
enabled = true
min_age_secs = 604800
settle_secs = 15
scan_interval_secs = 60

[[sources]]
id = "screenshots"
kind = "screenshots"
enabled = true
min_age_secs = 20
settle_secs = 8
scan_interval_secs = 10
"#;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub destination: Destination,
    pub watch: Watch,
    #[serde(default = "default_sources")]
    pub sources: Vec<SourceConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: General::default(),
            destination: Destination::default(),
            watch: Watch::default(),
            sources: default_sources(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub start_with_windows: bool,
    pub paused: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            start_with_windows: true,
            paused: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Destination {
    #[serde(default, deserialize_with = "empty_string_as_none")]
    pub root: Option<String>,
    pub downloads_template: String,
    pub screenshots_template: String,
    pub collision: Collision,
}

impl Default for Destination {
    fn default() -> Self {
        Self {
            root: None,
            downloads_template: DEFAULT_DOWNLOADS_TEMPLATE.to_string(),
            screenshots_template: DEFAULT_SCREENSHOTS_TEMPLATE.to_string(),
            collision: Collision::Suffix,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Collision {
    #[default]
    Suffix,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Watch {
    pub max_pending: u32,
    pub max_sources: u32,
    pub buffer_bytes: u32,
    pub tick_ms: u64,
    pub debounce_ms: u64,
}

impl Default for Watch {
    fn default() -> Self {
        Self {
            max_pending: MAX_PENDING,
            max_sources: MAX_SOURCES,
            buffer_bytes: RDC_BUFFER_BYTES,
            tick_ms: TICK_MS,
            debounce_ms: DEBOUNCE_MS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Downloads,
    Screenshots,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RawSource")]
pub struct SourceConfig {
    pub id: String,
    pub kind: SourceKind,
    pub enabled: bool,
    pub path: Option<String>,
    pub min_age_secs: u64,
    pub settle_secs: u64,
    pub scan_interval_secs: u64,
}

#[derive(Deserialize)]
struct RawSource {
    id: Option<String>,
    kind: SourceKind,
    enabled: Option<bool>,
    path: Option<String>,
    min_age_secs: Option<u64>,
    settle_secs: Option<u64>,
    scan_interval_secs: Option<u64>,
}

impl SourceConfig {
    pub fn downloads() -> Self {
        Self {
            id: "downloads".into(),
            kind: SourceKind::Downloads,
            enabled: true,
            path: None,
            min_age_secs: DOWNLOADS_MIN_AGE_SECS,
            settle_secs: DOWNLOADS_SETTLE_SECS,
            scan_interval_secs: DOWNLOADS_SCAN_INTERVAL_SECS,
        }
    }

    pub fn screenshots() -> Self {
        Self {
            id: "screenshots".into(),
            kind: SourceKind::Screenshots,
            enabled: true,
            path: None,
            min_age_secs: SCREENSHOTS_MIN_AGE_SECS,
            settle_secs: SCREENSHOTS_SETTLE_SECS,
            scan_interval_secs: SCREENSHOTS_SCAN_INTERVAL_SECS,
        }
    }

    pub fn resolved_path(&self, folders: &KnownFolders) -> Option<PathBuf> {
        if let Some(path) = self.path.as_deref().filter(|p| !p.is_empty()) {
            return Some(PathBuf::from(path));
        }
        match self.kind {
            SourceKind::Downloads => folders.downloads.clone(),
            SourceKind::Screenshots => folders.screenshots.clone(),
        }
    }
}

impl From<RawSource> for SourceConfig {
    fn from(raw: RawSource) -> Self {
        let kind_default = match raw.kind {
            SourceKind::Downloads => SourceConfig::downloads(),
            SourceKind::Screenshots => SourceConfig::screenshots(),
        };
        Self {
            id: nonempty(raw.id).unwrap_or(kind_default.id),
            kind: raw.kind,
            enabled: raw.enabled.unwrap_or(true),
            path: nonempty(raw.path),
            min_age_secs: raw.min_age_secs.unwrap_or(kind_default.min_age_secs),
            settle_secs: raw.settle_secs.unwrap_or(kind_default.settle_secs),
            scan_interval_secs: raw
                .scan_interval_secs
                .unwrap_or(kind_default.scan_interval_secs),
        }
    }
}

impl Config {
    pub fn resolved_root(&self, folders: &KnownFolders) -> Option<PathBuf> {
        match self.destination.root.as_deref().filter(|s| !s.is_empty()) {
            Some(root) => Some(PathBuf::from(root)),
            None => folders.default_root(),
        }
    }
}

/// Unknown keys are ignored.
pub fn parse(text: &str) -> Result<Config, toml::de::Error> {
    if text.trim().is_empty() {
        return Ok(Config::default());
    }
    toml::from_str(text)
}

pub fn load(path: &Path) -> io::Result<Config> {
    let text = fs::read_to_string(path)?;
    parse(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn load_or_default(path: &Path) -> io::Result<Config> {
    match load(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        other => other,
    }
}

pub fn write_default(path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, DEFAULT_TOML)
}

pub fn write_default_if_missing(path: &Path) -> io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    write_default(path)?;
    Ok(true)
}

/// Patch one key in an existing TOML file. Missing file is a no-op (`Ok(false)`).
pub fn persist_kv(path: &Path, section: &str, key: &str, encoded: &str) -> io::Result<bool> {
    let text = match fs::read_to_string(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        other => other?,
    };
    let next = patch_toml_key(&text, section, key, encoded);
    if next != text {
        fs::write(path, next)?;
    }
    Ok(true)
}

pub fn persist_bool(path: &Path, section: &str, key: &str, value: bool) -> io::Result<bool> {
    persist_kv(path, section, key, if value { "true" } else { "false" })
}

pub fn persist_string(path: &Path, section: &str, key: &str, value: &str) -> io::Result<bool> {
    persist_kv(path, section, key, &toml_quoted(value))
}

pub fn toml_quoted(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn patch_toml_key(text: &str, section: &str, key: &str, encoded: &str) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n')
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect()
    };
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }

    let header = format!("[{section}]");
    let mut in_section = false;
    let mut section_at: Option<usize> = None;
    let mut key_at: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_section {
                break;
            }
            if trimmed == header {
                in_section = true;
                section_at = Some(i);
            }
            continue;
        }
        if in_section && is_toml_key_line(line, key) {
            key_at = Some(i);
            break;
        }
    }

    let replacement = format!("{key} = {encoded}");
    if let Some(i) = key_at {
        lines[i] = replacement;
    } else if let Some(i) = section_at {
        lines.insert(i + 1, replacement);
    } else {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(header);
        lines.push(replacement);
    }

    let mut out = lines.join(nl);
    out.push_str(nl);
    out
}

fn is_toml_key_line(line: &str, key: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return false;
    }
    let Some(rest) = trimmed.strip_prefix(key) else {
        return false;
    };
    rest.trim_start().starts_with('=')
        && rest
            .chars()
            .next()
            .is_some_and(|c| c == '=' || c.is_whitespace())
}

/// `{token}` names in appearance order. v1 only promises [`V1_TEMPLATE_TOKENS`].
pub fn template_tokens(template: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        rest = &rest[start + 1..];
        match rest.find('}') {
            Some(end) => {
                out.push(&rest[..end]);
                rest = &rest[end + 1..];
            }
            None => break,
        }
    }
    out
}

pub fn unknown_template_tokens(template: &str) -> Vec<&str> {
    template_tokens(template)
        .into_iter()
        .filter(|tok| !V1_TEMPLATE_TOKENS.contains(tok))
        .collect()
}

fn default_sources() -> Vec<SourceConfig> {
    vec![SourceConfig::downloads(), SourceConfig::screenshots()]
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty())
}

fn empty_string_as_none<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(nonempty(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_toml_round_trips_to_struct_defaults() {
        let cfg = parse(DEFAULT_TOML).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn empty_and_missing_fields_use_defaults() {
        let cfg = parse("").unwrap();
        assert_eq!(cfg, Config::default());
        let cfg = parse("[general]\npaused = false\n").unwrap();
        let downloads = cfg
            .sources
            .iter()
            .find(|s| s.kind == SourceKind::Downloads)
            .unwrap();
        assert_eq!(downloads.min_age_secs, DOWNLOADS_MIN_AGE_SECS);
        assert_eq!(downloads.settle_secs, DOWNLOADS_SETTLE_SECS);
        assert_eq!(downloads.scan_interval_secs, DOWNLOADS_SCAN_INTERVAL_SECS);
        assert_eq!(cfg.watch.buffer_bytes, RDC_BUFFER_BYTES);
        assert_eq!(cfg.watch.tick_ms, TICK_MS);
        assert_eq!(cfg.watch.debounce_ms, DEBOUNCE_MS);
        assert!(cfg.general.start_with_windows);
        assert_eq!(cfg.destination.collision, Collision::Suffix);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let cfg = parse(
            r#"
future = 1
[destination]
root = "D:\\Weekcase"
experimental = true
collision = "skip"
[watch]
max_pending = 16
unused = 0
"#,
        )
        .unwrap();
        assert_eq!(cfg.destination.root.as_deref(), Some(r"D:\Weekcase"));
        assert_eq!(cfg.destination.collision, Collision::Skip);
        assert_eq!(cfg.watch.max_pending, 16);
        assert_eq!(cfg.watch.max_sources, MAX_SOURCES);
    }

    #[test]
    fn kind_only_source_fills_kind_defaults() {
        let cfg = parse(
            r#"
[[sources]]
kind = "downloads"
min_age_secs = 2
"#,
        )
        .unwrap();
        assert_eq!(cfg.sources.len(), 1);
        assert_eq!(cfg.sources[0].id, "downloads");
        assert_eq!(cfg.sources[0].min_age_secs, 2);
        assert_eq!(cfg.sources[0].settle_secs, DOWNLOADS_SETTLE_SECS);
    }

    #[test]
    fn default_templates_use_only_v1_tokens() {
        let cfg = Config::default();
        for template in [
            cfg.destination.downloads_template.as_str(),
            cfg.destination.screenshots_template.as_str(),
        ] {
            let tokens = template_tokens(template);
            assert!(!tokens.is_empty(), "{template}");
            assert!(
                unknown_template_tokens(template).is_empty(),
                "{template} {:?}",
                template_tokens(template)
            );
        }
        assert!(!V1_TEMPLATE_TOKENS.contains(&"ww"));
        assert_eq!(
            template_tokens("{root}/Screenshots/{yyyy}-{mm}"),
            ["root", "yyyy", "mm"]
        );
        assert_eq!(unknown_template_tokens("{root}/{ww}"), vec!["ww"]);
    }

    #[test]
    fn load_missing_is_default_without_creating() {
        let path = PathBuf::from("/no/such/weekcase/config.toml");
        let cfg = load_or_default(&path).unwrap();
        assert_eq!(cfg, Config::default());
        assert!(!path.exists());
    }

    #[test]
    fn write_default_if_missing_creates_once() {
        let dir = std::env::temp_dir().join(format!(
            "weekcase-cfg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("config.toml");
        assert!(write_default_if_missing(&path).unwrap());
        assert!(!write_default_if_missing(&path).unwrap());
        assert_eq!(
            parse(&fs::read_to_string(&path).unwrap()).unwrap(),
            Config::default()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn patch_toml_key_updates_existing_and_inserts_missing() {
        let patched = patch_toml_key(DEFAULT_TOML, "general", "paused", "true");
        let cfg = parse(&patched).unwrap();
        assert!(cfg.general.paused);
        assert!(cfg.general.start_with_windows);
        assert_eq!(cfg.destination, Config::default().destination);

        let patched = patch_toml_key(
            DEFAULT_TOML,
            "destination",
            "root",
            &toml_quoted(r"D:\Weekcase"),
        );
        let cfg = parse(&patched).unwrap();
        assert_eq!(cfg.destination.root.as_deref(), Some(r"D:\Weekcase"));

        let inserted = patch_toml_key(
            "[general]\nstart_with_windows = true\n",
            "general",
            "paused",
            "true",
        );
        assert!(inserted.contains("paused = true"));
        let cfg = parse(&inserted).unwrap();
        assert!(cfg.general.paused);

        let appended = patch_toml_key("[watch]\nmax_pending = 8\n", "general", "paused", "true");
        assert!(appended.contains("[general]"));
        let cfg = parse(&appended).unwrap();
        assert!(cfg.general.paused);
        assert_eq!(cfg.watch.max_pending, 8);
    }

    #[test]
    fn persist_kv_skips_missing_file() {
        let path = PathBuf::from("/no/such/weekcase/missing-persist.toml");
        assert!(!persist_bool(&path, "general", "paused", true).unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn persist_kv_rewrites_existing() {
        let dir = std::env::temp_dir().join(format!(
            "weekcase-persist-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, DEFAULT_TOML).unwrap();
        assert!(persist_bool(&path, "general", "paused", true).unwrap());
        assert!(persist_string(&path, "destination", "root", r"C:\Weekcase").unwrap());
        let cfg = parse(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(cfg.general.paused);
        assert_eq!(cfg.destination.root.as_deref(), Some(r"C:\Weekcase"));
        let _ = fs::remove_dir_all(&dir);
    }
}
