# Changelog

All notable user-visible and operational changes are recorded here.

## Unreleased

### Fixed

- 精确锁定的共享 core 尚未发布本轮安全修复时，jterm4 现在通过可独立构建的本地兼容层
  先行补齐边界：后台 `curl`/`git`/`sh`/`notify-send` 与 Flatpak bridge 不再从空或
  相对 `PATH` 执行项目内同名程序；AI、jsh 检查与探测子进程均有输出、超时、进程组
  终止和继承管道回收上限；凭据、会话快照、命令历史、执行日志及 installer cache
  统一拒绝软链接、FIFO、设备、硬链接和不可信父目录替换。OSC 标题/cwd/session、
  Kitty 分块槽位、Notebook fence、快捷键和不可见 Unicode 也在解析前应用明确预算。
- Block PTY 输入改为容量 64、单消息 256 KiB 的非阻塞有界队列；命令正文与提交回车
  作为一个原子消息入队，队列饱和或关闭时不会发送可执行前缀，也不会让 Agent、纠错
  或命令面板误以为命令已成功插入/执行。所有失败只报告字节数，不把命令内容写入日志。
- Block PTY 创建失败不再触发 panic 或留下半提交的界面状态：首标签在提交
  session/connection 编号前完成构造，失败时显示 toast 并降级到 VTE；分屏采用
  prepare/commit 边界；会话恢复先在占位树中准备全部 pane，任一失败便清理临时
  PTY 并保留可用的单 pane 标签。
- 旧的 `agent_auto_approve_readonly` 配置与环境变量现仅为兼容而解析，运行时恒为
  关闭；Shell Agent 的每条命令都必须经过明确人工审批，设置界面会标示该开关已退役。
- 配置、备份、锁文件与窗口快照现在统一通过有界、非阻塞、禁止跟随软链接的文件
  描述符读取，并校验 regular file、当前用户属主及单一硬链接；FIFO、设备、超大文件、
  软/硬链接锁会立即拒绝，不再卡住启动或把无关文件 `chmod 0600`。配置锁同时持有受
  保护父目录锁，锁文件名被替换也不能绕过；不可读 primary backup 会继续尝试有效的
  secondary backup，写端也不能发布超过自身读取预算的配置。
- 窗口恢复在创建任何 PTY 前限制为 32 个标签、每标签 16 个 pane、总计 64 个 pane；
  标签名、cwd、session id 与可恢复 argv 也分别应用字段、元素数、原始总字节及 shell
  引用后命令行预算；快照读写、claim、legacy 迁移与发布均绑定到验证后的目录描述符，
  使用 no-replace rename，父路径被替换也不能重定向写入或覆盖另一个窗口的状态；新的
  active 文件还记录进程 start-time token，PID 被复用时不会把陈旧快照误判成仍由当前进程持有。
- Block history 现在严格识别截断/未知 frame，限制单记录、记录数、压缩前后总字节、解码
  时间和目录扫描量；跨进程保存持有不可被锁文件替换绕过的目录锁，陈旧 writer 只合并新增
  记录而无权删除并发数据，任何损坏的旧文件都不会被下一次保存覆盖。GTK 线程在复制历史
  前即执行相同预算，回滚的未显示 pane 不会在 Drop 时写入历史。
- Shell Agent 快照恢复会先原子排他 claim 并单次消费，两个进程不能恢复并审批同一 proposal；
  无效快照会保留隔离证据，重复/耗尽 ProposalId、错序 observation 与不一致 approval state
  均被拒绝。命令纠错的本地探测拥有真实子进程 deadline、取消 kill、输出/PATH/候选预算，
  且运行在独立进程组，成功、失败、取消与超时都会清理继承 stdout 的后台孙进程并回收 reader；
  展示卡 generation 只能消费一次，避免迟到结果或双击重复执行。
- 所有 review-only 命令现在限制为 256 KiB，并拒绝双向/零宽格式字符；Block 历史召回还会
  剥离终端控制字节，超限或视觉欺骗文本不会连带发出 `Ctrl+U` 清空当前提示符。会话快照中的
  可恢复 argv 应用相同视觉检查，恶意状态文件不能借隐藏字符伪装将要恢复的 SSH 参数。
- 顶部栏模式下只剩一个标签页时，右侧的 Agent、新建标签与窗口最小化/最大化/关闭按钮会整体挤到最左边、贴着 ☰ 和标签位置切换按钮，右侧留一片空白。顶部栏靠"某个子控件横向撑开"把这组按钮顶到右边缘，而这个角色由标签条的 `ScrolledWindow` 担任；单标签时标签条按设计隐藏（`sync_tab_bar_visibility`），隐藏控件不参与扩展，偏偏 `apply_tab_placement` 已经因为"顶部栏模式"把备用 spacer 的 `hexpand` 关掉了——两处各自看一半状态，于是没有任何子控件扩展。spacer 的开关改由 `sync_tab_bar_visibility` 独占（它是唯一知道标签条最终可见性的地方），标签条真正可见时才让位。新建/关闭标签、会话恢复与侧栏↔顶部栏切换都经过该函数，因此 1↔多标签来回切换时按钮位置保持钉在右侧。

- 桌面集成安装后应用列表里没有 jterm4 图标：三处成因已分别修复。其一，条目里的 `Exec=jterm4` / `TryExec=jterm4` 依赖 `PATH`，而桌面会话的 `PATH` 在登录时固定，默认安装位置 `~/.local/bin` 常常不在其中——`TryExec` 失败会让条目整个从应用列表里消失；`scripts/install.sh` 与发行包的 `install-release.sh` 现在把这两行改写成二进制绝对路径（`/usr/bin` 等系统 bin 目录仍保持相对形式以便重定位）。其二，安装脚本从不刷新桌面缓存，新条目与新图标要等下次登录才可见，陈旧的 `icon-theme.cache` 甚至会一直盖住刚装进去的图标；现在安装与卸载都会校验条目并刷新 `update-desktop-database` 和 `gtk-update-icon-cache`（`DESTDIR` 打包时跳过，且这些缓存以放宽的 umask 生成，避免 `sudo --prefix /usr` 装出别的用户读不到的 `0600` 缓存）。其三，`StartupWMClass` 写的是 application ID，而 GTK4 的 X11 `WM_CLASS` 取自程序名（实测为 `jterm4`），X11 会话下窗口因此无法与条目关联，dock 里会多出一个没有图标的重复项；现已改为 `jterm4`，Wayland 侧仍按 app_id 匹配不受影响。
- 安装脚本现在会提示 `PATH` 问题：目标 bin 目录不在 `PATH` 中，或 `PATH` 上已有另一份 jterm4（例如旧的 `cargo install` 副本）会遮蔽刚装好的二进制。

- Block 模式短输出的伪滚动条与行数不一致：同一条 `ls` 有时完整显示、有时只剩末尾两行且带块内滚动条，成因有二并已分别修复。其一，VTE 会按**实际分配的内容高度**重新推导网格行数，而 CSS 边框/内边距的记账差异可能让分配高度比 `行数 × 行高` 少几个像素——网格因此少一行，快照首行被挤进 scrollback，本可完整显示的输出多出滚动条；现在所有 finished VTE 的高度请求都带一个小于一行的像素余量（`finished_vte_height_px`），网格行数不再因像素记账掉行。其二，`feed()` 是异步的，负载下（如另一标签页在流式输出，VTE 的处理调度器为全进程共享）固定两次 idle 的定稿测量会落在喂入中途，把网格缩到当时恰好渲染的行数并永久卡在底部锚定；定稿现在改为确定性完成信号——轮询到快照的最后一行确实已渲染（封顶 2 秒后兜底）才测量收拢，封顶前的截断快照则每拍重申顶部锚定直到缓冲溢出或尾行可见。附带一个需要显示环境的忽略态回归测试（`diag_short_ls_block_geometry`），在真实 GTK 分配下断言 6 行输出恰为 6 行网格、无滚动条、顶部锚定，长输出保留视口滚动。

### Added

- `[[remote_hosts]]` 新增 `docker = true`：`host` 改为一个**正在运行的**容器名，标签页
  经 `docker exec` 而不是 ssh 连接，`user` 变成容器内用户（`-u` / 部署时的
  `--docker-user`），`deploy` 照常决定要不要把 jsh 送进去（送进去的一路已端到端验证：
  补全、菜单、OSC 133 块标记、窗口 resize 与本地一致）。共享库
  `jterm_core::jsh_remote` 和 `jsh-remote.sh` 早就支持 `--docker`，只有 jterm4 这一侧
  把它硬编码成了 `false`，因此配置里根本写不出容器目标。`ssh_args`、`multiplex`、
  `login_shell` 对容器无意义，写了会给出警告并忽略，而不是让主机加载失败。
- Block 模式运行中命令的体验改进（针对 claude 等长时流式 TUI）：
  - **运行中可框选文本**：在 live 终端面上拖选时，PTY 字节流被暂存（选区期间 + 松手后最多 5 秒宽限，或复制/输入/点击别处即恢复，上限 2 MiB），高频重绘不再瞬间冲掉选区；Shift+拖选在开启鼠标上报的应用里同样受保护。暂存生效时左下角显示 "Output paused — selection" 徽标，消除"卡住了"的错觉。
  - **运行中可回看输出**：滚轮在 live 终端面上优先滚动当前命令自己的回滚缓冲，滚到顶/底才交给外层 Block 历史（此前 VTE 吞掉滚轮且新输出会把视图拽回底部，运行中的早期输出实际不可达）；右侧出现细滚动条（overlay 覆盖式，出现/消失不改变列宽、不触发 SIGWINCH），跳底按钮同时归位内外两层滚动。空闲提示符上的滚轮现在可靠地滚动 Block 历史。
  - **运行中可搜索**：Ctrl+F 现在把正在运行命令的已产生输出纳入匹配（排在所有完成 Block 之后），VTE 原生高亮并支持 Next/Prev 跨面步进；关闭搜索一并清除 live 面高亮。
  - **sticky 运行头更实用**：向上翻历史时的运行中头部新增 Stop 按钮（一键发送 Ctrl+C，无需先找回终端焦点），耗时超过一小时显示 `1h04m` 格式。

- AI 聊天面板流式回复（`ai_stream` / `JTERM4_AI_STREAM`，默认开启）：回答在生成过程中逐段显示在会话里，三个 provider（Anthropic、OpenAI-compatible、Ollama）均支持；完成时以 provider 返回的完整文本替换进行中的消息并原样落库，保存的会话与关闭流式时完全一致（包括 `ai_max_tokens` 截断提示）。中途出错时已显示的部分内容保持可见，错误照常提示并可 Retry；Stop 与关窗仍会中断流式 curl。流式期间切换 chat 不会把片段写进别的会话，切回后已收到的部分回复会完整重现。仅聊天面板流式；Shell Agent、命令生成与纠错等严格 JSON 表面继续等待完整回复。开关同时提供于 Settings（Stream Chat Responses）。

- Kitty 图形协议（对齐 jterm1 的最小子集）：Block 模式解码 APC `G` 序列（`kitten icat`、matplotlib kitty 后端等的内联图片），把 PNG（`f=100`）与原始 RGBA/RGB（`f=32`/`f=24`）的 base64 直传载荷（含 `m=1`/`m=0` 分块）渲染为完成 Block 内文字输出下方的 GTK Picture，折叠按钮把图片与文字一并收起，纯图片命令也保留可折叠的 Block；此前这些序列被转发给没有图形协议的 libvte 而被静默丢弃。内存上限与 jterm1 完全一致：单图编码 16 MB、单 Block 解码合计 16 MB、边长至多 16384，超限静默丢弃；文件/共享内存传输与 `a=d`/`a=p` 动作不支持。与 jterm1 不同的是带 `i=`/`I=` 标识的命令会按 jterm2（家族参考应答实现）的语义在 PTY 上收到 `OK`/`EINVAL`/`ENOTSUP` 应答并遵守 `q=` 静默级别，`a=q` 探测会被校验后应答，因此 `kitten icat` 不再因等待应答而超时。图片仅在本次会话内展示，不写入 Block 历史，会话恢复后自然省略。
- OSC 9 / OSC 777 桌面通知：终端内程序（含 SSH 远端）可通过 `ESC ] 9 ; 正文 BEL` 或 `ESC ] 777 ; notify ; 标题 ; 正文` 请求桌面通知，由 `notify-send` 以 jterm4 身份发出（缺标题时回退为应用名）。文本在共享解析器中去除控制字符并截断至 256 字符，空通知不发；由于序列源自 PTY 内部，进程派生受应用级速率限制——每批至多一条、距上一条不足 2 秒的一律丢弃。
- 一键安装/更新配套 shell `jsh`：命令面板的 **Install or update jsh** 在独立标签页里运行安装脚本（标签页即进度界面，可 Ctrl+C 中断，结束后等待 Enter 再关闭）；缺少 jsh 或有新版本时顶栏下方出现可忽略的提示条。安装脚本来自 jsh 仓库并随二进制内嵌，校验和、原子替换、回滚与「`PATH` 上的 `jsh` 是同名其他程序」的提示都由它统一处理。更新检查在后台线程进行、从不自动安装，检查频率由 `jsh_update_check`（`startup` / `daily`（默认）/ `never`）控制，缓存与同机其他 jterm 共享。

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

- Kitty 图形协议的结构层迁移到共享库 `jterm_core::kitty_graphics` 并删除本地副本：控制段解析、`m=1` 分块重组、base64 解码、原始像素长度校验与 PNG IHDR 预检现在与 jterm1/jterm2/jterm3 使用同一份经测试加固的实现（52 个用例），本仓库只保留 GDK 解码路径、`a=q` 探测校验、PTY 应答（`OK`/`EINVAL`/`ENOTSUP` 与 `q=` 静默级别）以及 Block 级图片预算。内存上限沿用同一组数值（`Caps::BLOCK`：单图编码 16 MiB、解码 16 MiB、所有在途上传合计 16 MiB、边长至多 16384、控制段至多 16 KiB）。因家族统一而产生的行为变化：`f=` 缺省从 PNG 改为协议规定的 RGBA（`f=32`），因此不带 `f=` 的命令现在要求 `s=`/`v=`；原始像素载荷长度必须与 `s*v*通道数` 精确相等，此前允许多余尾部字节；`t=f`/`t=t`/`t=s` 从静默忽略改为明确应答 `ENOTSUP`；`f=` 只接受 `100`/`32`/`24`；`i=` 与 `I=` 不能同时出现；分块续传只能携带 `m=`（可选 `q=`），重复元数据的续块会中止该次上传并应答 `EINVAL`（此前会被当作续块接受）；base64 拒绝长度模 4 余 1 与中间出现的 `=`；控制键之间不再容忍空格。带 `i=`/`I=` 的命令仍照旧收到应答，包括共享解析器拒绝的命令。
- 进程探测与 shell 引用迁移到共享库 `jterm_core::process` 并删除本地副本：`/proc` 的 cmdline/ppid/stat 解析、PTY 前台进程发现、可恢复命令识别（ssh / mosh / `nix develop` / `docker|podman exec`）与各处 shell 单引号包装现在与 jterm1 使用同一份经测试加固的实现。行为上的两点变化：前台进程识别现在先检查 PTY 子进程本身（与 jterm1 一致），因此受管 ssh/mosh 面板的命令也能被检测与命名；单引号转义风格从 `'\''` 统一为 `'"'"'`（两者皆为合法 POSIX 引用），涉及 `bash -lc` 登录包装、jsh 的 `exec` 行（改用 core 的 `build_jsh_exec_command`）与文件树的路径插入（改用 core 的 `shell_quote_path`，明显安全的路径不再加引号、保持可读）。`startup_commands` 的逗号分隔配置语法不变，但只在应用边界解析一次，终端后端不再自行拆分。
- Block 模式长输出改为块内滚动（对齐 jterm1）：超出当前 pane 可视高度的 finished Block 不再撑满外层历史，而是保留自适应视口并在右侧显示专属滚动条，鼠标滚轮与滑块都只移动该 Block，滚到首/末行才把滚动交还外层历史；短输出仍取自然高度、不显示滚动条，展开按钮可把单个 Block 恢复为整段铺开。改变窗口或分屏大小时，屏幕上已可见的 Block 会立即按新高度重新适配（此前只有滚出视口再回来的 Block 才会重算），虚拟化高度同步更新，展开状态让位于新的 pane 尺寸。
- Block 头部元数据更易读：分钟级耗时保留秒数（`1m32s`、`1h04m`），不再丢失 61s 与 179s 的差异；信号退出码标注信号名（如 `exit:130 SIGINT`、`exit:137 SIGKILL`，悬停解释 128+n 约定），长命令完成通知同样附带信号名；从历史恢复的非当日 Block 时间戳带日期（`MM-DD HH:MM`），悬停显示完整本地日期时间，旧输出不再伪装成当天结果。
- Block 前台与后台输出改用固定上限环缓冲，持续大输出不再为每个 PTY chunk 搬移整个 8 MiB 尾缓冲。
- 关闭 pane/tab 会同步关闭 Block PTY 输入 worker、释放 GTK root controller，并让 live VTE、完成块、右键菜单、滚动/筛选/选择控制器以弱引用跨越 widget 边界；批量关闭标签或嵌套分屏后读写线程可回落到基线。
- 嵌套分屏折叠会在 `GtkPaned` 子节点仍有效时清空根焦点，再聚焦保留的 sibling，消除关闭聚焦分屏时的 GTK 运行时警告。
- `remote_hosts` 配置现在完整校验嵌套字段、未知键、类型、空值和控制字符。
- Block 样式不再注入未使用且 GTK 无法解析的组合关键帧规则，消除每次新建 Block pane 的 CSS 警告。
- Default shortcuts now share the jterm ergonomic layout: directional Pane
  focus/resize layers, browser-style tab digits, symmetric zoom/opacity keys,
  and shell-owned `Ctrl+P` passthrough.
- Session snapshots and Block history now use owner-only Unix permissions and durable atomic replacement.
- Session autosave 与 Block history 读写现由有界单 worker 串行处理，同一目标的待写快照采用 latest-wins 合并；后台失败会显示去重提示，关闭窗口前会保存并 detach 全部 pane、发布最终 session snapshot，并有界 flush 队列。
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
- 快捷键 chord 的解析/显示迁移到家族共享的 `jterm_core::keybindings`：配置里的 chord 语法按家族并集放宽——新增 `control`/`option`/`cmd`/`win`/`meta` 等修饰键别名、`esc`/`del`/`ins`/箭头等按键别名、`f1..f24`、更多符号名与 Unicode 字母，重复修饰键（含别名，如 `ctrl+control+t`）现在会被报错拒绝；解除绑定除 false、空串、"none"、"disabled" 外新增 "unbind"。`--doctor`/加载期的 chord 静态校验与运行时覆写走同一解析器，校验通过的 chord 必定能生效（此前两边各自实现可能不一致）。显示形式保持既有的 `Ctrl+Shift+Alt+…` 修饰键顺序与 `Enter` 拼写不变；唯一变化是侧栏快捷键现在显示为 `Ctrl+\` 而非泄漏 X11 keysym 名的 `Ctrl+backslash`（配置里两种写法都照常解析）。默认键位与冲突拒绝策略不变，新增测试钉住家族 38 项默认 chord 契约；示例配置中 resize/AI 面板 chord 注释改为与显示一致的 `Ctrl+Shift+Alt+…` 顺序。旧配置里任意 X11 keysym 名（如 `Menu`）作为键名的写法不再被接受，会在加载时得到明确警告。

- 终端子进程的终止与回收迁移到共享库 `jterm_core::process`（`ChildLifecycle` / `ProcessRef` / `EscalationPolicy`），删除本地的 `process_exists`、`signal_pid_and_group`、`wait_for_process_exit`、`terminate_terminal_process`。pane 现在随 widget 保存一个内核绑定的子进程句柄（Linux 上是 pidfd）而不是裸 pid，并显式记录由谁 `waitpid`：Block pane 的 PTY 由 jterm4 自己 fork、自己回收；常规 VTE pane 的子进程由 glib/VTE 的 child watch 回收，其生命周期只经 pidfd 发信号、绝不 `waitpid`，退出码由 `child-exited` 记账。HUP → TERM → KILL 阶梯与既有的 120ms/250ms 宽限期不变，但最后一发 KILL 改为按 PTY 会话清扫（两个 backend 都对 PTY 子进程 `setsid`）：关闭 pane 时会一并清掉 shell job control 放进别的进程组、此前只补一发进程组信号会遗漏的后台任务。常规 VTE pane 的 `child-exited` 状态现在按家族约定归一（正常退出取退出码、被信号杀取 128+信号号），与 Block pane 上报的数字一致；此前把 `waitpid` 原始状态字直接当成退出码交给远端重连逻辑（只有 0 的语义碰巧是对的）。窗口快照“属主进程是否还在”改为纯 `/proc` 探测，不再向别的 jterm4 窗口发信号 0。

### Fixed

- 关闭 pane / 标签页 / 窗口时的四个子进程缺陷（PID 复用与僵尸）一并消除，均由共享的 `ChildLifecycle` 在类型上保证：**其一**，终止不再先对裸 pid 发信号、事后才读 `/proc` 判断要不要打进程组——句柄是内核绑定的，pid 被复用时信号直接 `ESRCH` 失败，负 pid 的进程组信号也改为先验证组长身份再发，此前那句"防 PID 复用"的注释与实现并不相符。**其二**，升级线程不再靠 `kill(pid, 0)` 判断存活——它与唯一的回收者之间没有任何同步，僵尸和已被复用的 pid 都会被判为"还活着"并继续升级；现在每一级信号都与 `waitpid` 在同一把锁下串行，状态一经观测即拒绝再发信号。**其三**，`fork` 成功但随后 `master.try_clone`、PTY 输入 worker 或读线程创建失败时（例如 fd 耗尽），子进程此前只被信号处理、从不 `waitpid`，会留下与进程同寿的僵尸；这些路径现在统一 kill 后同步回收。**其四**，显式关闭 pane 后紧跟着 widget drop 会对同一个 pid 起两条升级线程（第二条在第一条回收之后仍在发信号），现在第二次终止请求会发现已有终止在进行中并直接返回。新增测试覆盖失败 spawn 路径不留僵尸、以及"显式关闭 + drop"只跑一条升级阶梯并完成回收。

- Block 模式的动态颜色查询（OSC 10/11/12 `?`）不再返回过期的主题色：应用通过 OSC 10/11/12 设置前景/背景/光标色后（主题切换工具、vim `background=` 探测等；原始字节仍照常透传，live VTE 原生变色），共享解析器现在发出 `ColorSet`/`ColorReset` 事件，每个 pane 据此跟踪动态覆盖值——支持 `#RRGGBB` 十六进制、X11 `rgb:R/G/B`（每通道 1–4 位十六进制，按 XParseColor 语义缩放）与颜色名，无法解析的值忽略——后续查询以既有的 `rgb:RRRR/GGGG/BBBB` 格式回答动态值，OSC 110/111/112 复位后回落到主题色。动态前景/背景生效期间新完成的 Block 快照 VTE 也叠加同一覆盖色，不再与已变色的 live 视图形成明显割裂。

### Security

- 会话恢复回放加固（移植 jterm1 的同类修复）：可恢复命令此前以空格拼接成字符串保存、恢复时按 `", "` 拆分后原样写入 PTY——远端参数里的 `;`、逗号或换行可以在重启后被拆成一条新的本地命令。现在窗口快照保存结构化 argv（JSON 数组），回放前用配置 shell 对应的语法（POSIX 单引号或 PowerShell `& '...'`）把整个 argv 安全引用为恰好一条命令；含控制字符的 argv 或未知 shell 语法一律跳过回放并记录警告。旧快照里拼接字符串形式的命令仍可正常加载（标签页、目录、会话 ID 不受影响），但绝不自动回放，丢弃时记 debug 日志。
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
