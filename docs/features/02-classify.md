> 总体设计见 [../design.md](../design.md)。本文是可独立实施的功能规格。

# 功能 2 — 分类与落点

## 该功能要解决的用户问题

稳定文件要去一个 **可预测** 的地方。用户不应编写规则语言。打开归档根目录应能凭扩展名或月份找到文件。

## 范围 / 非范围

**范围**

- 内置扩展名 → `bucket`。
- 按 `source.kind` 选模板。
- 展开 v1 保证的 token：`{root}` `{bucket}` `{yyyy}` `{mm}`。
- 落点不得落在任一源之内的校验（纯路径运算，不建目录）。

**非范围**

- 用户自定义谓词（regex 条件、大小、内容）。
- 读魔数 / MIME。扩展名是唯一类型信号。无扩展名 → `Other`。
- 移动动作本身。

## 行为规格

### 触发

T2 在候选已稳定且（若未暂停）准备 execute 时调用。纯函数 `classify(cfg, snap: &FileSnapshot) -> Result<Placement, ClassifyError>`。不创建目录。

### 成功

返回 `Placement { dest_dir, dest_name, bucket }`，`dest_name` 等于源文件名（含扩展名，不做美化）。`dest_dir` 绝对路径、规范化、不存在也可以（执行器创建）。

### 失败

| 条件 | 结果 |
|------|------|
| 模板展开后不是绝对路径 | `ClassifyError::BadTemplate`：短锁 **移出候选**，ERROR 限流 1 次/分钟（配置错误，不每秒重试）。修好模板后「重新加载配置」或显式 Rescan 再入。 |
| `dest_dir` 等于某源或位于某源之下 | `ClassifyError::DestInsideSource`：短锁置 `poisoned`（路径问题，留痕避免立刻再入），ERROR 限流。改 root 后 Rescan 清 poisoned。 |
| `dest_dir` 位于盘符根、`FOLDERID_Profile` / `Windows` / `ProgramFiles*` / `ProgramData` | 同 `DestInsideSource`：`poisoned` + ERROR 限流 |
| 文件名无法当文件名（空、`.`、`..`、含 `\` `/`） | 移出候选，WARN 一次。文件留源，不进自动循环 |

### 空状态

无稳定文件则本模块不运行。

### 默认映射

`source_kind = screenshots`：**忽略扩展名表**，`bucket = Screenshots`，只用 `screenshots_template`。截图目录里的 `foo.zip` 也当截图走月模板——这是刻意的：源语义优先于扩展名，避免截图目录被拆散。

`source_kind = downloads`（额外源若 `kind = downloads` 走同一张表）：

| bucket | 扩展名 |
|--------|--------|
| Images | png jpg jpeg gif webp bmp tif tiff heic heif svg |
| Documents | pdf doc docx xls xlsx ppt pptx txt md csv rtf odt ods odp epub |
| Archives | zip rar 7z tar gz tgz bz2 xz iso |
| Audio | mp3 wav flac aac m4a ogg wma |
| Video | mp4 mkv avi mov webm mpeg mpg wmv |
| Installers | exe msi msix appx cab |
| Other | 其余 |

默认模板：

- downloads: `{root}/Downloads/{bucket}`
- screenshots: `{root}/Screenshots/{yyyy}-{mm}`  
  `{yyyy}` `{mm}` 来自文件 `created` 的日历年、月，不是「归档发生时刻」。这样 8 月 31 日截的图 9 月 1 日归档仍进 `2026-08`。`{mm}` 两位零填充。

`{root}` 默认 `SHGetKnownFolderPath(FOLDERID_Documents) + "\\Weekcase"`（OQ-3，用户已确认 2026-08-30）。若解析后的 root 位于 `FOLDERID_OneDrive` 之下，或规范化路径含 `\OneDrive\`，首次对话框必须警告（功能 5）；分类函数本身不拒绝 OneDrive 路径。截图模板按月（OQ-2，用户改判 2026-08-30：不按 ISO 周）。

v1 **不**实现 `{ww}`、`{yyyy-mm}` 整词、`{source}`。配置里出现未知 token → `BadTemplate`。没有 `SourceKind::Custom`：第三条源只能 `kind = downloads | screenshots` 且必须写绝对 `path`。

### 边界

- `File.PDF` → 扩展名小写匹配 `pdf` → Documents。
- `tar.gz`：只取最后一个点 → `gz` → Archives（足够；不解析复合扩展名）。
- 跨年：2026-12-31 的截图进 `2026-12`，不是 `2027-01`。
- 用户把 `screenshots_template` 改成 `{root}/Screenshots`：所有截图进同一目录。

## 与总体架构的接口

```rust
pub fn classify(cfg: &Config, f: &FileSnapshot) -> Result<Placement, ClassifyError>;

pub enum Bucket { Images, Documents, Archives, Audio, Video, Installers, Other, Screenshots }

pub struct Placement {
    pub dest_dir: PathBuf,
    pub dest_name: OsString,
    pub bucket: Bucket,
}
```

无 IO。dest-in-source / denylist 只比较已解析路径字符串。可在 Linux 上单测。建目录属于功能 3。

## 关键实现要点

模板展开只替换花括号 token，不做通用 format 语言。未知 token → `BadTemplate`。

年月用文件 `created` 的本地日历日期（Windows 上 `FILETIME` → 本地 `SYSTEMTIME` 的 `wYear`/`wMonth`）。不引入额外日期 crate。单测注入固定 `created`：

- 2026-08-30 → `{yyyy}=2026` `{mm}=08`
- 2026-01-01 → `{yyyy}=2026` `{mm}=01`
- 2025-12-31 → `{yyyy}=2025` `{mm}=12`

## 验收标准

1. Downloads 下 `a.PDF` → `{root}/Downloads/Documents`。
2. Screenshots 下 `Screenshot (3).png`，创建于 2026-08-30 → `{root}/Screenshots/2026-08`。
3. 配置 `root` 落在 Downloads 内 → 所有分类返回 `DestInsideSource`。
4. 无扩展名 `LICENSE` → Other。
5. 单元测试不碰真实磁盘。
6. `BadTemplate` 后同一路径不会在下一秒再次 classify（已不在候选表）。

## 本功能开放问题

无。截图默认按月（OQ-2，用户改判 2026-08-30：不按 ISO 周）。

---

