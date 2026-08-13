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
- [ ] **超限 zone 的快照寻址**。metadata 记录仍可供 palette/export/filter 使用，但 marker authority 退役后精确跳转目前 fail closed；尚未保存 bounded per-zone 输出快照。

## P2 · Unified 的已知缺口（1b 期间逐项关闭）

- [x] metadata 会话导出：JSON/Markdown 已基于 `records()` 导出 command/status/time/duration/cwd/background，并用 `output_available: false` 明确表示 Unified 没有伪造的空输出。
- [ ] 带输出的会话导出：仍依赖 bounded per-zone 快照。
- [x] Unified `Ctrl+F`：按 native cursor 顺序优先扫描 viewport→tail，再 wrap 到旧历史；共享 4 MiB/时间预算。可信 ring bounds 暂失时由 VTE native cursor 返回真实的 `1+` limited result，屏上命中不会被很长的旧历史饿死。
- [ ] per-zone output 搜索与精确跳转：metadata filter 可返回稳定 record id，但 `record_search_target()` 在 Unified 仍 fail closed 为 location unavailable。
- [ ] 会话恢复：当前跳过 Block 历史存取。设计方案是有界快照回放（最近 ~64 zone / 4 MiB 经注入器重新 feed），避免重启后空屏。
- [ ] inline 卡片安置：当前在 Unified 下诚实拒绝（agent/correction/palette 卡片）。设计方案是底部**占位**停靠区（VTE 的垂直兄弟节点，开关各触发一次 SIGWINCH），不是覆盖层——覆盖会精确遮住用户正在盯的 prompt 行。
- [ ] organism：`ascii_organism_enabled` + `OrganismMotion::Static` 组合下 Unified 无任何 organism 表面（卡片是它在该组合下的唯一载体）。需要在停靠区设计里给它位置。
- [ ] sticky 运行头 / jump FAB：Unified 下已不可达（1a 发现它此前只是借了卡片撑开的外层滚动条，属意外而非特性）。需要在覆盖层设计里重新接线。
- [ ] badge/分隔线头行归因边缘：^C 中断的 prompt 或带 ghost 建议的多行 prompt 经 idle-A 复用重绘后，相邻 zone 的 marker 首格可不落在 CWD 行，badge/分隔线随之下移 1-2 行（验收实测，非破坏、fail-visible）。修复方向：注入器在 idle 重申时把重开位置钉回 zone 首行，或 head 归因对"URI 首行是输出行"做 CWD-行回溯校验。
- [ ] jsh 启动噪音：Block 的每-prompt 清屏顺手吞掉了它，Unified 不清屏所以会永久留在屏上。判断是否需要 shell 侧或首-prompt 侧处理。另两个验收实测的 shell 侧现象一并考虑：SIGWINCH 重绘按 rewrap 前行数向上定位，会覆盖 rewrap 后的输出尾行；prompt 处滚轮被 jsh 鼠标上报吃掉（历史导航而非滚动视口，Shift+滚轮/Ctrl+Up 可绕过）。
- [ ] kitty 图片：当前 Unified 答 `ENOTSUP`（解析即弃）。v2 可用探针行锚定 overlay `Picture`。

## P2 · anvil 镜像

- [x] **锚点绕过修复**：submission/status/click/capture 共用 `SubmissionSurface::prompt_anchor`，策略与 backend switch 同源（Block rebase、Unified identity）。
- [x] **dispatch 测试线束**：`SubmissionSurface`、test openpty、建模 `RecordingBackend` 与 safe config 已落地；覆盖 effect/query 顺序、anchor、alt、clipboard/notify、kitty 与 verified-submission guard。
- [x] Phase 1 增量 1a 镜像：config/CLI/completions/settings/workspace 与单一 Unified backend 路由均已完成，受管远程仍强制 Block。
- [x] Phase 1 增量 1b 核心镜像：`records()`、metadata-only record、惰性 finalize、A→C marker、probe chrome、authority 淘汰、visible-first bounded `Ctrl+F` 与 metadata export 已按 Anvil 的 Relm4/Agent/journal 边界同步。
- [x] ED3/RIS 顺序镜像：pinned `jterm_core` 尚无 reset event，Anvil 在 core parser 之前用流式 ANSI-aware splitter建立 reset 边界；reset hook、原始 reset bytes、suffix 严格按线序执行，control-string lookalike 与跨 chunk 输入 fail closed。

## P3 · 卫生与债务

- [x] forge：finalize 路径的 config 借用已收窄；widget 构造/高度估算/通知只接 owned snapshot 或提前复制的 scalar。
- [x] forge：`ui/config_apply.rs` 已在 per-view 循环前 clone config，不再跨 view 的 `borrow_mut` 持有 UiState config `Ref`。
- [ ] 两仓：菜单方法内冗余的 `*_for_menu` 再克隆层（为等价性证明故意保留），可在等价窗口关闭后删除。
- [ ] forge：`BlockBackend` 自身的 finalize 链与 `command_capture_anchor` 的 rebase 行需要真 VTE，测试套件够不到（设计使然），保持 GUI 验证。

## 方法论备忘（血泪换来的）

- 变异测试**必须**用私有 `CARGO_TARGET_DIR`，且不得并发跑：共享 target 会让另一进程的 mutated build 冒充"抓到的缺陷"，曾污染整轮结论；变异脚本里的 `git checkout --` 差点抹掉 1626 行未提交工作。用 `git worktree add --detach` 更安全。
- 新测试套件在信任前先做变异测试。第一版 dispatch 线束 1237 全绿却抓不住任何注入缺陷。
- 测试不得依赖开发者环境：旧线束通过是因为本机 `config.toml` 恰好把 cursor 设成了断言期待的颜色。
- 大改动前先做「只跑通、不出功能」的骨架增量：1a 花的代价换回六个纸面设计想不到的缺陷，其中一个会让 Block 用户的远程会话丢历史。
