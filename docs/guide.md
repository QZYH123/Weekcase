# 从零理解 Weekcase

这份文档讲 **这个项目在干什么、为什么这样干、代码怎么串起来**。读完应能自己在脑子里走完一条文件的一生，并知道去哪个模块改哪一类问题。

行为以 [design.md](design.md) 和 [features/](features/) 为准。本文是讲解，不是规格；冲突时信规格。

建议读法：第 1–4 节建立心智模型，第 5–10 节把模型钉到代码上，第 11 节当地图用。不必一次读完实现细节。

---

## 1. 它是什么

Windows 的「下载」和「截图」会堆成垃圾场。浏览器把安装包、PDF、压缩包全丢进 Downloads；Win+PrtSc 把 PNG 丢进 Screenshots。人不会每天收拾。

Weekcase 是一个 **Windows 11 托盘小工具**：盯着这两个顶层目录，等文件写完、也过了你还能用的窗口之后，按固定规则 **移动** 到本机归档文件夹。搬错可以撤销。

默认归档长这样：

```text
文档\Weekcase\
  Downloads\
    Images\  Documents\  Archives\  Audio\  Video\  Installers\  Other\
  Screenshots\
    2026-08\
    2026-09\
```

下载按扩展名分类型；截图按 **文件创建时间的日历月** 分箱，不按 ISO 周。产品名叫 Weekcase，组织方式却不是「按周一卷」——名字不锁定模型。

它平时没有窗口。装上或解压后托盘常驻，默认开机启动。第一次打开会确认归档根；**第一次不会把已经堆着的旧文件搬走**。旧的要自己点「整理现有文件」。

约束写死在产品层：

- 只在本机工作：零网络，无云同步，无自动更新
- 不读文件内容，不做规则引擎
- 单实例；第二个进程立即退出
- 需要 Windows 11（x64 与 ARM64），不支持 Windows 10

卸载或删掉程序 **不会** 删除已经归档走的文件。归档是用户数据，程序只是搬运工。

---

## 2. 它故意不是什么

同类软件里，File Juggler / DropIt / Hazel 是 **规则引擎**：用户写「如果扩展名是 pdf 且大于 10 MB 就复制到 D:」。Windows Storage Sense 是 **删文件腾空间**。PowerToys 没有这项能力。

Weekcase 选的是第三条路：**归档器**。规则内置、动作只有 Move、界面只有托盘。做规则 IDE、读内容 / OCR / AI、默认监视桌面、递归整盘、复制/删除/压缩/上传、插件钩子、重复文件清理，都会把它做成另一条产品。

这不是「功能少所以简单」，而是边界。分类表、冷静期、冲突策略都可以改配置；「用户编写谓词」不行。读代码时如果发现自己在加「通用规则语言」，就已经走出项目了。

---

## 3. 用户眼里的一生

先从外面看一遍，后面每一节都是在拆这一段。

1. **第一次启动。** `config.toml` 不存在。弹出对话框：监视哪两个 Known Folder、归档到哪、下载大约留 7 天、截图几十秒、不会自动清旧文件、会随登录启动。路径落在 OneDrive 下会警告。点「开始」才写配置和 `first_run_at`；点「退出」什么都不留。
2. **托盘常驻。** 无主窗口、无控制台。菜单：暂停、撤销上一次、整理现有文件、选择/打开归档目录、打开日志、重新加载配置、开机启动、退出。
3. **新文件出现。** 浏览器把 `report.pdf.crdownload` 写成 `report.pdf`，或系统把截图放进 Screenshots。Weekcase **看见** 它，但还不搬。
4. **等到能搬。** 下载默认约 7 天（刚下的安装包还能双击）；截图约 20 秒。到龄之后还要确认：尺寸和修改时间连续不变，并且没有进程以写入方式开着它。
5. **分类并移动。** 下载按最后一个点后面的扩展名进 `Downloads\{bucket}`；截图源里的文件一律当截图，进 `Screenshots\{yyyy}-{mm}`。同名默认变成 `foo-1.pdf`，永不覆盖。
6. **能撤销。** 托盘「撤销上一次」把文件搬回源路径。这是 Weekcase 自己的日志，不是资源管理器的 Ctrl+Z。

「整理现有文件」是另一条入口：暂时不管首次运行时间和冷静期，专门收当前顶层的旧文件。一次最多进队 256 个，多了再点一次。自动周期扫描 **不会** 去收 `created < first_run_at` 的存量——否则第一次启动就会把几年来的 Downloads 清空。

---

## 4. 三个必须分开的时间

这是整个系统的钥匙。把它们混成「等一会儿再搬」，后面所有设计都会看不懂。

| 名字 | 默认值（下载 / 截图） | 回答的问题 | 在哪判定 |
|------|----------------------|------------|----------|
| **冷静期 `min_age`** | 7 天 / 20 秒 | 人是不是还可能马上用它？ | 进候选表 **之前** |
| **沉降 `settle`** | 15 秒 / 8 秒 | 文件是不是已经写完？ | 已经在候选表里，T2 每秒采样 |
| **锁探测** | 能以只读共享打开 | 还有没有人握着写句柄？ | 沉降的一部分，Move 前再挡一次 |

### 冷静期不是「写完」

浏览器下载一个 ISO，创建时间大约是点「保存」的那一刻。文件可能几分钟就写完了，但用户接下来就要双击安装。如果写完立刻搬走，Downloads 里找不到安装包。所以下载默认留 604800 秒。截图没有这个「还要打开用」的窗口，20 秒够让截图工具写完、标注完。

未到龄的文件 **不进内存候选表**，就在磁盘上待着。到点靠周期扫顶层目录收进来（下载 60 秒一次，截图 10 秒一次）。这里有一个刻意的否定：

**不用 watermark 把「还太年轻」记成已处理。** watermark 是「我扫到这里了，更早的不再管」。如果把年轻文件标成已处理，7 天后它永远不会被收。年轻文件的正确状态是「我看见了，但现在不管」——而「不管」的实现是 **根本不记**。

Windows 复制文件常常会刷新 creation time；浏览器下载通常 creation ≈ 开始下载。若 creation 在未来，代码把它当成 mtime，避免时钟错误让文件永远到不了龄。

### 沉降不是冷静期

到龄之后，文件仍可能在变：下载器还在写、杀毒在扫、截图工具还开着 PNG。沉降看两件事：

1. `size` 和 `mtime` 连续 `settle_secs` 不变
2. 锁探测通过：`CreateFileW` 以 `GENERIC_READ` + **只共享读**（`FILE_SHARE_READ`）打开。若另有进程握着写句柄，这次打开会失败，本轮不算稳定

还有一条空文件规则：创建后 60 秒内的 0 字节文件不视为稳定。浏览器常先建空文件再填内容。60 秒后仍是 0 字节且稳定，则允许搬走——有人确实会下载空文件。

### 为什么分三层，而不是一个超时

一个「等 7 天再搬」解决不了「7 天后文件还在被写」；一个「尺寸稳定 15 秒就搬」会把刚下完的安装包立刻拿走。三层各挡一类事故。实现上它们也不在同一层：`min_age` 是监视线程的入场券，`settle` 和锁探测是稳定线程的循环。

---

## 5. 四个核心对象

代码里反复出现的不是「文件夹」和「规则」，而是下面四个。把它们当领域词汇。

### 源 Source

一个被监视的顶层目录，加上它的语义。默认两个：

- `id = "downloads"`，`kind = downloads`，路径默认 Windows Known Folder「下载」
- `id = "screenshots"`，`kind = screenshots`，路径默认 Known Folder「截图」

`kind` 决定分类方式，不是装饰字段。截图源里的 `foo.zip` 也当截图走月模板——**源语义优先于扩展名**，避免截图目录被拆散。第三条源可以配，但 v1 只能是这两种 kind，且必须写绝对 `path`。没有 `Custom`。

源只处理 **顶层普通文件**。`Downloads\foo\bar.pdf` 不看、不搬。文件夹本身不搬。这是内存和复杂度的硬约束：候选表最多 256 条，不能把目录树留在内存里。

### 候选 Candidate

「已经到龄、等着被判定稳定并搬走」的内存记录。字段里真正驱动行为的是：

- `last_size` / `last_mtime` / `created`：沉降采样
- `stable_since`：尺寸开始不变的时刻；满 `settle_secs` 才 ready
- `attempts`：Move 失败次数，上限 5，退避 5s / 15s / 60s
- `poisoned`：这条路径有结构性问题（落点在源里、跨盘双份等），不再自动搬，直到 Rescan 或对端消失

`FileSnapshot` 是候选的只读拷贝，给分类和 Move 用。**没有跨线程的「已稳定队列」**。稳定线程扫同一张表，拷出快照，放下锁，再搬。

### 落点 Placement

分类的纯函数结果：`dest_dir` + `dest_name` + `bucket`。`dest_name` 等于源文件名，不做美化。`dest_dir` 此时可以还不存在，执行器会建。分类本身无 IO。

### 撤销记录 JournalRecord

一次成功 Move 追加一行 `op=move`。撤销成功再追加一行 `op=undo`，**id 指向那条 move**。日志只追加，不改历史行。详见第 9 节。

---

## 6. 进程怎么活着

```text
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

### 单实例

`CreateMutexW(..., L"Local\\Weekcase.SingleInstance")`。第二个进程发现 `ERROR_ALREADY_EXISTS` 立刻以 0 退出，不把命令转给第一实例。`Local\` 前缀表示当前用户会话，不是机器全局。

### 为什么是三个线程，事件不另起进程

每个文件事件再起一个进程，空闲内存会炸。目标是干净 Win11 虚拟机上空闲 Working Set **stretch 12 MB、发版硬上限 20 MB**。所以：

- 禁止 Electron / WebView / 常驻 Python
- 候选表有上限
- T1 绝不在监视回调里搬文件
- T2 搬文件前必须放下候选表的锁——跨盘 copy 可能很久，不能把监视卡住

暂停只停 Move，不丢候选，T1 照样 upsert。恢复后已稳定的项下一秒就搬，不必等 60 秒列举。

### 启动顺序（Windows）

`main` 并不立刻开 T1/T2。顺序是：

1. 解析路径（便携或 `%APPDATA%` / `%LOCALAPPDATA%`）
2. 抢单实例锁
3. 没有配置则首次对话框；有则读 `config.toml`
4. 初始化滚动日志、加载 `state.json`、压缩过长的 `undo.jsonl`
5. 应用开机启动注册表
6. **T0 进入消息泵后立刻解析 Known Folder 并启动 T1/T2**；15 秒后再解析一次并重建监视

立刻启动是为了手动打开时截图不用再空等一轮。15 秒（`KF_DELAY_MS`）是给 OneDrive / 外壳的 Known Folder 重定向时间：登录瞬间 `FOLDERID_Downloads` 可能还指向旧位置。开机启动用 HKCU Run 键，不写 HKLM，不要求管理员。进程内补一次解析，而不是再注册一个计划任务。

Linux 上没有托盘：启动后立刻开 T1/T2，用来跑不依赖 Win32 的单测。产品二进制需要 Windows + MSVC。

`#![windows_subsystem = "windows"]` 让 exe 不带控制台。出错用 MessageBox，不是 stderr。

---

## 7. 怎么看见文件

监视要解决的不是「列出目录」，而是：**文件出现的瞬间通常还不能搬**，但 7 天后还得找得到它。

### Known Folder，不要自己拼路径

Windows 把「下载」「文档」「截图」做成系统身份（GUID），不是 `%USERPROFILE%\Downloads`。OneDrive 挂钩后，Known Folder 已经是重定向后的真实路径。代码用 `SHGetKnownFolderPath(..., KF_FLAG_DONT_VERIFY)`：截图文件夹尚未创建时不要顺手建出来。

源目录不存在（尤其是 Screenshots）时进程继续跑，每 60 秒试一次打开。映射盘 / UNC / 网络盘直接拒绝。U 盘可以，弹出就停监视，之后再试。

源本身若是 reparse（junction / OneDrive 挂钩）：先带 `OPEN_REPARSE_POINT` 探测，再 **跟随** 拿到最终路径，对最终路径做 denylist，监视最终路径。不能因为「是 reparse」直接拒绝——Known Folder 被 OneDrive junction 挂钩是预期情况。

### 双通道：RDC + 周期列举

**ReadDirectoryChangesW（RDC）** 是「目录变了告诉我」。Weekcase 用非递归（`bWatchSubtree = FALSE`）、64 KiB 缓冲，关心文件名、尺寸、最后写入。只接受相对名里不含 `\` 的条目，从根上保证顶层。

RDC 会丢事件：缓冲打满返回 `ERROR_NOTIFY_ENUM_DIR`，或进程当时没在跑。所以还有第二条腿：

**周期顶层列举**：下载每 60 秒、截图每 10 秒，把目录里现在还在的顶层文件走一遍入场规则。这是冷静期能成立的原因——年轻文件当时不进表，7 天后列举才会 upsert。

启动时先列举一次。RDC 溢出时立刻再列举该源。

不用 USN Journal：要权限、太重。不用递归监视：产品范围就是顶层。

### 入场规则（`admit`）

一个路径要成为候选，必须同时满足：

1. 顶层普通文件，不是目录
2. 不是忽略项（见下）
3. 不是云占位（`REPARSE` / `RECALL_ON_DATA_ACCESS` / `RECALL_ON_OPEN`）
4. 不在 `state.json` 的 `blocked.from` 里
5. 不是被 skip 策略记下的路径（「整理现有」除外）
6. `created >= first_run_at`（「整理现有」除外）
7. 已经过了该源的 `min_age`（「整理现有」把这项豁免为 0）

RDC 的 ADDED / MODIFIED / RENAMED_NEW_NAME 会先进入 500 ms 去抖表，再走 `admit`。`report.pdf.crdownload` 被忽略；改名为 `report.pdf` 时，`RENAMED_NEW_NAME` 当作新候选，`first_seen = now`。文件在稳定前消失：从表里删掉，静默。

### 忽略什么

硬编码，配置可追加、不可删减安全项：

- 临时扩展名：`crdownload` `part` `partial` `tmp` `temp` `download` `opdownload` `!ut` `bc!` `filepart` `!qb` `qbmd` `aria2` `ytdl`
- 文件名：`desktop.ini` `thumbs.db` `.ds_store`；前缀 `~$`
- 路径：自己的 exe、便携 `data\`、`state.json` / `undo.jsonl` / 日志

未列出的临时名不靠 glob 引擎，靠沉降 + 锁探测。`weekcase.exe` 若被放进 Downloads 顶层，7 天后仍在原处。

### 候选表上限 256

只装已经到龄、等着搬的文件。满了：

- 未 ready 的可以挤掉最旧一条
- 全是 ready（或 poisoned）则拒绝新来的
- 托盘变成「有文件没进队，整理未完成」
- 下一轮列举再试；「整理现有」一次不保证收完，用户再点

暂停期间打满 256 个 ready 项，新到龄文件可以进不了表。恢复并搬走腾位后，列举能收回仍满足收件条件的文件。年轻文件全程不占槽——这是 `min_age` 放在 upsert 前的另一条理由。

`upsert` 对已在表中的路径返回 `Existing`，**不覆盖** `attempts` / `first_seen`。周期列举扫到同一文件时，失败计数必须保留。

---

## 8. 稳定之后：分类

T2 每秒一次。锁顺序写死：

1. **阶段 A，短锁：** 采样每个候选的尺寸/mtime，做锁探测，更新 `stable_since`，把 ready 的拷成 `FileSnapshot`，**放锁**
2. **阶段 B，无锁：** `classify` 然后 `execute_move`。跨盘 copy 期间 T1 仍可 upsert

`classify(cfg, snap, folders) -> Placement` 是纯函数，可在 Linux 上单测。

### 下载：最后一个点

`File.PDF` → `pdf` → Documents。`archive.tar.gz` → `gz` → Archives。不解析复合扩展名。无扩展名 → Other。大小写不敏感。

| bucket | 扩展名 |
|--------|--------|
| Images | png jpg jpeg gif webp bmp tif tiff heic heif svg |
| Documents | pdf doc docx xls xlsx ppt pptx txt md csv rtf odt ods odp epub |
| Archives | zip rar 7z tar gz tgz bz2 xz iso |
| Audio | mp3 wav flac aac m4a ogg wma |
| Video | mp4 mkv avi mov webm mpeg mpg wmv |
| Installers | exe msi msix appx cab |
| Other | 对不上的，包括没扩展名 |

不读魔数、不看 MIME。`setup.exe` 是 Installers，即使它其实是自解压图包。这是可预测性换准确性：打开归档目录应能凭扩展名找到文件，而不是凭内容猜。

### 截图：忽略扩展名表

`source_kind = screenshots` 时 `bucket = Screenshots`，只用 `screenshots_template`。`{yyyy}` `{mm}` 来自文件 **created** 的本地日历，不是归档发生时刻。8 月 31 日截的图，9 月 1 日才归档，仍进 `2026-08`。

Windows 上用 `FILETIME` → 本地 `SYSTEMTIME` 的年/月。不引入日期 crate。

### 模板

v1 只认 `{root}` `{bucket}` `{yyyy}` `{mm}`。没有 `{ww}`、没有 `{source}`。未知 token → `BadTemplate`：候选移出表，ERROR 限流 1 次/分钟。修好模板后「重新加载配置」或 Rescan 再入。

默认：

- `{root}/Downloads/{bucket}`
- `{root}/Screenshots/{yyyy}-{mm}`

`{root}` 默认 `文档\Weekcase`。分类函数不拒绝 OneDrive 路径；警告发生在选根的时候。

落点若等于某源、位于某源之下，或落在盘符根 / 用户配置根 / Windows / Program Files / ProgramData：`DestInsideSource`，候选标 `poisoned`。改 root 后 Rescan 清 poisoned。非法文件名（空、`.`、`..`、含 `\` `/`）移出候选，文件留源。

---

## 9. 移动、冲突、撤销

动作只有 `MoveFileExW(from, dest, MOVEFILE_COPY_ALLOWED | MOVEFILE_WRITE_THROUGH)`。永远不传 `MOVEFILE_REPLACE_EXISTING`（压缩自己的 `undo.jsonl` 是唯一例外）。

### 同盘与跨盘

同一卷上，Move 是改名：很快，NTFS 备用数据流（含浏览器下载的 `Zone.Identifier`）还在。跨卷时 Windows 内部变成 Copy + Delete 源。Rust 的 `std::fs::rename` 跨卷会失败，所以直接打 Win32。

成功的定义不只是 API 返回真：

- `from` 不存在，`dest` 存在
- 追加 `op=move`
- 尺寸对不上只打 ERROR，不回滚（文件已经在落点）

### 冲突

默认 `collision = suffix`：`foo.pdf` → `foo-1.pdf` → … → `foo-99.pdf`。99 仍冲突则失败，不覆盖。后缀插在最后一个点之前；没有扩展名就加在末尾。

`collision = skip`：不移动，路径写入 `state.json` 的 `skipped`，不占 attempts。周期列举不会再收，直到 RDC 报告 MODIFIED（从 skipped 去掉）或用户点「整理现有文件」。同样禁止用 watermark 把 skip 当成「已处理时间界」。

### 失败怎么活

| 情况 | 行为 |
|------|------|
| 共享冲突 / 拒绝访问 | `attempts++`，留在候选。满 5 次后本 tick 不再搬；锁探测一旦成功，**attempts 清零** 再允许 |
| 磁盘满 | 停这一轮 Move，attempts 不空转，日志限流 |
| 跨卷 copy 成功但源没删掉 | 删 dest。删成功 = 源还在、无 undo。删 dest 也失败 = **双份**：poison + `blocked[{from,to}]`，禁止再对 from upsert/Move，否则会 suffix 出第三份。用户删掉 `to` 后，下次扫描发现 dest 不在就解除 block |

`blocked` 最多 64 条，`skipped` 最多 256 条，超出丢最旧的。

### 撤销协议

`undo.jsonl` 一行一条 JSON。字段：`v=1`、`id`、`ts`（RFC3339）、`op`（`move`/`undo`）、`from`、`to`、`size`、`source_id`。

**可撤销的上一条** = 倒读时遇到的第一条 `op=move`，使得后面没有任何 `op=undo` 带同一个 id。第二次「撤销上一次」不会再命中同一条。

步骤：

1. 没有可撤销的 move → 菜单灰
2. `to` 不存在 → 仍追加 `op=undo`（避免反复命中），警告「文件已不在落点」
3. `from` 已存在 → 不覆盖，不追加 undo，提示「源位置已有文件」
4. 否则把文件从 `to` 搬回 `from`，成功则追加 `op=undo`

这不是「改那一行的 op」。历史只追加。崩溃最多丢最后一次记录（写完 `FlushFileBuffers`），不损坏上一行。不在 RAM 里留全部记录。

启动时若文件 > 2 MB 或 move 行超过 200：从尾部保留最近 200 条 move，**以及 id 指向这些 move 的全部 undo**。只留 move 丢掉对应 undo，会让已撤销的 move 再次变成「可撤销」。

撤销走 T2 的 `ExecCmd::UndoLast`，带 oneshot 回复给托盘。Move 期间不持有候选锁；撤销也不该和监视抢。

---

## 10. 首次运行、暂停、整理现有

三类事故要在产品层挡住，而不是靠用户小心：

1. 第一次打开就把几年来的 Downloads 全部搬走
2. 正在下大型 ISO / 正在连拍时，只能退程序才能停
3. 归档根指到 Downloads 里面，搬完再监视落点，形成循环

### `first_run_at`

写在 `state.json`，RFC3339 UTC，只戳一次。之后所有自动收件都要求 `created >= first_run_at`。进程退出再启动，Downloads 里的旧 PDF 仍然不走；启动之后新放入的文件才进入 7 天倒计时。

首次对话框点「退出」不写配置。没有 `config.toml` 就是首次；不是「state 里有没有时间戳」。

### 暂停

`Arc<AtomicBool>`，并写回 `general.paused`。T1 仍 upsert，T2 仍更新稳定字段，只跳过 execute。未到龄的下载暂停期间照样不进表。256 上限仍然只约束已到龄项。

### 整理现有文件

确认框之后发：

```text
WatchCmd::Rescan {
    source: None,
    include_existing: true,
    min_age_override: Some(Duration::ZERO),
}
```

这是领域里 **唯一** 的监视命令形状（再加上 `Shutdown`）。暂停不是 WatchCmd。Rescan 会清 poisoned，让改过 root / 模板的文件有第二次机会。

一次最多进队 256。300 个旧 PDF 要点两次。自动周期列举不会替你收完 `created < first_run_at` 的剩余存量。

### 选归档根

首次对话框和托盘「选择归档文件夹」走同一套 `IFileDialog` + 同一套 denylist。拒绝：源内部、盘符根、Profile 根、Windows、Program Files、ProgramData、UNC、网络盘。OneDrive 不拒绝，但必须警告「归档会上传到云端」。

---

## 11. 代码地图

仓库很小，模块边界按管道切，而不是按「Windows 封装层」。

| 文件 | 职责 | 建议阅读深度 |
|------|------|----------------|
| `src/main.rs` | 启动、单实例、首次运行分支、把通道交给托盘 | 扫一遍启动顺序 |
| `src/paths.rs` | 便携 vs Roaming/Local | 短，先读 |
| `src/config.rs` | TOML、默认值、补丁写回（暂停/开机/root 不重写整文件） | 默认常量和 `SourceConfig` |
| `src/state.rs` | `first_run_at` / `blocked` / `skipped` / `overflow_unacked` | 语义，不必抠日期算法 |
| `src/known_folders.rs` | Known Folder 解析、denylist、路径比较 | denylist 规则 |
| `src/candidate.rs` | 候选结构、256 上限 upsert | **必读** |
| `src/watch.rs` | 入场、忽略、RDC、周期列举、去抖 | **必读** `admit`；RDC/IOCP 实现可后看 |
| `src/stabilize.rs` | 1s tick、沉降、锁探测、调用 classify/execute | **必读** 锁顺序和 `apply_outcome` |
| `src/classify.rs` | 扩展名表、模板、年月 | 表和 `expand_template` |
| `src/execute.rs` | Move、冲突、半失败、撤销动作 | **必读** `settle_after_move` |
| `src/undo.rs` | jsonl 协议、compact、last_undoable | **必读** 协议，compact 次之 |
| `src/tray.rs` | 消息泵、菜单、15s 启动管道、开机启动 | 菜单命令如何变成 WatchCmd/ExecCmd |
| `src/settings.rs` | 首次对话框文案与提交 | 和 tray 共用选根校验 |
| `src/log_init.rs` | 1 MiB × 3 滚动日志 | 略 |

测试按规格钉行为，不按实现细节：

- `tests/classify.rs`：落点、源内拒绝、截图按 created 月份
- `tests/collision.rs`：`foo-1.pdf` 与 skip
- `tests/undo_parse.rs`：可撤销定义、compact 必须带着对应 undo
- `tests/config.rs` / `testdata/config_minimal.toml`：未知字段忽略、缺省填充
- `tests/win_move.rs`：Windows 上的真实 Move

Linux 可跑分类、文件名、配置、候选表这类单测。Move / RDC / 托盘需要 Windows。

### 数据放哪

| 文件 | 普通安装 | 便携（exe 旁有 `portable.ini`） |
|------|----------|--------------------------------|
| `config.toml` | `%APPDATA%\Weekcase\` | `exe_dir\data\` |
| `state.json`、`undo.jsonl`、日志 | `%LOCALAPPDATA%\Weekcase\` | 同上 `data\` |

`portable.ini` **内容被忽略**，有这个文件即开启便携。归档根仍按配置，不跟着 `data\` 走。卸载不删归档。

配置用手工补丁改单个键，为的是保住注释。未知字段反序列化时丢掉，正向兼容。

### 安全底线（实现层对照）

- 只动配置里启用的源
- 落点不能落在源里面
- 拒绝盘符根、Profile 根、Windows、Program Files、UNC、网络盘
- 正在写的不搬；临时下载后缀直接忽略
- 只处理顶层；文件名带 `\` `/` 的跳过
- 硬忽略自己的 exe 和便携 `data\`
- 源侧跳过 OneDrive 占位；归档根在 OneDrive 下只警告不改默认算法
- 不要求管理员，不写 HKLM，无网络代码

### 有意写得很短的部分

- **日期换算**（Howard Hinnant civil-from-days）：为了不引入 chrono。知道「created → 本地年/月」即可
- **IOCP / overlapped RDC**：知道「T1 专跑完成端口、回调里只 upsert」即可；关闭句柄时若还有 pending I/O 会 leak 缓冲防 UAF
- **TOML 补丁、日志滚动、图标嵌入**（`build.rs` + `assets/weekcase.rc`）：基础设施，不参与领域
- **打包**（`scripts/package.ps1`）：zip 根上放 `weekcase.exe` + `portable.ini`
- **内存测量**（`scripts/measure-rss.ps1`）：发版前在干净虚拟机跑，**不要**把 20 MB 配成 CI 失败

---

## 12. 一条文件的完整路径（串起来）

把 `Chrome` 刚下完的 `report.pdf` 走一遍：

1. 浏览器先写 `report.pdf.crdownload`。RDC 报到 T1，扩展名在忽略表，丢弃。
2. 改名为 `report.pdf`。去抖 500 ms 后 `admit`：是顶层文件，但 `now - created < 7 天`，**不进候选表**。
3. 之后六天，周期列举每次都看到它，每次都因 min_age 拒绝。内存里没有它。用户可以双击安装、删除、再下载。
4. 第 7 天某次 60 秒列举，`admit` 通过，`upsert` 插入 Candidate，`stable_since = None`。
5. T2 采样：若 size/mtime 在变，重置 `stable_since`。连续 15 秒不变，且 `CreateFileW` 只读共享成功，则 `stable_since` 有值。
6. 再过满 15 秒、未暂停、attempts 允许：进入 ready 快照，放锁。
7. `classify`：Downloads + `.pdf` → `{root}\Downloads\Documents\report.pdf`。
8. `execute_move`：建目录，若已有 `report.pdf` 则试 `report-1.pdf`，`MoveFileExW`，追加 undo。
9. 短锁把该路径从候选表删掉。托盘「今日已归档」+1（进程内计数，重启清零）。
10. 用户点撤销：倒读 jsonl 找到这条 move，把文件从 Documents 搬回 Downloads，追加 `op=undo`。

截图同一条管子，参数不同：min_age 20 秒、settle 8 秒、列举 10 秒、分类忽略扩展名、落点按 created 的月。

「整理现有」从第 4 步插进去：`include_existing + min_age=0`，旧文件立刻有资格进表，后面仍要沉降和锁探测。

---

## 13. 读完之后你应该能回答的问题

如果下面都能不翻代码答出来，这份文档的目的就达到了：

1. 为什么刚下载的 exe 不会立刻从 Downloads 消失？
2. 为什么 7 天这个数字不能靠「扫过就打戳」实现？
3. 暂停时，已经到龄的截图和还没到龄的 ISO，分别在哪？
4. 为什么截图目录里的 zip 不进 Archives？
5. 同盘移动和跨盘移动失败时，怎样避免「源和目标各一份」被当成成功？
6. 点两次「撤销上一次」，第二次为什么不会把同一文件再搬回去？
7. 第一次启动，Downloads 里躺了三年的 PDF 为什么还在？怎样让它们走？
8. 要把落点从「按类型」改成「按月」，应该动配置的哪一行，还是动产品边界？

第 8 题的答案：下载模板改成带 `{yyyy}-{mm}` 是配置；给用户一个规则编辑器是另一条产品。Weekcase 停在前者。
