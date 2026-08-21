# Unified 模式待办

本文件记录 `edb1be5` 之后 Unified 渲染后端的剩余工作。设计文档见会话产出的《Unified 模式设计》（单一 VTE + 覆盖层 block chrome）；此处只记可执行项、验收标准和已知债务。

跨仓：forge 是参考实现，anvil 逐项镜像。两仓当前进度不同，见「anvil 镜像」一节。

## 已完成（不再重复）

- Phase 0（两仓）：reader 闭包拆为 `ReaderCtx` 生命周期引擎 + `RenderBackend` trait；forge `98b4f4b`、anvil `2b02f4e`。
- forge dispatch 测试线束：`a6b4dc8` + `9bfc363`，1260 测试，16 组变异验证。
- Phase 1 增量 1a：`edb1be5`，`TerminalMode::Unified` + 无 chrome 的 `UnifiedBackend`，六个探针缺陷已修。

## P1 · 增量 1b（顺序由 1a 实测确定，不要跳步）

- [x] **zone 表升为后端查询 `records()`**。find/export/history/palette 已统一读取 `RenderBackend::records()`，不再靠 `unified_zones` 或空的 Block widget 集合猜模式；Unified `Ctrl+F` 搜索一整个持久 VTE。
- [x] **`finalize_block` 签名瘦身**。引擎只构造 `CompletedCommandRecord`；Block 通过 memoized 惰性访问器物化渲染负载，Unified metadata-only 路径不读取输出。journal 与 Agent/output observer 也先做 capability/interest 判断。
- [x] **A→C 标记注入器**。128-bit pane nonce、全局 zone id、每次 live feed 重申、C 后无条件 close、idle A 复用及 pre/post-C alt-screen 恢复均已接线；注入字节不进入 prompt/output capture。
- [x] **探针驱动的 chrome（功能核心）**。严格校验 canonical URI + nonce + active authority；逐可见行一次探针，可信 predecessor span 可从长 zone 中段续画；gutter/分隔线/completed badge 与宽字符安全的空白检测已落地。真实 VTE 证明 `check_hyperlink_at` 接受 widget 坐标，因此探针使用 content origin + cell center，不能预先减 CSS padding。行坐标只由可见 canonical marker 校准，无法证明时 fail closed。
- [x] **chrome 性能/resize 验收**。`FORGE_UNIFIED_CHROME_STATS` 门控的 draw 计时（Drop guard 覆盖所有 early-return 出口）实测：70 zone、21 可见行、Xvfb 软渲染下滚动 p95 = 328-377µs/draw（p50 243-315µs，max<600µs），约为 60fps 帧预算 2%；探针 O(可见行) 线性，更高窗口无风险。列宽变化（侧栏切换往返）无错位残留：jsh 收到 SIGWINCH 重绘 prompt 即触发 idle-A 复用，chrome 立即重新标定，无需等下一条命令。VTE 0.58+ 的 `rewrap-on-resize` 已废弃恒为 TRUE。发现两个非 chrome 缺陷的边缘：见下。
- [x] **marker authority 上限与安全淘汰**。pending 也计入硬上限 256，并同时服从更小的 `max_visible_blocks`；ED3 仅退休已证明完整位于 live grid 上方的 completed span，RIS 全清 authority，natural trim/rewrap/config resize 经 row epoch + quarantine 硬门失效。旧 URI、空白探针或晚到 completion 均不能复活已淘汰 authority。
- [x] **超限 zone 的快照寻址**。finalize 从引擎自有的 raw ring 取有界纯文本尾（每 zone 64 KiB、全局 4 MiB，按 id 存于 `UnifiedZoneStore`）。快照是可选卫星：淘汰只减字节不动记录，快照没了就重新诚实报告「无输出」。`truncated` 覆盖全部四个丢字节来源——尾部截断、finalize 前 ring 已回绕、ANSI 重放触到自身 grid 预算而中止、strip 后再钳位。尾部切割会跨过骑在切点上的转义序列但**绝不越过缓冲区末尾**，否则退回原始切割，避免一个终止于最后一字节的 OSC 把整份快照吞掉。导航因此多出一级：chrome 证明得了行就精确跳转，否则只读呈现快照，两级跨块面板都能到达。

## P2 · Unified 的已知缺口（1b 期间逐项关闭）

- [x] metadata 会话导出：JSON/Markdown 已基于 `records()` 导出 command/status/time/duration/cwd/background，并用 `output_available: false` 明确表示 Unified 没有伪造的空输出。
- [x] 带输出的会话导出：JSON 增 `output`/`output_truncated`，`output_available` 改为快照存在性；Markdown 用围栏输出并在截断时注明。无快照的记录**根本不写 `output` 键**（诚实缺席，而非空串）。
- [x] Unified `Ctrl+F`：按 native cursor 顺序优先扫描 viewport→tail，再 wrap 到旧历史；共享 4 MiB/时间预算。可信 ring bounds 暂失时由 VTE native cursor 返回真实的 `1+` limited result，屏上命中不会被很长的旧历史饿死。
- [x] per-zone output 搜索与精确跳转：`BackendRecordRef::output()` 现返回快照文本，`matching_record_ids`/`cross_block_search` 随之点亮。`record_search_target()` 仍**故意** fail closed（共享 VTE 无法为单条记录限定高亮范围），跳转改走新的 `scroll_to_record`/`can_scroll_to_record` 接缝；跨块面板据此启用行激活，并按 `Navigated`/`SnapshotView`/`LocationUnavailable` 分派。
- [x] 会话恢复（`5d15898`）：zone 文档持久化到 Block 历史文件的兄弟路径（stem 加 `-zones`，同样按 session 分文件），上限 64 zone / 4 MiB；预算不够时**先丢输出再丢记录**，所以重启至少留下命令与结果。恢复在 `start_history_load` 同步执行——必须早于 shell 首个 prompt，否则恢复的行会落在它本该领先的输出下面。回放是纯显示：字节直接进 VTE 不过 parser，故恢复的 zone 不会被记成本次会话执行过的命令；marker 用**新分配的 id**（持久化里根本不存 id），保住注入器的单调重放防御；每个持久化字段先剥离控制字节——历史文件是数据不是程序。
- [x] inline 卡片安置（`d83c453`）：底部**占位**停靠区落地（`notice_dock`，`block_scroll` 的垂直兄弟）。`RenderBackend::docks_inline_notices()` 决定卡片进文档还是进停靠区；Unified `supports_inline_notices()` 因此转为 true，agent/correction/palette 卡片不再被拒。GUI 实测：organism 卡片挂在底部、prompt 未被遮挡、表面由 71×21 缩为 71×17——每次开关一次 SIGWINCH，不是每张卡片一次。
- [x] organism：停靠区即其载体，Unified 下卡片正常出现（实测显示 juvenile · repo memory · no LLM）。
- [x] sticky 运行头 / jump FAB（`d83c453`）：两者都读同一个 `user_scrolled_up`，而 Unified 下外层滚动条从不移动。改由 live VTE 自己的 adjustment 驱动该标志（留一行余量），FAB 实测在向上滚动后出现；FAB 的点击本就会把 VTE adjustment 拉到底，无需改动。
- [x] 停靠区的挂载决策已钉住：抽出纯 `dock_mount_decision(parent, dock) -> DockMount{Append,Keep,Refuse}`，display-backed 测试覆盖三种父节点状态（变异验证：把 `Refuse` 改成 `Append` 会被抓）。真正需要 `TermView` 的只剩 GTK 管道（append/set_visible/relayout），与 `BlockBackend` finalize 链同属设计使然的盲区，保持 GUI 验证。
- [x] badge/分隔线头行归因（forge `19386f2`，anvil `5669477`）。根因不是注入器也不是 epoch：chrome 每行**只探列 0**，而 zone 的首行始于 shell 恢复写入的位置——tty 回显 ^C 之后、或上条输出没有结尾换行时，prompt 被画进某行中段，该行列 0 是 marker 关闭期间写的，探针落空，传播把它判给上一个 zone。修法是**至多加宽一行**，且三个 guard 缺一不可：必须是正上方那一行（循环遇 order 回退会 `continue` 且不更新 unowned 标志，`result.last()` 未必相邻）、该行不能已是某 zone 的已验证 head（已验证 head 能扛住 guest OSC 8 覆盖列 0，抢走它会**整段删除**其 span，那个 zone 就彻底没有分隔线）、且必须由 ring 证明该行列 0 之后确实带本 zone 的 canonical marker。三个 guard 各有变异测试把守。
- [x] 启动噪音（forge `c808b2a`，anvil `6d251ba`）。**与 jsh 无关**：噪音来自 rc wrapper bash，在它 exec 掉自己之前就打完了。wrapper 用继承来的 PATH 解析 `bash`，而从 `nix develop` 启动时排在最前的是 stdenv bash——该构建**没有可编程补全**（无 `complete` builtin，`progcomp`/`hostcomplete` 不是合法 shopt 名）。标准 `~/.bashrc` 会 source `/usr/share/bash-completion/bash_completion`，于是它的 65 条指令条条失败。改为优先解析系统交互式 bash（`/usr/bin/bash` → `/bin/bash` → `/run/current-system/sw/bin/bash` → PATH 兜底），PTY 实测 65 → 0 行。**同一函数还吞掉了 session id**：`bash -c CMD a b` 把 `a` 给 `$0`、其余给 `$@`，而命令串里写死了 shell 路径，所以追加的 `--session <id>` 变成 wrapper 自己的操作数（屏幕上那个 `--session:` 前缀就是这么来的），从未到达 shell。现在 shell 走 `$0`、id 经 `$@` 传入，实测 shell 回发 OSC 7770 带上了自己的 session id。**不在终端侧过滤首个 prompt 之前的输出**：那会同时藏掉真实的 rc 故障。
- [x] kitty 图片 v2（forge/anvil 已镜像实现）。Unified 现在在 reply-before-admit 边界内校验首块 `r=`/`c=`/`C=` 与最终块 cursor，按行写入 nonce-scoped placement marker，并用 VTE probe 驱动 GTK 图片层；无法可靠放置时才答 `ENOTSUP`。定标结论与实现不变量如下：
  - **定位靠探针，不靠 projection**。`ViewportProjection` 故意 fail-closed，且在 scrollback 到达容量后稳态恒为 `None`——靠它定位会让图片在产生它的命令整个生命周期里闪不见。实测 per-row OSC 8 URI **能扛住 scrollback 冻结与 rewrap**（5 行块在 99→52→137 列下始终按序报 `j=0..4`），所以 trim/rewrap 对定位**不需要任何失效规则**。URI 第二段必须是**每次放置的流水号**，绝不能用 kitty 的 `i=`（一个 id 可有多个 placement 且会被复用）；图片块的 closer 之后必须**立刻重申 zone opener**（实测不重申会静默解链同一 feed 里后续所有单元格）。
  - **必须预留行，且只能用 LF**。APC G 字节被 `on_apc_sequence` 吃掉、从不进 VTE，所以没有任何东西推进光标；CUD 不创建行，只有 LF/IND 延长 ring。行数**只能取 `r=`**：实测 `ceil(v/cell_height)` 会算错（40px/17 得 3，实际 5 行）。预留 `rows − 1` 个换行（kitty 让光标停在图片**最后一行**）。超出视口时**拒绝而非钳制**——悄悄钳制等于按客户端没要求的比例作画。决策点在 `kitty_feed`，不能在 `kitty_admit_pending`（后者不收 payload，且 reply-before-admit 是 trait 文档钉死的次序）。
  - **`Assembled` 不带 `r=`/`c=`/`C=`，且这些只出现在分块上传的第一块**（实测 chafa 的几何永远落在产出 `NeedMore` 的块上，产出 `Ready` 的那块字面就是 `Gm=0`），所以事后宽松重扫扫不到。新 arm 必须加在 `parse_command` 的**第二个** match，且宽松解析。注意 `forge/src/kitty_graphics.rs` 与 `jterm_core` 的同名文件**逐字节相同**但 forge 编译自己那份副本；anvil 其实**不需要** core 改动（`parse_command`/`Command::get`/`Assembled.id` 都是 pub，app 侧按 id 建旁表即可）。
  - **y 公式与一个藏在后面的行身份缺陷**。`m_fallback_scrolling` 默认开启，adjustment 值不按行量化，误差上界是**半个** cell（探针在带中心）。但只修 y 不够：同一个小数值在 `top_ring_row = floor(value) + row_offset` 处被下取整用作**行身份**，当小数 ≥ ½ 时与 band-0 探针实际返回的行差一，会错配 `observe_head`/`cache.heads`/badge 空白探测；更糟的是 `projection_from_calibration_anchor` 也下取整同一个值，在小数滚动位置标定会把**永久错误的 `row_offset`** 烧进整个 epoch。应统一算 `scroll_shift = (value*cell_height).round()` 与一个 `row_at_band(band)` 辅助。附带发现：`adjustment.lower()` 恒为 0，所以绘制期的 `current_ring_lower` 守卫与 `connect_changed` 的地板比较都是**死代码**，图片层的失效逻辑不能建在它们上面。
  - **overlay 放在 organism surface 之下**（不是 organism 与 chrome 之间——Unified 下 organism body 是它唯一的表面，实测被图片盖住会完全静默消失）。`gtk4::Fixed` + `overflow=Hidden` + `measure_overlay=false` + `clip_overlay=true`；`set_size_request` 的**两个维度都是必需的**（`can_shrink(true)` 下 Picture 测得 0×0，沿用 finished-block 那份配方的 `-1` 宽度会得到 0 宽不可见图）。诚实代价：`can_target(false)` 意味着拖选划过图片选中的是底下的空白格，Ctrl+F 也搜不到、导出近乎空行。
  - **生命周期锚在行、不在 zone**（zone 上限 256/200，而 5000 行 ring 装得下远多于此的命令的图片；反之一条命令也能在一个 zone 里发几百张图）。**只有 RIS 无条件全清**；ED3 必须**选择性**淘汰（复用 `UnifiedChrome::erase_scrollback` 已算好的 cutoff，全清会抹掉仍在屏上的图）；`reset_kitty_pipeline` 只该丢未 admit 的那张（Unified 的 `reset_active_surface` **故意不清屏**）；`scrollback-lines` 调**大**不销毁任何行。淘汰依据用模块已算出的 `ForwardTrim`/`UnprovenAtCapacity` 证明 + quarantine 标志，不用帧 LRU。`MountedImage` 必须像本模块其他行坐标一样带 `row_epoch`。
  - **alt screen**：前提与直觉相反——`set_alt_screen(true)` 把 chrome DrawingArea 设为不可见，绘制函数（行探针唯一所在）**根本不运行**，而图片是第四个兄弟 overlay 不是子节点，所以整个 alt-screen 期间没有任何东西会移动或隐藏它们。显式隐藏不是收尾润色，是唯一手段。退出半边必须照抄 `live_organism_alt_transition`：进入时设 desired=false **并**加 override，退出**只**清 override——「绝不让 rmcup 同步复活 TUI 之前的坐标」是代码里钉死的不变量。
- [x] **生命周期可信度**：完成来源与退出结果正交记录为 `shell_reported`、`journal_recovered`、`boundary_inferred`、`unknown`。缺 D 只在 shell 确认重获 PTY 前台时由 A 恢复，标为 degraded 且不补造 exit/end/duration；旧 zone v1 默认 unknown/incomplete，弱来源重放不升权。RIS 原子清除运行中 command/Agent 关联，下一 A 不会生成 phantom block。

## P2 · anvil 镜像

- [x] **锚点绕过修复**：submission/status/click/capture 共用 `SubmissionSurface::prompt_anchor`，策略与 backend switch 同源（Block rebase、Unified identity）。
- [x] **dispatch 测试线束**：`SubmissionSurface`、test openpty、建模 `RecordingBackend` 与 safe config 已落地；覆盖 effect/query 顺序、anchor、alt、clipboard/notify、kitty 与 verified-submission guard。
- [x] Phase 1 增量 1a 镜像：config/CLI/completions/settings/workspace 与单一 Unified backend 路由均已完成，受管远程仍强制 Block。
- [x] Phase 1 增量 1b 核心镜像：`records()`、metadata-only record、惰性 finalize、A→C marker、probe chrome、authority 淘汰、visible-first bounded `Ctrl+F` 与 metadata export 已按 Anvil 的 Relm4/Agent/journal 边界同步。
- [x] 会话恢复 + 底部停靠区镜像（anvil `c3b780a`）：zone 文档持久化/回放与 `notice_dock` 均已同步。anvil 侧差异：id 带进程命名空间且有 reserved 集合，故 `UnifiedBackend` 持有 `reserved_history_block_ids` 以便回放从同一分配器取 id；anvil 无 per-session 历史文件名，zone 文档就是 `<stem>-zones.<ext>`；卡片 gate 走 Relm4 的 `pane.terminal.supports_inline_notices()`。GUI 实测：停靠区令表面由 49×20 缩为 49×17，organism 卡片响应命令事件，重启后横幅与两个带 badge 的 zone 正常回放，向上滚动出现 jump FAB。
- [x] ED3/RIS 顺序镜像：pinned `jterm_core` 尚无 reset event，Anvil 在 core parser 之前用流式 ANSI-aware splitter建立 reset 边界；reset hook、原始 reset bytes、suffix 严格按线序执行，control-string lookalike 与跨 chunk 输入 fail closed。
- [x] 有界 zone 快照镜像（anvil `646e65d`）。两处 anvil 边界强制的偏离：(1) **不做 replay-abort 标志** —— anvil 的 `strip_ansi_with_clear_detect` 无 cell 预算、无跳出字节循环的 `break`，CUF 用 `.min(cells.len())` 钳位而非填充，既不会中止也不会放大（带预算的 `replay_visibility` 只喂布尔探针 `ansi_has_visible_text`），永假的标志是装饰性覆盖；改用「重放一旦获得预算就失败」的测试钉住这个前提。(2) **回绕标记不进 newtype** —— anvil 的 ring 是与 `ActiveBlock` 共享的裸 `VecDeque`，改 newtype 会波及五处；改为 `append_bounded_output` 返回是否丢字节（`#[must_use]`）+ 单一 `clear_live_raw_output` 拥有全部四个清空点 + payload 构造时捕获标记。
- [x] anvil 专属 blocker（审查发现，forge 无对应代码）：快照对话框的 slot 在 `force_close` 期间仍被借用。libadwaita 1.8.4 的 `force_close` **同步**发出 `closed`，其 handler 写同一 cell，于是在 C signal trampoline 里 panic 且无法 unwind → SIGABRT 整个工作区。默认路径可达（面板里连按两次回车）。修法是把 `take()` 提升为独立语句，与 `cross_block_search.rs` 既有写法一致。`clippy::significant_drop_in_scrutinee` **抓不到**这个（不把 `RefMut` 算作 significant drop，全仓只在一处 `Mutex::lock` 触发），所以保护只能靠代码注释陈述约束。

## P3 · 卫生与债务

- [x] forge：finalize 路径的 config 借用已收窄；widget 构造/高度估算/通知只接 owned snapshot 或提前复制的 scalar。
- [x] forge：`ui/config_apply.rs` 已在 per-view 循环前 clone config，不再跨 view 的 `borrow_mut` 持有 UiState config `Ref`。
- [x] forge：菜单方法内冗余的 `*_for_menu` 再克隆层已折叠——每个 `self.X` 曾被克隆两次（先进等价性证明用的中间局部，再进闭包），22 对合并为一次。anvil **本就没有**这一层（其提取保留 ctx 字段于原位，直接从 `self` 克隆），所以此项只涉及 forge。
- [ ] forge：`BlockBackend` 自身的 finalize 链与 `command_capture_anchor` 的 rebase 行需要真 VTE，测试套件够不到（设计使然），保持 GUI 验证。

## 方法论备忘（血泪换来的）

- 变异测试**必须**用私有 `CARGO_TARGET_DIR`，且不得并发跑：共享 target 会让另一进程的 mutated build 冒充"抓到的缺陷"，曾污染整轮结论；变异脚本里的 `git checkout --` 差点抹掉 1626 行未提交工作。用 `git worktree add --detach` 更安全。
- 新测试套件在信任前先做变异测试。第一版 dispatch 线束 1237 全绿却抓不住任何注入缺陷。
- 测试不得依赖开发者环境：旧线束通过是因为本机 `config.toml` 恰好把 cursor 设成了断言期待的颜色。
- 大改动前先做「只跑通、不出功能」的骨架增量：1a 花的代价换回六个纸面设计想不到的缺陷，其中一个会让 Block 用户的远程会话丢历史。
- **测试不得自证**：`zone_marker_bytes_never_enter_the_snapshot_capture` 初版自己往 ring 里塞无标记数据再断言 ring 无标记——生产接线怎么改它都绿。改为驱动 `ReaderHarness` 跑完整命令周期后，「删掉引擎 raw-output 捕获」这个变异才被杀死。判据：**这个测试能被哪个生产改动杀死？** 答不上来就是装饰。
- **契约字段要枚举全部信息丢失源**。`truncated` 初版只算尾部切割，漏了三处：ring 回绕、`strip_ansi` 触到自身 grid 预算而 `break` 掉剩余全部字节（重放不是过滤）、以及 strip 后的钳位。写下「任何未存活的字节都要报告」这种契约时，先把能丢字节的路径列全再实现。
- **默认路径可能不是你以为的那条**。jsh 下 journal 提交在 `finalize_block` **之前**物化 payload，所以 Unified 每条前台命令走的都是「已物化」分支——挂在 ring 分支上的 `dropped_front` 管线在生产中整个是死的，而小样本 GUI 测试恰好看不出来（小输出本来就该报 false）。
- 小样本 GUI 验证能证明「通路存在」，证不了「边界正确」。本轮 GUI 全绿的同时，对抗性审查在同一份代码里找出三条静默丢数据的路径；反过来审查也漏了「新功能的行 `activatable(false)` 根本点不动」这种只有真跑才撞得到的问题。两者都要。
- **镜像最危险的是没有对照的那部分**。anvil 侧唯一的 blocker（同步 `closed` 回调里的 RefCell 重入 → 进程 abort）出在 forge 根本不存在的代码上（forge 不跟踪 dialog slot）。镜像评审要专设一个「无对照代码」视角，别只做逐条比对。
- **别指望 lint 兜底再去验证**：`clippy::significant_drop_in_scrutinee` 看起来正对这个缺陷，实测零命中（不把 `RefMut` 当 significant drop）。先验证工具真的会响，再决定要不要依赖它。
