# Weekcase 总体设计

| | |
|------|------|
| 状态 | Accepted（2026-08-30） |
| 平台 | Windows 11，x64 与 ARM64 |
| 形态 | 单机托盘小工具，零网络 |

功能规格（实现按那些文件，不按本文展开）：

| 文档 | 内容 |
|------|------|
| [features/01-watch.md](features/01-watch.md) | 监视源、冷静期、稳定判定 |
| [features/02-classify.md](features/02-classify.md) | 分类与落点模板 |
| [features/03-execute.md](features/03-execute.md) | Move、冲突、撤销 |
| [features/04-tray.md](features/04-tray.md) | 托盘、开机启动 |
| [features/05-first-run.md](features/05-first-run.md) | 首次运行、整理现有、暂停 |

---

## 这是什么

Downloads 和截图目录会堆成垃圾场。Weekcase 盯着这两个顶层目录，等文件写完、也过了你还能用的窗口之后，按固定规则 **移动** 到归档文件夹。搬错可以撤销。

- **下载**：在 Downloads 里先留约 **7 天**（刚下的安装包还能双击），然后按扩展名分类型。
- **截图**：大约几十秒后，按 **月** 进 `Screenshots/2026-08`。不按 ISO 周。
- 不上传、不读文件内容、不做规则引擎。平时没有窗口。

归档是目标。监视文件夹、按类型/按月分箱、托盘常驻都是手段，以后可以换。名字像「按周一卷」，只是名字。

## 给自己怎么用

装上或解压后托盘常驻，开机启动。默认监视 Windows 的「下载」和「截图」Known Folder。

需要时：暂停、撤销上一次、打开归档目录、改归档根、点「整理现有文件」。

**第一次不会把已经堆着的旧文件搬走。** 旧的要你自己点整理。

## 落点

默认根目录：`文档\Weekcase`（首次可改；若落在 OneDrive 下会警告）。

```
文档\Weekcase\
  Downloads\
    Images\  Documents\  Archives\  Audio\  Video\  Installers\  Other\
  Screenshots\
    2026-08\
    2026-09\
```

下载分档只看最后一个点后面的扩展名：

| 目录 | 例子 |
|------|------|
| Images | png jpg jpeg gif webp bmp heic svg … |
| Documents | pdf docx xlsx pptx txt md csv epub … |
| Archives | zip rar 7z tar gz iso … |
| Audio | mp3 wav flac m4a … |
| Video | mp4 mkv avi mov webm … |
| Installers | exe msi msix appx cab |
| Other | 对不上的，包括没扩展名 |

截图源里的文件一律当截图，即使叫 `foo.zip`。完整扩展名表见 [功能 2](features/02-classify.md)。

## 做什么 / 不做什么

**做：** 顶层文件、写完再搬、冷静期、内置规则表、Move、冲突加后缀、本机撤销、托盘静默、开机启动、低内存。

**不做：** 规则 IDE、读内容 / OCR / AI、云同步、默认监视桌面、递归整盘、复制/删除/压缩/上传、Electron / 常驻 Python、插件和任意脚本钩子、重复文件清理。

不直接当 File Juggler / DropIt / Hazel 用：那些是全能规则器，会把这个小工具做成另一条产品。Windows Storage Sense 是删文件腾空间，不是归档。PowerToys 没有这项能力。

## 系统结构

单进程、三线程，每个文件事件不另起进程。

```
weekcase.exe（用户会话，单实例）
  T0  托盘消息泵
  T1  监视：ReadDirectoryChangesW + 周期顶层列举
      只把「已到龄」的文件放进候选表
  T2  每秒扫候选：稳定 → 分类 → 串行 Move → 写撤销日志
```

```mermaid
flowchart LR
  源目录 --> 监视
  监视 -->|已到龄才进| 候选表
  候选表 --> 稳定
  稳定 -->|未暂停| 分类
  分类 --> Move
  Move -->|成功| 撤销日志
  Move -->|失败| 候选表
```

T1 绝不在监视回调里搬文件。T2 搬文件前必须放下候选表的锁，避免大文件跨盘 copy 把监视卡住。没有跨线程的「已稳定队列」。暂停只停 Move，不丢候选。

### 什么文件会被自动收

必须同时满足：

1. 源目录 **顶层** 普通文件（不进子文件夹，不搬文件夹）
2. 不在忽略名单（`*.crdownload`、自身 exe、云占位等）
3. 创建时间 **晚于首次运行**（旧库存量不自动收）
4. 已经过了该源的冷静期（下载 7 天，截图 20 秒）

未到龄的下载 **不进内存候选表**，就在磁盘上待着。到点靠每 60 秒扫一遍顶层目录收进来。不用 watermark 把「还太年轻」的文件记成已处理——否则 7 天后会漏。

点「整理现有文件」会暂时不管首次运行时间和冷静期，专门收当前顶层的旧文件。一次最多进队 256 个，多了再点一次。

细节、忽略列表、锁探测见 [功能 1](features/01-watch.md)。

### 搬之前还要等写完

冷静期 ≠ 写完。到龄之后还要：

- 尺寸和修改时间连续不变一段时间（下载约 15 秒，截图约 8 秒）
- 没有别人以写入方式开着这个文件

### 内存

干净 Win11 虚拟机、默认监视两源、空闲 5 分钟，看任务管理器「内存」（Working Set）：

| | stretch | 发版硬上限 |
|--|---------|------------|
| 空闲 | 12 MB | **20 MB**（超了不能发版） |

12 MB 不是 CI 失败线。GitHub 的 `windows-latest` 不卡这个数。候选表最多 256 条，只装已经到龄、等着搬的文件。

禁止：Electron / WebView、常驻 Python、把目录树留在内存里、每个事件再起一个进程。

## 技术选型

**Rust + Win32 单进程。** 无运行时。实现和集成测试需要 Windows + MSVC。

监视用非递归 `ReadDirectoryChangesW`，加上启动和周期顶层列举。不用 USN Journal（要权限、太重）。不要网络盘当源（UNC 和映射盘都拒绝）。U 盘可以，弹出就停监视。

默认动作只有 `MoveFileExW`。同盘是改名，跨盘允许 copy 再删源。冲突默认变成 `foo-1.pdf`，绝不覆盖。撤销是自己的 `undo.jsonl`，不靠资源管理器 Ctrl+Z。

## 数据放哪

| 文件 | 位置 | 作用 |
|------|------|------|
| `config.toml` | `%APPDATA%\Weekcase\` | 用户配置 |
| `state.json` | `%LOCALAPPDATA%\Weekcase\` | `first_run_at`、跳过/卡住的路径 |
| `undo.jsonl` | 同上 | 撤销日志，只追加 |
| 日志 | `logs\weekcase.log` | 滚动 1 MB × 3 |

exe 旁边有 `portable.ini` 时，全部改到 `exe_dir\data\`。不删用户已经归档走的文件。

默认配置要点：

- 下载 `min_age_secs = 604800`（7 天），`settle_secs = 15`，每 60 秒扫顶层
- 截图 `min_age_secs = 20`，`settle_secs = 8`，每 10 秒扫顶层
- `downloads_template = "{root}/Downloads/{bucket}"`
- `screenshots_template = "{root}/Screenshots/{yyyy}-{mm}"`
- 开机启动默认开

v1 模板只认 `{root}` `{bucket}` `{yyyy}` `{mm}`。没有 `{ww}`，不按 ISO 周。

## 安全底线

- 只动配置里启用的源；落点不能落在源里面（防循环）
- 拒绝盘符根、用户配置根、Windows、Program Files、UNC、网络盘
- 正在写的文件不搬；临时下载后缀直接忽略
- 只处理顶层，文件名里带 `\` `/` 的跳过
- 硬忽略自己的 exe 和便携 `data\`
- 源侧跳过 OneDrive 占位文件；归档根如果在 OneDrive 下，首次对话框必须警告
- 不要求管理员，不写 HKLM，无网络代码

## 关键决策

1. 目标是归档，不是规则引擎。
2. 栈用 Rust 单进程 Win32。
3. 空闲内存 stretch 12 MB，发版硬上限 20 MB。
4. 监视 = RDC + 周期列举，不用 USN；拒绝网络盘。
5. 只处理源顶层普通文件。
6. 内置查找表，不读内容。
7. 动作只有 Move。
8. 冲突加后缀，禁止覆盖。
9. 下载冷静期 7 天，截图 20 秒。未到龄不进候选表。
10. 首次不自动清旧文件。
11. 产品名不锁定组织模型。截图按月，下载按类型，不按 ISO 周。
12. 零网络；数据在用户目录；便携用 `portable.ini`。
13. 自己的撤销日志。
14. 托盘静默，默认不弹 Toast。
15. 开机启动默认开，登录后再等 15 秒解析 Known Folder。

## 已拍板

| | 决定 |
|--|------|
| 装上要不要清旧文件 | 不自动清，要点「整理现有文件」 |
| 截图怎么分目录 | 按月（`2026-08`），不按 ISO 周 |
| 归档根 | `文档\Weekcase`，在 OneDrive 下会警告 |
| 下载冷静期 | 7 天 |

## 实现顺序

需要 Windows 开发机。Linux 只能跑分类、文件名这类单测。CI 用 `windows-latest`，不把 20 MB 当 CI 失败。

0. 仓库骨架、单实例 Mutex、CI  
1. 配置、Known Folder、`state.json`  
2. 监视与稳定（还不移动）— [功能 1](features/01-watch.md)  
3. 分类模板（可与 2 并行）— [功能 2](features/02-classify.md)  
4. Move + 冲突 + 撤销 — [功能 3](features/03-execute.md)  
5. 接到一条管线  
6. 托盘、开机启动、选归档根 — [功能 4](features/04-tray.md)  
7. 首次运行对话框 — [功能 5](features/05-first-run.md)  
8. 便携 zip；发版前在干净虚拟机测内存  

```
PR0 → PR1 → PR2 ─┐
         └→ PR3 → PR4 ─┴→ PR5 → PR6 → PR7
                              └→ PR8
```

## 仓库结构

```
src/main.rs config.rs known_folders.rs watch.rs candidate.rs
    stabilize.rs classify.rs execute.rs undo.rs tray.rs
    settings.rs log_init.rs paths.rs state.rs
.github/workflows/ci.yml          # windows-latest
```

