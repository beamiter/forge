# forge 用户指南

## 1. 启动、诊断与恢复

forge 默认启动 Block 后端并恢复最近一个可领取的窗口快照：

```bash
forge
forge ~/project
forge --working-directory ~/project
forge --mode vte --no-restore
forge --execute bash -lc 'cargo test'
```

`--execute` 后的参数原样作为 argv，不经过额外 shell 拆词。显式 cwd、`--execute`、`--no-restore` 和 `--safe-mode` 都不会意外领取普通恢复快照；execute/safe-mode 窗口也不发布会话快照。单独使用 `--mode` 只覆盖本窗口后端，仍可恢复窗口布局。

以下命令在 GTK 初始化前完成，可用于 SSH、TTY 和 CI：

```bash
forge --help
forge --version
forge --doctor
forge --doctor --json
forge --config-path
forge --init-config
forge --check-config
forge --check-config --json
forge --restore-config-backup
forge --print-default-config
```

`--doctor` 报告配置语义与权限、有效轮换备份、配置写锁、显示/input 环境、可选工具、AI provider/密钥存在性、workflow 与欢迎 Notebook 发现结果、远程 SSH 就绪度以及 ready/active 快照数量，但不输出快照中的目录、标签或命令，也不会探测任何网络 endpoint。使用独立配置：

```bash
forge --config ~/configs/work.toml
forge --check-config ~/configs/work.toml
```

安装版还提供隐私保护的支持归档工具：

```bash
forge-support-bundle ~/Desktop
```

它通过脱敏诊断模式收集权限/大小元数据、聚合计数、非敏感系统特征和选定环境变量是否存在，不打包配置正文、历史、会话、终端输出、剪贴板、API key、SSH 目标、主机名或本地路径，也不会发起网络请求。归档权限为 `0600`；发送给他人前仍应检查每个文件。

`forge --safe-mode` 完全跳过用户配置及 `FORGE_*` 外观/行为覆盖，使用内置 VTE 主题和默认快捷键；同时禁用配置重载、恢复、配置/会话持久化、历史、仓库探测、远程主机、通知、AI 和可执行 Notebook。它适合确认故障来自用户配置还是图形/终端环境，不能与 `--mode` 或 `--execute` 同时使用；即使同时给出 `--config`，该文件也不会被读取。

## 2. Shell 集成

Block 后端可在没有集成脚本时工作，但加载脚本后能通过 OSC 133/7 精确获取 prompt/command 边界、退出码和 cwd。无需查找安装路径：

```bash
# ~/.bashrc
[[ $TERM_PROGRAM == forge ]] && source <(forge --shell-integration bash)

# ~/.zshrc
[[ $TERM_PROGRAM == forge ]] && source <(forge --shell-integration zsh)
```

fish 和 PowerShell 对应 `fish`、`pwsh`。原生安装还会把四种脚本放到 `${prefix}/share/forge/shell-integration/`。其他终端会忽略这些 OSC 序列。

Flatpak 的交互 shell 运行在宿主机，宿主 rc 不应直接引用沙箱内的
`/app/share`。bash/zsh 可在对应 rc 中使用
`source <(flatpak run io.github.beamiter.forge --shell-integration bash)`；fish
使用 `flatpak run io.github.beamiter.forge --shell-integration fish | source`。
两种后端都会在读取 rc 前注入 `TERM_PROGRAM=forge`，因此可继续用该变量做条件保护。

## 3. 终端模式与 Pane

默认配置是：

```toml
terminal_mode = "block"
```

- `block` 把命令保存为独立块，提供退出状态、耗时、筛选、跨块搜索、历史回填和 AI 上下文。
- `unified` 使用一个持续存在的 VTE 滚屏，同时保留可信的命令分区、状态 badge、搜索、
  有界输出快照与恢复；Kitty `a=T` 图片按 `r=`/`c=` 单元格覆盖显示，并随滚屏与重排定位。
  缺失 OSC 133 `D` 时，只有确认 shell 已重获 PTY 前台的新 prompt 才会恢复该记录；记录标为
  `boundary_inferred` / `degraded`，不伪造退出码、结束时刻或耗时。
- `vte` 是传统终端，适合要求完整滚屏语义的 TUI 或兼容性排查。

Kitty 支持刻意限定为 direct `a=T` 静态显示（`i`、`c/r/C`，PNG/RGB/RGBA）与 `a=q`
探针；`a=t`、`I`、crop/z/relative placement、delete/replacement 返回 `ENOTSUP`。
Unified 遵守这些单元格坐标；Block 保留既有“图片作为完成卡片附件”的 profile，不保证
`c/r` 原位坐标或 placement replacement，因此不能当作完整 Kitty placement 实现。

### 实验性 ASCII organism

Block pane 可以显式启用一个完全本地、无 LLM 的 ASCII organism：

```toml
ascii_organism_enabled = true
```

临时试用可设置 `FORGE_ASCII_ORGANISM_ENABLED=1`；修改该键后需新建 Block
pane 或重启 Forge。它监听 Forge 从 OSC 133
边界捕获的真实 command start/finished 事件。生命体平时沿 live terminal surface
缓慢移动，命令运行时靠近输出区观察；用户上滚查看历史时，它缩成 sticky header
里的单行形态。PTY 真正接收键入、粘贴或进程控制键后，live body 会立即收起；
持续键入只延长约 900ms 的停笔窗口，结束后直接贴位回来，不播放奔跑过渡，避免
打断工作专注。Agent、纠错和命令面板的程序化写入不会伪装成真人输入。alternate screen
和放不下完整 sprite 的窄小 surface 会直接隐藏空间身体，结果仍保留在 prompt
上方的 inline widget。图层不参与 GTK 测量、不改变 PTY 行列、不可点击，也不向
PTY 注入 ANSI；VTE pane 不显示。

感知只在内存中保留“不含内容”的 accepted-input/output-activity 脉冲，不记录
按键、剪贴板或输出文本。它会观察 build/test、检查非零退出，并在失败后成功或
`git push` 时作出克制反应；不执行任何命令，也不发送网络请求。连续状态与按
“本地日期 + 完整 Git 根路径”
隔离的失败、恢复耗时、间歇性翻转、构建时长、push 统计和八个三小时活动桶会私密地保存到
`${XDG_STATE_HOME:-~/.local/state}/forge/ascii-organism-native.json`。状态文件有大小、
schema、权限和跨进程事务边界；损坏或未来版本会停用持久化而不会被默认值覆盖。
只有本机可验证的 Git checkout 会进入记忆，不保存命令文本或输出。若同一 repo
今天从首次失败到恢复的时间短于昨天，恢复后的 `git push` 会得到一句跨日反应。
为合并多窗口乱序完成的事件，文件仅保留至多 256 条、每个 repo/day 至多 64 条
不含命令内容的短期 transition ordering metadata，以及至多 512 个不含 PID/命令的
不透明去重 token；较旧条目会折叠回统计基线。迟于该有界排序窗口的全新事件仍计入
单调总数，但不会改写已经折叠的失败/恢复顺序。
活动桶只记本地墙钟时段里的 build/test 与 push 完成次数，不记命令、输出、时区或
UTC 偏移；事件创建时会把日期和时段一起冻结，因此夏令时跳变和事务队列延迟不会
重新解释历史。

事件之间它也活着：内在状态以约 100ms 的帧连续演化——清醒工作缓慢消耗能量，
启用 organism 的本地 Block pane 全部安静约一分钟后进入静息、能量回升（远程
与 VTE pane 不参与感知）；精疲力竭时即使命令仍在跑也会强制打盹，能量在低位
自稳而不会归零。有人敲键盘时无聊下降、好奇上升，被冷落则相反；压力自然衰
减，心情向能量/信心/压力合成的目标缓动。多个 Block pane 共享同一颗心灵、一个
时钟和一份作息，身体数量不会加快生命节奏。该模拟只存在于内存，落盘仍只
发生在命令生命周期事件。

积累最近 28 天中至少三个完整本地日期、六个以上活动样本后，它会在活动足够集中
时学习一个连续九小时的惯常工作窗口；零散或双峰作息保持未学习，不强行归类。
惯常时段内清醒能量趋向较高目标，时段外则更容易安静休息，真正睡眠仍照常回能。
每个本窗口的工作 session 第一次由真人启动命令时会轻声问候（白天是「早。」，
晚间是「来了。」）；跨午夜的夜班只算一次，Agent 命令和时段外命令不会消耗这句
问候。若同一命令还触发更具体的「回来了。」，repo 问候优先。样本不足时能量演化
完全保持原先规则。

它也会长期长大。全局记忆把跨 repo 的不同工作日与「一段失败被一次成功收束」的
恢复 episode 做成有界、去重的纯计数：少于 7 个工作日是 juvenile，之后是 adult；
至少 60 个工作日且经历 12 次恢复后成为 seasoned。阶段显示在 inline badge 中，
也直接留在身体表型上：juvenile 眼睛更圆、细小动作更快，adult 保持标准轮廓，
seasoned 带一侧耳缺口、动作节拍更从容。每个阶段在同一姿态组中仍占用相同
包围盒，因此成长不会让安全尺寸判定忽隐忽现。成长只改变表达，不改变失败/成功
带来的状态量或姿态强度；seasoned 面对一次大的真人恢复仍保持完整庆祝，只把
「终于。」收成一句「嗯。」。最近工作日集合有 64 日排序窗口和
压缩游标，极端迟到的旧事件宁可不催长，也不会因 daily record 淘汰而重复长大。
旧 v1 记忆从仍可验证的日记录与翻转计数迁移为安全下界，不猜测已经遗失的历史。
渲染时，语义行为、连续内在状态量化出的身体语言、成长阶段与输出节奏是彼此独立
组合的四层上下文，而不是为每种排列另造一个持久状态。例如紧张仍能压耳、困倦
仍能卧下，同时保留 juvenile/seasoned 的表型；错误、恢复与 push 等规范姿态的
语义标记不会被装饰层遮掉。这个组合只发生在显示端，不新增记忆字段。

动效可以收敛：`ascii_organism_motion = "full" | "calm" | "static"`——calm 只在
行为边界换姿（无游走、无摆动、无帧交替、位移直接贴位），static 只保留 prompt
上方的 inline 卡片；未设置时跟随桌面动画偏好（系统关闭动画则自动 calm），
临时试用可设 `FORGE_ASCII_ORGANISM_MOTION`。驱动本身也克制：只有当前获焦、未进入
alternate screen 的 full pane 在活动时使用每秒至多十次的 glib 定时器（不强制 GTK 帧时钟
满速运转）；其他 pane、calm、static 以及整窗安静一分钟后的 full 都降为约 0.9 秒
一次的心跳，隐藏 tab/pane 不会随数量线性叠加高频动画成本。焦点转移会立即刷新
新 owner，并安全地把它的下一拍重排回全速。

它分得清「陪主人」和「看 Agent 干活」：Shell Agent 提交的命令（在 CommandStart
处经身份校验）让它蹲到一旁静观，成功只得到无言的小幅认可——大庆祝、
「终于。」和「收好了。」只留给主人亲手敲的命令；Agent 的失败也不动摇主人的
信心、不触发敏感化。Agent 会话的粗粒度阶段（工作中/等审批/完成/离开）作为
不含内容的脉冲喂给社交需求与依恋——Agent 占用主人注意力越久，它越想念主人，
Agent 离开后更可能坐到提示符旁守着。命令若失去结束标记，它以警示色调克制
说明，绝不当作成功。

真正闲下来时（无命令、无反应、无人敲键盘），行为由内部状态经效用打分自主
涌现：困了蜷在左下角睡觉（睡着会回能量，睡饱自然醒来做别的）、无聊且好奇时
四处踱步探索、想念主人时坐到提示符一侧守着；已选行为带保持计时和些许惯性，
不会每帧重掷。命令、键入或任何反应都会立即打断当前性情。

它现在会把一轮调试从失败陪到 push，而不只对单个事件做短暂反应。同一 repo/day
有一到两次尚未恢复的 build/test 失败时，反应结束后留下 `[!]`；累计到三次及以上
则降成更安静的 `[!!]`，不追加催促或语音。失败与卡住姿态只会守在当前输出边缘
以下的完整空白带；空间不够就隐藏 live sprite、只保留 inline 卡片，绝不会覆盖终端
文字。未知结果、普通命令和失败的 push 都不能抹掉这段未完成工作。

成功 build 会把失败守望转成提示符旁的 `[ok]`，一直等到真正成功的 `git push`；
后续干净 build、普通命令、失败的 push 与 `git push --dry-run`/`-n` 都不会被误当成
收尾。若同一天已经发生至少三次失败→成功翻转，等待 push 的姿态会变成谨慎的
`[?]`：它只表示“今天值得再确认一次”，不会断言当前测试一定 flaky。新的 build
失败会重新进入 `[!]`/`[!!]`，成功 push 或本地日期跨日才结束当日弧线。

离开 checkout 只暂停当前身体的守候；同一天回到该 repo、下一次 build/push 解析出
canonical root 后仍会从记忆恢复。同窗口里已经解析到完全相同 repo/day 的 pane 会
立即接力同一个类型化工作快照；临时侧目不会覆盖任何 `[!]`、`[!!]`、`[ok]` 或
`[?]` 守望。其他窗口在下一次语义命令刷新记忆时收敛。普通路径离开由原始 cwd
立即判断，嵌套 Git repo 则在下一次 build/push 重新解析 canonical root 时完成切换。
整条弧线直接复用已有的 open failure、`recovered_pending_push` 与翻转计数，不新增
持久化字段；能量跌到强制休息线以下时它仍会先睡，并在回升到带滞回的唤醒线后
回来守着。

内在状态也能直接从身体上读出来：能量低时先打盹、随即蜷起来睡着并停止游走，
sticky 单行形态同步换成打盹字形；压力高时耳朵压平；无聊到顶会偶尔打哈欠、
游走占比提高。
卡片状态行只显示最多三个自然词（如 `sleepy · tense · curious`），完整八维数值移到
tooltip，避免日常界面长期暴露 `E72 M62 …` 这一类调试仪表盘。
行走有交替步态帧，观察输出越密集甩尾越快，庆祝会保留猫耳并眨星、push 后守着尾巴打盹。
同一姿态组内的所有动画帧共享包围盒，空间身体的 fail-closed 尺寸判定不会因
动画帧抖动；错误/庆祝等反应姿态保持规范形态，不被身体语言覆盖。
每次命令反应与跨 pane 侧目都从各自的规范首帧开始，不会继承闲逛时恰好落到的
全局动画相位；idle 游走本身仍连续，不会因卡片刷新重新起步。

空间身体不再瞬移：姿态切换时它以整格步进走过去（剩余距离的四分之一每帧、
至少一格，跨全屏约一秒，途中播放步态帧）。键入撤退与 alternate screen 都会
立即隐藏；键入期间帧循环也不会把它提前唤回。被隐藏后再出现是直接贴位，它从不
在看不见的地方走路。
命令运行和反应时的空间姿态也不再覆盖最新输出：它优先守在光标下一行开始的
空白带，并随光标下移一路跟下来（只取行号这一内容无关几何量）；完整身体放不下
就隐藏 live sprite、保留 inline 卡片。姿态宽高、字体或 scrollbar gutter 改变时，
旧位置会失效并重新贴到安全点，不会在插值途中越过输出或滚动条。

同一窗口现在只有一具完整空间身体：它属于当前真正获焦的本地 Block pane；其他
pane 仍保留自己的 inline 事件卡和 sticky 微姿态，但不再各自游走。焦点落到搜索、
对话框、VTE/远程 pane，或窗口失活时，live body 会全部撤下；切回本地 Block 后先
重新测量再出现。切 tab、分屏关闭、zoom 和 alternate screen 都沿用同一弱引用
仲裁，不搬运 GTK widget，也不会让旧 pane 从 rmcup 恢复一个过期位置。
Notebook 切页会在 signal 当下先撤销旧 owner，待 GTK 提交新页后再认领，因此切换
间隙的旧页既不能以隐藏睡姿回能，也接不到本应属于新页的侧目脉冲。

后台本地 Block pane 若收到权威的非零退出状态，当前可见且空闲的空间身体会短暂
侧目；跨 pane 传递的只有“失败”这一类型化事实，不含命令、cwd、输出、退出码或
repo 身份，也不会抢焦点、弹窗或改写任一 pane 的 inline/sticky 记录。当前身体正
在键入、守望、反应、任一 repo 调试守望、alternate screen、Static 模式或完整姿态
放不下时，这次侧目直接丢弃而不排队，因此切焦点后不会补演过期失败。

长命令它会陪跑：跑过十秒的命令，卡片状态里出现「· 2m 30s in」的陪跑时长
（每拍最多更新一次，只是文本）；守望超过一分钟，它从警觉守望换成卧姿的
打盹式守望——还在看，只是眯着眼。成功构建的墙钟耗时会以饱和聚合的形式
（总和+次数两个纯标量，单次封顶六小时，不存命令文本）计入该 repo 的日记录；
累计三次以上的历史样本形成基线，当前构建本身不会稀释比较；成功构建若比此前
平时慢一倍以上或快一倍以上（且
绝对差超过十秒），description 里静静补一句 slower/quicker than usual here——
不升级姿态、不打断。陪跑时长以十秒为步进更新，避免朗读辅助技术逐秒播报。

运行中的观察还会从纯 output-activity 脉冲时间推导粗粒度节奏：滚动的约 1.2 秒
窗口内出现至少三个脉冲时专注跟看，约三秒没有新脉冲时安静等待（包括启动后始终
没有输出的命令）；沉寂后输出恢复，只短暂
抬头约 0.9 秒再回到当下节奏。既有的长命令守望超过一分钟后仍会进入卧姿
`WatchSettled`。它不读取字节、行内容或 ANSI，也不把“安静”猜成成功、失败或某个
构建阶段；权威的 command finished 事件仍是唯一结果来源。节奏只存在于内存，
只影响身体语言和动画拍子，不进入 repo 记忆或卡片事实。

反应强度是历史的函数而非刺激的常函数：同一 repo 当日每多一次干净通过，
兴奋度按 1/(1+已有通过次数/4) 衰减，先安静下来、随后不再说话；长串全绿后的第一次
失败反而刺痛更深。反复失败后的恢复始终全力庆祝。记忆里从未见过的 checkout
会让它怯生（更安静、信心下压），积累一周以上日记录的老仓库在切回时得到一句
「回来了。」。会话内任意命令连续三次非零退出后，它坐到一旁、不再逐次开口。
纠错卡片的接受/关闭只作为不含文本的脉冲传入：被采纳的修正在命令成功后得到
一次无言的小幅认同，连续关闭则让它学会闭嘴。上滚时的 sticky 单行形态也会用
定宽五字符的微姿态（竖耳/压耳/星点/打盹）低频反映当前行为。以上全部派生自
内容无关观察；新增落盘信息只有上述有界计数、聚合和时段桶，仍不包含命令或输出。

熟悉 repo 后，它也会形成不打扰工作的“领地习惯”。显示层以已验证 canonical repo
identity 的进程内稳定哈希选择偏爱的窝边与行走路线；陌生 checkout 的第一次反应
安定后，若没有更重要的失败/恢复守望，会做一次很短的探索；冲突时直接放弃而不
延迟补演。哈希只在本次进程中用于布局，不显示路径、不传入
reducer、不新增落盘字段；repo 无法验证或身份切换中时使用中性的安全位置，几何
不足则照常 fail closed。离开 checkout 仍按既有规则释放当前守候，不把一个仓库的
领地带到另一个。

短时间内多个表达同时到来时，一份窗口/session 内的注意力预算只放行最重要的可选
语音或低优先级动态表达：失败与未完成守望优先于恢复闭环和 push 收尾，随后是长
命令变化，最后才是问候与低优先级 insight。持久的 `[!]`/`[!!]`/`[ok]`/`[?]`
事实不会因仲裁丢失；已放行的表达占用一段共享 focus window，并为自己的类别留下
冷却。期间被压住的低优先级表达直接丢弃，不排队补演。新输入、alternate screen
和尺寸安全仍可立即撤下所有表达，注意力预算不能越过这些边界。

同一天内若已至少三次出现失败→成功翻转，而当前只积着一次失败便恢复，它会轻声
提示「像是偶发的。」并把姿态限制在普通庆祝；Agent 触发时保持无言。它不会把
疑似 flaky 的测试表现成更大的胜利，也不会凭该提示提高反应强度。

反应停留也不再统一占住八秒：安静通过约 1.8 秒、普通通过约 2.5 秒、首次错误约
5 秒、大恢复约 7 秒，连续错误旁坐约 10 秒；任何新输入或命令仍会立即打断。
因此快速测试循环不会让轻微成功长期霸占空间，而真正需要注意的失败会多陪一会。

几个关键行为边界在 full 模式下会用四个固定包围盒帧连接：即时错误反应
坐定为失败守望，庆祝收束为恢复或谨慎守望，长时间卧姿守望被成功唤起进入庆祝，
恢复守望在 push 收尾后卧下休息。过渡帧只解释“怎么到达”目标姿态，不改变 reducer
已确定的事实、反应优先级或停留时长；新事件可以立即取消旧过渡并从自己的规范
首帧开始。完整过渡身体放不下仍隐藏 live sprite。calm 直接切到目标姿态，static
继续只显示 inline 卡片；键入撤退和 alternate-screen yield 在 full 下也始终立即
执行，不等待过渡播完。

两个后端共享输入路由、字体/主题、cwd、进程检查和关闭清理。分屏继承当前 pane 的后端：Block 创建 Block sibling，VTE 创建 VTE sibling；两种 sibling 都可继续嵌套，也可在恢复的混合布局中共存。这避免隐藏 PTY，并让分屏行为不受随后修改的默认后端设置影响。

| 操作 | 快捷键 |
|---|---|
| 左右 / 上下分屏 | `Ctrl+Shift+E` / `Ctrl+Shift+D` |
| 方向聚焦 | `Ctrl+Alt+方向键` |
| 调整大小 | `Ctrl+Alt+Shift+方向键` |
| 放大当前 Pane | `Ctrl+Shift+Z` |
| Pane 移到新标签 | `Ctrl+Shift+!` |
| 关闭当前 Pane 或标签 | `Ctrl+Shift+W` |

关闭 pane、标签、多个选中标签或窗口时，forge 会扫描所有后端的真实 PTY 和前台进程；存在运行中任务时先给出统一确认。缩放的 pane 会先恢复 pane tree 再关闭，避免漏掉隐藏 sibling。

分屏布局恢复仍建议与命令自身的持久化方案配合使用，尤其是 SSH/TUI 长任务。

## 4. 标签页与窗口恢复

| 操作 | 快捷键 |
|---|---|
| 新建标签 | `Ctrl+Shift+T` |
| 下一个 / 上一个标签 | `Ctrl+Tab` / `Ctrl+Shift+Tab` |
| 标签 1 到 8 / 最后一个 | `Ctrl+1`…`Ctrl+8` / `Ctrl+9` |
| 过滤标签 | `Ctrl+Shift+L` |
| 标签栏位置 | `Ctrl+Alt+B` |

标签支持按落点前后拖放排序、双击重命名、固定、标记、复制和右键菜单。过滤框固定显示在侧栏的 Tabs 视图中；`Ctrl+Shift+L` 会自动打开侧栏、切换到 Tabs 并聚焦过滤框。侧栏在 Tabs 与 Files 之间切换；开关、宽度和视图会持久化。

每个进程维护独立 active 快照。正常关闭后才原子发布为 ready；并发窗口不会读取或覆盖彼此 active 状态。后续启动逐个领取最近快照，确认 owner PID 已结束后才回收崩溃遗留的 active 快照，最多保留 32 个 ready 快照。旧版 `tabs.state` 会在首次启动时迁移。

## 5. 搜索与 Block 操作

`Ctrl+Shift+F` 打开当前标签搜索：普通文本不区分大小写，`/expression/` 使用正则；Enter/Shift+Enter 前后跳转，Escape 关闭。清空输入立即清除 VTE 和 Block 高亮。查询上限为 8 KiB；增量搜索最多检查 4 MiB，并以 12 ms 为 surface 间的停止目标，最多记录 10,000 个命中。达到任一边界时状态栏会显示结果不完整，请缩小关键词。可能产生零宽命中的正则会明确拒绝，避免游标与高亮失去同步。

| Block 功能 | 快捷键 |
|---|---|
| 命令历史面板 | `Ctrl+Shift+H` |
| 跨块行搜索 | `Ctrl+Shift+G` |
| 跳到首个失败块 / 最早块 | `Ctrl+Shift+X` / `Ctrl+Shift+N` |
| workflow | `Ctrl+Shift+M` |
| AI 分析选中块 | `Ctrl+Shift+Q` |
| Shell Agent | `Ctrl+Alt+G` |
| 全选 / 回填 / 清空 | `Ctrl+Shift+A` / `Ctrl+Shift+I` / `Ctrl+Shift+K` |

跨块搜索的 `Aa`、`.*`、`W` 分别控制区分大小写、正则和 Unicode 整词匹配，亦可用
`Ctrl+I`、`Ctrl+R`、`Ctrl+W` 切换。三个选项可组合，结果列表和跳转后的 VTE 高亮不会
使用不同的查询语义。范围下拉框的 `All / Cmd / Out` 分别搜索全部文本、仅命令或仅输出；
`Ctrl+O` 可循环切换，且范围在 500 条命中上限之前应用。`Failed` 只显示真正失败的块（不把用户
主动中断算失败），`Slow`
只显示耗时至少 1 秒的块；筛选先于 500 条上限应用并可组合。查询为空时，启用任一筛选会为每个
符合条件、且在当前范围有内容的保留块显示一条可定位代表结果，可直接浏览失败或慢块。
搜索状态会显示 `当前位置 of 总数`；`↑/↓` 循环移动，`Home/End` 跳到首尾，
`PageUp/PageDown` 每次移动十条。列表会跟随选择滚动，但焦点仍留在查询框。
`Enter` 定位后关闭面板；`Shift+Enter` 成功定位实时终端结果后保持面板并自动选中下一条。
快照结果仍进入快照窗口，已经失效的结果不会关闭或前进。
再次打开会恢复本进程内上次有效的查询、匹配选项、范围及 Failed/Slow/Bookmarked/Background
筛选（不会写入配置或会话快照）；`Ctrl+U` 只清空查询，**Reset** 或 `Ctrl+Shift+U` 一次恢复
查询、匹配开关、范围及四个
元数据筛选的默认值。超过 8 KiB
的无效查询不会进入这份内存；鼠标修改控件后焦点会自动回到查询框，可直接继续输入。
打开期间会以 500 ms 的轻量版本探针感知新完成块、retention 轮换与 bookmark revision，再防抖
更新结果；仍存在的
稳定命中保持选中，探针本身不复制命令或输出文本。
Block Search 3.8 在该命中已被淘汰时回退到最接近的原排名，避免刷新后跳到列表顶部；主动修改
查询、匹配选项或范围仍会明确从第一项重新开始。按 `F5` 可立即重建当前索引而不改变查询意图
或选择锚点，手动刷新同时同步自动版本探针，避免重复工作。
Block Search 3.9 在标题栏提供可点击的刷新按钮；它与裸 `F5` 触发同一个刷新动作：同步版本
探针、取消待执行的防抖刷新并保留当前选择，最后把焦点还给查询框。带
`Ctrl`、`Shift`、`Alt`、`Super`、`Hyper` 或 `Meta` 的 F5 不会被面板截获，仍继续传播。
按钮向辅助技术公布完整的“Refresh block search results”动作名称及 `F5` 快捷键。一次物理按下
F5 到释放期间最多刷新一次，长按自动重复不会反复重建；若最初带修饰键，保持 F5 时松开修饰键
也不会把同一次按键重新解释为裸 F5 刷新。窗口失焦会清除 latch，避免 GTK 遗漏 release 后永久
抑制后续刷新。
Block Search 4.0 只在标题栏保留 **Refresh / Reset**，并把匹配与范围、元数据筛选拆成两行紧凑
工具栏；工具栏带自动水平 overflow，窄窗或大字体下可滚动访问超宽控件，并保留稳定
Tab 顺序。窗口 Capture 按 hardware keycode
把 `block:search` opener 的 held 状态传给 dialog fallback：一次物理按键只切换一次，release 前
的 repeat 只消费，既不会闪关，也不会在刚关闭后重开；中途松开 chord 修饰键也不会把 repeat
写入查询框，窗口失活会清理丢失 release 的状态。
手动刷新先显示并以中等优先级向辅助技术播报 `Refreshing blocks…`；首个 frame tick 只允许状态
完成绘制，下一 tick 才同步重建。新查询、筛选或再次刷新会取消旧 callback，选择锚点和立即回到
查询框的焦点语义不变。
Block Search 4.1 新增 **Background** 元数据筛选。传统 Block 以后台块没有命令来分类，Unified
以记录的显式 background 字段分类；显示与筛选时其命令、退出码和耗时都归一为空，因此 Background
与 Failed、精确退出码、Slow、最短/最长耗时互斥，遗留矛盾字段也不会改变这一点。空查询配合
`Cmd` 不产生结果；`All` / `Out` 只使用真实保留输出的首个非空行，没有快照或输出为空时不会
合成占位结果。筛选与范围继续在 500 条上限之前执行。该开关参与进程内记忆、关闭/重开和 Reset。
Cross Block Search 的关闭动画期间仍由当前 dialog 占用唯一 slot；其自身 `closed` 到达后才释放，
快速关闭、松键、再按打开不会让旧回调清除新面板。
Block Search 4.2 新增 **Bookmarked** 元数据筛选。它与 Failed、Slow、Background 按 AND
组合，筛选与范围都在 500 条上限之前执行；空查询也可浏览当前 pane 的书签。每个结果行都显示
`☆` / `★` 切换按钮，或在选中结果上按 `Ctrl+Shift+B`；一次物理按键只切换一次，长按和中途
松开修饰键不会反复切换或把 `b` 写进查询框。Block 卡片星标、CSS、Bookmarked 卡片筛选和搜索
结果使用同一集中状态，Unified 结果也能创建书签。书签集合只存在于当前 pane 的运行期内，不会
写入配置、历史或 session restore；Reset 只关闭 Bookmarked 搜索筛选而不删除集合。打开的面板
通过 bookmark revision 自动刷新；Unified 只有在真实记录被 retention 淘汰时才清理对应 id，
输出快照或 chrome marker 淘汰不会丢失书签。

选择语义与 anvil 对齐：

- `Ctrl+Up` 从最新块进入选择；普通 `Up/Down` 移动 active edge，`Shift+Up/Down` 扩展范围。
- `Enter` 或 `Ctrl+Shift+I` 按终端顺序回填所有选中命令，不自动执行；`Escape` 清除选择。
- `Ctrl+Enter`（或右键 **Re-run Command**）重跑单个选中块的命令。这是 Block 模式唯一会
  自行发送 Enter 的路径，条件很窄：只针对本 pane 中用户自己已经运行过的单行命令，
  提示符必须空闲、无未提交输入、无待处理 typeahead；多选、background 块和会被截断为首行的
  多行命令一律只回填不执行。模型给出的候选（AI、Agent、命令面板 `?`）仍然只能
  **Insert for review**。
- alt-screen 程序运行期间，`Ctrl+Up` 不进入块选择、`Delete` 不删除隐藏块；进入 alt-screen
  会清除既有块选择，避免快捷键作用在看不见的卡片上。
- `Delete` 删除整个选区，一次按键即可移除用 `Shift+Up/Down` 建立的整段范围；命令面板中的
  **Undo removing blocks** 把它们放回按 id 该在的位置，因此期间新跑的命令不会把恢复的块挤到
  最前面。撤销槽是单级的：删除和清空共用它，新的一次移除会替换上一次。
- **Show only failed blocks** / **Show only slow blocks**（≥1s）/ **Show only bookmarked blocks**
  把块流收窄到匹配的卡片：不匹配的被隐藏且不再占据虚拟画布高度，`Up/Down` 选择会跳过它们，
  `Ctrl+Shift+A` 只选中可见的，过滤期间新完成且不匹配的命令也不会出现。**Show all blocks** 恢复
  全部（没有过滤时它保持原来的「跳到最早的块」含义）。被信号停止的命令不计入 failed。
  逐个跳转仍由 `Ctrl+Shift+X` 和书签跳转负责。
- **Collapse all blocks** / **Expand all blocks** 一次收起或展开全部完成块的输出，
  **Collapse or expand block** 只作用于选中块（没有选中时作用于最新块）。折叠会同时缩小该块在
  虚拟画布上的高度，因此滚动条长度与实际内容一致。这三个动作和 Undo 一样默认不占快捷键，
  可在 `[keybindings]` 中绑定；折叠状态目前不跨重启保存。
- `Ctrl+Shift+B` 收藏 active 块，`Ctrl+,` / `Ctrl+.` 在收藏块之间跳转。
- 多选右键可批量复制命令、输出、完整块或回填命令；复制按界面顺序合并。
- 长块提供顶部/底部导航与 sticky header，后台异步输出使用独立 Block 样式。
- 块内输出过滤 `Alt+Shift+F` 打开后，`Escape` 或再次 `Alt+Shift+F` 可关闭：查询文本保留，
  焦点交还实时提示符。
- 历史恢复和撤销清空重建出来的块与新块拥有完全相同的右键菜单。
- 在块内拖选文本后键盘焦点会留在那张卡片上，Block 快捷键仍然全部可用：方向键、
  `PageUp/PageDown`、`Home/End`、`Ctrl+Up`、书签跳转、`Alt+Shift+F` 都照常工作。
  有选中块时 `Enter`/`Ctrl+Enter`/`Delete`/`Escape` 按卡片提示条执行；没有选中块时
  它们保持"把焦点交还提示符"的原意。普通打字始终把焦点带回提示符，
  块内过滤输入框和弹出菜单内的按键归它们自己。
- 完成块同时受 `max_visible_blocks` 和每 pane 128 MiB 估算内存预算约束；预算包含 ANSI 原文、重复显示副本、VTE/控件与图片，超限时从最旧块开始淘汰，最新块始终保留。每个完成 VTE 最多保留 1,048,576 个 cells（最多 4096 列），因此极端长输出只在界面中保留有界终端窗口；这一几何裁剪不会再额外影响复制/导出中已捕获的文本（最多 8 MiB）。Block Kitty 图片每块最多 64 张；Unified 图片使用同样的 16 MiB/64-placement 上限，超出可见网格的几何会明确拒绝而非静默缩放。

命令运行中或 alt-screen TUI 活跃时，Enter 和应用所需按键继续发送给前台进程，不会误触发旧块回填。

命令运行超过约 2 秒后，pane 顶部出现常驻运行状态条：`▶ 命令 用时`，计时每秒推进，
并带一键 Stop。滚动到历史中时它立即出现（此时实时卡片已不可见），alt-screen 程序活跃时让位。
瞬时命令不会让它闪现。

退出状态区分「失败」与「被停止」：`130`（SIGINT，包括 Ctrl+C 和状态条的 Stop）、
`141`（SIGPIPE）和 `143`（SIGTERM）显示为中性的 `⊘` 卡片与 `exit:N · interrupted` 徽章，
不参与滚动条失败标记、`Ctrl+Shift+X` 失败跳转和 Failed 过滤；原始退出码在徽章、导出和历史里
完整保留，因此按精确 exit code 过滤仍能找到它们。SIGSEGV、SIGABRT、SIGQUIT、SIGKILL
这类真正的故障仍然是红色失败。

## 6. 统一命令面板、历史与 workflow

`Ctrl+Shift+P` 打开的面板统一模糊搜索四类来源：

| 前缀 | 来源 | 接受后的行为 |
|---|---|---|
| `>` | 应用动作与当前快捷键 | 执行动作 |
| `@` | JSONL 命令历史 | 写入编辑行，不提交 |
| `:` | YAML/TOML workflow | 填参数后写入编辑行，不提交 |
| `?` | 自然语言命令请求 | 交给 AI 生成候选，先审阅 |

JSONL 历史默认位于 `${XDG_STATE_HOME:-~/.local/state}/forge/history.jsonl`，只保存 command、cwd、exit code 和完成时间，不保存终端输出。文件权限为 `0600`，重复命令按最新记录展示，损坏或超限记录会跳过，文件会按上限压缩。`Ctrl+Shift+H` 面板最多创建最近 500 行，即使磁盘保留上限更高；状态行会明确标出显示边界。

用户 workflow 放在 `~/.config/forge/workflows/`，支持 `.toml`、`.yaml`、`.yml`；也可用 `FORGE_WORKFLOW_DIR` 增加以路径列表表示的目录。用户定义优先于已安装示例，同名项不会被示例覆盖。

安装包附带 feature branch、大文件查找、交互式 rebase、SSH 本地端口转发、Docker 日志跟随和端口进程终止示例。所有示例都只生成可编辑的单行命令；选中后不会自动执行，其中会结束进程或建立长连接的模板仍须由用户逐字审阅。

TOML 示例：

```toml
name = "Deploy"
description = "Deploy a branch"
command = "deploy --branch {branch} --env {env}"
tags = ["release"]

[[args]]
name = "branch"
default = "main"

[[args]]
name = "env"
default = "staging"
```

YAML 可使用共享格式的 `{{name}}` placeholder。未提供的必填参数不会静默执行，生成内容始终只进入当前 pane 的编辑行。为保证“只插入、不提交”，history、workflow、文件路径和 AI 候选只接受不含 CR、LF、NUL 或其他终端控制字符的单行文本；不安全条目会被拒绝并提示，而不会写入 PTY。

## 7. 可执行 Notebook

`.jtnb.md` 是普通 Markdown，其中 bash/sh/zsh/fish/pwsh/powershell/shell 或无标签 fence 可执行。双击文件树中的 Notebook 打开；内置 quick start 可在命令面板搜索 **Open welcome & quick start notebook**。

- 每个 cell 可单独 Run/Stop，也可 Run All/Stop All。
- stdout 与 stderr 分开显示，并保留 exit status；单 cell 合计输出有 256 KiB 上限。
- 显式 shell fence 使用对应解释器；`shell` 和无标签 fence 使用 forge 的配置 shell argv。
- 非 shell fence 只展示，不执行。
- cell 在独立进程组运行，停止、Stop All 或关闭对话框会清理完整进程组。
- 命令不会注入当前终端，也不会绕过 Notebook 自己的运行按钮；安全模式禁用执行。

安装资产位于 `${prefix}/share/forge/notebooks/`；Flatpak 中是 `/app/share/forge/notebooks/`。

## 8. 文件树

侧栏 Files 页以当前标签 cwd 为根：双击目录展开/折叠；双击普通文件把 shell 引号保护后的路径插入编辑行，不自动执行；双击 `.jtnb.md` 打开 Notebook。向上按钮进入父目录，主页按钮回到当前终端 cwd。Block 走自管 PTY 输入，VTE 走 VTE child input。

## 9. Flatpak 与桌面安装

Flatpak 应用 ID 是 `io.github.beamiter.forge`。打包版本通过 `flatpak-spawn --host` 启动宿主 Shell、SSH、Git、curl 和通知工具，避免命令误跑在一次性应用沙箱；因此 forge Flatpak 本身不是命令隔离边界。

```bash
flatpak run io.github.beamiter.forge --doctor
flatpak run io.github.beamiter.forge
```

文件树需要宿主文件系统权限。AI 密钥可通过可信启动器、显式 Flatpak override 或 sandbox 内可见的 owner-only 独立文件提供。完整权限说明见 `docs/FLATPAK.md`。

## 10. 远程会话与容器

远程主机严格按用户配置启用。配置文件没有 `remote_hosts` 键、显式写成 `remote_hosts = []`，或配置加载失败时，主机选择器都保持为空；forge 不会注入地址、用户名或容器名。下面的 SSH/容器片段只是可复制模板：端口写在 `ssh_args` 里，不能写成 `host = "box:22"`；登录名写在 `user` 里，不能写成 `host = "root@box"`。

设置面板（`Ctrl+Shift+O` → Remote Hosts）可以添加/编辑/删除主机：铅笔图标用同一个对话框打开已保存的条目，面板没有控件的进阶字段（`ssh_args`、`session`、`remote_shell`、`login_shell`、`multiplex`、`deploy_artifact`）在编辑时原样保留，要改这些仍然直接编辑配置：

```toml
[[remote_hosts]]
name = "dev"
host = "dev.example.com"
user = "alice"
remote_shell = "jsh"
session = "dev-main"
ssh_args = ["-p", "2222"]
login_shell = true
multiplex = true
```

`Ctrl+Shift+S` 打开主机选择器。连接复用由 OpenSSH ControlMaster 完成，异常断开按上限退避重连；用户正常退出不会重连。

Files 侧边栏顶部也可以直接选择 `ssh: 名称` / `docker: 名称` 浏览目标文件系统；选择器旁的终端按钮可立即进入该 profile。本地位置会精确在当前文件树根目录新开标签，远端位置从 profile 的默认目录启动（远端启动器没有通用的“指定 cwd”契约）。连接前的 home 探测不会阻塞 GTK；失败会显示原因并回到 Local，重新选择即可重试。配置增删、编辑或重排后，Forge 只按完整 profile 身份保留/重映射已经打开的远端树和文件剪贴板；目标身份不再唯一可证时会清理旧状态，绝不会让同一个数字索引悄悄指向另一台机器。若新建、重命名或删除确认框打开期间又切换了根目录/目标，旧操作会要求重新打开，不会把旧绝对路径交给新的后端。

直接在终端执行交互式 `ssh root@example.com -p 22` 也会自动进入对应远端文件树。这个便利功能覆盖 Block、Unified 和 VTE，但它只通过 `jterm_core::process::observed_ssh_command` 读取 `/proc` 中真实前台进程树（也能识别来源验证通过的 jsh SSH 升级 launcher），不信任终端输出或 OSC 133 的命令字符串。Forge 会优先复用唯一匹配的已保存 profile；没有唯一匹配时创建仅驻留内存、在位置选择器中标为 `temporary` 的 SSH 目标。显式 `-S` / `ControlPath` 及 jsh 派生的复用 socket 只进入不可变的执行快照，不参与 saved/temporary 身份和唯一性判断，并贯穿目录扫描、文件操作、剪贴板和传输；同一目标的两种表示也按同一文件系统处理，不会误走远端到远端 relay。后台 home 探测成功之前，当前文件树不会被清空或切走；完成时还会复核 SSH 仍在同一标签和焦点代际运行、自动跟随 token、配置身份、用户导航及文件操作 generation。失败通知会在点击重试时重新检查实时进程和 socket；不支持安全复用的参数可跳到 Files profile 选择器。退出 SSH 后远端树保持在原位，方便继续使用独立的无交互 probe，不会突然跳回 Local。`temporary` 位置旁的终端按钮运行普通交互式 SSH（不附加远端命令），因此不要求目标机器安装 jsh。

超长 DSW endpoint 在位置下拉项中从中间省略，保留 `root@dsw…aliyuncs.com` 这类可辨识前后缀；悬停可查看经过安全显示处理的完整目标。

`deploy` 决定目标机器上没有 jsh 时怎么办：`"off"`（默认）直接运行 `remote_shell`，取到什么算什么；`"persist"` 和 `"incognito"` 会把一份 jsh 送过去，前者在对方 `$HOME` 留下 dot-files 和二进制缓存（重连免传输），后者用退出即删的沙箱 HOME。Block、cwd 跟踪和退出码都来自 jsh，所以对端只有 `sh` 时不开 deploy 会静默丢掉这些。

`docker = true` 时 `host` 是**正在运行的**容器名，走 `docker exec` 而不是 ssh，`user` 变成容器内的用户（`-u`）；`ssh_args`、`multiplex`、`login_shell` 对容器无意义，会被忽略。容器默认以 root 运行，而旧版 jsh 在 root 下会把 `/usr/bin/git` 和 `/usr/bin/bash` 判为不可信 helper，于是 git 补全、git 提示符和 `.bashrc` 导入在容器里静默消失。容器标签页请搭配修复过这一点的 jsh（jsh CHANGELOG 中的 “a root shell trusts the system helpers it could write”）。

```toml
[[remote_hosts]]
name = "build容器"
host = "my-service"
docker = true
user = "devuser"
deploy = "persist"
```

**通常不需要配置从哪拿 jsh**：本机装的 jsh 是静态构建时（Linux 安装的默认），launcher
会直接把它出借给目标——不查 release、不联网，远端跑的就是本机这份的同版本。release
只是动态链接或跨架构时的回退。

`deploy_artifact` 因此只剩一个用途：明确要推**另一份**构建（比如某个分支的产物）而不是
本机正在用的这份。路径必须是绝对路径，且必须是目标机跑得起来的二进制；`--check-config`
会在文件不存在、或写了它却没开 `deploy` 时给出警告。

## 11. AI 与 Agent 安全边界

AI 总开关、provider 和 endpoint 由配置控制。支持 Anthropic、OpenAI-compatible 和 Ollama wire protocol。`ai_base_url` 必须是绝对 HTTPS URL，或是明确的 loopback HTTP endpoint（`localhost`、IPv4 loopback 或 `[::1]`，可带数字端口）；该本机例外对三个 provider 一致，便于连接本机兼容服务和代理，任何远程明文 HTTP endpoint 仍会在联网前被拒绝。明确无需鉴权的 loopback 服务可不配置 Key。密钥内容不会写入 TOML；环境变量优先。也可直接在 **Settings → AI & Agent → API Key** 输入密钥并按 Apply：forge 会将它原子写入 owner-only 的 `~/.config/forge/ai.key`，并只把文件路径写入配置。设置面板不会回显已经保存的密钥，再次输入并 Apply 可替换它。

测试本机 provider mock 时可直接使用 loopback HTTP；若选择 HTTPS，仍须让系统 `curl` 信任包含实际 `localhost`/loopback SAN 的测试证书，Forge 不会使用 `-k` 或跳过证书校验。`--check-config` 会在联网前拒绝所有非 loopback HTTP URL。

也可通过环境变量配置：

```bash
export ANTHROPIC_API_KEY='...'
# 或 OPENAI_API_KEY / OLLAMA_API_KEY / 通用 FORGE_AI_API_KEY
forge
```

若要手工管理密钥文件，可执行：

```bash
mkdir -p ~/.config/forge
install -m 600 /dev/null ~/.config/forge/ai.key
read -rsp 'AI API Key: ' FORGE_KEY; printf '\n'
printf '%s\n' "$FORGE_KEY" > ~/.config/forge/ai.key
unset FORGE_KEY
chmod 600 ~/.config/forge/ai.key
```

并在 `config.toml` 中设置：

```toml
ai_api_key_file = "~/.config/forge/ai.key"
```

文件必须是当前用户所有的普通文件，Unix 权限不得向 group/other 开放，最大 16 KiB，且只能包含一行非空密钥。环境 Key 优先于文件；`FORGE_AI_API_KEY_FILE` 可覆盖文件路径。相关配置为 `ai_enabled`、`ai_provider`、`ai_base_url`、`ai_api_key_file`、`ai_model`、`ai_max_tokens`、`ai_stream` 和 `ai_redact_secrets`。请求通过系统 `curl`/Flatpak host bridge 发送；运行 `--doctor` 可离线检查凭据文件和 curl。右侧聊天面板使用 `Ctrl+Alt+Shift+A`，Block 选择后 `Ctrl+Shift+Q` 可发送命令、退出码、cwd 和截断输出。

面板可拖动分隔条，实际宽度会在 400 ms 防抖后写回 `ai_panel_width`，并在启动、配置热重载和重新打开面板时恢复。输入框中 `Enter` 与 `Ctrl+Enter` 均发送，`Shift+Enter` 换行；输入法正在选词时，Enter 只确认候选，不会误发。焦点位于输入框时，`Ctrl+Shift+C/V` 也会作用于输入框，而不是后台终端。空会话提供三个快捷提示，它们只填入 composer，绝不会自动发送。

聊天回复默认流式显示（`ai_stream = true` / `FORGE_AI_STREAM`）：回答在生成过程中逐段出现在会话里，完成时以 provider 返回的完整文本原样落库，与关闭流式时保存的会话完全一致；中途出错时已显示的部分内容保持可见，错误按既有方式提示并可 Retry。流式只用于聊天面板；Agent、命令生成与纠错等严格 JSON 表面始终等待完整回复。关闭 `ai_stream` 则恢复等待完整回复的旧行为。

发送后状态行提供 **Stop**；它会终止并回收对应 curl（流式时同样中断传输），而不只是隐藏迟到回复。失败或停止后可 **Retry** 原请求，generation 仍绑定原 chat，期间新输入的 draft 不会被覆盖。删除 busy chat 和关闭窗口同样会先取消 transport。选中 Block 的 command/exit 会显示为 composer 上方的 context chip，可在空闲时 **Clear**；Ask Block 失败后，Retry 实际将使用的 pending context 也会明确显示，若输出因行数或字节预算被裁剪，chip 会标出 `output truncated`。关窗前仍留在内存中的 Ask Block retry 会转成该 chat 的可恢复 draft/context。

**New chat** 会创建并立即选中一个新会话，旧会话不会被清除。打开 **Chats** 会话库可搜索和选择所有保留的 chat；首条问题会生成自动标题，也可 Rename。Archive 将 chat 移入归档列表而不删除内容，Unarchive 可恢复；Delete 会先要求确认，再永久移除该 chat。切换 chat 时，未发送 draft、该 chat 实际发给 provider 的选中 Block context 以及当前选中的 chat 都会跟随窗口快照持久化。

每个窗口最多保存 50 条 chat metadata，每个 chat 最多恢复 100 个完整 turn；active 与 archived chat 都计入集合上限。所有 chat 共用一个 8 MiB 紧凑 JSON 总预算，而不是每个 chat 各有 8 MiB。超过全局预算时会优先裁剪最旧内容、保留 chat 条目，并在受影响会话显示 `truncated`，提示更早内容已不在快照中。工作区的 20 MiB Pane/Tab 上限之外另有 64 KiB 专用于完整 chat metadata；空间紧张时会继续裁剪 payload，而不会静默省略整个 Chats 库。旧版单会话 schema v1 会在读取时自动迁移为 v2 Chats 集合。

运行时和持久化预算彼此独立：Chat 单条输入不超过 64 KiB；live message history 每个 chat 至多 100 个 turn、所有 chat 合计至多 8 MiB；一次 provider 请求保留最近至多 40 个 turn、合计至多 256 KiB；selected Block command/output/cwd 分别限制为 16 KiB/64 KiB/4 KiB；解析后的模型文本不超过 256 KiB，curl stdout/stderr 分别限制为 8 MiB/64 KiB。Chat 与 Agent 的可见 activity buffer 各不超过 1 MiB，Agent 核心 transcript 另有 128 KiB/128 entries 上限。全局最多 4 个 provider 请求并行，其余请求等待槽位且仍可取消。达到 `ai_max_tokens` 时回答会显示明确的截断提示。更早 history 被请求预算省略时，模型会收到说明；超出 live/persistence 预算时只移除完整旧 pair，在途问题不会被裁掉，并标记 `truncated`。

selected Block、pane cwd 与配置 shell 不再拼进高信任 system prompt。它们会经过字节截断、JSON 转义和可选脱敏后，作为明确标记的“不可信 terminal/environment data”放入 user-role 请求；命令输出、路径中的提示词、代码围栏或伪造策略都只应作为待分析证据。

后台请求绑定其发起时的稳定 chat ID：切换到其他 chat 不会改变回复目的地，也可让不同 chat 的请求各自完成；如果原 chat 已 Delete，迟到回复会直接丢弃，不能重新创建或污染当前 chat。在途用户 turn、错误回合和命令生成审阅事件不会伪装成已完成回答恢复；待完成或失败的问题会回到可重试 draft，发送期间键入的下一条 draft 也会保留，Ask selected Block 不会清掉已有草稿，关窗会先刷新防抖中的最新内容。开启 `ai_redact_secrets` 时，持久化脱敏覆盖 active、non-active、archived chat，包括标题、turn、draft 和 Block context，而不只处理当前可见对话。该数据与标签/Pane 状态一起使用有界、原子替换的 owner-only 文件；`--safe-mode` 不读取也不发布会话库，`--no-restore` 和显式新工作区仍不领取旧快照，其中 `--no-restore` 继续按既有语义建立新的可持久化工作区。对话仍可能包含敏感命令或输出，发送和保留前应自行检查。

自然语言转命令与 Agent 坚持 review-first：模型只能提出候选，不会自行写入 PTY、提交 Enter 或执行。在命令面板输入 `? 请求` 会把一次性建议固定到当前 Block pane，以块内卡片显示请求、provider、cwd、selected Block context、busy/error 状态和可编辑候选；请求可 **Stop/Retry**，成功后可 **Regenerate**、复制或 **Insert for review**。插入只写入普通 shell 编辑行，不发送 Enter；VTE pane 不提供这张上下文感知卡。

一次性建议、失败命令纠正与 Shell Agent proposal 共用同一套审阅卡逻辑：编辑时实时重算危险模式，Copy 永不写入 PTY，Enter 只触发卡片上明确标出的主操作。已验证的本地纠正只有在文本完全未改且非危险时才显示 **Run verified command**；任何编辑或新风险都会立即降级成 **Insert for review**。

`Ctrl+Alt+G` 或顶部栏的 **Agent** 开关在当前 active Block pane 打开原生 **Shell Agent**；开关保持选中时表示 Agent 会话正在激活。Agent 卡显示固定目标 cwd、安全状态、回合进度、实时 prompt readiness 和 proposal 审阅区，活动消息以普通块留在同一条 conversation flow 中；设置按钮显示 provider/model、shell、命令纠正开关和只读自动放行开关。readiness 会区分空闲、已有输入、命令运行中、全屏程序、prompt 初始化和缺少 shell integration，审批失败时给出对应恢复步骤。打开 Agent 时若已有 selected finished Block，它会作为可见的“不可信上下文”chip 附加，也可移除；会话空闲（Ready）时可用 **Attach selected Block** 把当前选中的 finished Block 附加或替换为新的上下文。请求还会附带有界的 git 元数据（branch、dirty、ahead/behind），与 cwd/shell/OS 一样只作为不可信 user-role 数据发送。Agent 在打开时固定目标 pane，切换标签不会悄悄改变执行目标。VTE pane 不提供 Agent。

**Approve & Run 的当前执行边界**是本机 native Block pane、直接启动的交互式 bash/zsh，以及本版本自带且已在当前 prompt 完成私有握手的 shell integration。Forge 通过一次性私有 FD 把关联 token 交给启动 shell，脚本读取后立即关闭并清除 FD 变量；token 不进入命令 argv、普通环境快照或 jsh execution journal。managed remote pane、Flatpak host bridge、jsh、fish、PowerShell、未知 shell 和 `-c`/一次性 wrapper 不提供 Agent 自动提交；这些环境仍可使用 **Insert only**、普通 AI Chat 和只插入不回车的纠正建议。

审核执行分两阶段：先只插入文本，等 VTE 证明当前编辑器从 prompt anchor 到光标逐字等于审核文本、光标右侧严格为空，再单独发送 Enter；CommandStart 还必须带当前 prompt 私有握手对应的 C/D ID，且上报/渲染的命令身份逐字匹配。leading/trailing whitespace、右提示符、可见 autosuggestion、未闭合引号/heredoc 等无法在时限内形成可信 CommandStart 的情况会 fail closed；若 Enter 已发送但身份随后无法证明，UI 会要求先检查终端再重试，而不会声称“肯定没执行”。需要保留这些 shell UI 特性时使用 **Insert only**。

这个边界把普通前台子进程伪造的 OSC 生命周期与 Agent observation 隔开，但不把交互 shell 本身当成敌对沙箱：用户加载的 shell rc、函数、hooks 和键位绑定仍属于可信 shell 配置，同一 shell 内代码可读取 shell 私有状态。私有 token 是关联与误绑定防护，不是针对恶意 shell 配置的密码学认证。

一次 Agent 会话的安全流程是：

1. 输入任务后，模型回复必须是严格 JSON `say`、`run` 或 `done`；夹杂 prose、未知字段、错误类型、过期 proposal 或非法控制字符都会 fail closed，不能退化为可运行命令。用户任务也有 16 KiB 上限。
2. `run` 只能包含一条可见单行命令，CR、LF、Tab、NUL、ESC 等控制字符无论来自模型还是编辑结果都会被拒绝。proposal 卡可复制和编辑；风险提示会随编辑实时重算。
3. 每张卡片可 **Reject**、**Insert only** 或显式 **Approve & Run**。Reject 会进入 transcript 并要求模型换方案；Insert only 把最后编辑值写入普通 shell 编辑行供手动处理，不发送 Enter，并把“未执行”写入 Agent transcript；批准执行的是用户最后编辑后的精确文本。识别到顶层 `rm -rf`、`mkfs`、提权、强制 Git 改写、下载后 pipe 到 shell 等模式时，除醒目提示外还必须在显示精确命令的第二个确认框中再次批准。
4. 批准前再次检查固定 Block prompt：正在运行任务、已有未提交输入、可见右提示/建议或当前 shell 未完成受支持的私有握手时拒绝写入，待 prompt 空闲且严格清空后才能重试。
5. 已批准命令形成 finished block 后，匹配的 exit code 和有界输出作为 observation 回灌，Agent 才能提出下一步。不相关命令不会被当成该 proposal 的结果。
6. 模型请求进行中可 **Stop** 当前 turn，并在保留 Agent session 的前提下 **Retry**，不会复制 user turn。模型以 `done` 完成任务后，**Follow up** 会保留 transcript 并重新开放输入；`agent_max_turns` 达到上限后，**New task** 可在同一 pane 清空旧模型上下文并恢复完整回合预算。**Cancel Agent** 或关闭窗口则取消整个会话并等待 transport 回收。已经由用户批准并启动的普通终端命令不会被这些按钮暗中 kill，仍使用标准 pane/tab 关闭确认管理。若窗口在已批准命令的 finished-block observation 回灌前关闭，重启不会猜测结果或把旧执行绑定到新 PTY；该有效但无法续接的 checkpoint 会被安全废弃，Agent 从新的 Ready 会话开始。

dashboard 和 Settings 中的 **AI command correction** 开关控制 `command_correction_enabled`。开启后，Block 命令出现 typo、unknown executable/package、invalid subcommand/option 等窄范围错误时才会提供可编辑纠正；候选不会自动插入或执行。关闭开关会立即阻止新的纠正，也会丢弃仍在解析中的待显示结果。默认开启，可用 `FORGE_COMMAND_CORRECTION_ENABLED` 临时覆盖；确定性目标提示与本地索引优先，AI 仅为 fallback，完整边界见 `docs/SMART_COMMAND_CORRECTION.md`。

`agent_enabled = false` 可独立关闭 Agent，`agent_max_turns` 限制模型回合数；`ai_enabled = false` 和 safe mode 都会同时阻止打开。Agent 必须被视为有用户权限的命令执行辅助工具，危险模式提示不是完整 shell 安全分析，也不替代逐字审阅。

`agent_auto_approve_readonly` 与 `FORGE_AGENT_AUTO_APPROVE_READONLY` 仅作为旧配置兼容键保留，运行时始终归一化为关闭。所有 proposal 都必须逐条批准。原因是命令文本本身无法证明实际执行对象：alias、function、Git helper、工具的写入/执行 flag，以及读取后会发送给模型的敏感文件都跨越了字符串白名单的安全边界。Settings 会明确显示该能力已停用，旧配置为 `true` 时 `--check-config` 会给出迁移警告。

`ai_redact_secrets = true` 默认遮蔽常见密钥格式，并在持久化前重新处理所有 active、non-active、archived chat 及其 draft/context；但脱敏不是秘密保护边界，发送前仍应检查上下文。`--safe-mode` 同时关闭 AI 与 Agent。

开发、回归与发布检查见 [AI / Agent / Chat 验收矩阵](AI_AGENT_CHAT_ACCEPTANCE.md)；该矩阵是测试要求，不代表其中所有目标均已实现。

## 12. 配置保存与快捷键

完整字段见 `config.toml.example`。保存后自动热重载，`Ctrl+Shift+R` 手动重载。语法或语义错误不会替换当前有效配置。

应用内设置保存还会：获取进程级 advisory lock、检查加载时 revision、拒绝并发编辑冲突、用 owner-only 临时文件 `fsync` 后原子替换，并轮换 `.bak` / `.bak.1` 两份经过验证的备份。恢复前的当前文件另存为 `.before-restore`。冲突、验证拒绝、锁超时和 I/O 错误会在窗口中明确提示；内存中的临时改动仍有效，但磁盘不会被覆盖。发生冲突时先重载配置再重新应用改动；必要时运行 `forge --restore-config-backup`。safe mode 中的设置只影响当前窗口，也会明确提示不会保存。

覆盖或解除快捷键：

```toml
[keybindings]
show_remote_picker = "F8"
toggle_ai_panel = false
```

修饰键名称不区分大小写：`Ctrl` / `Control`、`Alt` / `Option` 等价，`Super` / `Cmd` / `Command` / `Win` / `Meta` 映射为同一跨平台修饰键。数字小键盘的 `0`–`9` 与主键区数字共用组合（例如 `Ctrl+1`）；小键盘 Enter 和运算符仍保持独立。若两个 action 使用同一组合，配置检查器会报告冲突。`Ctrl+R` / `Ctrl+P` 留给 shell/readline。

`[keybindings]` 只管理命令面板中可发现的应用动作。Block 视图的上下文键
`Alt+Shift+F`（过滤）、`Ctrl+Shift+B`（书签）和 `Ctrl+,` / `Ctrl+.`（前后书签）
目前是固定绑定，不能通过该表覆盖或解除。

## 13. 状态与历史位置

- 配置：`~/.config/forge/config.toml` 及 `.bak` / `.bak.1`。
- 窗口快照：`~/.config/forge/windows/window-*.active|state`。
- JSONL 命令历史：`${XDG_STATE_HOME:-~/.local/state}/forge/history.jsonl`，可用配置覆盖。
- 可选 Block 全量历史：由 `block_history_path` 指定，可能包含输出。
- 用户 workflow：`~/.config/forge/workflows/*.{toml,yaml,yml}`。
- 已安装示例与 Notebook：`${prefix}/share/forge/`。

配置、快照与历史包含敏感工作信息，备份或分享前应主动检查。

## 14. 故障排查

```bash
forge --doctor
forge --check-config
FORGE_LOG=debug forge --no-restore
forge --safe-mode
forge-support-bundle .
```

- GUI 无法启动：确认 `DISPLAY` 或 `WAYLAND_DISPLAY` 以及 GTK/VTE 动态库。
- 中文输入无预编辑：检查 `GTK_IM_MODULE`、`XMODIFIERS` 和 fcitx5/ibus GTK4 模块。
- Block 缺少准确 exit/cwd：加载对应 shell integration。
- AI 不可用：检查 `ai_enabled`、provider 对应密钥、base URL 和 `curl`。
- 欢迎 Notebook 找不到：重新安装资产，或设置 `FORGE_ASSET_DIR=/path/to/share/forge`。
- workflow 示例找不到：检查 `${prefix}/share/forge/workflows`；非默认 prefix 可设置 `FORGE_WORKFLOW_DIR`。
- 长命令无通知：检查 `notify_long_blocks`、阈值、`notify-send` 和通知服务。
- SSH 无目标：添加 `[[remote_hosts]]` 后按 `Ctrl+Shift+S`。
- 配置修改没生效：先运行 `--check-config`；并发冲突需要重载后再保存。
