# Unified 模式待办

本文件记录 `edb1be5` 之后 Unified 渲染后端的剩余工作。设计文档见会话产出的《Unified 模式设计》（单一 VTE + 覆盖层 block chrome）；此处只记可执行项、验收标准和已知债务。

跨仓：forge 是参考实现，anvil 逐项镜像。两仓当前进度不同，见「anvil 镜像」一节。

## 已完成（不再重复）

- Phase 0（两仓）：reader 闭包拆为 `ReaderCtx` 生命周期引擎 + `RenderBackend` trait；forge `98b4f4b`、anvil `2b02f4e`。
- forge dispatch 测试线束：`a6b4dc8` + `9bfc363`，1260 测试，16 组变异验证。
- Phase 1 增量 1a：`edb1be5`，`TerminalMode::Unified` + 无 chrome 的 `UnifiedBackend`，六个探针缺陷已修。

## P1 · 增量 1b（顺序由 1a 实测确定，不要跳步）

- [ ] **zone 表升为后端查询 `records()`**。当前 `TermView` 有 ~15 个方法直接读 `finished_blocks`/`block_data`，1a 被迫加 `unified_zones: Option<...>` 当模式标志并在四处分支。真正的 Block 耦合在 `TermView` 而非 trait。验收：find/export/history/palette 在两种模式下读同一来源，`if unified` 分支归零；`Ctrl+F` 在 Unified 下能命中屏上文本。
- [ ] **`finalize_block` 签名瘦身**。现在同时收 `BlockData`（含 `estimated_height`/`line_count`/`cols` 等渲染字段）和 12 字段的 `FinalizedBlockArgs`；引擎每条命令都会 `strip_ansi` + 截断 + 分配输出文本，纯粹为了 widget 渲染，Unified 一个字节不用。验收：一条命令记录只带身份/结果字段，输出负载走惰性访问器或后端能力协商；Block 行为不变。
- [ ] **A→C 标记注入器**。在 FTCS `A` 注入 `OSC 8 ;; block://<nonce>/<zone_id>`，到 `C`（不是 `B`）关闭；A..C 窗口内每段 fed Bytes 重发 open（幂等，封顶 guest OSC 8 / DECRC 的破坏半径）；空闲 prompt 重渲染复用同一 zone id。理由：jsh 每键重印 prompt 且不重发 OSC 133，A→B 包裹会在第一次按键后被抹掉（spike2 G1 实证）。验收：`less`/`vim` 进出后标记仍在；连续按键不新增 zone。
- [ ] **探针驱动的 chrome**。逐可见行 `check_hyperlink_at` 在**第 0 列**探（guest PS1 的 OSC 8 覆盖不到）；URI 必须精确匹配本 pane nonce 且对照有序 zone 表校验；已知头行探到不匹配 URI 仍归该 zone。绘制 gutter 竖条 → 分隔线 → badge（落点空白检测）。探针坐标需减去 widget CSS padding。验收：0.2µs/次量级的探针开销不影响滚动 p95；rewrap 后 chrome 仍对齐。
- [ ] **zone 上限与淘汰**：活 zone 上限 256（frost 先例），超限最旧者降为快照寻址并停止注入标记；`[3J` 只淘汰严格位于视口顶之上的 zone，RIS/reset 全量淘汰；`rows_evicted` 是所有 ring 抽取路径的硬门。验收：`[3J` 后已清除内容不能经 badge 空白检测或重扫描复活。

## P2 · Unified 的已知缺口（1b 期间逐项关闭）

- [ ] 会话导出：当前诚实拒绝（`ErrorKind::Unsupported`），待 `records()` 落地后基于 zone 表实现。
- [ ] 会话恢复：当前跳过 Block 历史存取。设计方案是有界快照回放（最近 ~64 zone / 4 MiB 经注入器重新 feed），避免重启后空屏。
- [ ] inline 卡片安置：当前在 Unified 下诚实拒绝（agent/correction/palette 卡片）。设计方案是底部**占位**停靠区（VTE 的垂直兄弟节点，开关各触发一次 SIGWINCH），不是覆盖层——覆盖会精确遮住用户正在盯的 prompt 行。
- [ ] organism：`ascii_organism_enabled` + `OrganismMotion::Static` 组合下 Unified 无任何 organism 表面（卡片是它在该组合下的唯一载体）。需要在停靠区设计里给它位置。
- [ ] sticky 运行头 / jump FAB：Unified 下已不可达（1a 发现它此前只是借了卡片撑开的外层滚动条，属意外而非特性）。需要在覆盖层设计里重新接线。
- [ ] jsh 启动噪音：Block 的每-prompt 清屏顺手吞掉了它，Unified 不清屏所以会永久留在屏上。判断是否需要 shell 侧或首-prompt 侧处理。
- [ ] kitty 图片：当前 Unified 答 `ENOTSUP`（解析即弃）。v2 可用探针行锚定 overlay `Picture`。

## P2 · anvil 镜像

- [ ] **锚点绕过修复**（与 forge M6 同款，anvil 侧仍开着）：anvil 的 `command_capture_anchor` 是单路径钩子，`VerifiedSubmissionCtx::submit` 守卫、其验证轮询、`command_prompt_status`、`click_cursor.rs` 四处仍读 raw `prompt_end_pos`。照 forge 的做法引入 `SubmissionSurface::prompt_anchor` + 单一 `prompt_anchor_for_surface`，标志位与后端选择同源。
- [ ] **dispatch 测试线束**：需要 forge 已验证的两个 seam（`SubmissionSurface` 隔离 VTE 句柄、`#[cfg(test)] OwnedPty::from_openpty`）+ `RecordingBackend`。照抄 forge 的经验：stub 要建模不要顺从，配置用 `load_safe_config()` 不读开发者配置文件。
- [ ] Phase 1 增量 1a 镜像（`TerminalMode::Unified` + 无 chrome 后端），可直接落 forge 修完六个 major 后的最终形态。

## P3 · 卫生与债务

- [ ] forge：三处 finalize 路径的 config 借用跨越 `new_with_pool`/高度估算/通知调用，收窄为显式短作用域（当前安全，因被跨越的调用不可重入到 `borrow_mut`，但不变量脆弱）。
- [ ] forge：`ui/config_apply.rs:129` 在 per-view `borrow_mut` 期间持有 UiState config 的 `Ref`——今天安全（两个 cell 不同），但若将来统一 cell 会必然 panic。改为循环前 clone。
- [ ] 两仓：菜单方法内冗余的 `*_for_menu` 再克隆层（为等价性证明故意保留），可在等价窗口关闭后删除。
- [ ] forge：`BlockBackend` 自身的 finalize 链与 `command_capture_anchor` 的 rebase 行需要真 VTE，测试套件够不到（设计使然），保持 GUI 验证。

## 方法论备忘（血泪换来的）

- 变异测试**必须**用私有 `CARGO_TARGET_DIR`，且不得并发跑：共享 target 会让另一进程的 mutated build 冒充"抓到的缺陷"，曾污染整轮结论；变异脚本里的 `git checkout --` 差点抹掉 1626 行未提交工作。用 `git worktree add --detach` 更安全。
- 新测试套件在信任前先做变异测试。第一版 dispatch 线束 1237 全绿却抓不住任何注入缺陷。
- 测试不得依赖开发者环境：旧线束通过是因为本机 `config.toml` 恰好把 cursor 设成了断言期待的颜色。
- 大改动前先做「只跑通、不出功能」的骨架增量：1a 花的代价换回六个纸面设计想不到的缺陷，其中一个会让 Block 用户的远程会话丢历史。
