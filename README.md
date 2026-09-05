<p align="center">
  <img src="assets/weekcase.png" width="112" height="112" alt="Weekcase">
</p>

# Weekcase

Windows 11 托盘小工具：盯着「下载」和「截图」顶层目录，等文件写完并过了冷静期后，按固定规则 **移动** 到本机归档文件夹。搬错可以撤销。

**需要 Windows 11**（x64 与 ARM64）。不支持 Windows 10。

- 只在本机工作：零网络，无云同步，**无自动更新**
- 不读文件内容，不做规则引擎
- 单实例；第二个进程立即退出

## 怎么用

装上或解压后托盘常驻，开机启动。默认监视 Windows 的「下载」和「截图」Known Folder。

| | 冷静期 | 归档方式 |
|--|--------|----------|
| **下载** | 在 Downloads 里先留约 **7 天**（刚下的安装包还能双击） | 按扩展名分类型 |
| **截图** | 大约几十秒 | 按月进 `Screenshots\2026-08`，不按 ISO 周 |

第一次打开会确认归档根。路径落在 OneDrive 下会警告。

**第一次不会把已经堆着的旧文件搬走。** 旧的要自己点「整理现有文件」。一次最多进队 256 个，多了再点一次。

托盘：暂停、撤销上一次、「整理现有文件」、选择 / 打开归档目录、打开日志、重新加载配置、开机启动。

卸载或删掉程序 **不会** 删除已经归档走的文件。

## 归档长什么样

默认根：`文档\Weekcase`

```text
文档\Weekcase\
  Downloads\
    Images\  Documents\  Archives\  Audio\  Video\  Installers\  Other\
  Screenshots\
    2026-08\
    2026-09\
```

下载只看最后一个点后面的扩展名。截图源里的文件一律当截图，即使叫 `foo.zip`。同名默认变成 `foo-1.pdf`，永不覆盖。

<details>
<summary>下载扩展名怎么分</summary>

| 目录 | 扩展名 |
|------|--------|
| Images | png jpg jpeg gif webp bmp tif tiff heic heif svg |
| Documents | pdf doc docx xls xlsx ppt pptx txt md csv rtf odt ods odp epub |
| Archives | zip rar 7z tar gz tgz bz2 xz iso |
| Audio | mp3 wav flac aac m4a ogg wma |
| Video | mp4 mkv avi mov webm mpeg mpg wmv |
| Installers | exe msi msix appx cab |
| Other | 对不上的，包括没扩展名 |

完整规则见 [docs/features/02-classify.md](docs/features/02-classify.md)。

</details>

## 数据放哪

| 文件 | 位置 |
|------|------|
| `config.toml` | `%APPDATA%\Weekcase\` |
| `state.json`、`undo.jsonl` | `%LOCALAPPDATA%\Weekcase\` |
| 日志 | `%LOCALAPPDATA%\Weekcase\logs\weekcase.log`（滚动 1 MiB × 3） |

exe 同目录有 `portable.ini` 时，以上全部改到 `exe_dir\data\`。有这个文件即开启便携，内容被忽略。归档仍按配置里的根目录存放，卸载不删它们。

便携 zip 把 `weekcase.exe` 和一份 `portable.ini` 样例放在同一目录。打包：

```powershell
powershell -File scripts/package.ps1
```

产出 `target\package\weekcase-portable.zip`。

## 构建

产品二进制需要 Windows + MSVC（Rust 1.80+）：

```text
cargo test --all
cargo build --release
```

Linux 上可以跑不依赖 Win32 的单测。`CreateMutexW` 等接口在 `cfg(windows)` 后面。

## 内存

干净 Win11 虚拟机、默认监视两源、空闲 5 分钟，看任务管理器「内存」（Working Set）：

| | stretch | 发版硬上限 |
|--|---------|------------|
| 空闲 | 12 MB | **20 MB**（超了不能发版） |

12 MB 不是 CI 失败线。GitHub 的 `windows-latest` 不卡这个数。发版前在干净虚拟机上测：

```powershell
powershell -File scripts/measure-rss.ps1
```

脚本打印 `WorkingSetSize` 后以 0 退出；**不要** 把 20 MB 配成 GHA 失败。

## 规格

行为以这些文件为准，不按本文展开：

| 文档 | 内容 |
|------|------|
| [docs/design.md](docs/design.md) | 总体设计：做什么 / 不做什么、线程模型、底线 |
| [docs/features/01-watch.md](docs/features/01-watch.md) | 监视源、冷静期、稳定判定 |
| [docs/features/02-classify.md](docs/features/02-classify.md) | 分类与落点模板 |
| [docs/features/03-execute.md](docs/features/03-execute.md) | Move、冲突、撤销 |
| [docs/features/04-tray.md](docs/features/04-tray.md) | 托盘、开机启动 |
| [docs/features/05-first-run.md](docs/features/05-first-run.md) | 首次运行、整理现有、暂停 |
