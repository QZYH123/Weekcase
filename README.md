# Weekcase

Windows 11 托盘小工具：盯着「下载」和「截图」顶层目录，等文件写完并过了冷静期后，按固定规则 **移动** 到本机归档文件夹。搬错可以撤销。

**需要 Windows 11**（x64 与 ARM64）。不支持 Windows 10。

- 只在本机工作：零网络，无云同步，**无自动更新**
- 不读文件内容，不做规则引擎
- 单实例；第二个进程立即退出

## 数据放哪

| 文件 | 位置 |
|------|------|
| `config.toml` | `%APPDATA%\Weekcase\` |
| `state.json`、`undo.jsonl` | `%LOCALAPPDATA%\Weekcase\` |
| 日志 | `%LOCALAPPDATA%\Weekcase\logs\weekcase.log`（滚动 1 MiB × 3） |

exe 同目录有 `portable.ini` 时，以上全部改到 `exe_dir\data\`。

## 构建

产品二进制需要 Windows + MSVC：

```text
cargo test --all
cargo build --release
```

Linux 上可以跑不依赖 Win32 的单测。`CreateMutexW` 等接口在 `cfg(windows)` 后面。

空闲 Working Set 目标约 12 MB，发版硬上限 20 MB。这不是 CI 失败线。
