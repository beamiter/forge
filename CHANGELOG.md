# Changelog

All notable user-visible and operational changes are recorded here.

## Unreleased

### Highlights

- 卡片头部不再在鼠标下抖动：快捷操作按钮组原先在 hover 时才出现，它位于 header 的
  弹性空白之后，所以每次指针划过一张卡片，右侧的时间戳/耗时/exit 徽章都会横向滑动
  按钮组的整个宽度。按钮组现在始终占位、只做淡入淡出（淡出时不可点击也不吃 Tab 焦点），
  选中提示条也移到弹性空白左侧，出现时吃掉空白而不是推动右列。时间戳、耗时和 exit 徽章
  补上了省略号，窄分屏里不再把 header 撑出 pane。卡片的输出、折叠摘要和图片现在共用一条
  与 `❯` 对齐的左边界，折叠不再让文字横跳。
- 跨块搜索（`Ctrl+Shift+G`）的结果行显示 `out L482`，点进去却不落在 482 行：跳转只是
  装上正则并从上一次跳转留下的选区往前走一步，所以落点是"下一个命中"，重复激活同一行
  还会一路向后走。结果行现在带着自己在该 surface 中的命中序号（按命中数计，不是按行数：
  一行里的三个命中会让 VTE 的游标走三步），跳转从清空的选区开始按序号定位，步数有上限，
  因此同一行重复激活总是落在同一处。
- 在块内拖选文本（复制用的那个手势）之后，键盘焦点会留在那张卡片上——从那一刻起
  所有 Block 快捷键都失效：方向键、`PageUp/PageDown`、`Home/End`、`Delete`、书签跳转、
  过滤快捷键全部没反应，因为整个键面只挂在实时 VTE 一个控件上，而
  `stranded_focus_key_recovers` 恰恰把这些键排除在"打字才交还焦点"之外。同一个处理器
  现在挂两处：实时 VTE 一份，pane root 一份，后者只在焦点确实落在本 pane 的卡片上
  （不是实时终端、不是文本输入框、不是弹出菜单）时才接管。有选中块时
  `Enter`/`Ctrl+Enter`/`Delete`/`Escape` 归选择所有——卡片提示条正好承诺这四个动作；
  没有选中块时它们保持原来的"交还焦点"含义。
- Block 模式体验一轮：卡片上一直宣传却没人实现的 `Ctrl+↵ run` 现在真的重跑选中块——
  仅限本 pane 里用户自己跑过的单行命令，提示符必须空闲，多选/background/会被截断的多行
  命令一律只回填（右键新增等价的 **Re-run Command**）；模型给出的候选仍然只能
  Insert for review。历史恢复与撤销清空重建出的卡片终于和新卡片拥有同一套右键菜单——
  三条挂载路径合并成一个 helper，不会再各自漂移。被信号停止的命令（130 SIGINT、
  141 SIGPIPE、143 SIGTERM）不再画成硬失败：中性的 `⊘ exit:N · interrupted` 卡片，
  不进滚动条失败标记、失败跳转和 Failed 过滤，原始退出码在徽章/导出/历史中完整保留，
  而 SIGSEGV/SIGABRT/SIGQUIT/SIGKILL 这类真故障仍然是红色。命令跑过约 2 秒后，顶部
  常驻运行状态条（`▶ 命令 用时` + 一键 Stop）不再要求先滚动离开底部才出现。
- Block 内的搜索与过滤修了三处会骗人的地方：每次查询前先丢掉卡片 VTE 上遗留的选区，
  否则从上一次命中之下向前搜索会对屏幕上看得见的文字报 "No matches"；卡片因 resize、
  Expand 或输出过滤被重新灌入后，find 记录的原生游标已经失效，现在会带 render stamp
  识别并就地重建整轮搜索，而不是报 No matches 或跳错位置；块内过滤框不再是键盘单向门，
  `Escape` 或再次 `Alt+Shift+F` 即可关闭并把焦点交还提示符，查询文本保留。
- 一条外来的 OSC 133 `D` 不再替本地命令收尾：`ssh`、`docker exec`、`tmux attach` 或
  `cat` 一份含这些字节的日志，都会让本地卡片提前结束并盖上远端命令的退出码和耗时。
  `on_command_end` 现在与 `on_command_start` 的前台判定对称——前台属于别人时拒绝该标记，
  等 shell 拿回终端后用它自己的 `D`（即真正的退出码）收尾。
- Block 卡片的四种状态不再互相覆盖：失败、hover、选中、书签曾经全部经由 `box-shadow`
  和 `background-color` 表达，文件里最后一条同优先级规则通吃——把鼠标移到失败卡片上会洗掉
  它的红色，给卡片加书签会抹掉它的选中环。书签改用独立的 `background-image` 通道，
  失败/选中与 hover 的组合各有显式规则，并有单元测试守住"新状态必须写复合规则"。
- Block 模式的三处主线程开销：完成一条命令时不再为同一个行数把整份 transcript 走第二遍
  （大输出结束到提示符回来的停顿约减半）；`Ctrl+滚轮` 缩放把一次滚轮串合并成一次控件遍历，
  被虚拟化的卡片记下目标字号、回到视口时才采用，显示器级样式表在文本没变时不再重装；
  alt-screen 期间 `Ctrl+Up` 不再选中隐藏卡片、`Delete` 不再删除它们，进入 alt-screen 会清除既有选择。

- Unified 模式现在原生显示 Kitty `a=T` 图片：分块上传保留首块 `r/c/C` 与最终块 cursor，
  nonce 行探针在滚屏、半格滚动和 rewrap 后重新定位图片，ED3/RIS/alt-screen 按可信行边界
  管理生命周期；探针成本按可见行 × 唯一 placement 列有界。Block 的图片预算也统一计入
  PNG backing 与 GTK 对象成本。Block/Unified 完成记录新增独立于 exit status 的 provenance/
  health；缺失 OSC 133 D 只在 shell 重获前台时恢复，且不虚构耗时或结束时刻。每个已接受
  的 C 现在也只关闭一次 observer lifecycle：可信 A 恢复会在 Block/Unified finalize 前发出
  unknown/degraded finish，之后的 A、后台输出与 RIS 都不能重放或伪造 finish，organism 与共享
  activity 不会再因丢失 D 而永久停在 running。
- 设置面板的 Remote Hosts 现在能**编辑**已保存的主机，不再只有添加和删除。每行新增
  铅笔按钮，用同一个对话框打开（标题与按钮变成 Edit / Save），条目就地替换，因此在
  选择器里的位置不变。对话框没有控件的字段——`ssh_args`、`session`、`remote_shell`、
  `login_shell`、`multiplex`、`deploy_artifact`——原样保留并在对话框里列出，行的副标题
  也会显示 `ssh_args`：改个名字顺手把 `-p 2222` 删掉，正是事后没人会去核对的那种改动。
  改名不再和自己撞重名检查；对话框打开期间配置被重新加载时，按名字重新定位条目，找不到
  就报错而不是悄悄新增一条。
- 远程主机现在严格按用户配置启用：缺少 `remote_hosts`、配置不可读或配置无效时都使用
  空列表，不再注入示例目标。可复制的 SSH/容器写法只保留在示例配置与用户指南中，避免
  新安装或损坏配置在主机选择器里呈现并尝试连接并非用户添加的地址。
- 高频设置保存改为 250 ms 去抖的后台 latest-wins 队列；拖动透明度、字体缩放和 AI 面板
  宽度不再阻塞 GTK 主线程。文件监听以内容 revision 识别应用自己的写入，外部冲突与写盘
  失败继续保留内存设置并给出可操作的界面提示，窗口关闭前会提交并有界等待最后一次写入。
- 剪贴板边界现在覆盖所有 Block 复制入口：跨 VTE、选中块、输出和 Markdown 都流式写入
  32 MiB 上限，超限时整次复制原子失败；粘贴在改变选区或编辑状态前按 256 KiB 上限预检，
  队列拒绝时不会发送命令前缀或制造“已经粘贴”的假象。
- 默认字体改为系统可用的 `Monospace 14`，纯图标操作恢复键盘焦点与可访问名称，并以 GTK
  symbolic icon 取代 Nerd Font 私用区字符。八套内置主题的普通、次级和语义状态文字都经
  4.5:1 对比度校正；终端中的 ANSI 原色保持不变。
- Block 内搜索改为 150 ms 可取消去抖，并按 VTE surface 压缩命中状态；查询限制 8 KiB，
  单次最多记录 10,000 个命中、扫描 4 MiB，并以 12 ms 为 surface 间停止目标，不再为稀有查询遍历并永久复制全部历史。无结果、
  无效/零宽正则、精确计数与扫描受限都有独立状态，截断结果不会越过已知边界环绕。
- 后台持久化队列新增 512 MiB 全局 retained-byte 预算，运行中与待处理 snapshot 共同计账，
  被拒绝的替换不会丢掉已排队版本，后续成功也会清除尚未展示的旧失败。完成 Block 另有
  每 pane 128 MiB 保留预算，按实际 ANSI 输出、重复副本、VTE/控件和图片成本估算，历史
  恢复同样先预算再创建控件。每个完成 VTE 另有 1,048,576 cells / 4096 列的绝对几何上限，
  Kitty 图片每块最多 64 张并计入对象成本；VTE 几何裁剪不会再额外影响复制/导出中已捕获的文本（最多 8 MiB）。命令历史面板最多实例化最近 500 行。
- 快捷键配置现在以完整 effective map 原子校验/应用：与未修改默认组合冲突会在
  `--check-config` 中指出占用动作，显式交换或解绑后转移仍可用；GTK 已兑现 Super/Meta
  与数字小键盘数字映射。搜索、AI、远程主机和标签图标补齐可访问名称，标签过滤框恢复
  Tab 可达；调试脚本遵循 `FORGE_CONFIG`/XDG，默认隐藏绝对路径，敏感 strace 需显式授权。

#### Reliability and UX

- AI endpoint 校验与升级后的 jagent 共享传输契约对齐：Anthropic、OpenAI-compatible 和
  Ollama 都可连接明确的 `localhost`、IPv4 loopback 或 `[::1]` 明文 HTTP 服务；任何远程
  HTTP endpoint 仍会在读取凭据或发起网络请求前失败。

- 顶部栏模式下只有一个标签页时，标签不再被藏起来：标签条在该模式下始终显示，当前
  标签的标题（含 OSC 改名与 cwd）一直看得见，新建/关闭标签时顶部栏也不再改变高度与
  布局。原先 `sync_tab_bar_visibility` 用 `n_pages() > 1` 决定顶部标签条的可见性，
  单标签时整条隐藏，只剩一排图标按钮。撑开右侧控件的 spacer 仍由同一处开关，顶部栏
  模式下由标签条负责扩展，侧边栏模式下交回 spacer。

- 顶部栏模式下侧边栏的 Tabs 视图不再是空的——它现在始终列出所有标签。侧边栏镜像列表
  按每个标签按钮的 `is_visible()` 判断该行是否被过滤掉，而 GTK 的 `is_visible()` 连
  同祖先一起回答：单标签时顶部标签条整条隐藏，于是所有按钮都报告"不可见"，镜像把每
  一行都当成被过滤而隐藏。改用按钮自身的 `get_visible()`（即过滤器唯一写入的那个标
  志），镜像只反映过滤结果，与标签条容器当下是否可见无关。

- 窄面板中的完成 Block 不再在两帧内容之间持续闪烁。根因：完成块的行数与高度一直按块
  完成时记录的列宽计算，而 VTE 的网格实
  际跟随分配宽度换行——面板一旦更窄，每次重挂载都会先按记录列宽申请偏矮的高度，随
  后异步 settle 按真实换行量把卡片撑高，高度往返抖动又推动外层滚动与虚拟化反复重挂
  载边界卡片，形成自持振荡。现在渲染列宽取记录宽度与实际分配宽度的较小值（面板更宽
  时仍按记录宽度保留原始换行），输出 VTE 记住上次渲染的几何（列宽、行数上限、展开态、
  过滤文本代），几何未变的重挂载不再重新喂字节；宽度变化也会触发已有的整批 re-fit
  （原先只有高度变化会触发），命令行 VTE 在窄面板下同样按换行后的行数申请高度。

- 配置文件因为权限被拒绝时不再是一场无声的回退。forge 要求配置文件的父目录不能被组或其他人
  写（umask 0002 下手工建出来的 `~/.config/forge` 正好是 775），此前这种情况只写一行
  `log::warn!` 就退回内置默认值——主题、快捷键、`[[remote_hosts]]` 全部失效，界面上没有任何
  迹象，`--doctor` 里的 `remote hosts: none configured` 还标着 `(ok)`。现在：窗口启动时弹出
  一条不自动消失的 toast 说明配置未加载及原因；不可读、非 UTF-8、TOML 语法错误与
  语义错误现在走同一条可见失败路径，语法错误给出脱敏后的行列号与
  `forge --check-config <path>` 操作提示；权限错误直接给出可执行的补救命令
  （`run: chmod g-w,o-w <目录>`）；`--doctor` 在配置未加载时把远程主机一行标为
  `unknown: the configuration file was not loaded (warning)`，不再假装配置里什么都没有。
- `--doctor` 的远程主机检查按 transport 分别判断：ssh 目标看 `ssh`，`docker = true` 的目标看
  `docker`，此前无论目标是什么都只报告 ssh 是否可用。

- 精确锁定的共享 core 尚未发布本轮安全修复时，forge 现在通过可独立构建的本地兼容层
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
  时间和目录扫描量；跨进程保存持有不可被锁文件替换绕过的目录锁，磁盘版本不匹配的陈旧 writer
  会拒绝写入并要求重载，不再猜测合并而复活已删除历史；任何损坏的旧文件都不会被下一次保存覆盖。GTK 线程在复制历史
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

- 桌面集成安装后应用列表里没有 forge 图标：三处成因已分别修复。其一，条目里的 `Exec=forge` / `TryExec=forge` 依赖 `PATH`，而桌面会话的 `PATH` 在登录时固定，默认安装位置 `~/.local/bin` 常常不在其中——`TryExec` 失败会让条目整个从应用列表里消失；`scripts/install.sh` 与发行包的 `install-release.sh` 现在把这两行改写成二进制绝对路径（`/usr/bin` 等系统 bin 目录仍保持相对形式以便重定位）。其二，安装脚本从不刷新桌面缓存，新条目与新图标要等下次登录才可见，陈旧的 `icon-theme.cache` 甚至会一直盖住刚装进去的图标；现在安装与卸载都会校验条目并刷新 `update-desktop-database` 和 `gtk-update-icon-cache`（`DESTDIR` 打包时跳过，且这些缓存以放宽的 umask 生成，避免 `sudo --prefix /usr` 装出别的用户读不到的 `0600` 缓存）。其三，`StartupWMClass` 写的是 application ID，而 GTK4 的 X11 `WM_CLASS` 取自程序名（实测为 `forge`），X11 会话下窗口因此无法与条目关联，dock 里会多出一个没有图标的重复项；现已改为 `forge`，Wayland 侧仍按 app_id 匹配不受影响。
- 安装脚本现在会提示 `PATH` 问题：目标 bin 目录不在 `PATH` 中，或 `PATH` 上已有另一份 forge（例如旧的 `cargo install` 副本）会遮蔽刚装好的二进制。

- Block 模式短输出的伪滚动条与行数不一致：同一条 `ls` 有时完整显示、有时只剩末尾两行且带块内滚动条，成因有二并已分别修复。其一，VTE 会按**实际分配的内容高度**重新推导网格行数，而 CSS 边框/内边距的记账差异可能让分配高度比 `行数 × 行高` 少几个像素——网格因此少一行，快照首行被挤进 scrollback，本可完整显示的输出多出滚动条；现在所有 finished VTE 的高度请求都带一个小于一行的像素余量（`finished_vte_height_px`），网格行数不再因像素记账掉行。其二，`feed()` 是异步的，负载下（如另一标签页在流式输出，VTE 的处理调度器为全进程共享）固定两次 idle 的定稿测量会落在喂入中途，把网格缩到当时恰好渲染的行数并永久卡在底部锚定；定稿现在改为确定性完成信号——轮询到快照的最后一行确实已渲染（封顶 2 秒后兜底）才测量收拢，封顶前的截断快照则每拍重申顶部锚定直到缓冲溢出或尾行可见。附带一个需要显示环境的忽略态回归测试（`diag_short_ls_block_geometry`），在真实 GTK 分配下断言 6 行输出恰为 6 行网格、无滚动条、顶部锚定，长输出保留视口滚动。

### Added

- `[[remote_hosts]]` 新增 `deploy_artifact`：指定一份本机构建的 jsh 交给 `deploy` 推送，
  代替去取已发布的 release。这是在"本机 jsh 版本还没发 release"或者离线时唯一能真正部署的
  方式——否则部署会先连不上发布地址，再静默降级成 shell 集成（Block 还在，jsh 的补全没了）。
  必须是绝对路径：相对路径会相对标签页启动目录解析，`-` 开头会被 launcher 当成选项，两种都
  直接拒绝该主机而不是悄悄忽略。`--check-config` 会警告文件不存在、或写了它却没开 `deploy`。
- `[[remote_hosts]]` 新增 `docker = true`：`host` 改为一个**正在运行的**容器名，标签页
  经 `docker exec` 而不是 ssh 连接，`user` 变成容器内用户（`-u` / 部署时的
  `--docker-user`），`deploy` 照常决定要不要把 jsh 送进去（送进去的一路已端到端验证：
  补全、菜单、OSC 133 块标记、窗口 resize 与本地一致）。共享库
  `jterm_core::jsh_remote` 和 `jsh-remote.sh` 早就支持 `--docker`，只有 forge 这一侧
  把它硬编码成了 `false`，因此配置里根本写不出容器目标。`ssh_args`、`multiplex`、
  `login_shell` 对容器无意义，写了会给出警告并忽略，而不是让主机加载失败。
- Block 模式运行中命令的体验改进（针对 claude 等长时流式 TUI）：
  - **运行中可框选文本**：在 live 终端面上拖选时，PTY 字节流被暂存（选区期间 + 松手后最多 5 秒宽限，或复制/输入/点击别处即恢复，上限 2 MiB），高频重绘不再瞬间冲掉选区；Shift+拖选在开启鼠标上报的应用里同样受保护。暂存生效时左下角显示 "Output paused — selection" 徽标，消除"卡住了"的错觉。
  - **运行中可回看输出**：滚轮在 live 终端面上优先滚动当前命令自己的回滚缓冲，滚到顶/底才交给外层 Block 历史（此前 VTE 吞掉滚轮且新输出会把视图拽回底部，运行中的早期输出实际不可达）；右侧出现细滚动条（overlay 覆盖式，出现/消失不改变列宽、不触发 SIGWINCH），跳底按钮同时归位内外两层滚动。空闲提示符上的滚轮现在可靠地滚动 Block 历史。
  - **运行中可搜索**：Ctrl+Shift+F 现在把正在运行命令的已产生输出纳入匹配（排在所有完成 Block 之后），VTE 原生高亮并支持 Next/Prev 跨面步进；关闭搜索一并清除 live 面高亮。
  - **sticky 运行头更实用**：向上翻历史时的运行中头部新增 Stop 按钮（一键发送 Ctrl+C，无需先找回终端焦点），耗时超过一小时显示 `1h04m` 格式。

- AI 聊天面板流式回复（`ai_stream` / `FORGE_AI_STREAM`，默认开启）：回答在生成过程中逐段显示在会话里，三个 provider（Anthropic、OpenAI-compatible、Ollama）均支持；完成时以 provider 返回的完整文本替换进行中的消息并原样落库，保存的会话与关闭流式时完全一致（包括 `ai_max_tokens` 截断提示）。中途出错时已显示的部分内容保持可见，错误照常提示并可 Retry；Stop 与关窗仍会中断流式 curl。流式期间切换 chat 不会把片段写进别的会话，切回后已收到的部分回复会完整重现。仅聊天面板流式；Shell Agent、命令生成与纠错等严格 JSON 表面继续等待完整回复。开关同时提供于 Settings（Stream Chat Responses）。

- Kitty 图形协议（对齐 anvil 的最小子集）：Block 模式解码 APC `G` 序列（`kitten icat`、matplotlib kitty 后端等的内联图片），把 PNG（`f=100`）与原始 RGBA/RGB（`f=32`/`f=24`）的 base64 直传载荷（含 `m=1`/`m=0` 分块）渲染为完成 Block 内文字输出下方的 GTK Picture，折叠按钮把图片与文字一并收起，纯图片命令也保留可折叠的 Block；此前这些序列被转发给没有图形协议的 libvte 而被静默丢弃。内存上限与 anvil 完全一致：单图编码 16 MB、单 Block 解码合计 16 MB、边长至多 16384，超限静默丢弃；文件/共享内存传输与 `a=d`/`a=p` 动作不支持。与 anvil 不同的是带 `i=`/`I=` 标识的命令会按 ember（家族参考应答实现）的语义在 PTY 上收到 `OK`/`EINVAL`/`ENOTSUP` 应答并遵守 `q=` 静默级别，`a=q` 探测会被校验后应答，因此 `kitten icat` 不再因等待应答而超时。图片仅在本次会话内展示，不写入 Block 历史，会话恢复后自然省略。
- OSC 9 / OSC 777 桌面通知：终端内程序（含 SSH 远端）可通过 `ESC ] 9 ; 正文 BEL` 或 `ESC ] 777 ; notify ; 标题 ; 正文` 请求桌面通知，由 `notify-send` 以 forge 身份发出（缺标题时回退为应用名）。文本在共享解析器中去除控制字符并截断至 256 字符，空通知不发；由于序列源自 PTY 内部，进程派生受应用级速率限制——每批至多一条、距上一条不足 2 秒的一律丢弃。
- 一键安装/更新配套 shell `jsh`：命令面板的 **Install or update jsh** 在独立标签页里运行安装脚本（标签页即进度界面，可 Ctrl+C 中断，结束后等待 Enter 再关闭）；缺少 jsh 或有新版本时顶栏下方出现可忽略的提示条。安装脚本来自 jsh 仓库并随二进制内嵌，校验和、原子替换、回滚与「`PATH` 上的 `jsh` 是同名其他程序」的提示都由它统一处理。更新检查在后台线程进行、从不自动安装，检查频率由 `jsh_update_check`（`startup` / `daily`（默认）/ `never`）控制，缓存与同机其他 jterm 共享。

- Shell Agent 会话进行中可用 **Attach selected Block** 附加或替换当前选中的 finished Block 作为不可信上下文，不再局限于开卡瞬间的一次性捕获。
- Agent 请求现在附带有界的 git 元数据（branch、dirty、ahead/behind），与 cwd/shell/OS 一样仅作为不可信 user-role 数据发送，帮助模型贴合仓库状态提出命令。
- Bash、zsh、fish 与 PowerShell 的内置 CLI 补全生成（`--generate-completion`）。
- 顶栏、标签栏与文件导航的完整键盘焦点链、语义化无障碍标签，以及 Enter/Space 标签激活。
- Project licensing under `MIT OR Apache-2.0`, including canonical license texts, Cargo/AppStream metadata, inbound-contribution terms, and license files in release artifacts.
- Reproducible GNOME 50 Flatpak packaging, stable desktop application ID, AppStream metadata, scalable/raster icons, checksummed CI bundles, and X11/Wayland VTE/Block smoke tests.
- A Flatpak host-command bridge so shells, SSH, Git probes, AI curl requests, and desktop notifications operate on the host instead of the application sandbox.
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
- Privacy-preserving `forge-support-bundle` diagnostics plus richer doctor checks for config permissions/backups/locks, provider readiness, workflows, Notebook assets, history, display, and remote tooling.
- Review-only workflow examples for interactive rebase, SSH port forwarding, Docker log streaming, and signaling a process by listening port.
- Deterministic relocatable Linux release archives with SHA-256 checksums, a user-local bundle installer, tag-driven release publishing, and Nix package/app/check outputs.

### Changed

- Kitty 图形协议的结构层迁移到共享库 `jterm_core::kitty_graphics` 并删除本地副本：控制段解析、`m=1` 分块重组、base64 解码、原始像素长度校验与 PNG IHDR 预检现在与 anvil/ember/frost 使用同一份经测试加固的实现（52 个用例），本仓库只保留 GDK 解码路径、`a=q` 探测校验、PTY 应答（`OK`/`EINVAL`/`ENOTSUP` 与 `q=` 静默级别）以及 Block 级图片预算。内存上限沿用同一组数值（`Caps::BLOCK`：单图编码 16 MiB、解码 16 MiB、所有在途上传合计 16 MiB、边长至多 16384、控制段至多 16 KiB）。因家族统一而产生的行为变化：`f=` 缺省从 PNG 改为协议规定的 RGBA（`f=32`），因此不带 `f=` 的命令现在要求 `s=`/`v=`；原始像素载荷长度必须与 `s*v*通道数` 精确相等，此前允许多余尾部字节；`t=f`/`t=t`/`t=s` 从静默忽略改为明确应答 `ENOTSUP`；`f=` 只接受 `100`/`32`/`24`；`i=` 与 `I=` 不能同时出现；分块续传只能携带 `m=`（可选 `q=`），重复元数据的续块会中止该次上传并应答 `EINVAL`（此前会被当作续块接受）；base64 拒绝长度模 4 余 1 与中间出现的 `=`；控制键之间不再容忍空格。带 `i=`/`I=` 的命令仍照旧收到应答，包括共享解析器拒绝的命令。
- 进程探测与 shell 引用迁移到共享库 `jterm_core::process` 并删除本地副本：`/proc` 的 cmdline/ppid/stat 解析、PTY 前台进程发现、可恢复命令识别（ssh / mosh / `nix develop` / `docker|podman exec`）与各处 shell 单引号包装现在与 anvil 使用同一份经测试加固的实现。行为上的两点变化：前台进程识别现在先检查 PTY 子进程本身（与 anvil 一致），因此受管 ssh/mosh 面板的命令也能被检测与命名；单引号转义风格从 `'\''` 统一为 `'"'"'`（两者皆为合法 POSIX 引用），涉及 `bash -lc` 登录包装、jsh 的 `exec` 行（改用 core 的 `build_jsh_exec_command`）与文件树的路径插入（改用 core 的 `shell_quote_path`，明显安全的路径不再加引号、保持可读）。`startup_commands` 的逗号分隔配置语法不变，但只在应用边界解析一次，终端后端不再自行拆分。
- Block 模式长输出改为块内滚动（对齐 anvil）：超出当前 pane 可视高度的 finished Block 不再撑满外层历史，而是保留自适应视口并在右侧显示专属滚动条，鼠标滚轮与滑块都只移动该 Block，滚到首/末行才把滚动交还外层历史；短输出仍取自然高度、不显示滚动条，展开按钮可把单个 Block 恢复为整段铺开。改变窗口或分屏大小时，屏幕上已可见的 Block 会立即按新高度重新适配（此前只有滚出视口再回来的 Block 才会重算），虚拟化高度同步更新，展开状态让位于新的 pane 尺寸。
- Block 头部元数据更易读：分钟级耗时保留秒数（`1m32s`、`1h04m`），不再丢失 61s 与 179s 的差异；信号退出码标注信号名（如 `exit:130 SIGINT`、`exit:137 SIGKILL`，悬停解释 128+n 约定），长命令完成通知同样附带信号名；从历史恢复的非当日 Block 时间戳带日期（`MM-DD HH:MM`），悬停显示完整本地日期时间，旧输出不再伪装成当天结果。
- Block 前台与后台输出改用固定上限环缓冲，持续大输出不再为每个 PTY chunk 搬移整个 8 MiB 尾缓冲。
- 关闭 pane/tab 会同步关闭 Block PTY 输入 worker、释放 GTK root controller，并让 live VTE、完成块、右键菜单、滚动/筛选/选择控制器以弱引用跨越 widget 边界；批量关闭标签或嵌套分屏后读写线程可回落到基线。
- 嵌套分屏折叠会在 `GtkPaned` 子节点仍有效时清空根焦点，再聚焦保留的 sibling，消除关闭聚焦分屏时的 GTK 运行时警告。
- `remote_hosts` 配置现在完整校验嵌套字段、未知键、类型、空值和控制字符。
- Block 样式不再注入未使用且 GTK 无法解析的组合关键帧规则，消除每次新建 Block pane 的 CSS 警告。
- Default shortcuts now share the jterm ergonomic layout: directional Pane
  focus/resize layers, browser-style tab digits, symmetric zoom/opacity keys,
  and shell-owned `Ctrl+P` passthrough.
- Session autosave 与 Block history 读写现由有界单 worker 串行处理，同一目标的待写快照采用 latest-wins 合并；后台失败会显示去重提示，关闭窗口前会保存并 detach 全部 pane、发布最终 session snapshot，并有界 flush 队列。
- Block is now the default terminal backend. New splits inherit the focused pane's backend, so Block and VTE layouts both remain structurally consistent.
- Repeated same-axis splits rebalance by pane-tree span, keeping three or more panes evenly sized instead of recursively squeezing newer siblings.
- Directional pane focus now recognizes the complete focused Block/VTE subtree and retains the last active leaf across transient container focus, so all four focus shortcuts work from finished blocks and other pane descendants.
- Runtime configuration updates propagate to Block leaves nested in pane trees.
- Pane-to-tab moves preserve stable process/session identities, tab chrome, and remote reconnect ownership across repeated primary or remote pane moves.
- Application config saves now validate syntax/semantics, serialize through an advisory lock, reject stale revisions, rotate two valid backups, and use private durable atomic replacement.
- Safe mode now constructs a fully isolated built-in VTE profile without reading user config or behavior overrides; configuration reload is disabled and save failures are visible in the UI.
- The installer, uninstaller, and Flatpak bundle now manage shell integration, workflows, and Notebook runtime assets under `share/forge`.
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
- 快捷键 chord 的解析/显示迁移到家族共享的 `jterm_core::keybindings`：配置里的 chord 语法按家族并集放宽——新增 `control`/`option`/`cmd`/`win`/`meta` 等修饰键别名、`esc`/`del`/`ins`/箭头等按键别名、`f1..f24`、更多符号名与 Unicode 字母，重复修饰键（含别名，如 `ctrl+control+t`）现在会被报错拒绝；解除绑定除 false、空串、"none"、"disabled" 外新增 "unbind"。`--doctor`/加载期的 chord 静态校验与运行时覆写走同一解析器，校验通过的 chord 必定能生效（此前两边各自实现可能不一致）。显示形式保持既有的 `Ctrl+Shift+Alt+…` 修饰键顺序与 `Enter` 拼写不变；唯一变化是侧栏快捷键现在显示为 `Ctrl+\` 而非泄漏 X11 keysym 名的 `Ctrl+backslash`（配置里两种写法都照常解析）。默认键位与冲突拒绝策略不变，新增测试钉住家族 38 项默认 chord 契约；示例配置中 resize/AI 面板 chord 注释改为与显示一致的 `Ctrl+Shift+Alt+…` 顺序。旧配置里任意 X11 keysym 名（如 `Menu`）作为键名的写法不再被接受，会在加载时得到明确警告。

- 终端子进程的终止与回收迁移到共享库 `jterm_core::process`（`ChildLifecycle` / `ProcessRef` / `EscalationPolicy`），删除本地的 `process_exists`、`signal_pid_and_group`、`wait_for_process_exit`、`terminate_terminal_process`。pane 现在随 widget 保存一个内核绑定的子进程句柄（Linux 上是 pidfd）而不是裸 pid，并显式记录由谁 `waitpid`：Block pane 的 PTY 由 forge 自己 fork、自己回收；常规 VTE pane 的子进程由 glib/VTE 的 child watch 回收，其生命周期只经 pidfd 发信号、绝不 `waitpid`，退出码由 `child-exited` 记账。HUP → TERM → KILL 阶梯与既有的 120ms/250ms 宽限期不变，但最后一发 KILL 改为按 PTY 会话清扫（两个 backend 都对 PTY 子进程 `setsid`）：关闭 pane 时会一并清掉 shell job control 放进别的进程组、此前只补一发进程组信号会遗漏的后台任务。常规 VTE pane 的 `child-exited` 状态现在按家族约定归一（正常退出取退出码、被信号杀取 128+信号号），与 Block pane 上报的数字一致；此前把 `waitpid` 原始状态字直接当成退出码交给远端重连逻辑（只有 0 的语义碰巧是对的）。窗口快照“属主进程是否还在”改为纯 `/proc` 探测，不再向别的 forge 窗口发信号 0。

### Fixed

- Block 模式下"每输入一条命令，整个 block 先占满全屏、一闪而过再恢复"的闪屏已修复。
  live cell 的高度此前由**状态**决定而不是由内容决定：`CommandStart` 一到就从 ~6 行的
  prompt 直接跳到整个视口，并一直保持到下一个 prompt —— follow-bottom 把所有已完成块顶
  出屏幕，而最终替换它的卡片只有输出那么高，于是每条命令周围都套着一次"整页空白出现再
  塌陷"的闪烁，哪怕这条命令只打印一行。现在 live 卡片跟着**已经产生的输出**长高
  （`max(MIN_INPUT_ROWS, 已写入行数)`，以视口封顶），也就是 ember / frost 一直在用的规则：
  历史留在屏幕上，输出流进来时一行一行地向上平移。

  底下的终端没有变：运行中的命令仍然通过 `vte.set_size` 拿到整个视口的网格——那正是
  `pty_grid_size` 告诉子进程的 winsize，也是任何按绝对行号重绘的程序（`top`、`watch`、
  裸 `clear`）需要的行数。变短的只有**卡片**，靠一层 clip 实现：`gtk4::Fixed` 放在一个
  不参与测量、`Overflow::Hidden` 的 overlay 里，终端拿到自己请求的完整高度，而卡片只测量
  一个占位 Box。GTK 是从分配尺寸推导 VTE 网格的，所以别的控件都做不到这件事——
  ScrolledWindow/Viewport 和普通的非 FILL overlay 子控件实测都会把网格压到可见高度。

  行数从 live 终端本身读出（屏幕顶端到光标），并按命令锁存一个高水位，因此 `\r` 进度条
  或 `ESC[1A` 重绘不会让卡片缩到已经显示的输出之下。命令若清掉了 scrollback（`ESC[3J`），
  VTE 的 adjustment 与光标会落在两套坐标里，这种情况会被识别出来并退回原来的整页卡片，
  而不是冒着藏住输出的风险。`preserve_live_scrollback = true` 时同样退回整页。
- Block 模式下概率性出现的“输入时什么都不显示、按下回车后整行命令才一次性出现并正常执行”已修复。
  `OSC 133;B` 之后 forge 会短暂拦下 PTY 字节（feed fence），让 prompt 光标锚点先稳定下来再回放，
  这个拦截的唯一所有者是锚点稳定回调。此前该回调在**放弃**稳定时（用户已经在编辑、有待提交输入、
  上一条命令留下 typeahead，或光标在安全期限内始终没稳定）只回放当时已拦下的字节就结束，却把
  `prompt_anchor_ready` 留在 false —— 于是同一个 prompt 后续每一段字节（正是 shell 对用户按键的回显）
  继续被拦下，而下一次真正的排空要等到 `CommandStart`，也就是回车之后。最容易复现的是命令运行期间
  先打字（typeahead）：下一个 prompt 一打开就是 dirty 的，稳定回调第一帧即放弃，该 prompt 的输入
  全程不可见。现在放弃与超时路径都会显式释放 fence，其后的字节直接进入 live VTE；被更新的 PromptEnd
  取代的回调则完全不碰环形缓冲与标志（那属于新的 prompt）。锚点丢失时的命令捕获行为不变——它本来
  就已按 `prompt_anchor_ready` 回落到输入影子文本。

- 关闭 pane / 标签页 / 窗口时的四个子进程缺陷（PID 复用与僵尸）一并消除，均由共享的 `ChildLifecycle` 在类型上保证：**其一**，终止不再先对裸 pid 发信号、事后才读 `/proc` 判断要不要打进程组——句柄是内核绑定的，pid 被复用时信号直接 `ESRCH` 失败，负 pid 的进程组信号也改为先验证组长身份再发，此前那句"防 PID 复用"的注释与实现并不相符。**其二**，升级线程不再靠 `kill(pid, 0)` 判断存活——它与唯一的回收者之间没有任何同步，僵尸和已被复用的 pid 都会被判为"还活着"并继续升级；现在每一级信号都与 `waitpid` 在同一把锁下串行，状态一经观测即拒绝再发信号。**其三**，`fork` 成功但随后 `master.try_clone`、PTY 输入 worker 或读线程创建失败时（例如 fd 耗尽），子进程此前只被信号处理、从不 `waitpid`，会留下与进程同寿的僵尸；这些路径现在统一 kill 后同步回收。**其四**，显式关闭 pane 后紧跟着 widget drop 会对同一个 pid 起两条升级线程（第二条在第一条回收之后仍在发信号），现在第二次终止请求会发现已有终止在进行中并直接返回。新增测试覆盖失败 spawn 路径不留僵尸、以及"显式关闭 + drop"只跑一条升级阶梯并完成回收。

- Block 模式的动态颜色查询（OSC 10/11/12 `?`）不再返回过期的主题色：应用通过 OSC 10/11/12 设置前景/背景/光标色后（主题切换工具、vim `background=` 探测等；原始字节仍照常透传，live VTE 原生变色），共享解析器现在发出 `ColorSet`/`ColorReset` 事件，每个 pane 据此跟踪动态覆盖值——支持 `#RRGGBB` 十六进制、X11 `rgb:R/G/B`（每通道 1–4 位十六进制，按 XParseColor 语义缩放）与颜色名，无法解析的值忽略——后续查询以既有的 `rgb:RRRR/GGGG/BBBB` 格式回答动态值，OSC 110/111/112 复位后回落到主题色。动态前景/背景生效期间新完成的 Block 快照 VTE 也叠加同一覆盖色，不再与已变色的 live 视图形成明显割裂。

### Security

- 删除会以写令牌执行 PR 分支脚本的一次性 `pull_request_target` 迁移工作流；新增已知个人
  地址/路径隐私护栏、完整 `cargo deny` 依赖策略和独立 Xvfb GTK 回归 job。维护脚本的
  `bash -n` / ShellCheck 现在动态覆盖 `scripts/` 与 `packaging/` 下的全部 shell 文件，
  调试脚本默认只显示会话元数据，查看原始内容必须显式 opt-in。

- 会话恢复回放加固（移植 anvil 的同类修复）：可恢复命令此前以空格拼接成字符串保存、恢复时按 `", "` 拆分后原样写入 PTY——远端参数里的 `;`、逗号或换行可以在重启后被拆成一条新的本地命令。现在窗口快照保存结构化 argv（JSON 数组），回放前用配置 shell 对应的语法（POSIX 单引号或 PowerShell `& '...'`）把整个 argv 安全引用为恰好一条命令；含控制字符的 argv 或未知 shell 语法一律跳过回放并记录警告。旧快照里拼接字符串形式的命令仍可正常加载（标签页、目录、会话 ID 不受影响），但绝不自动回放，丢弃时记 debug 日志。
- AI credential contents remain outside `config.toml`: environment variables take priority, with an optional owner-only `ai_api_key_file` fallback; safe mode disables AI/Agent, executable notebooks, history, remote hosts, restoration, and persistence.
- AI chat metadata, completed pairs, drafts, and provider-bound Block context share the bounded, owner-only, atomically replaced per-window snapshot. Redaction covers active, non-active, and archived chats, and in-flight requests are never restored as completed replies.
- AI/Agent command proposals never submit or execute a command without an explicit user action.
- Agent approval is refused while the bound Block prompt is busy or already contains input; malformed model output never degrades into a runnable proposal.
- Agent proposals and edits must be one visible line with no control characters; recognized privilege, destructive filesystem/system/container, and forced-Git operations require a second exact-command confirmation.
- Selected Block command/output/cwd bytes are bounded, JSON-escaped and sent as explicitly untrusted user-role data instead of being interpolated into the system prompt.
- History, workflow, file-tree and AI review insertions reject line breaks and terminal control characters before writing to a PTY.
- Support archives are created owner-only, make no network requests, and exclude configuration/history/session contents, credentials, host identity, SSH targets, and local paths.
- The repository is dual-licensed under `MIT OR Apache-2.0`; crates.io publication remains separately disabled with `publish = false`.

## [0.2.0] - 2026-07-14

### Added

- Modern GTK4 `TreeListModel`/`ListView` file browser with asynchronous lazy directory scans.
- Per-window session snapshots with atomic claiming, stale-process recovery, legacy migration, retention, and doctor counts.
- Target-aware logging with relative timestamps and module filters.
- Cargo-or-Nix installation, safe uninstall, project governance documentation, Dependabot, and RustSec auditing.

### Changed

- Session snapshots and Block history use owner-only Unix permissions and durable atomic replacement.
- CI validates maintained shell scripts and preserves complete formatting diagnostics.
- Temporary source-export migration workflows and marker files were removed after publication.

### Security

- Persisted commands, output, working directories, and session metadata are restricted to `0700` directories and `0600` files on Unix.
