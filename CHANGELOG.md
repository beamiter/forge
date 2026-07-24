# Changelog

All notable user-visible and operational changes are recorded here.

## Unreleased

### Added

- Shell Agent 可选的只读命令自动放行（`agent_auto_approve_readonly` / `JTERM4_AGENT_AUTO_APPROVE_READONLY`，默认关闭）：通过严格白名单的单条只读命令免点击执行，写操作、管道/链式/重定向/命令替换、可执行旁路 flag 与危险模式仍逐条人工审批；自动批准留下可见记录、照常消耗回合预算，开启期间 Agent 卡显示 `auto: read-only` chip，开关位于 Settings 与 Agent 卡设置弹窗。
- Shell Agent 会话进行中可用 **Attach selected Block** 附加或替换当前选中的 finished Block 作为不可信上下文，不再局限于开卡瞬间的一次性捕获。
- Agent 请求现在附带有界的 git 元数据（branch、dirty、ahead/behind），与 cwd/shell/OS 一样仅作为不可信 user-role 数据发送，帮助模型贴合仓库状态提出命令。
- Bash、zsh、fish 与 PowerShell 的内置 CLI 补全生成（`--generate-completion`）。
- 顶栏、标签栏与文件导航的完整键盘焦点链、语义化无障碍标签，以及 Enter/Space 标签激活。
- Project licensing under `MIT OR Apache-2.0`, including canonical license texts, Cargo/AppStream metadata, inbound-contribution terms, and license files in release artifacts.
- Reproducible GNOME 50 Flatpak packaging, stable desktop application ID, AppStream metadata, scalable/raster icons, checksummed CI bundles, and X11/Wayland VTE/Block smoke tests.
- A Flatpak host-command bridge so shells, SSH, Git probes, AI curl requests, and desktop notifications operate on the host instead of the application sandbox.
- Modern GTK4 `TreeListModel`/`ListView` file browser with asynchronous lazy directory scans.
- Per-window session snapshots with atomic claiming, stale-process recovery, legacy migration, retention, and doctor counts.
- Target-aware `JTERM4_LOG` / `RUST_LOG` filtering with relative timestamps and module targets.
- Cargo-or-Nix installer, safe uninstaller, Rust toolchain metadata, CODEOWNERS, contribution/security/architecture/release documentation, Dependabot, and RustSec auditing.
- Packaged OSC 133/7 shell integration for bash, zsh, fish, and PowerShell, also printable through `--shell-integration`.
- Headless JSON diagnostics, config initialization and backup restore, cwd/argv launch overrides, backend override, no-restore, and isolated safe mode.
- Metadata-only bounded JSONL command history shared by Block and VTE palettes.
- One fuzzy command palette spanning actions, history, YAML/TOML workflows, and a review-first natural-language command entry.
- Installed YAML workflow examples and multi-directory workflow precedence with both `{name}` and `{{name}}` placeholders.
- Executable `.jtnb.md` notebooks with per-cell/Run All execution, separate stdout/stderr, bounded output, and process-group cancellation.
- Provider-neutral AI for Anthropic, OpenAI-compatible endpoints, and Ollama, plus natural-language command generation and a native Block-bound Shell Agent. Its bounded multi-turn UI strictly parses JSON proposals, permits edit/reject/per-command approval, flags recognizable destructive patterns, feeds completed command results back to the model, and supports cancellation.
- Settings can now accept and replace the AI API key directly while storing it atomically in a separate owner-only credential file instead of `config.toml`.
- A searchable per-window AI Chats library with automatic titles, selection, rename, archive/unarchive, confirmed deletion, and durable per-chat drafts and provider-bound selected-block context.
- Foreground-process discovery and close confirmation across Block/VTE panes, split tabs, batch tab closure, zoomed layouts, and whole windows.
- Privacy-preserving `jterm4-support-bundle` diagnostics plus richer doctor checks for config permissions/backups/locks, provider readiness, workflows, Notebook assets, history, display, and remote tooling.
- Review-only workflow examples for interactive rebase, SSH port forwarding, Docker log streaming, and signaling a process by listening port.
- Deterministic relocatable Linux release archives with SHA-256 checksums, a user-local bundle installer, tag-driven release publishing, and Nix package/app/check outputs.

### Changed

- Block 前台与后台输出改用固定上限环缓冲，持续大输出不再为每个 PTY chunk 搬移整个 8 MiB 尾缓冲。
- 关闭 pane/tab 会同步关闭 Block PTY 输入 worker、释放 GTK root controller，并让 live VTE、完成块、右键菜单、滚动/筛选/选择控制器以弱引用跨越 widget 边界；批量关闭标签或嵌套分屏后读写线程可回落到基线。
- 嵌套分屏折叠会在 `GtkPaned` 子节点仍有效时清空根焦点，再聚焦保留的 sibling，消除关闭聚焦分屏时的 GTK 运行时警告。
- `remote_hosts` 配置现在完整校验嵌套字段、未知键、类型、空值和控制字符。
- Block 样式不再注入未使用且 GTK 无法解析的组合关键帧规则，消除每次新建 Block pane 的 CSS 警告。
- Default shortcuts now share the jterm ergonomic layout: directional Pane
  focus/resize layers, browser-style tab digits, symmetric zoom/opacity keys,
  and shell-owned `Ctrl+P` passthrough.
- Session snapshots and Block history now use owner-only Unix permissions and durable atomic replacement.
- Block is now the default terminal backend. New splits inherit the focused pane's backend, so Block and VTE layouts both remain structurally consistent.
- Repeated same-axis splits rebalance by pane-tree span, keeping three or more panes evenly sized instead of recursively squeezing newer siblings.
- Directional pane focus now recognizes the complete focused Block/VTE subtree and retains the last active leaf across transient container focus, so all four focus shortcuts work from finished blocks and other pane descendants.
- Runtime configuration updates propagate to Block leaves nested in pane trees.
- Pane-to-tab moves preserve stable process/session identities, tab chrome, and remote reconnect ownership across repeated primary or remote pane moves.
- Application config saves now validate syntax/semantics, serialize through an advisory lock, reject stale revisions, rotate two valid backups, and use private durable atomic replacement.
- Safe mode now constructs a fully isolated built-in VTE profile without reading user config or behavior overrides; configuration reload is disabled and save failures are visible in the UI.
- The installer, uninstaller, and Flatpak bundle now manage shell integration, workflows, and Notebook runtime assets under `share/jterm4`.
- CI now checks maintained shell scripts and exports complete formatting diagnostics.
- Notebook output transport now applies bounded backpressure, and both cancellation
  and normal interpreter exit terminate the cell process group before joining pipes.
- The AI panel now restores and persists its dragged width, has a themed empty/composer/status UI, routes focused copy/paste correctly, and uses Enter or Ctrl+Enter to send while Shift+Enter inserts a newline without stealing IME candidate confirmation.
- The Shell Agent is now a target-aware dashboard with provider/shell context, turn progress, richer proposal/status surfaces, transcript clearing, and a persistent toggle for review-first typo-like command correction.
- **New chat** now creates and selects a separate retained chat instead of clearing the previous conversation. Background replies remain bound to their originating chat, and a late reply cannot resurrect a deleted chat.
- AI persistence schema v2 stores the current selection and up to 50 chat rows, automatically migrates v1 single-chat snapshots, retains at most 100 turns per chat, and compacts the oldest history with a visible `truncated` marker to keep the complete JSON collection within 8 MiB.
- Failed or interrupted sends are recoverable as drafts, selected-Block requests preserve unrelated composer text, and window close flushes pending draft persistence before the final snapshot.
- Window snapshots reserve 64 KiB exclusively for all bounded chat metadata; constrained Pane/Tab state triggers deterministic payload compaction instead of silently omitting the whole chat collection.
- AI Chat now has true transport-level Stop, generation-safe Retry (including selected-Block requests), visible/clearable active and pending-retry context chips, bounded quick prompts, owner-specific error status, and shutdown cancellation for busy or deleted chats.
- Shell Agent can attach the selected finished Block as visible untrusted context, stop/retry only the current model turn, copy proposals, recompute risk after edits, and settle spinner/input state correctly at completion or the turn limit.
- Shell Agent now exposes the pinned prompt's live readiness reason instead of a generic busy error, keeps completed context available through **Follow up**, can reset an exhausted session in place with **New task**, and uses explicit theme-aware contrast for its composer, hints, and turn counter.
- Block-mode natural-language suggestions, verified corrections, and Shell Agent proposals now share one inline command-review interaction. One-shot suggestions are pane/context-aware and cancellable with Retry/Regenerate; Agent proposals can be inserted for manual review without execution, while verified corrections automatically downgrade to insertion after any edit or newly detected risk.
- Provider traffic now uses a four-request global concurrency bound, recent-history request compaction, explicit output-limit notices, cancellable curl child reaping, and hard request/context/response capture budgets.
- Live Chat/Agent activity and the Agent core transcript are now independently bounded; compaction preserves in-flight questions, Block output truncation is visible, and memory-only Ask Block retries become durable draft/context during shutdown.
- Agent pane environment metadata moved out of the system prompt into bounded untrusted user-role JSON; edited approvals preserve exact whitespace and dangerous-command recognition handles common `env`/`command`, assignment, and Git global-option wrappers.
- Temporary round-two source-export workflows and marker files were removed.

### Security

- Persisted commands, output, working directories, and session metadata are restricted to `0700` directories and `0600` files on Unix.
- AI credential contents remain outside `config.toml`: environment variables take priority, with an optional owner-only `ai_api_key_file` fallback; safe mode disables AI/Agent, executable notebooks, history, remote hosts, restoration, and persistence.
- AI chat metadata, completed pairs, drafts, and provider-bound Block context share the bounded, owner-only, atomically replaced per-window snapshot. Redaction covers active, non-active, and archived chats, and in-flight requests are never restored as completed replies.
- AI/Agent command proposals never submit or execute a command without an explicit user action.
- Agent approval is refused while the bound Block prompt is busy or already contains input; malformed model output never degrades into a runnable proposal.
- Agent proposals and edits must be one visible line with no control characters; recognized privilege, destructive filesystem/system/container, and forced-Git operations require a second exact-command confirmation.
- Selected Block command/output/cwd bytes are bounded, JSON-escaped and sent as explicitly untrusted user-role data instead of being interpolated into the system prompt.
- History, workflow, file-tree and AI review insertions reject line breaks and terminal control characters before writing to a PTY.
- Support archives are created owner-only, make no network requests, and exclude configuration/history/session contents, credentials, host identity, SSH targets, and local paths.
- The repository is dual-licensed under `MIT OR Apache-2.0`; crates.io publication remains separately disabled with `publish = false`.
