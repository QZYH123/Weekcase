# Weekcase

Windows 11 托盘小工具：盯着「下载」和「截图」顶层目录，等文件写完并过了冷静期后，按固定规则 **移动** 到本机归档文件夹。搬错可以撤销。

**需要 Windows 11**（x64 与 ARM64）。不支持 Windows 10。

- 只在本机工作：零网络，无云同步，**无自动更新**
- 不读文件内容，不做规则引擎
- 单实例；第二个进程立即退出

## 怎么用

装上或解压后托盘常驻，开机启动。默认监视 Windows 的「下载」和「截图」Known Folder。

- **下载**：在 Downloads 里先留约 **7 天**（刚下的安装包还能双击），然后按扩展名分类型。
- **截图**：大约几十秒后，按月进 `Screenshots/2026-08`。不按 ISO 周。

托盘：暂停、撤销上一次、「整理现有文件」、打开归档目录。第一次不会把已经堆着的旧文件搬走；旧的要自己点整理。

卸载或删掉程序 **不会** 删除已经归档走的文件。

默认归档根：`文档\Weekcase`

```
文档\Weekcase\
  Downloads\
    Images\  Documents\  Archives\  Audio\  Video\  Installers\  Other\
  Screenshots\
    2026-08\
```

## 数据放哪

| 文件 | 位置 |
|------|------|
| `config.toml` | `%APPDATA%\Weekcase\` |
| `state.json`、`undo.jsonl` | `%LOCALAPPDATA%\Weekcase\` |
| 日志 | `%LOCALAPPDATA%\Weekcase\logs\weekcase.log`（滚动 1 MiB × 3） |

exe 同目录有 `portable.ini` 时，以上全部改到 `exe_dir\data\`。有这个文件即开启便携，内容被忽略。归档仍按配置里的根目录存放，卸载不删它们。

便携 zip 把 `weekcase.exe` 和一份 `portable.ini` 样例放在同一目录。打包：

```text
powershell -File scripts/package.ps1
```

产出 `target\package\weekcase-portable.zip`。

## 构建

产品二进制需要 Windows + MSVC：

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

```text
powershell -File scripts/measure-rss.ps1
```

脚本打印 `WorkingSetSize` 后以 0 退出；**不要** 把 20 MB 配成 GHA 失败。
