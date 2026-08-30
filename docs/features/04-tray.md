> 总体设计见 [../design.md](../design.md)。本文是可独立实施的功能规格。

# 功能 4 — 托盘常驻、静默运行与开机启动

## 该功能要解决的用户问题

工具必须自己活着，不能占一个窗口，不能每天下载高峰弹气泡。用户要能暂停、退出、开机自启、打开归档目录。

## 范围 / 非范围

**范围**

- 单实例 Mutex。
- 隐藏窗口 + 消息泵 + `Shell_NotifyIconW`。
- 菜单：暂停/恢复、撤销上一次、整理现有文件、**选择归档文件夹**、打开归档根、打开日志、重新加载配置、开机启动勾选、退出。
- 开机启动：HKCU `Run` 或当前用户登录计划任务（延迟 15 s）。
- Tooltip 与图标状态。
- 无控制台子系统（`#![windows_subsystem = "windows"]`）。

**非范围**

- WinUI/WPF 设置中心。
- Toast 通知中心（默认关；不实现）。
- 多语言框架（v1 菜单中文硬编码，配置注释中英均可）。
- 自动更新。

## 行为规格

### 触发

`main` 启动。已有实例：把命令行转给第一实例（可选；v1 更简单：**发现 Mutex 则立即退出** 0）。

### 成功

任务栏溢出区出现图标。无主窗口。不闪控制台。

### 菜单行为

| 项 | 行为 |
|----|------|
| 暂停归档 | 置 `paused` 原子 + 写回 `general.paused=true`。T2 跳过 execute；T1 仍 upsert；候选不丢。 |
| 恢复 | 清 `paused`；已稳定候选在下一 T2 tick 执行，不必等 60 s 列举。 |
| 撤销上一次 | 调功能 3；失败 MessageBox 一句原因 |
| 整理现有文件 | 确认后对启用源发 `WatchCmd::Rescan { source: None, include_existing: true, min_age_override: Some(Duration::ZERO) }` |
| 选择归档文件夹 | `IFileDialog` + `FOS_PICKFOLDERS`；走与首次对话框相同的 denylist / dest-in-source / OneDrive 警告；写回 `destination.root` 并重载 |
| 打开归档文件夹 | `ShellExecuteW(open, root)` |
| 打开日志 | `ShellExecuteW(open, log_path)` |
| 开机启动 | 勾选 ↔ 写 Run 键 |
| 重新加载配置 | 重新读 TOML；源路径变化则重建 watch |
| 退出 | 停 T1/T2，删 notify icon，`PostQuitMessage` |

### 失败

`Shell_NotifyIcon` 失败（Explorer 未起）：订阅 `TaskbarCreated` 消息再试，这是托盘程序标准动作。

### 开机启动

默认 **开**。写入：

```
HKCU\Software\Microsoft\Windows\CurrentVersion\Run
  Weekcase = "C:\full\path\weekcase.exe"
```

便携模式同样写绝对路径。卸载/退出时若用户关掉勾选则删值。

若实现者改用计划任务：`schtasks` 对当前用户 `/SC ONLOGON /DELAY 0000:15`，不要求管理员。二选一，不要两个都注册。**推荐 Run 键**：实现简单；OneDrive 竞态用进程内 15 s 延迟解析 Known Folder（R10）。

### 空状态

从未归档：撤销灰、Tooltip「监视中 · 今日已归档 0」。

## 与总体架构的接口

T0 持有 `Sender<WatchCmd>`（仅 `Rescan` / `Shutdown`）、暂停用的 `Arc<AtomicBool>`、`Sender<ExecCmd::UndoLast>`。今日计数用 `AtomicU32`，execute 成功时 +1，零点不需准确（进程内计数即可，重启清零）。

## 关键实现要点

- `CreateMutexW(NULL, TRUE, L"Local\\Weekcase.SingleInstance")`。
- 窗口类：`WNDCLASSEXW`，`lpfnWndProc` 处理 `WM_TRAY`、`WM_COMMAND`、`TaskbarCreated`。
- 图标：嵌入 `.ico` 于 exe（`winres` / `embed-resource`）。无图标也能跑（LoadIcon 默认）。
- 「选择归档文件夹」与首次对话框共用同一套 `IFileDialog` + `FOS_PICKFOLDERS` 校验，不要自绘文件管理器。v1 **不是**只能改 toml。

## 验收标准

1. 启动无控制台窗口。
2. 双开第二进程立刻退出，第一进程继续。
3. 杀 `explorer.exe` 再恢复，托盘图标回来。
4. 勾选开机启动后注册表出现 exe 绝对路径；取消则删除。
5. 暂停期间放下稳定文件：不移动且仍在候选表；恢复后在下一 T2 tick 移动（不必等 60 s）。
6. 整进程 idle working set 在干净 VM 上 ≤ 20 MB（12 MB 为 stretch，失败不挡 PR 合并）。

## 本功能开放问题

无。

---

