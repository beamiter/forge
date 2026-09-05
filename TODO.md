# Forge TODO

本文件记录 `122546b` 之后已确认、但尚未进入实现的工作。优先保持故障可见、数据不丢失和资源占用有界。

P1 四项已在 `jterm_core` `9f94f77` 这一轮全部完成；P2 中的 history fail-closed 入口同时落地。
剩余两项 P2（跨块搜索 worker、per-session history GC）的分析与未完成原因见 `handoff.md`
的 “Not done this round, with the reason”。

## P1 · 下一轮优先

- [x] 解决配置编辑与外部重载的竞态：为 UI 未落盘更改增加 dirty epoch、冲突提示和显式解决路径；用确定性测试覆盖 200–250ms 保存/监控窗口。
- [x] 消除 GTK 主线程等待持久化锁：磁盘 I/O 不得持有 revision mutex，UI 读取使用快照或 `try_lock` + 重试；慢文件系统下回调保持非阻塞。
- [x] 将块视口计算从每次滚动 `O(N)` 降为前缀索引/二分与可见集合差分；覆盖 1k、10k、100k 块，滚动 p95 目标小于 16ms。
- [x] 让 Git 元数据探测完全异步：立即返回 TTL 缓存并通过通知更新，GTK 回调不再等待探测线程；慢 Git/FUSE 场景目标小于 1ms。

## P2 · 正确性与体验

- [x] Finished VTE 因 resize、filter、expand 或重新渲染而 reset 时统一使 `FindState` 失效；每个 surface 记录 render stamp，连单命中/边界不移动的导航也会先校验并用保留查询重建。
- [ ] 对齐 live block 的搜索数据源与 VTE 实际缓冲区，避免 prompt、command 和保留 scrollback 导致 Rust 计数与 PCRE2 选中项错位。
- [x] 跨块搜索记录 surface 内 occurrence/line，选择结果会从 surface 顶部精确步进到对应命中；超过 4096 步或中途耗尽时 fail closed，不再高亮较早的错误命中。
- [ ] 将跨块搜索扫描移到可取消 worker，避免大历史扫描阻塞 GTK 主线程。
- [ ] 为 per-session history 设计可证明所有权的安全 GC：依据 state manifest 与 active/restorable session 集合清理；禁止恢复基于文件名或 mtime 的猜测式删除。
- [x] 当 history 因预算、损坏或 revision 冲突进入 fail-closed 时提供明确的 Reload/Retry 入口和持久状态提示。
- [ ] 文件树根目录或子目录扫描失败时显示可聚焦错误、Retry 与 toast，区分空目录和权限/I/O 错误。

## 质量门禁

- [ ] 扩展 DISPLAY/Xvfb 场景矩阵，覆盖搜索、历史恢复、过滤重渲染、滚动和关闭流程；把 RSS 与帧延迟基准输出为可比较 JSON，并设置回归阈值。
- [ ] 在可用的 aarch64 Linux runner 上执行 `nix flake check --all-systems`，补齐当前 x86_64 门禁未覆盖的平台。
