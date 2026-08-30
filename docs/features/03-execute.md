> 总体设计见 [../design.md](../design.md)。本文是可独立实施的功能规格。

# 功能 3 — 归档执行、冲突处理与撤销

## 该功能要解决的用户问题

把稳定文件从源头挪到落点，不覆盖已有归档，失败不丢文件，搬错能搬回。

## 范围 / 非范围

**范围**

- 创建落点目录。
- `MoveFileExW`，同卷/跨卷。
- 冲突：`suffix` 或 `skip`。
- 失败重试（候选 `attempts`，上限 5，退避 5s、15s、60s）。
- `undo.jsonl` 追加与撤销上一条。
- 跨卷半失败清理。

**非范围**

- 回收站（不做 Delete）。
- Explorer 的 IFileOperation Undo 栈。
- 事务 NTFS（`MoveFileTransacted` 已弃用路径，不依赖）。
- 并行多文件 Move（T2 串行，简单、磁盘友好）。

## 行为规格

### 触发

T2 tick **阶段 A 已放锁** 之后，对 `ready` 快照调用 `classify`；成功且未暂停才 `execute_move`。Move 期间不得持有候选表锁。

### 成功

1. `dest_dir` 存在（`CreateDirectoryW` 递归）。
2. 最终路径 `dest` 在 Move 前不存在。
3. `MoveFileExW(from, dest, MOVEFILE_COPY_ALLOWED | MOVEFILE_WRITE_THROUGH)` 返回真。
4. `from` 不存在，`dest` 存在，`GetFileSize` 等于原 size。
5. 追加 undo 记录。
6. 日志 INFO。

同卷：这是 rename，ADS（含 `Zone.Identifier`）保留。跨卷：Windows 用 Copy+Delete；`CopyFile` 文档称保留 ADS。我们不额外手工拷 Zone。

### 冲突

`collision = suffix`（默认）：

```text
foo.pdf
foo-1.pdf
foo-2.pdf
...
foo-99.pdf 仍冲突 → 失败，不覆盖，attempts 用尽后放弃并 ERROR
```

后缀插在扩展名之前。`collision = skip`：不移动，写 skip 日志（不是 undo 行），**不占** attempts，从候选表移出，路径写入 `state.json` `skipped`。周期列举 **不会** 再收该路径，直到 RDC `MODIFIED`（从 skipped 去掉）或显式「整理现有文件」。禁止用 watermark 把 skip 当成「已处理时间界」。

**永远不** 传 `MOVEFILE_REPLACE_EXISTING`。

### 失败

| Win32 | 行为 |
|-------|------|
| `ERROR_SHARING_VIOLATION` | 视为仍未稳定：`attempts++`，留在候选表。`attempts >= 5` 后本 tick 不再 execute；一旦锁探测成功，**把 `attempts` 清零** 再允许 Move。60 s 列举若 upsert 同一路径，保留已有 `attempts`（不要当成新文件清零），除非锁探测已成功。 |
| `ERROR_DISK_FULL` | 停 Move，托盘失败态；attempts 不空转（下次 tick 仍可试，日志限流 1 次/分钟） |
| `ERROR_ACCESS_DENIED` | `attempts++`；满 5 次后与 sharing 相同：留在候选，锁/ACL 变化后清零再试 |
| 跨卷 copy 成功但 delete 源失败（MoveFileEx 文档：此时 API 仍可能返回成功并留下源） | 若源仍在且 dest 也在：`DeleteFileW(dest)`。成功则视为失败、不写 undo、源仍在。**若 `DeleteFileW(dest)` 也失败**：两边各一份；`cand.poisoned = true`；`state.json.blocked` 追加 `{from,to}`（最多 64）；ERROR + 托盘失败态；**不写成功 undo**；**禁止**再对 `from` upsert/Move（否则会 suffix 出第三份）。`to` 被用户删掉后，下次列举发现 dest 不存在则解除 block。 |
| `dest` 在 Move 中被别人创建 | 失败，suffix 再试（未 poisoned 时） |

### 撤销（协议写死：只追加，不改历史行）

jsonl **一行一条**，字段表：

| 字段 | 类型 | 含义 |
|------|------|------|
| `v` | number | 协议版本，恒为 `1` |
| `id` | string | 本行 id。`op=move` 新建；`op=undo` 填所撤销那条 move 的 `id` |
| `ts` | string | RFC3339 |
| `op` | string | `move` 或 `undo` |
| `from` | string | 源绝对路径（move 时的 from；undo 时仍记录原 move 的 from） |
| `to` | string | 落点绝对路径 |
| `size` | number | 字节 |
| `source_id` | string | 归档源 id |

**可撤销的上一条** = 倒读时遇到的第一条 `op=move`，使得文件中 **不存在** 任何后续行满足 `op=undo && id == 该 move 的 id`。第二次「撤销上一次」不会再命中同一条 move。

步骤：

1. 找到上述「可撤销的上一条」move。没有则菜单灰。
2. 若 `to` 不存在：仍追加 `op=undo`（避免反复命中），WARN「文件已不在落点」。
3. 若 `from` 已存在：不覆盖，不追加 undo，失败提示「源位置已有文件」。
4. 否则 `MoveFileExW(to, from, MOVEFILE_COPY_ALLOWED)`。成功则追加 `op=undo`（`id` = 那条 move 的 id）。

截断：启动时若文件 > 2 MB 或 move 行超过 `undo.capacity`（默认 200），从尾部保留最近 200 条 `op=move` **以及 `id` 指向这些 move 的全部 `op=undo`**。禁止「只留 move 而丢掉对应 undo」——那会让已撤销的 move 再次变成「可撤销」。

### 空状态

没有 undo 记录时菜单项灰。

### 边界

- 只读源文件：Move 同卷通常成功（rename）；跨卷 copy 保留只读属性。
- 打开中的文件：功能 1 应挡住；若仍撞 sharing violation，按失败重试。
- `from` 与 `dest` 同一路径：classify 已禁止 dest 在源内；再防御一层，相等则拒绝。

## 与总体架构的接口

```rust
pub struct JournalRecord {
    pub v: u8,                 // 1
    pub id: String,            // move：新 id；undo：被撤销 move 的 id
    pub ts: String,            // RFC3339
    pub op: JournalOp,         // Move | Undo
    pub from: PathBuf,
    pub to: PathBuf,
    pub size: u64,
    pub source_id: String,
}

pub fn execute_move(cfg: &Config, f: &FileSnapshot, p: &Placement) -> Result<JournalRecord, ExecError>;
pub fn undo_last(cfg: &Config) -> Result<JournalRecord, UndoError>;
```

## 关键实现要点

- 路径一律 UTF-16 Win32 API，不用 Rust `std::fs::rename` 的跨卷限制（Rust rename 跨卷失败）。直接 `MoveFileExW`。
- 冲突探测：`GetFileAttributesW != INVALID` 视为存在。
- 写 undo：`OpenFile` 追加，一行 JSON + `\n`，然后 `FlushFileBuffers`。崩溃最多丢最后一次记录，不损坏上一行。
- 不在 RAM 保留全部记录。

半失败清理伪代码：

```text
ok = MoveFileExW(from, to, COPY_ALLOWED | WRITE_THROUGH)
if ok:
  if exists(from) && exists(to):   // 跨卷删源失败
     if DeleteFileW(to):
        return Err(CopyLeftSource) // 源还在，无 undo
     else:
        poison + block(from,to)    // 双份；禁止再 Move
        return Err(SplitCopy)
  append op=move
else:
  if exists(to) && !exists(from):  // 不该发生：文件已在落点
     append op=move
  else if exists(to) && exists(from):
     if not DeleteFileW(to):
        poison + block(from,to)
     return Err(...)
```

## 验收标准

1. 同卷 Move：ADS `Zone.Identifier` 仍在 dest（可用 `fsutil` 或 `CreateFile(path:Zone.Identifier)` 测）。
2. 预置 dest 同名：结果为 `foo-1.ext`，原 dest 未改。
3. skip 策略：源文件仍在，dest 未新建。
4. undo：文件回到原路径，源空。紧接着再点一次「撤销上一次」不得把同一条 move 再执行一遍。
5. undo 时源已有同名：不覆盖，文件仍在落点，不追加 `op=undo`。
6. 模拟跨卷（若测试机能建 VHD 或用 SUBST）半失败：不出现「源和 dest 各一份」的成功 `op=move`。`DeleteFile(dest)` 也失败时写入 `blocked`，不会再 suffix 出第三份。
7. 连续 5 次 sharing violation：ERROR 日志，文件留源且留在候选；关闭占用进程后锁探测成功，`attempts` 清零，随后 Move 成功。

## 本功能开放问题

无（冲突默认 suffix 已定，见 KD-8）。

---

