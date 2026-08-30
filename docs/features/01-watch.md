> 总体设计见 [../design.md](../design.md)。本文是可独立实施的功能规格。

# 功能 1 — 归档源监视与稳定判定

## 该功能要解决的用户问题

文件出现在 Downloads / Screenshots 的瞬间通常 **还不能搬**：浏览器在写 `*.crdownload`，下载还要留约 7 天给人用，截图工具可能仍打开 PNG 标注。本功能只负责：**看见顶层文件、未到龄不进候选表、到龄后再等 settle/锁**。不分类、不移动。稳定之后仍留在候选表里，由 T2 执行（功能 3）。

## 范围 / 非范围

**范围**

- 解析 Known Folder 与配置覆盖路径；打开源前的盘符类型 / denylist / reparse 解析。
- 非递归 `ReadDirectoryChangesW`（T1）。
- 启动一次顶层列举 + 按源 `scan_interval` 周期列举，按 [总体设计](../design.md) 入候选资格。
- 忽略列表、云占位、reparse、目录跳过、自身 exe / 便携 `data\`。
- 去抖；**仅 `age >= min_age` 才 upsert**。
- **稳定算法规格**（T2 每秒扫已到龄候选）：settle、锁探测。min_age 不在表内等待。

**非范围**

- 计算落点、Move、托盘、撤销。
- 跨线程 `StableFile` 队列。
- `WatchCmd::Pause`（暂停是进程级 `AtomicBool`，见功能 5）。
- 递归子目录、UNC / `DRIVE_REMOTE` 源、USN。
- 用户自定义 glob 引擎。
- `SourceKind::Custom`（v1 只有 `downloads` | `screenshots`）。

## 行为规格

### 触发

1. 进程启动且 `sources[i].enabled`：打开 RDC，再顶层列举（资格表规则 1/2）。
2. RDC 报告 `FILE_ACTION_ADDED` / `MODIFIED` / `RENAMED_NEW_NAME`（规则 3：未到龄不 upsert）。
3. 周期列举（下载 60 s，截图 10 s）发现已到龄且 `created >= first_run_at` 的文件。
4. `WatchCmd::Rescan { include_existing: true, ... }`（规则 4，功能 5）。

### 成功

- 合格文件出现在候选表中。
- T2 判定稳定的条件：该项 **upsert 时已到龄**（或本次 Rescan 豁免 min_age），且 `size`/`mtime` 连续 `settle` 时长不变 **且** 锁探测通过。稳定后 **不删除** 候选，只置 `stable_since`；Move 成功才移出（功能 3）。

### 失败 / 空状态

- Known Folder 解析失败：该源逻辑关闭，日志 ERROR，不崩溃。
- 源是 UNC / `DRIVE_REMOTE` / denylist：拒绝，ERROR，不 watch。
- 目录不存在：不创建源目录（截图文件夹尤其如此）。每 60 s 尝试一次打开目录；出现后再 watch。
- 候选表满 256：已到龄 ready 项不丢；未 ready 可挤掉最旧一条；全是 ready 则拒绝新 upsert。`WARN pending_overflow`，托盘「有文件没进队」。溢出后下一轮周期列举再试。显式 sweep 未完成则可再点一次。
- RDC 返回 `ERROR_NOTIFY_ENUM_DIR`：视为溢出，立即顶层列举该源（规则 2）。
- 文件在稳定前消失：从候选表删除，静默。
- `blocked` 中的 `from`：不 upsert。

### 边界

| 输入 | 行为 |
|------|------|
| `report.pdf.crdownload` | 忽略名，不进候选 |
| `report.pdf.crdownload` → rename → `report.pdf` | `RENAMED_NEW_NAME` 作为新候选，`first_seen=now` |
| 0 字节文件，年龄 < 60 s | 不视为稳定（浏览器先创建空文件） |
| 0 字节文件，年龄 ≥ 60 s 且稳定 | 允许（有人确实下载空文件）；分类后仍会 Move |
| 子目录 `Downloads\foo\` | 不 watch、不进候选、不 Move |
| `desktop.ini` / `Thumbs.db` / `~$*` | 忽略 |
| 云占位属性 | 忽略，直到属性消失再当新文件（届时走 RDC 或规则 2） |
| 路径长于 32 KiB 或含 `\\?\` 异常形式 | 规范化失败则跳过并 WARN |
| 源目录本身被重命名/删除 / U 盘弹出 | 停 watch，ERROR，60 s 后尝试重开 |
| `Z:\Downloads`（映射盘） | 打开前 `GetDriveTypeW == DRIVE_REMOTE`，拒绝 |
| `weekcase.exe` 位于 Downloads 顶层 | 硬忽略，7 天后仍在原处 |
| 暂停期间已到龄且稳定 | 留在候选表，不 Move；恢复后下一 T2 tick 执行 |
| 下载后 3 天的 PDF | 不进候选表；第 7 天后周期列举才 upsert |

### 忽略规则（硬编码，配置可追加不可删减安全项）

扩展名（小写）：`crdownload` `part` `partial` `tmp` `temp` `download` `opdownload` `!ut` `bc!` `filepart` `!qb` `qbmd` `aria2` `ytdl`。

v1 **承诺**覆盖常见浏览器临时名，以及 qBittorrent / aria2 / youtube-dl 一类下载器后缀。未列出的临时名靠尺寸稳定 + 锁探测，不为此做 glob 引擎。

文件名：`desktop.ini` `thumbs.db` `.ds_store`。前缀：`~$`。

路径硬忽略：当前 exe 规范化全路径；便携模式下 `exe_dir\data\` 的任何子项；`undo.jsonl` / `state.json` / 日志文件。

## 与总体架构的接口

领域模型里 **唯一** 的 `WatchCmd`（功能 4 / 5 必须引用这一份，禁止再定义）：

```rust
pub enum WatchCmd {
    Rescan {
        source: Option<SourceId>,           // None = 所有启用源
        include_existing: bool,             // true = 忽略 first_run_at 收存量
        min_age_override: Option<Duration>, // Some(0) = 本次豁免 min_age
    },
    Shutdown,
}

// T1：只 upsert。candidates 与 T2 共享。
pub fn start_watch(
    cfg: Arc<Config>,
    state: Arc<Mutex<AppState>>,            // first_run_at / blocked / skipped
    candidates: Arc<Mutex<HashMap<PathBuf, Candidate>>>,
    cmd: Receiver<WatchCmd>,
) -> JoinHandle<()>;

// T2 扫描候选时用的只读快照，不是跨线程队列。
pub struct FileSnapshot {
    pub path: PathBuf,
    pub source_id: String,
    pub source_kind: SourceKind, // Downloads | Screenshots
    pub size: u64,
    pub mtime: SystemTime,
    pub created: SystemTime,
}
```

暂停 **不是** `WatchCmd` 变体。配置依赖：`[[sources]]`、`[watch]`、`state.json`。不读 `destination` / `undo`。

## 关键实现要点

### Known Folder

```text
FOLDERID_Downloads   = {374DE290-123F-4565-9164-39C4925E467B}
FOLDERID_Screenshots = {B7BEDE81-DF94-4682-A7D8-57A52620B86F}
FOLDERID_Documents   = {FDD39AD0-238F-46AF-ADB4-6C85480369C7}
FOLDERID_OneDrive    = {A52BBA46-E9E1-435F-B3D9-28DAA648C0F6}   // 仅用于 root/源警告
FOLDERID_Profile     = {5E6C858F-0E22-4760-9AFE-EA3317B67173}   // 源 denylist
FOLDERID_ProgramData = {62AB5D82-FDC1-4DC3-A9DD-070D1D495D97}
```

`SHGetKnownFolderPath(rfid, KF_FLAG_DONT_VERIFY, NULL, &pszPath)`，然后 `CoTaskMemFree`。`DONT_VERIFY` 避免为不存在的 Screenshots 去创建。

禁止 `HOME/Downloads` 或拼接 `%USERPROFILE%`。OneDrive 挂钩后 Known Folder 已经是重定向路径。

### RDC

打开源目录前：

1. 拒绝 UNC、`GetDriveTypeW` ∈ {REMOTE, NO_ROOT_DIR}、denylist（盘符根 / Profile / Windows / Program Files* / ProgramData）。
2. 用 `FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT` 打开，探测自身是否 reparse。
3. 若是 reparse：再打开 **不带** `OPEN_REPARSE_POINT` 的句柄（或 `GetFinalPathNameByHandleW`）得到最终路径；对最终路径再做 denylist；监视最终路径。Known Folder 被 OneDrive junction 挂钩时必须走这一步，不能因为「是 reparse」直接拒绝。
4. 监视句柄：

```text
hDir = CreateFileW(final_source,
    FILE_LIST_DIRECTORY,
    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    OPEN_EXISTING,
    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED)
    // 最终路径已解析，此处跟随是预期行为

ReadDirectoryChangesW(hDir, buf=64KiB, bWatchSubtree=FALSE,
    FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_SIZE | FILE_NOTIFY_CHANGE_LAST_WRITE,
    overlapped)
```

`bWatchSubtree = FALSE` 是硬约束。完成例程或 IOCP 把 `FILE_NOTIFY_INFORMATION` 链解析为相对名，拼到 `final_source` 下。只接受「相对名不含 `\`」的条目（顶层）。

T1 线程专跑完成端口；upsert 用 `Mutex<HashMap<PathBuf, Candidate>>`，短锁。完成路径上禁止 `Move`、禁止等待有界队列。

### 稳定算法（仅 T2，1 s tick）

**锁顺序写死：**

```text
# 阶段 A：短锁，只读 + 采样 + 更新稳定字段，拷贝快照，放锁
lock candidates
ready = []
for cand in candidates:
  if cand.poisoned: continue
  if not path.is_file() or ignored(path): mark_drop; continue
  attrs = GetFileAttributesW          // 必须先于 CreateFile
  if attrs & (REPARSE | RECALL_ON_DATA_ACCESS | RECALL_ON_OPEN): mark_drop; continue
  (size, mtime, created) = GetFileExInfoStandard
  if size != last_size or mtime != last_mtime:
       reset stable_since; update last_*; continue
  if size == 0 and now - created < 60s: continue
  // 表中项 upsert 时已满足 min_age（sweep 豁免除外）；此处不再用 min_age 占槽等待
  if !lock_probe_ok(path):            // CreateFile + FILE_FLAG_OPEN_REPARSE_POINT
       continue
  cand.attempts = 0                   // 锁探测成功即清零；与功能 3 同一句话，无第二套状态
  if stable_since is None: stable_since = now
  if now - stable_since >= source.settle and not paused:
       ready.push(FileSnapshot{...})
unlock candidates                     // Move 开始前必须放锁

# 阶段 B：无锁。classify + MoveFileExW。禁止此时持有候选表锁。
for snap in ready:
  match classify(cfg, snap):
    Err(e) if e is 不可重试:          // BadTemplate / DestInsideSource / denylist / 非法文件名
         short_lock: remove 或 poisoned（见功能 2）；ERROR 限流 1 次/分钟
    Ok(placement):
         execute_move(...)            // 可能跨卷 copy，此时 T1 可 upsert
         short_lock: 成功则 remove；失败则 attempts++ / poisoned

apply mark_drop under short lock
```

禁止在 `for cand in candidates` 持锁循环里调用 `MoveFileExW` / `classify` 里的任何可能阻塞调用（classify 本身无 IO，但仍在放锁后做，避免以后误加 IO）。

`min_age` 在 **T1 upsert 前** 用文件创建时间判定；未到龄不进表。Windows 复制会刷新 creation；浏览器下载通常 creation ≈ 开始下载。若 `creation` 在未来，当作 `mtime`。测试可将 `min_age_secs` 注入成几秒验证「未到龄不 upsert / 到龄后列举才 upsert」；默认配置值仍是 7 天。

### 失败处理

- `CreateFile` 打开源目录失败：指数退避 1s、5s、15s、60s 封顶。
- 完成例程失败：重发 `ReadDirectoryChangesW`；连续 5 次失败则重建目录句柄。

## 验收标准

1. 在测试目录写入 `a.crdownload`，3 s 后改名为 `a.pdf` 并停止写入：`a.crdownload` 不进候选。默认 7 天内 `a.pdf` 也不进候选；测试注入 `min_age_secs=2` 时，到龄后由周期列举 upsert，settle 后 ready_for_execute 恰好一次。
2. 打开文件保持写共享占用：不 ready；关闭后进入 ready（仅已在表中的到龄文件）。
3. 源下建子目录并在其中写文件：无事件进入候选。
4. 64 KiB 缓冲被短时间大量 rename 打满：日志 `watch_overflow`，周期列举仍能把 **已到龄且 created >= first_run_at** 的存活文件收进候选。
5. 本模块私有分配空闲不应超过约 2 MB。整进程 idle WS 以 20 MB 硬上限为准。
6. Screenshots Known Folder 不存在时进程仍运行，Downloads 正常。
7. 映射盘路径作源：拒绝并 ERROR。
8. 把 `weekcase.exe` 放进测试 Downloads：7 天后仍在原处。
9. 首次运行：Downloads 里预放旧文件，列举不 upsert；进程启动后新放入的文件 7 天内不进表；到龄后启动或 60 s 列举 upsert（不依赖 watermark）。
10. 源里放 300 个 **已到龄** 文件，点「整理现有文件」：第一次最多进 256 条；托盘提示整理未完成；再点一次（或等前一批搬走后周期列举对 **created >= first_run_at** 的剩余自动补，存量仍要再点整理）收完。不允许已到龄的自动收件对象永远留在源。
11. 暂停期间打满 256 个已到龄 ready 项后再到龄一批：新到龄文件可以进不了表；恢复并搬走腾位后，下一轮列举能收回收件条件内的文件。年轻文件全程不占槽。
12. 文档/默认配置测试写明 downloads `min_age = 7 days`。逻辑测试允许注入更短 min_age。

## 本功能开放问题

无。下载 `min_age = 7 天` 已拍板（OQ-4，2026-08-30）。冷静期在 upsert 前执行，不占候选表。

---

