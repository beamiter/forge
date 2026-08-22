# forge

forge 是一个面向开发工作流的原生 GTK4 终端。它默认使用 Block 后端，把命令、输出、退出状态和工作目录组织成可搜索的结构化块；需要传统终端语义时也可切换到 VTE 后端。

## 能力概览

- 默认 Block、连续滚屏 Unified、传统 VTE 的三终端后端
- 标签页、混合后端分屏、方向导航、缩放、前台进程关闭确认与多窗口独立会话恢复
- 拖动普通单-pane 标签到目标 pane 四边即可无损并入分屏；拖动分屏标题栏回标签栏即可恢复为普通标签。短暂悬停会预览目标页，取消或中心释放不会重启、复制 PTY
- 跨命令块搜索、失败/慢命令筛选、只记录元数据的 JSONL 命令历史
- 统一模糊面板：动作、历史、YAML/TOML workflow 和自然语言命令入口
- 可执行 `.jtnb.md` Notebook，逐 cell 运行/停止并分离 stdout 与 stderr
- 基于现代 GTK4 列表模型的异步文件树、Git 分支/脏状态条、长命令桌面通知
- SSH 主机选择、连接复用与自动重连
- 可选多 provider AI、可搜索和归档的多会话 Chats 库，以及绑定当前 Block pane 的原生 Shell Agent；每条候选命令均可编辑且需单独批准
- 配置热重载、可覆盖的应用动作快捷键、8 套内置主题
- CJK 输入法和 Unicode 安全的搜索/通知显示

分屏会继承当前 pane 的后端：Block 创建 Block sibling，VTE 创建 VTE sibling，并可继续嵌套。每个可见 pane 都独立拥有并清理其进程，关闭 pane、标签或窗口前会统一检查前台任务。

## 构建与运行

推荐使用仓库提供的 Nix 开发环境：

```bash
nix develop
cargo run
```

也可以在安装 GTK4、libadwaita、VTE GTK4、PCRE2 与 `pkg-config` 开发包后直接使用 Cargo。

GTK4 栈必须足够新（glib >= 2.80、pango >= 1.52、gtk4 >= 4.14、libadwaita >= 1.5，
以及 GTK4 版 VTE `vte-2.91-gtk4` >= 0.76）。Ubuntu 22.04 等稳定发行版自带的这些库过旧，
或根本没有 `vte-2.91-gtk4`，会导致 `cargo install --path .` 在 `*-sys` 构建脚本处失败。
运行 `./scripts/bootstrap_deps.sh` 一键准备依赖：

```bash
./scripts/bootstrap_deps.sh            # 配好推荐工具链（Nix）
./scripts/bootstrap_deps.sh --check    # 只检测缺什么，不安装
./scripts/bootstrap_deps.sh --backend system --install   # 改用发行版系统包
```

脚本默认使用 Nix（精确固定匹配的库版本、不污染系统包），也可用 `--backend system`
改装发行版 `-dev` 包。

完整本地质量门禁由核心验证和扩展安全检查两部分组成：

```bash
make verify
make security
```

`make verify` 运行格式、测试、Clippy、Rustdoc、release 构建、shell 语法和已跟踪文本隐私
检查；`make security` 运行 RustSec、依赖策略/许可证/来源门禁、重复依赖审计与 ShellCheck，
需要本机已安装 `cargo-audit`、`cargo-deny` 和 `shellcheck`。只需运行轻量隐私检查时可使用
`make privacy`。

安装脚本默认优先使用 Nix；没有 Nix 时自动退回 Cargo，并且不会覆盖已有配置：

```bash
./scripts/install.sh
./scripts/install.sh --backend cargo
./scripts/install.sh --binary /path/to/forge   # 跳过构建，安装已有产物
./scripts/install.sh --prefix /opt/forge --no-config
./scripts/install.sh --dry-run
```

无参数源码安装沿用 `~/.cargo/bin/forge`，显式 `--prefix` 时二进制改为
`PREFIX/bin/forge`；安装与卸载使用同一规则。脚本同时安装 `forge-support-bundle`，并把
shell 集成、内置 workflow 和欢迎 Notebook 安装到 `~/.local/share/forge/`。配置使用
`0600`。脚本支持 `DESTDIR`、`XDG_CONFIG_HOME` 和 `CARGO_TARGET_DIR`；使用非默认 prefix
时可通过 `FORGE_ASSET_DIR` / `FORGE_WORKFLOW_DIR` 指向对应的 `share/forge` 目录。

`--binary` 输入必须是可读、非符号链接的普通文件。Bash 的初始 no-follow 检查与 open
本身并非原子操作；只有成功打开且用 GNU `stat` 复核路径和描述符指向同一设备号/inode 后，
之后替换路径名才不会改变通过 Linux `/proc/self/fd` 复制的 inode。目标文件先在同目录用
`mktemp` 写完，再由 GNU `mv -T` 原子替换；rename 前失败或退出会清理临时文件并保留旧版。
rename 是二进制更新的提交点：只保证提交前失败/退出保留旧目标并清理未提交临时文件；
之后若其他资源安装失败，不会回滚已经提交的二进制。
支持工具、shell 集成、workflow、Notebook、AppStream 与图标也都在目标目录内先写入
mode 正确的临时文件，再原子 rename；所有源文件在构建和首次写入前完成预检。
首次配置用同目录临时文件加无覆盖硬链接原子发布：若另一个进程先创建
`config.toml`，安装器保留并报告并发赢家。`--binary` 拒绝零字节文件，且不能与显式
`--backend` 同用。非根 `DESTDIR` 会先折叠重复 `/` 和词法 `.` 段，再从 `/` 起检查其
完整既存祖先链；任何符号链接都会在安装写入或卸载删除前被拒绝（`--purge-config` 的
递归根也先整体预检）。这是既存状态预检，不承诺抵御检查后的并发路径替换；普通主机
安装/卸载不套用这条打包边界策略。`--prefix`、`--bin-dir`、XDG 路径与 `DESTDIR` 必须是
无控制字符、无词法 `..` 段的绝对路径；运行时路径仍可含空格、Unicode 与 `.` 段。
卸载默认保留用户配置、状态与历史：

```bash
./scripts/uninstall.sh
./scripts/uninstall.sh --purge-config   # 明确删除全部配置和状态
bash scripts/test-install-paths.sh      # 私有 DESTDIR 安装/卸载合同
```

### 桌面集成（应用列表里的图标）

`./scripts/install.sh` 默认一并安装桌面集成，无需额外步骤，安装后 forge 就会出现在
GNOME/KDE 的应用列表里，可以搜索、点击启动、固定到 dock：

| 安装内容 | 位置（默认 prefix） |
| --- | --- |
| 启动器条目 | `~/.local/share/applications/io.github.beamiter.forge.desktop` |
| 应用图标 | `~/.local/share/icons/hicolor/{scalable,128x128,256x256}/apps/io.github.beamiter.forge.*` |
| AppStream 元数据 | `~/.local/share/metainfo/io.github.beamiter.forge.metainfo.xml` |

安装时脚本会把 `Exec=` / `TryExec=` 改写成二进制的绝对路径（系统 prefix 如 `/usr` 除外），
因为桌面会话的 `PATH` 在登录时就固定了：若 `~/.local/bin` 不在其中，`TryExec=forge`
会失败并让条目**整个从应用列表中消失**——这是"装好了却找不到图标"最常见的原因。
随后脚本会校验条目并刷新 `update-desktop-database` 与 `gtk-update-icon-cache`（陈旧的图标
缓存会盖住刚装进去的图标）；`DESTDIR` 打包场景下跳过刷新，交由包管理器处理。
`--no-desktop` 可只装二进制。

自检与手动刷新：

```bash
desktop-file-validate ~/.local/share/applications/io.github.beamiter.forge.desktop
gtk-launch io.github.beamiter.forge          # 按启动器条目实际启动一次
```

图标若一时没刷新，注销重登（或 X11 下 `Alt+F2` → `r` 重启 GNOME Shell）即可。
Wayland 下窗口按 app_id 与条目关联，X11 下则依赖 `StartupWMClass=forge`——GTK4 的
X11 `WM_CLASS` 取自程序名而非 application ID，写成 application ID 会导致 dock 里出现
一个没有图标的重复条目。

`nix build` / `nix run` 分别构建和启动 flake 中的默认 package/app，
`nix flake check` 验证同一 package。也可为已有 release binary 生成确定性、
带 SHA-256 的本地安装归档：

```bash
cargo build --release --all-features --locked
./scripts/package-release.sh target/release/forge
(cd target/dist && sha256sum --check *.sha256)
```

该归档可换目录后安装，但仍动态依赖兼容的 GTK4、libadwaita、VTE GTK4
和 PCRE2 系统运行库，并非静态或自包含的 portable 应用。


## Flatpak 与桌面集成

项目使用稳定应用 ID `io.github.beamiter.forge`，提供 desktop、AppStream、
SVG/PNG 图标以及可复现 Flatpak 清单。Flatpak 中的 Shell、SSH、Git、curl
和通知命令通过 `flatpak-spawn --host` 运行，因此终端操作的是宿主环境而
不是一次性沙箱；原生安装路径保持直接执行。内置 shell 集成、workflow 和
欢迎 Notebook 一并安装在 `/app/share/forge/`。

```bash
flatpak-builder --user --install-deps-from=flathub --force-clean \
  --disable-rofiles-fuse --repo=flatpak-repo flatpak-build \
  packaging/flatpak/io.github.beamiter.forge.yml
flatpak build-bundle flatpak-repo io.github.beamiter.forge.flatpak \
  io.github.beamiter.forge
```

权限模型、宿主桥接、安全边界、安装命令与已知限制见
[Flatpak 指南](docs/FLATPAK.md)。

## 启动与配置

默认配置路径为 `~/.config/forge/config.toml`。从完整示例开始：

```bash
forge --init-config
forge --check-config
```

也可使用独立配置：

```bash
forge --config ~/my-forge.toml
forge --check-config ~/my-forge.toml
```

常用启动覆盖不会修改配置：

```bash
forge ~/project
forge --mode block --no-restore
forge -d /tmp --execute bash -lc 'printf "hello\\n"'
forge --safe-mode
```

`--safe-mode` 不读取指定或默认配置，也不采用 `FORGE_*` 外观/行为覆盖；它使用内置 VTE 主题与默认快捷键，并禁用配置重载、恢复、持久化、远程主机、历史、仓库探测、AI/Agent 与 Notebook 执行，适合排查损坏配置或启动环境。

诊断命令均可在没有图形显示的 SSH/CI 环境运行：

```bash
forge --help
forge --doctor --json       # 同时报告 ready / active 会话快照数量
forge --check-config --json
forge --config-path
forge --restore-config-backup
forge --print-default-config
forge --shell-integration bash
forge --generate-completion zsh
forge-support-bundle ~/Desktop
```

`--doctor` 除配置语义和运行时依赖外，还检查配置权限、有效轮换备份、写锁、AI provider/密钥存在性、workflow 搜索位置、欢迎 Notebook、历史和 SSH 就绪度；不会发起网络请求。support bundle 使用额外的脱敏诊断模式，只收集权限/大小、计数、非敏感系统特征和选定环境变量的“存在/不存在”，不包含配置、命令/输出、会话内容、密钥、主机名或本地路径。分享前仍应逐项检查归档内容。

配置文件保存后会自动热重载；`Ctrl+Shift+R` 可手动重载。应用内保存会先验证 TOML 与语义、获取进程锁并检查磁盘 revision，再以 `0600` 临时文件同步、原子替换并轮换两份有效备份；冲突、锁超时、无效内容或 I/O 错误会显示原生提示，且不会覆盖磁盘。`--restore-config-backup` 可恢复最近的有效备份。

远程连接在最终 argv、重连、会话恢复和 remote Files probe 边界都会重跑同一个
应用级 gate（字段/总 argv 字节预算、视觉欺骗字符、目标语义与结构化 OpenSSH
选项）。`ssh_args` 可以使用 `-p 22`、`-o Name=value` 等选项，但不能塞入第二个
destination 或提前的 `--`。最多前 128 个 profile 可执行；索引越界或运行态被改坏的
profile 只显示安全有界诊断，不会 spawn。

日志支持普通级别和标准 target 指令，并输出进程内相对时间、级别与模块名：

```bash
FORGE_LOG=debug forge
RUST_LOG='warn,forge=debug,forge::state=trace' forge
```

`FORGE_LOG` 优先于 `RUST_LOG`；未知指令会被忽略，默认级别保持 `warn`。

CLI 补全可按需加载，支持 bash、zsh、fish 和 PowerShell，不需要额外运行时依赖：

```bash
# bash
source <(forge --generate-completion bash)

# zsh
source <(forge --generate-completion zsh)

# fish
forge --generate-completion fish | source

# PowerShell
forge --generate-completion pwsh | Out-String | Invoke-Expression
```

Block 模式可通过 `finished_block_viewport_rows` 调整长块出现顶部/底部导航控件的行数阈值；`block_compact = true` 可启用更接近 anvil/Warp 的紧凑块间距。两项配置均保持 GTK4 原生实现，不增加运行时依赖。

### 安装与更新 jsh

forge 优先使用配套 shell [`jsh`](https://github.com/beamiter/jsh)，找不到时才退回 bash。
命令面板中的 **Install or update jsh** 会在一个独立标签页里运行安装脚本：标签页本身就是进度界面，
可以 Ctrl+C 中断，脚本结束后等待 Enter 再关闭，失败原因不会一闪而过。

安装脚本来自 jsh 仓库并内嵌在二进制里，因此一台从未装过 jsh 的机器也能引导；校验和验证、
`rename(2)` 原子替换（**运行中的 shell 不受影响，新标签页才使用新版本**）、旧二进制回滚副本，
以及 `PATH` 上的 `jsh` 其实是同名的其他程序时的提示，全部由脚本统一处理。

缺少 jsh 或有新版本时，顶栏下方出现一条可忽略的提示条。检查在后台线程进行，从不自动安装：

```toml
jsh_update_check = "daily"    # "startup" 每次启动联网；"daily" 复用缓存；"never" 关闭
```

`daily` 复用安装脚本自己的缓存（`~/.cache/jsh/update-check.json`），同机同时开着多个 jterm 也只产生一次网络请求。
检查失败（离线等）时提示条保持隐藏，只写日志。

## 核心快捷键

| 功能 | 快捷键 |
|---|---|
| 新建 / 关闭 | `Ctrl+Shift+T` / `Ctrl+Shift+W` |
| 下一个 / 上一个标签 | `Ctrl+Tab` / `Ctrl+Shift+Tab` |
| 标签 1–8 / 最后一个 | `Ctrl+1`…`Ctrl+8` / `Ctrl+9` |
| 搜索 / 命令面板 | `Ctrl+Shift+F` / `Ctrl+Shift+P` |
| 左右 / 上下分屏 | `Ctrl+Shift+E` / `Ctrl+Shift+D` |
| 聚焦 / 调整 Pane | `Ctrl+Alt+方向键` / `Ctrl+Alt+Shift+方向键` |
| 复制 / 粘贴 | `Ctrl+Shift+C` / `Ctrl+Shift+V` |
| 配置 / 重载 | `Ctrl+Shift+O` / `Ctrl+Shift+R` |
| SSH 主机选择 | `Ctrl+Shift+S` |
| Block 历史 / 跨块搜索 | `Ctrl+Shift+H` / `Ctrl+Shift+G` |
| workflow / 失败块 / 最早块 | `Ctrl+Shift+M` / `Ctrl+Shift+X` / `Ctrl+Shift+N` |
| 全选 / 回填 / 清空 Block | `Ctrl+Shift+A` / `Ctrl+Shift+I` / `Ctrl+Shift+K` |
| 回填 / 重跑选中块 | `Enter` / `Ctrl+Enter` |
| Block 过滤 / 书签 / 标签栏位置 | `Alt+Shift+F` / `Ctrl+Shift+B` / `Ctrl+Alt+B` |
| AI 面板 / 询问选中块 | `Ctrl+Alt+Shift+A` / `Ctrl+Shift+Q` |
| Shell Agent（Block） | `Ctrl+Alt+G` |
| 字号增 / 减 / 复位 | `Ctrl+=` / `Ctrl+-` / `Ctrl+0` |

命令面板中的应用动作及其当前绑定可在 `Ctrl+Shift+P` 中搜索；这些动作可在
`[keybindings]` 中覆盖，设为 `false` 可解除绑定。Block 视图内的上下文键
`Alt+Shift+F`（过滤）、`Ctrl+Shift+B`（书签）和 `Ctrl+,` / `Ctrl+.`（前后书签）
目前固定，不会被 `[keybindings]` 覆盖。`Ctrl+R` 与 `Ctrl+P` 保留给 shell/readline；
Block 历史统一使用 `Ctrl+Shift+H`。

AI provider、model、endpoint 和 API key 均可在 Settings 的 **AI & Agent** 分组配置；面板输入的 key 会原子保存到独立的 owner-only 文件，绝不会写入 `config.toml`，环境变量仍具有最高优先级。AI 面板的分隔条宽度会随配置持久化。**New chat** 会创建并选中一个新会话，旧会话继续保留在可搜索的 **Chats** 会话库中；会话自动取标题，也可 Rename、Archive/Unarchive，Delete 前会要求确认。输入框使用 `Enter` 或 `Ctrl+Enter` 发送，`Shift+Enter` 换行，并保留输入法候选确认语义。请求期间可 **Stop**，失败或停止后可按原 chat/context **Retry**；选中 Block 会显示可清除的 context chip，输出被截断时 chip 会明确提示。空会话也提供只填充、不自动发送的快捷提示。

当前选择、每个 chat 的草稿和实际发送的选中 Block 上下文会跟随各自窗口快照恢复；快速关窗会先强制刷新草稿，发送失败或中途退出的问题也会作为可重试 draft 恢复，Ask selected Block 不会覆盖正在编辑的文字，关窗时其内存重试也会转成可恢复 draft/context。集合最多保存 50 条 chat metadata、每个 chat 最多 100 个 turn，紧凑 JSON 总预算仍为 8 MiB；超出总预算时只裁剪最旧的完整问答对，不会删除在途问题，并在对应会话显示 `truncated`。出站请求另保留最近至多 40 个 turn/256 KiB，单条输入、Block 输出和模型文本分别有 64 KiB、64 KiB、256 KiB 硬上限，可见 AI/Agent activity 各限制为 1 MiB，同时最多运行 4 个 provider 请求。非流式成功响应保持 raw bytes，先通过 jagent 的 1 MiB envelope gate 才解码 JSON；非 2xx 响应最多只有前 2 KiB 可进入诊断 JSON 解析器。窗口状态另为完整 chat metadata 预留 64 KiB，Pane/Tab 数据挤压空间时也不会静默删除整个 Chats 库。旧版单会话 schema v1 会自动迁移。后台回复始终绑定发起请求的 chat，切换不会串话，已经 Delete 的 chat 收到迟到回复时会直接丢弃。默认脱敏覆盖 active、non-active、archived chat 及其 draft/context；`--safe-mode` 与 `--no-restore` 的隔离和恢复语义保持不变。

命令面板使用模糊匹配；输入 `>` 只看动作、`@` 只看 JSONL 历史、`:` 只看 workflow、`?` 提交自然语言命令请求。历史和 workflow 只写入当前编辑行；`?` 请求会绑定当前 Block pane，在块流中显示可 Stop/Retry/Regenerate 的审阅卡，并携带可见的 selected Block 不可信上下文。它与命令纠正、Shell Agent proposal 共用可编辑、复制、动态风险提示的审阅逻辑，主操作只会 **Insert for review**，不会执行。所有审阅式插入都拒绝 CR、LF、Tab、NUL 和终端控制字符，避免多行条目越过“不提交”边界。`Ctrl+Alt+G` 或顶部栏的 **Agent** 开关会打开绑定当前 Block pane 的 Shell Agent；若打开时已选中 finished Block，它会作为可见、可移除的不可信上下文附加。Agent 显示目标、provider/model、shell、回合进度、activity 与实时 prompt readiness，可单独 Stop/Retry 当前模型请求，并可切换持久化的 typo-like 命令纠正。严格 JSON proposal 可复制、编辑、Reject、**Insert only** 或逐条 **Approve & Run**；Insert only 只回填普通 shell 编辑行并在 Agent 上下文记录“未执行”，危险命令执行仍需第二次确认。完成块的退出码和截断输出随后回灌到下一轮；`done` 后可用 **Follow up** 保留上下文追问，回合耗尽后可用 **New task** 在同一 pane 重置 Agent transcript 与预算。

若希望 Block 准确记录命令边界、退出码和 cwd，可加载内置 shell 集成：

```bash
source <(forge --shell-integration bash)
```

也可从已安装的 `share/forge/shell-integration/` 加载 bash、zsh、fish 或 PowerShell 脚本。

Block 模式与 anvil 保持相同的选择语义：`Ctrl+Up` 从最新块进入选择，`Shift+Up/Down`
扩展范围，普通 `Up/Down` 移动 active edge，`Enter` 按终端顺序把所有选中命令回填为
可编辑文本而不执行，`Escape` 取消选择。`Ctrl+Enter` 直接重跑**单个**选中块的命令
（右键菜单的 **Re-run Command** 等价）：仅限本 pane 里用户自己已经执行过的单行命令，
且提示符必须空闲、无未提交输入；多选、后台块和会被截断为首行的多行命令一律拒绝执行，
只回填。AI / Agent / 命令面板的建议仍然只 **Insert for review**，永不自行提交。
右键多选区域可批量复制命令、输出或完整块；长 Block 提供顶部/底部跳转与 sticky header，
后台异步输出使用独立 Block 样式。历史恢复和撤销清空重建的块与新块拥有完全相同的右键菜单。

命令运行超过 2 秒后，顶部会出现常驻的运行状态条（`▶ 命令 用时` 加一键 Stop），
不必先滚动离开底部才能看到；滚动到历史中时它会立即出现，并在 alt-screen 程序下让位。
被信号停止的命令（`130` SIGINT、`141` SIGPIPE、`143` SIGTERM）使用独立的 `⊘ interrupted`
中性样式，不计入失败：滚动条失败标记、`Ctrl+Shift+X` 失败跳转和 Failed 过滤都会跳过它们，
原始退出码仍完整保留在徽章、导出和历史中。

块内输出过滤（`Alt+Shift+F`）打开后可用 `Escape` 或再次 `Alt+Shift+F` 关闭，
查询文本会保留、焦点交还提示符。alt-screen 程序运行期间，`Ctrl+Up` 不会进入块选择，
`Delete` 也不会删除隐藏的块；进入 alt-screen 会清除既有块选择。

在块内拖选文本（用于复制）之后，键盘焦点会落在那张卡片上。此时所有 Block 快捷键
依然可用：方向键、`PageUp/PageDown`、`Home/End`、`Ctrl+Up` 进入选择、书签跳转、
`Alt+Shift+F` 过滤都照常工作；有选中块时 `Enter`/`Ctrl+Enter`/`Delete`/`Escape`
按卡片提示条的字面意思执行，没有选中块时它们仍然把焦点交还实时提示符。
普通打字始终把焦点带回提示符。

后台输出只会在提示符空闲且用户尚未开始编辑时归入独立 Block；一旦输入开始，后续输出保持在当前终端中，避免把 shell 回显、补全或交互输出错误拆块。

## 许可证

forge 以 **MIT OR Apache-2.0** 双许可证发布，使用者可任选其一；完整文本见
[`LICENSE-MIT`](LICENSE-MIT) 与 [`LICENSE-APACHE`](LICENSE-APACHE)。向本仓库提交
贡献即表示贡献者同意按相同的双许可证条款授权该贡献。仓库许可与 crates.io 发布
是两个独立决定，因此 Cargo 包目前仍保留 `publish = false`。

## 安全默认值

- 原始与出站脱敏后的 system prompt 都受 64 KiB 硬上限约束；可选分隔符与完整的 history 省略计数 notice 必须同时容纳，否则请求 fail closed，不会截断高信任指令。预先有界的 history 若仍被 jagent 报告省略，请求同样拒绝发送。
- AI 会话的公开 owning-string `Turn` 只支持序列化；恢复必须通过有预算的 `ConversationSnapshot::from_json` decoder，不提供可绕过该边界的普通 serde 反序列化。
- 新安装不会写入任何远程主机、用户名、IP 或个人路径。
- OSC 52 远程剪贴板写入默认关闭。
- AI 会话库默认对常见云密钥、PAT、JWT 和私钥进行脱敏，覆盖 active、non-active、archived chat 以及草稿和 Block 上下文。
- Agent 只支持显式选中的 Block pane；prompt 忙或已有输入时拒绝提交，危险模式会醒目标注，但最终批准仍由用户负责。
- Agent snapshot 只通过 jagent 的有预算 decoder 进入内存，Forge 再直接审计该 bounded view 的 proposal 连续性、观察生命周期和状态绑定，不会二次走普通 serde collection 解码。
- 可执行 Notebook 在独立进程组运行，关闭或停止 cell 会终止其进程组；安全模式完全禁用 Notebook 执行。
- 命令历史只保存 command、cwd、exit code 和完成时间，不保存输出，并限制单条/总文件大小。
- 每个窗口使用独立的原子会话快照；并发窗口互不覆盖，崩溃遗留快照会在下次启动回收。
- 配置、会话快照、JSONL 命令历史和 Block 历史使用 owner-only 权限；关键替换路径使用同步写入与原子 rename，降低信息泄露和断电损坏风险。
- `forge-support-bundle` 不读取或打包上述内容，只报告脱敏诊断与文件元数据，并以 `0600` 创建归档。
- 项目采用 `MIT OR Apache-2.0` 双许可证；Cargo 包仍有意保留 `publish = false`，不将仓库许可自动等同于 crates.io 发布。依赖继续由每周 RustSec 审计与 Dependabot 检查。

进一步说明见 [用户指南](docs/USER_GUIDE.md)、[架构说明](docs/ARCHITECTURE.md)、[Block 模式验收清单](docs/BLOCK_MODE_ACCEPTANCE.md)、[AI / Agent / Chat 验收矩阵](docs/AI_AGENT_CHAT_ACCEPTANCE.md)、[性能指南](docs/PERFORMANCE.md)、[发布流程](docs/RELEASING.md) 和 [Tailscale/SSH 配置](docs/tailscale-setup.md)。参与开发前请阅读 [贡献指南](CONTRIBUTING.md)、[安全策略](SECURITY.md) 与 [变更日志](CHANGELOG.md)。
