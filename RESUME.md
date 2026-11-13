# 续接笔记（2026-11-08 更新）

> 这是**工作笔记**，不是用户文档。接着干先看这里，再看 `tools/ROADMAP.md`。
>
> ⚠️ **2026-11-01 起仓库布局变了**（`rust/` 子目录已取消）：crate 直接在**仓库根**，
> 本仓库就是 GitHub 上的发布仓库（`git@github.com:fwt11/puttyplus.git`），
> 改完 `git push` 即发布。
> ⚠️ **2026-11-11 起台账也入库了**（`AGENTS.md` / `RESUME.md` / `ROADMAP.md` /
> `FEATURE_PARITY.md` / `DBX_PLAN.md` / `UI_AUDIT.md` 与 UI 审查文档 /
> `docs/功能说明-内部.md` / `docs/README-开发版.md`）⇒ **换台机器 clone 下来
> 就能接着干**（用户：「把本地台帐也加入git，方便在其他平台继续开发」）。
> 仍是纯本地的只剩 `c/`、`tools/compat_check.sh`、`tools/shot_*.sh` 与产物目录。
> 旧开发历史在 `../putty-dev-history-2026-11-01.bundle`。细节见 AGENTS.md 末节。
>
> ⚠️ **下面的第二节及以后是 2026-10-27 那轮的历史记录**（阶段 19/20 的收尾），
> 留着是为了别把当时的坑忘了；**当前状态看第一节**。

## 〇之二之七、最新进展（2026-11-14：阶段 85 —— dbx 对标补「生成值」+「列宽拖拽柄」）

- **起因**：用户 2026-11-13 问「还有什么我们没有实现的吗」⇒ 列了 dbx 对标清单里
  还差的项 ⇒ 用户拍板「按你的建议来」⇒ 做**用户最容易察觉的两件**：
  ① **生成值**；② **列宽拖拽柄**。
- **① 生成值（`ed92618`）**：结果网格**行右键菜单**加「生成值…」⇒ 对话框里选
  **序列/自增、UUID、当前时间、当前日期、随机数**五种（实时预览「将写入：=xxx」）。
  - **按方言分派**（`dbmeta::GenValue::expr`）：UUID = PG `gen_random_uuid()` /
    MySQL `UUID()`；随机数 = PG `random()` / MySQL `RAND()`；当前时间 / 当前日期
    两家通用；**序列 / 自增两家都用 `DEFAULT`**（列的默认值：PG 的 serial/identity、
    MySQL 的 AUTO_INCREMENT ⇒ 不用知道序列叫什么名字）。
  - ⚠️ **值由数据库算**：序列必须由数据库分配（客户端自己数会撞号）；当前时间要用
    **数据库的时钟**（客户端时钟可能跟库差几分钟、时区也可能不同）。
  - ⚠️ **写进格子的是一条表达式**（`=CURRENT_TIMESTAMP`）：`dbmeta::edit_value()`
    认 `=` 前缀 ⇒ 原样拼进 SQL（不加引号）⇒ 库里存的是算出来的值。
    ⚠️ **另起函数而不是改 `edit_literal`**：后者还被导入 / 导出共用 ⇒ 那些地方的
    文本是**数据**，不能当 SQL 执行（改那儿就是导入漏洞）。
- **② 列宽拖拽柄**：
  - ⚠️⚠️ **本轮最值钱的一条：原先那条判断是错的**——台账里写着「GTK4 无内置，
    得自实现 GestureDrag + fixed_width，收益低、风险高」⇒ **差点白写一整套手势**。
    实际：**GTK 自己就能拖**（`set_resizable(true)` + 抓住列按钮的外边缘）；那个
    "Deprecated: 4.10" 说的是 **TreeView 整个过时**（该换 `GtkColumnView`），
    **不是**"不能拖了"。2026-11-14 实测：把它设成 `false` 就真的拖不动了。
    ⇒ **教训已写进 AGENTS.md 第 13 条**：别把 GTK 的 "Deprecated" 当成"功能没了"。
  - **真正缺的是"拖了之后别被冲掉"**：`apply_column_widths` 每次都按"可用宽度"
    重排所有列 ⇒ 不记住用户拖出来的宽度，拖完一松手、面板一窄就没了。
  - ⚠️ **怎么记住**：**看 `fixed_width` 而不是 `width`**——`width` 是 GTK 实际分配
    的宽度（要等布局跑完才准）⇒ 拿它跟"打算给的"比会把**每一列**都误判成"用户
    拖过"（实测四列全被钉死 ⇒ 自动分配整个废掉）；`fixed_width` 是我们自己设的值
    （一设就准），而 GTK 在**用户拖拽**时**也会改它** ⇒ 一比就知道。
  - **宽度分配**：新增纯函数 `alloc_widths_with_fixed()`；列菜单加「恢复默认宽度」
    （只有拖过才出现）；换结果集时清掉用户宽度。
  - **顺带修一个真 bug**：列头**右键**菜单用的是**显示位置**，而菜单里的操作全按
    **原始列下标**走 ⇒ 藏过列之后右键第 2 列会作用到别的列上。
- **顺带修一个 flaky 单测（`7b23cab`）**：`connmgr::tests::save_roundtrip_and_update`
  偶发失败（跑三次挂一次）⇒ 根因是断言依赖**全局凭据库**的解锁状态 ⇒ 改成看 ini
  的内容；顺带堵住"库还开着、路径已切回用户真实凭据库"那个会**写用户真实配置**
  的窗口。
- **验证**：单测 745+5 绿；契约 73；新冒烟 `examples/grid_colwidth_smoke.rs`
  （**xdotool 真拖**列头边缘 ⇒ 列宽真变了**且被记住**、单击列头照常排序、恢复默认
  宽度、藏列后菜单作用到正确的列）；`db_ui_smoke` 加了「生成值」的完整真实路径。
  ⚠️ **四组反向验证**（不记用户宽度 / 列菜单退回用显示位置 / `build_updates` 退回
  `edit_literal` / 菜单里不生成「生成值…」⇒ 当场红）。

## 〇之二之六、最新进展（2026-11-12：数据库工作台对标 dbx）

- **起因**：用户说「不光是筛选，我们可以多拿一些它的优点」⇒ 我**实际打开 dbx 点了
  一遍按钮并截图对比**（工具条 / 底栏 / 列头 chevron 菜单 / 网格右键菜单 / 表属性
  抽屉 / 预览 SQL 抽屉 / 新建查询编辑器 / 左侧树右键），再对着自己这边（aly 真库）
  拍图比差距 ⇒ 用户拍板本轮做 **D1 + D2 + D3**。
- **D1（`2a56f24`）常驻 WHERE 框**：一个宽输入框 + **回车即检索**（自由文本），
  结构化挑条件收进 ▾ **条件助手**（按列类型转义）；**条件 chip 行整个退场**。
  ⚠️ WHERE 的唯一真相是自由文本；⚠️ 只拦引号外的分号；⚠️ placeholder 必须短
  （GtkEntry 的自然宽跟着它走 ⇒ 长例子把这一行的最小宽度撑到 300 多 px ⇒ 右边
  整排按钮被切掉）。
- **D2（`6802607`）条件并进工具条一行 + 底栏三段式**：条件条以前独占一行 + chip
  再占一行 ⇒ 网格被挤掉一大截 ⇒ 并进工具条（网格多出两行）；页签「状态」从工具
  条搬到底栏；底栏 = 左（行数·耗时·待保存）+ 中（**当前 SQL 常驻**）+ 右
  （**「统计总行数」** + 每页行数 + 分页）。
  ⚠️ **「总行数」= COUNT(\*)，是真值**（对象树里那个 ≈ 是估算值）；⚠️ 只有表
  浏览页签能数（手写查询不知道表名 ⇒ 按钮灰着）。
- **D3 属性页加「查看 DDL」**：属性页下面多一段 **CREATE TABLE 文本**（只读 + 可
  复制 + SQL 高亮）；⚠️ **视图不假装有**（视图定义要问数据库自己，我们没这条通
  路 ⇒ 只显示一句说明）。
- ⚠️ **本轮查清的一件事**：工具条右侧「保存 / 放弃」**不是被切掉**，是按设计在非
  编辑模式下隐藏（`set_edit_toolbar`）—— 量过每个控件的实际分配宽度才确认的
  （**被切掉的按钮 min 不会变 0，隐藏的才会**）。
- ⚠️ **踩到的 GTK 坑**：属性页那块统计 Grid 一开始写成 `vexpand(false)` ⇒ Grid
  是**回包之后**才加行的 ⇒ 量高度时只有表头那一行 ⇒ 11 行属性全被裁掉（只看得
  见「属性 | 值」两个表头字）⇒ **要 flex 的那块必须 flex**。
- **验证**：单测 740 绿；新冒烟 `where_box_smoke` / `foot_bar_smoke`；真库 e2e 验
  count(\*)=3 与网格行数一致、属性页显示完整 CREATE TABLE、点「复制」后剪贴板里
  真的有那段语句；契约 73 绿；⚠️ **新断言都做了反向验证**（改坏实现 ⇒ 对应断
  言当场红）。
- **本轮提交**：`2a56f24`(D1) / `6802607`(D2) / D3（待提交号）⇒ 发版 tag 最终定了 **v0.6.0**（2026-11-12 发出，Release 资产齐全）。
  ⚠️ **2026-11-13 用户拍板：保留 v0.6.0，下个版本发 v0.7.0**（当时问卷里
  选过 v0.5.5，但那是在 v0.6.0 已经发出之后才到达的 ⇒ **不动已发布的
  tag**：删 tag 重发会让 v0.6.0 的下载链接失效，代价大于收益）。

## 〇之二之五、最新进展（2026-11-11 深夜～12 日：修「上传时界面整体卡死」+ 传输队列角标重做）

- **用户报的现象**（2026-11-11 深夜）：在堡垒机上上传 1GB 文件（8Mbps）时，
  **界面突然完全卡住**（终端、菜单都点不动），进度停在 2%，很久后突然跳到
  80%，然后才恢复。用户补充：文件列表不大；猜是堡垒机限制多连接、上传把
  别的请求挤掉了。
- ⚠️ **根因其实是两个（互补的）**：
  ① **整表重画（主因）**：昨晚 `8d49871` 引入的呼吸动画每 60ms 对**整棵文件
     列表** `queue_draw()`，每个进度 tick（每 64KB）又 `queue_draw()` ⇒ 每个
     可见行都要新建 Pango 布局 + 量文字宽度 ⇒ **主线程一帧 59ms**（实测：
     面板没挂到窗口时只有 12ms ⇒ 成本几乎全在「整棵列表重画」）⇒ 堡垒机的
     远程桌面再乘一个 X 往返 ⇒ GTK 主循环被拖死（菜单也卡）⇒ 积压的事件
     一起回放 ⇒「2% 突然跳到 80%」。
  ② **传输与面板请求挤同一条 SFTP 通道**：SSH 通道窗口是**每通道一份** ⇒
     大文件上传吃光它那条通道的窗口 ⇒ 服务端连目录列表都回不来 ⇒ 刷新/
     预检/目录跟随全排在上传后面。
- **修法（`6ad7229`）**：
  - `panel/pane.rs`：`set_progress`/`set_terminal`/`tick_fade` **不再**
    `pane.view.queue_draw()`（`model.set` 已经让**那一行**变脏 ⇒ GTK 只重画
    那一行）；新增 `repaint_xfer_rows()`（只把 `COL_XFER==true` 的行标脏）与
    `queue_draw_full()`（整表重画的统一入口 + 计数）；
  - `panel/mod.rs`：`sync_xfer_anim`（60ms 动画帧）改调 `repaint_xfer_rows`
    （原来每帧整表重画）；
  - `panel/worker.rs`：**进度事件限流 100ms**（数字仍然每次写进 `shared`，
    只是 UI 通知变稀 ⇒ 与网速解耦；100Mbps 下本来能到 195 次/秒）；
  - `backend/conn.rs`：新增 `transfer_backend()` ⇒ **传输走自己的 SFTP
    通道**（`subsystem sftp`，**同一条 SSH 连接** ⇒ 不多认证、不多 TCP；
    开不出来就退回共用那条）。⚠️ **守住了「一台服务器一条连接」这条硬约束**。
- ⚠️ **断言要选「与实现无关」的度量**（这轮最值钱的一条）：
  早先的断言数的是「我们自己调了几次 `queue_draw`」⇒ 改回整表重画照样绿
  ⇒ 改成数**单元格实际绘制次数**（`panel/namecell.rs` 的 `draw_count()`）
  ⇒ 按「每秒 ≤40 次」卡。实测：**5727 次 → 39 次**（147 倍）；整表重绘
  4 次/上传。⚠️ **新断言必须先反向验证**（把 `sync_xfer_anim` 临时改回
  `p.view.queue_draw()` ⇒ 5727 > 上限 694 ⇒ 退出码 101 ⇒ 断言真抓得住）。
- **顺带修的断言**（`9500ac9`）：`examples/xfer_pulse_smoke.rs` 原来只截两张
  图比像素 ⇒ 呼吸相位按时间算 ⇒ 差异随机（0.00027~0.00154）⇒ **断言本来
  就会随机翻红** ⇒ 改成扫完一个 1.6 秒周期取相邻差的最大值。
- **传输队列角标重做（`7465d28`）**：用户「这个感觉不是很好，或者你可以在 1
  周围有个圆圈在转？」。上一版（`Label` 蓝底 + `Overlay` 盖一个转圈）看着
  别扭，两个根因：① 角标**不是圆的**（`Label` 的自然高度由字体行高决定 ⇒
  蓝底是胶囊）；② **圈和数字不同心**（`GtkOverlay` 给覆盖层分配的是它自己的
  尺寸 —— `gtk_overlay_get_child_position` 按 `halign` 对齐，`FILL` 时铺满 ⇒
  22×22 的圈跟 15×15 的角标中心差了几像素，还被按钮边缘裁掉一截）。
  ⇒ 改成自成一体的控件 `src/panel/xferbadge.rs`：**蓝圆、数字、弧全部自己摆**，
  外框固定 26×26 ⇒ 保证同心、保证是正圆，弧在蓝圆外留 2.8px 的缝。
  顺带修一个真 bug：`queue::btn_state` 原来在按钮的直接子控件里找角标
  `Label` ⇒ 角标套进 `Overlay` 之后就**一直读不到数字**（返回空字符串）。
- **验证**：729 单测绿（+2 条角标几何单测）、契约 PASS=73 FAIL=0、
  `fileops_smoke` / `ring_smoke` / `xfer_icon_shot` / `xfer_stall_smoke` /
  `xfer_pulse_smoke` 全绿；新断言**都做了反向验证**。
- ⚠️ **本轮环境**：测试 sshd 在跑（`~/.cache/puttyplus-test-sshd` 端口
  **2233**、公钥 `~/.ssh/id_rsa`）；旧的 `/tmp/pp_selftest` 端口 2222 也起着。
  跑 `fileops_smoke` 时 `HOME` 指到临时目录 ⇒ **密钥要另指**
  （`PP_SSH_KEY=/home/fwt/.ssh/id_rsa PP_SSH_USER=fwt`）。

## 〇之二之四、最新进展（2026-11-11 晚：三个界面需求 + 顺带修「选中态看不清」）

- **用户提的三件事**：
  1. 「**编辑器查看上限改为可配置**」；
  2. 「上传下载文件时**传输队列图标**可以改为原来静态的图标上加个**转动的圈**」；
  3. 「数据库列头的「⋯」按钮**看着像菜单** ⇒ 换个不像菜单的呈现」。
- **顺带修的 bug**（用户看完截图补的）：「**选中的颜色都太深了，看不清文字了，
  文件列表中也是同样的问题**」。
- **四个提交**（一个功能一个提交）：
  - `acc1404` **编辑器查看上限可配置**：新增 `settings::editor_max_bytes()` /
    `set_editor_max_bytes()` + 纯函数 `parse_editor_max_bytes()`（写坏了的值回
    默认值，**不是**当 0＝不限）；`fileedit::MAX_BYTES` 常量改成 `max_bytes()`
    （读设置）；首选项「高级」页加「编辑器查看上限」（1..64 MB，下限 64 KiB、
    上限 64 MiB）；新增 `examples/editor_limit_smoke.rs`（钉住「上限**真的挡住**
    了打开文件」）。
  - `3072df6` **修：选中态的底色压住文字**（见下面的「坑 ① ②」）。
  - `27d5377` **传输队列按钮上叠一个真正在转的圈**：新增 `src/panel/ring.rs`
    （**自己画**，不用 `gtk::Spinner`——它的最小尺寸写死 16×16 ⇒ 盖在一个 16×16
    的图标上就是**整个盖住**；而且它吃图标主题的 spinner 资源 ⇒ 极简主题下可能
    是空白）；底下还是原来那个队列图标（**不再换成静态的
    `content-loading-symbolic`** ⇒ 那个看着像「卡住了」而不是「在转」）；
    颜色走 CSS 变量 `@pp_accent` ⇒ 换主题自动跟着换；新增
    `examples/ring_smoke.rs`（钉住「它**真的在转**」——角度在变，不是「设了个
    标志」）+ `fileops_smoke` 里钉住「传输中转、传完停」。
  - `770f3e1` **数据库列头「⋯」→ 齿轮**（`preferences-system-symbolic`）：
    ⚠️ **不用「▼」**（`pan-down-symbolic`）——下拉箭头**也是**菜单的通用记号 ⇒
    换过去还是像菜单；齿轮 = 「这一列的设置」，不像菜单。
- ⚠️ **踩到的坑（这轮最值钱的两个）**：
  ① **GTK4 的 CSS 选择器匹配的是「节点名」不是控件类型** ⇒ 按钮里的文字是
     **一个独立的 `label` 节点**（`GtkLabel` 的节点名就是 `label`，见
     `gtk/gtklabel.c` 的 `gtk_widget_class_set_css_name`）⇒ 我们那条 blanket
     规则 `label { color: @pp_fg }` 给它一个**显式**颜色 ⇒ 按钮自己的
     `color: @pp_on_accent`（只是**继承**下去）被盖掉 ⇒ 浅色主题下就是
     **强调蓝底 + 深色字**（首选项里选中的「标准」「中」两个按钮中招）。
     ⇒ 修法：给 label 节点**单独**定一条（`button:checked label` 等）。
     ⚠️ `GtkToggleButton` 的节点名就是 `button`（`gtktogglebutton.c`）⇒
     `button:checked` **匹配得上**，只是颜色没传到里面的文字。
  ② **自己画的 cell renderer，取色要自己算对状态**：文件列表的**文件名列是
     我们自己画的**（`panel/namecell.rs`）⇒ 颜色由我们取 ⇒ 少算了 `:focus`
     状态 ⇒ 键盘光标行（GTK 画的**底**是实心强调色）拿到的是 `:selected` 的
     **深色字** = 蓝底黑字（其余列是 GTK 自己画的 ⇒ 是白的 ⇒ **只有文件名
     看不清**，特别怪）。
     ⇒ 修法：取色时把 `:focus` 也算进去，且**只在树视图真有键盘焦点时**算
     （跟 GTK 的 `gtk_cell_renderer_get_state()` 一个口径）⇒ 抽成纯函数
     `namecell::cell_text_state()` 便于单测。
- ⚠️ **别再用 heredoc 改中文源码**：`python3 - <<'PY'` 里的中文会被 bash 弄出
  **U+FFFD**（一个字符变成两个替换符）⇒ 改中文一律用 `write` 工具写脚本文件
  再执行 + 改完 grep 验证。
- ⚠️ **批量替换大写常量会误伤**：`MAX_BYTES` → `max_bytes()` 时把
  `HIGHLIGHT_MAX_BYTES` 也改了 ⇒ 替换后必须 grep 一遍。
- ⚠️ **GTK 的 `mix()` 权重是「第二个颜色的占比」**：`mix(#ff0000, #0000ff, 0.25)`
  = **75% 红 + 25% 蓝** ⇒ 我们 `mix(@pp_accent, @pp_bg, 0.86)`（= 86% 底色）
  才是想要的「淡强调底」⇒ **别把它当成「第一个颜色的权重」**（差一次就反了）。
  （实测法：CSS 里 `@define-color x mix(...)` + `style_context().lookup_color("x")`
  ⇒ 直接读出算出来的 RGBA。）
- ⚠️ **CSS 单测别只 `contains()` 半句话**：注释里也写着那句话 ⇒ 只匹配半句的话，
  删掉规则单测照样绿（**假阳性**，钉不住）⇒ 要匹配**规则块本身**。
- **验收**：727 单测绿、契约 PASS=73 FAIL=0、`cargo check --locked --lib --examples`
  无错、editor_limit / ring / fileops / grid_header / db_ui / settingsui /
  settings_live / fileedit_panel 冒烟全绿；三条新断言**都做了反向验证**
  （退回旧行为 ⇒ 冒烟/单测当场红）。

## 〇之二之三、最新进展（2026-11-11 晚：连接中枢——一个服务器一条 SSH 连接）

- ⚠️ **用户 2026-11-11 晚拍板的新方向**：
  > 「我们默认情况下 sftp, 端口映射，数据库管理等等都走通道，
  > 　**我们可以保证对每个服务器的访问只需要一个连接。**」
- **三个子决定**（用户拍板）：① 终端也并入共享连接（留开关切回 spawn）；
  ② SFTP 走 `subsystem` 通道（同一条连接上的另一个 channel）；③ 按
  `host+port+user` **全局**共享一条连接（跨标签页也复用）。
- **已落地（提交）**：
  - `f601b7f` **阶段 A/A2**：新增 `src/backend/hub.rs`（连接中枢：全局
    multi_thread 运行时 / `ServerKey` 归一化 / `acquire` 统一在全局 runtime
    上建连 / 引用计数 / 转发规则按来源分组）+ `SftpBackend` 改持有共享连接
    （`close()` 只关通道不断连接）+ 面板 worker 改用全局运行时 +
    顺带修「停转发后立刻重绑同端口 ⇒ EADDRINUSE」的真 bug +
    新增 `examples/hub_share_smoke.rs`。
  - `8f0327b` **阶段 B**：进程内 SSH 走 `hub::acquire`（终端与面板共用一条
    连接 ⇒ 密码只问一次）+ 终端的 `pp-ssh-shell` 线程改用全局运行时 +
    端口映射与面板**算同一份规则**（`forward::inproc_rules`）+ 新增
    `examples/hub_shared_gui_smoke.rs`（**端到端**：终端 + 文件面板 ⇒
    全局仍然只有 1 条连接）。
- ⚠️ **还没做的**：
  - ✅ **阶段 B2（2026-11-13 已做）**：VTE 后端也能用进程内 SSH
    （**假 pty** 接入）⇒ Linux 默认后端现在**真的并入共享连接**了
    （VTE 的键盘输入必须过本地 pty ⇒ 自己开一对 pty：master 交给
    VTE、slave 留给我们桥接；slave 侧切 raw ⇒ 不双回显、Ctrl-C 能到
    远程）。代价：终端**不走用户自己的 ssh** ⇒ `~/.ssh/config` 不生效
    ⇒ 需要的话首选项 →「终端」页 →「终端连接方式」切回 spawn。
  - **阶段 C**：开关（首选项 →「终端」页 →「终端连接方式」）的界面 +
    台账补记（代码已写，文档随本条一起补）。
- ⚠️ **踩到的坑（别再踩）**：
  ① **russh 的 `Handle` 绑定在创建它的 tokio runtime 上** ⇒ 共享连接必须
  让所有功能在**同一个 runtime** ⇒ 有了全局 `hub::runtime()`；
  ② **`Runtime::block_on` 在 current_thread runtime 上会被独占** ⇒ 全局
  运行时必须是 multi_thread；
  ③ **`hub::acquire` 必须在全局 runtime 上建连**（哪怕调用方在别的
  runtime 上）⇒ 否则那条连接跟着调用方的 runtime 一起死；
  ④ **停转发规则时 `abort()` 是异步的** ⇒ 监听端口还占着 ⇒ 紧接着重绑
  同一个端口必撞 EADDRINUSE ⇒ 要等 accept 循环真的结束；
  ⑤ **`conn_count()` 不能数「注册表槽位」**（每个 key 只记一条 ⇒ 建了两条
  也看不出来）⇒ 要数真正活着的连接对象；
  ⑥ **冒烟的「已连接」信号要可靠**：面板的 `pane_cwd` 一开始就是 `/`
  ⇒ 不是信号；用 `panel.conn_ok()`（新加的访问器）。
- ⚠️ **本机环境**：测试 sshd 要自己起（`~/.cache/puttyplus-test-sshd` 端口
  **2233**；旧的 `/tmp/pp_selftest` 端口 2222 也起了一个）⇒ 冒烟默认连
  2233。

## 〇之二之二、最新进展（2026-11-11 上午：CI 连红 10 轮的根因）

- ⚠️ **用户 2026-11-11 早上问「github 构建似乎报错了」** ⇒ 查下来 **CI（Linux）
  从第 50 轮起就一直是红的**（连红 10 轮，每轮都挂在「GUI 冒烟」那一步），
  **不是**阶段 3 引起的（阶段 3 的 Windows CI 是**绿的**）。
- **根因**：`01efb43`（软链：双击跟进去）把公开函数
  `panel::on_row_activated` 的签名从 4 个参数改成 5 个（多了 `islink`），
  **漏改了 `examples/selfheal_smoke.rs`** ⇒ 那个 example 编译不过 ⇒
  CI 的 GUI 冒烟步骤（它 `cargo run --example` ⇒ 要编译）就挂。
- ⚠️ **为什么本地全绿**：`cargo test --lib` 与 `cargo build --release`
  **都不编 example** ⇒ 签名漂移本地永远发现不了。
- **修法**：① 补上那个参数（`islink=false`，"sub" 是普通目录）；
  ② **CI 加一步「编译全部 example」**（129 个 example 全查一遍，不只 CI
  跑的那 5 个）⇒ 签名漂移立刻红；③ **AGENTS.md 的每阶段动作补上
  `cargo build --release --examples`** ⇒ 本地就能抓到。
- **两个提交（已 push）**：`26b74fe`（CI 修红）+ `210a941`（台账入库）。
  ⇒ **CI 第 60 轮（Linux）**：单元测试 ✓、**编译全部 example ✓**（arm64 +
  amd64 都过）、GUI 冒烟 ✓、契约 ✓、打包 deb ✓ ⇒ **绿**；
  **CI 第 48 轮（Windows）**：绿。
- ⚠️ **本地一个坑**：按 CI 那套跑 GUI 冒烟时**改了 `HOME`** ⇒ cargo 找不到
  rustup/cargo 的家 ⇒ 换了 rustc ⇒ **依赖指纹全变 ⇒ 全量重编**（129 个
  example 要 10 分钟）⇒ `db_ui_smoke` 撞上 180s 超时被误判成失败。
  ⇒ 跑之前 `export CARGO_HOME=/home/fwt/.cargo RUSTUP_HOME=/home/fwt/.rustup`。
  ⚠️ **CI 没这个问题**（那边 `RUSTUP_HOME=/opt/rust/rustup`、
  `CARGO_HOME=/opt/rust/cargo` 是用 `$GITHUB_ENV` 固定到后续步骤的）。
- ⚠️ **教训**：改了 `pub fn` 的签名 ⇒ **先把所有调用点 grep 一遍**
  （`grep -rn "函数名" src/ examples/`）⇒ example 也是调用点。
- ⚠️ **看 CI 失败的办法**（没有 token、拿不到 run 级日志时）：
  `GET /repos/<owner>/<repo>/check-runs/<job_id>/annotations` ⇒ 注解里是
  CI 脚本自己塞进去的 **gzip+base64 分片日志** ⇒ 拼回来解压即可
  （⚠️ 分片可能被 GitHub 截断，解压报 EOFError 时用 `zlib.decompressobj`
  逐段解，能拿到多少是多少）。

---

## 〇、最新进展（2026-11-11：跨平台阶段 3 收官）

- **主线 `main`** = `66e9fb5`（已 push）。本轮做完跨平台改造的**阶段 1–5**
  （阶段 1 端口映射 `55c4709`、阶段 2 DB 工作台 `3cfdb53`、阶段 3
  `66e9fb5`、阶段 4 ghostty 默认特性 `57800d8`、阶段 5 大小列定宽
  `1444ef4`）⇒ 用户 2026-11-10 说「做完了就推送关机」⇒ 本轮到此收官。
- **阶段 3（`66e9fb5`）干了什么**：
  1. **Windows 的 SSH 走哪条路**改掉了：新增
     `util::find_program_in_path_win`（认 `.exe`）⇒ 以前只找无扩展名的
     `ssh` ⇒ **明明装了 OpenSSH 也说「找不到可用的客户端程序」**
     （用户 2026-11-08 报的根因之一）。现在
     `term::default_inproc_ssh`：Windows 上**有 `ssh.exe`/`plink.exe` 就
     spawn**（跑用户自己的客户端 ⇒ `~/.ssh/config`、agent、ProxyJump
     语义跟 Linux 一样），**一个都没有才走进程内 russh**。Linux 不变。
  2. **ConPTY 双向 UTF-16LE ⇄ UTF-8 转码**（原来直接透传原始字节 ⇒
     Windows 上满屏乱码、中文打不进去）⇒ 新增
     `src/platform/utf16.rs`（**不带平台门控** ⇒ Linux 上能单测）。
  3. **`tools/win_cross_check.sh` 现在也查 `conpty.rs`** ⇒ Windows 专属
     代码在 Linux 上几秒就能验编译，不用等半小时的 Windows CI。
  4. 新增 `examples/utf16_pipe_smoke.rs`（真实 TCP 管道当 ConPTY 替身）。
- **本轮验证**：单测 **716 绿** / 契约 **PASS=73 FAIL=0** / Windows 交叉
  检查零错误 / `term_reconnect_smoke`、`ssh_shell_smoke`、`fwd_inproc_smoke`
  全绿 / `utf16_pipe_smoke` 5 项全绿。**反向验证**：去掉 UTF-16 编码 ⇒
  冒烟 3 项失败（乱码形态 `곧肸ꇨ₌룤螖` 与 Windows 上一致）；conpty.rs 里
  故意加错参数 ⇒ 交叉检查当场报错。
- ⚠️ **还没在 Windows 真机上验过**（本机没法交叉编译整个程序）⇒
  Windows CI + `docs/跨平台-构建说明.md` §10 那张人工清单要跑一遍。
- ⚠️ 本轮踩的坑：**`edit` 工具匹配带中文全角标点的多行文本会失败**
  （同样的内容重试几次都不中）⇒ 改用 python 按行号替换 + `grep` 验证
  （AGENTS.md 早就写了这规矩）。

---

## 〇之二、最新进展（2026-11-10：Release 补上 Windows 版）

- **主线 `main`** = `b2ea4db`，**tag `v0.5.1` 已发** ⇒
  <https://github.com/fwt11/puttyplus/releases/tag/v0.5.1>，**6 个资产**（含
  `puttyplus-windows-x86_64.zip`，39,792,122 B，sha256
  `fe1d9efa3c95253cee4e65a4bea5cc8f06ae99d694168f109cca1242d1a64428`）。
- ⚠️ **第一节（2026-11-08 那份「当前状态」）已经过时**（那时还在
  `feature/cross-platform` 上）⇒ **以本节为准**。
- 本轮三个提交：
  - `3253f25` **发版流程：Release 里也挂上 Windows 版**（用户 2026-11-10 问
    「release 里面为什么没有 windows 版的？」）。
    **根因**：`release.yml`（tag 触发、真正发版的那条）**只编 Linux 两架构**
    ⇒ Release 里只有 4 个 Linux 资产；`ci-windows.yml`（push 触发）确实编了
    `puttyplus-windows-x86_64.zip`，但只 `upload-artifact` ⇒ 只是**会过期的
    CI 构建产物**，不是 Release 资产。
    **改法**：`ci-windows.yml` 加 `workflow_call` ⇒ `release.yml` 用
    `uses: ./.github/workflows/ci-windows.yml` 复用它出 Windows 资产；
    zip 纳入资产齐全校验 + SHA256SUMS + 上传列表。
    ⚠️ **concurrency 组名必须带 `github.workflow`**：复用时 `github.ref` 是
    **调用方**的 ref ⇒ 「CI 触发的」与「Release 触发的」会撞进同一组 ⇒
    **一边把另一边取消掉**。artifact 名从 `puttyplus-windows-x86_64` 改成
    **`pkg-windows`**（发布那边 `pattern: pkg-*` 收拢；zip **文件**名不变）。
    顺手修的老毛病：Release **已存在**时现在会 **PATCH 同步正文**（原来只
    补资产、不更新正文）。
  - `8676bb2` **发版 0.5.1**（补丁版，功能代码与 0.5.0 完全一致）：
    Cargo.toml/lock 0.5.0→0.5.1 + `debian/changelog` 加 0.5.1-1 条目 +
    `docs/发布说明/v0.5.1.md`。
  - `b2ea4db` README 下载表加 Windows zip 那一行。
- ⚠️ **v0.5.0 那个 Release 不会自动长出 Windows 资产**：补发要
  `workflow_dispatch`，需要 GitHub API 凭据，**本机没有**（只有 SSH 密钥、
  无 token、未装 `gh`）⇒ 只能再发一个补丁版本（= 0.5.1，已发）。
  ⇒ **想给 v0.5.0 也补上**：在 Actions 页手动跑一次「Release」，tag 填
  `v0.5.0`（publish job 会复用已存在的 Release、补上 zip 并同步正文）。
- 本轮验证：`cargo test --lib` **647 绿**；`bash tools/cli_contract_check.sh`
  **PASS=73 FAIL=0**；`python3 tools/check_version.py v0.5.1` 一致；
  CI 三条全绿（amd64 / arm64 / Windows MSVC，Windows 侧 647 单测 + 73 契约
  都过）；`SHA256SUMS` 476 B = 4 个旧资产 + Windows zip 一行（381+95）。
  `release/puttyplus` 7,420,536 B（sha256 `074c7a34…`）。
  ⚠️ `cp` 到 `release/puttyplus` 报「文本文件忙」⇒ 用户正跑着它
  （PID 9212）⇒ 改用「先写 `.new` 再 `mv` 原子替换」，**别杀用户进程**。

## 一、当前状态（2026-11-08：跨平台分支收尾中）

### 1.1 分支与提交

- **主线 `main` 未动**：仍是 v0.4.4（`aaed51d`）的 Linux 版。
- 跨平台改造全在 **`feature/cross-platform`**（已推 `origin`）：
  `git rev-list --count main..HEAD` = **30 个提交**，最新 `71aec46`
  （C9-二：Windows 本地开发环境脚本）。
- **工作区干净**（`git status` 空）。`AGENTS.md` / `RESUME.md` /
  `tools/ROADMAP.md` / `tools/FEATURE_PARITY.md` / `c/` 都是**本地不入库**
  （排除规则在 `.git/info/exclude`）⇒ **改它们不能 `git add`**（会整条失败）。
- 计划快照（不可变）：
  `/home/fwt/.minimax/v2/sessions/2026/11/07/12-19-27-542-session_bXZzXzZiMDY2NjMwOGVjNDQ5YTg4MjZmZGY4MzA3OWMyYjU3/artifacts/plan.md`
  （30KB，13 节；§八 = C0–C6 分阶段，§九 = 验收，§十 = 风险表）。

### 1.2 已完成（阶段 C0–C6）

- **C0** 平台差异收口到 `src/platform/`；**C1** `TermView` 抽象（Linux 仍走 VTE）；
  **C2** ghostty 绑定层；**C3** ghostty 自绘（cairo+pangocairo+IME）；
  **C4** ConPTY；**C5** SFTP 面板跨平台化 + CLI 契约在 Windows 落地；
  **C6** CI + 文档 + 打包脚本 + 交叉检查工具。
- 首版范围（用户拍板）= **终端 + SFTP + 命令行契约**；DB 工作台在 Windows 上
  **保留编译但隐藏入口**（`topmenus::entry_visible` + `panel::on_open_db` /
  `on_open_db_page` 收口 ⇒ 点了只弹「Windows 版尚未验证」）。

### 1.3 Windows CI：连挂 6 轮，第 7 轮在跑

工作流 **`.github/workflows/ci-windows.yml`**（独立文件，**不动 `ci.yml`**）。

| 轮 | 结论 | 原因 |
|---|---|---|
| 1 | 失败 | 网页只有「exit code 1」⇒ 加**错误注解**（run 级日志要 admin） |
| 2 | 失败 | `ghostty-vt-sys` 的 build.rs 在 Windows 上下载 Ghostty 源码失败 ⇒ **CI 自己下载** + `GHOSTTY_SOURCE_DIR` |
| 3 | 失败 | 4 个编译错：PG `UnixListener` / agent 转发 `UnixStream` / `Rc` 的 `#[cfg(unix)]` 误加 / `libc::atexit` 无保护 |
| 4 | 失败 | `glib::Pid::from_raw` **不存在**（glib 0.22）；`AgentClient::connect_env` 是 Unix 专属 |
| 5 | 失败 | `glib::Pid(pid as i32)` 在 Windows 上 E0308（**`GPid` 在 Windows 是 `*mut c_void`**）⇒ 改用自有 `platform::pty::ChildPid` |
| 6 | 失败 | **编译已绿**（01:27:48→01:35:36）⇒ 挂在**单元测试**：`~` 展开的分隔符（Windows 期望 `\`，代码给的是 `/`）⇒ 已在 `046c0f6` 修 |
| 7 | 失败 | **编译绿、单测还挂两条**（都只在 Windows 上炸）：① lint 跳过 `panel/syntax.rs` 的判断拿**字面量路径**去比 ⇒ Windows 上是 `panel\syntax.rs` ⇒ 没跳过 ⇒ 扫到高亮器测试语料里的 `**粗体**`；② 测试临时目录名只有 `<prefix>_<tag>_<pid>` ⇒ **同进程内多个测试用同一个 tag 就抢同一个目录**（`args::tests::parse` 是 `temp_home("args")`）⇒ 并发时互删 ⇒ Windows 上 `create_dir_all` 报 `AlreadyExists`(183) ⇒ 已在 `3729d11` 修 |
| 8 | 失败（**预期内**） | 契约步骤找不到二进制 —— CI 为躲 MAX_PATH 把 target 挪到 `D:\t`，而契约脚本里 RBIN 是**写死**的 `target/release/...` ⇒ ⚠️ **这个坑前七轮一直藏着**（每轮都在契约这一步之前就挂了，契约从没真跑过）⇒ 已在 `01527ef` 修 |
| 9 | 失败 | 单测又挂两条（都只在 Windows 上炸）：① `clipfiles` 的测试**手搓 `file://` URI** ⇒ Windows 上是**非法 URI** ⇒ 解不出路径；② `dnd` 的测试用 `gio::File::for_path("/tmp/a.txt")` ⇒ Windows 上那不是有意义的本地路径 ⇒ 取回的形状是 `\tmp\a.txt` ⇒ 已在 `77abd47` 修（URI 一律用 GIO 生成、路径一律用平台原生的临时目录）|
| 10 | 失败 | **单测过了** ✓ ⇒ 失败的是**命令行契约**这一步：大量用例 DIFF（连 `missing_l` 这种无路径无会话的都差）⇒ diff 形态是「整块全变」（`1,12c1,12`）⇒ ⚠️ 但注解里塞的是日志尾巴 ⇒ **diff 的期望侧整段被截掉** ⇒ 看不出差在哪 ⇒ 已在 `f0f6218` 修（CI 改成打完整 diff + DIFF 名单；归一化**去掉行尾的 CR**）|
| 11 | 失败 | 契约仍然 73 条全 DIFF ⇒ ⚠️ 上一轮只给**实际侧**去 CR，**基准侧没去** ⇒ 差异还在 ⇒ 说明差异确实在**行尾** ⇒ 定位到根因（见下）|
| 12 | 失败 | **单测全绿** ✓ ⇒ 契约从「73 条全 DIFF」降到 **8 条 DIFF**：`proto_serial`×2 / `sw_T` / `ignored_A` / `val_i` / `val_L` / `sshconfig_alias`×2 ⇒ 三个根因**全在测试/环境侧**（见 1.3.1）|
| 15 | 见下 | head `a2f429e`（**修 SFTP 面板的 adjustment 是 NULL**：用户 2026-11-08 在 Windows 上贴的那屏 GLib 警告）|
| 14 | **✅ 全绿** | head `d9a5fa3`（**阶段 C7：Windows 终端改成进程内 SSH**，不再依赖 ssh.exe / plink）⇒ 编译 ✓（07:41:30→07:44:01）单测 ✓ 契约 ✓ 打包 exe ✓（07:39:28→07:46:07，6.5 分钟，比上轮快 ⇒ 缓存命中）⇒ **artifact 是新的**（第 13 轮那个 exe 还是「找 ssh.exe」的旧版 ⇒ 要重下） |
| 13 | **✅ 全绿** | head `1d8009e` ⇒ **编译 ✓ 单测 ✓ 73 条契约 ✓ 打包 exe ✓**（04:52:12→05:09:10，17 分钟）⇒ **Windows CI 第一次全绿** ⇒ artifact `puttyplus-windows-x86_64`（39.5 MB，exe + GTK DLL，有效期到 2027-02-06）已上传 |

第 6 轮那两个失败：`args::tests::tilde_expand_keyfile` 与
`config::tests::ssh_alias_resolution` ⇒ 根因是 `~` 展开用**纯 POSIX 拼接**
（`home + "/" + rest`）⇒ Windows 上 home 是 `C:\Users\bob` ⇒ 拼出
`C:\Users\bob/.ssh/mykey` ⇒ 跟同文件里 `Path::join` 的结果（`\`）对不上。
⚠️ 顺带牵出一个**真 bug**：`~/.ssh/config` 的 `IdentityFile` 用
`!kf.starts_with('/')` 判绝对路径 ⇒ **Windows 的盘符路径被当成相对路径**
⇒ 再补一层 `~/.ssh/` ⇒ 密钥路径整个错掉。
`046c0f6` 的改法：① `~` 展开的**边界分隔符按平台**；② 抽出
`expand_tilde_for(p, home, windows)` ⇒ **Windows 的行为在 Linux 上就能测**；
③ 绝对判定改用新增的 `localpath::is_abs_shape`（**不分平台**的形状判定）。

### 1.3.1 ⭐ 契约在 Windows 上全 DIFF 的根因（阶段 C6 的最后一块）

**根因：Windows runner 的 Git 默认 `core.autocrlf=true`** ⇒ checkout 时把
文本文件转成 **CRLF** ⇒ **契约黄金基准（.txt）变成 CRLF** ⇒ 而程序输出是 LF
⇒ **每一行都算不同** ⇒ 73 条全 DIFF（diff 形态是「整块全变」`1,12c1,12`，
两边内容其实一模一样，只差行尾）。

⚠️ **为什么前九轮一次都没暴露**：每轮都在契约这一步**之前**就挂了
（编译 / 单测）⇒ 契约**从来没真跑过**。

⇒ **三道保险**（`cbf7e85`）：
1. **`.gitattributes`（新建，入库）**：`*.sh`、契约基准 `*.txt`、`*.rs`
   … 全部钉死 `eol=lf`（`.ps1` 用 CRLF）⇒ 两个平台 checkout 的字节一致；
2. **契约脚本**：比较时**基准也过一遍归一化**（去 CR）
   ⇒ 即使 checkout 把它转了，比较时也抹平；
3. **CI**：checkout **之前** `git config --global core.autocrlf false`。

**验证（Linux 上能复现）**：把 73 个基准**真的转成 CRLF** ⇒ 修复后仍
PASS=73；把「基准也归一化」回退 ⇒ 同样条件下 PASS=0 FAIL=73 ⇒ 真抓得住。

⚠️ **顺带修的契约脚本老毛病**（Linux 上也一样，只是从没触发过）：
`diff … | head -12` 只剩「实际侧」（diff 是 `<` 实际 / `>` 期望）
⇒ **期望侧整段被砍掉** ⇒ 真出 DIFF 时看不出差在哪一行 ⇒ 改成 `head -30`。

⚠️ **CI 的失败注解**现在拆成两条：① 全部失败用例名（一行 ⇒ 不会被截断）
② **第一个 DIFF 的完整 diff**（含期望侧）⇒ 另加 `contract-log` artifact。

### 1.3.1.1 ⭐ Windows 特有的三个坑（**全在测试/环境侧，不在程序**）

第 12 轮只剩 8 条 DIFF ⇒ 逐条对注解里的**完整 diff**定位 ⇒ 三个根因：

① **基准文件名大小写冲突**（`sw_T` / `ignored_A` / `val_L`）
   **Windows 的文件系统大小写不敏感** ⇒ `sw_t.txt` 与 `sw_T.txt` checkout 时
   **互相覆盖**（后写的赢）⇒ 读到的是**别的用例**的基准 ⇒ 凭空 3 条 DIFF。
   ⇒ 三对撞名用例加 `_lower`/`_upper` 后缀 + 基准同步改名
   （**内容一字未改** ⇒ git 认成 100% rename，0 增 0 删）。

② **Git Bash（MSYS 系）的参数路径自动转换**（`proto_serial`×2 / `val_i`）
   把 `/xxx` 参数交给原生 Windows 程序时自动转成 Windows 路径
   （`/dev/ttyUSB0` → `C:/Program Files/Git/dev/ttyUSB0`）
   ⇒ 契约用例收到的参数与 C 版基准不同。
   ⇒ 脚本在 Windows 下 `export MSYS_NO_PATHCONV=1` +
   `MSYS2_ARG_CONV_EXCL='*'`。

③ **ssh 别名用例被 PuTTY 会话截胡**（`sshconfig_alias`×2）
   会话 fixture 叫 `Demo`、ssh config 别名叫 `demo` ⇒ **只差大小写**
   ⇒ Windows 上 `puttyplus demo` **命中了 PuTTY 会话** ⇒ 别名展开没跑。
   ⚠️ **不是程序的行为差异**：C 版（`c/src/args.c:338`）与 Rust 版都是
   **「会话优先」**，在 Windows 上同样命中 ⇒ 差的是**测试环境**
   ⇒ 别名用例改用**单独的 fixture home**（只有 `.ssh/config`，没有 `.putty`）
   ⇒ 两个平台一致 ⇒ **基准一个字没改**。

⇒ 三条都进了 `--selftest`（少变量 / 基准撞车 ⇒ 自测立刻报错）。
⇒ 反向验证（Linux 上做）：① 造撞名基准 ⇒ 自测 FAIL=1；② 去掉两个 MSYS
变量 ⇒ 自测 FAIL=2；③ 别名 home 里放 `sessions/demo` ⇒ 契约 PASS=71
（正好那两条别名 DIFF）⇒ 三条都**真抓得住**。

### 1.3.1.2 Windows 上要装 MSYS2 吗？**不用**

- **跑 exe**：只要 GTK4 的 DLL（gvsbuild 的 `C:\gtk\bin\*.dll`）。
- **编译**：Rust（MSVC 工具链）+ VS 的 C++ 生成工具 + gvsbuild 的 GTK4
  预编译包 + Zig 0.17。**都不需要 MSYS2**。
- **跑 73 条契约**：要一个 `bash` ⇒ **Git for Windows 自带的 Git Bash 就够**
  （CI 就是用它）。⚠️ 但 Git Bash **本身就是 MSYS 系的** ⇒ 自带「参数路径
  自动转换」那个毛病 ⇒ 契约脚本自己关掉它（见上面第 ② 条）。

### 1.3.2 其它要记住的

- ⚠️ **Windows 上契约失败过一次**：第 10 轮单测全过，但 73 条里大量 DIFF。
- 最可能的原因：**Git Bash 捕获原生 Windows 程序的输出时，行尾变成了
  `\r\n`** ⇒ 基准（Linux 生成的）没有 `\r` ⇒ **每一行都算不同**
  ⇒ 正好是「整块全变」那种 diff 形态，肉眼根本看不出是行尾问题。
  ⇒ 归一化现在**先去掉 CR**（Linux 上是空操作 ⇒ 不影响基准），
  并给 `--selftest` 加了一条「带 CRLF 的输入归一化后与不带的一样」
  ⇒ **回退后这条自测确实变红** ⇒ 以后这类问题在 Linux 上就能发现。
- ⚠️ **askpass 不受影响**：它用的是 `write!(out, "{}\n", pw)`（显式写 LF），
  不是 `println!` ⇒ 密码不会带上回车。
- ⚠️ **CI 的失败注解必须能看全**：现在拆成两条 —— ① 全部失败用例名
  （一行 ⇒ 不会被截断）② **第一个 DIFF 的完整 diff**（含期望侧）。
  另加 `contract-log` artifact（contract.log / selftest.log / test.log）。

### 1.4 接着干先做这三件

> ⚠️ **2026-11-08 更新（本节已重写）**：C7/C8/C9 都做完了，Windows CI 第 19
> 轮全绿。下面是**当前**该做的事。

0. ⭐ **等用户在他自己的 Windows 上复验 C8/C9**（第 19 轮 artifact）：
   - C8 修了他报的三个问题（终端打不出字 / 密码要输三遍 / 密码存不住）；
   - C9 把密码统一成一套（Linux 与 Windows 同一套对话框 + 缓存）。
   - 复验要点：**终端能不能打字**（最关键）、SFTP/终端/分屏**只问一次**、
     凭据库启用后密码**重启还在**。
1. **「要不要迁移到 Windows 开发」——用户 2026-11-08 问的** ⇒ 我的答复是
   **迁移**（剩下要验的东西几乎全是 Windows 专属；每轮 CI 往返 20~30
   分钟太贵）。⇒ 已写好两个脚本（**只加脚本，不装东西**）：
   - `tools/win_env.ps1`：把 CI 那套环境变量**持久化**（用户级）⇒ 之后
     每个终端都带着 ⇒ 在 Windows 上直接 `cargo build` / `cargo test --lib`
     / `cargo run --example`（**有桌面会话 ⇒ GUI 冒烟能真跑**）；`-Check`
     只检查不修改；**不下载不安装**。
   - `tools/win_msvc_env.ps1`：在当前 PowerShell 会话里激活 MSVC
     （`VsDevCmd.bat` ⇒ 解决 `linker link.exe not found`）。
   - ⚠️ **装东西前必须先得到用户同意**（VS 生成工具 ~3-5GB 是大头）。
2. **合并 `feature/cross-platform` → `main`**：等用户拍板（他之前说
   「你验 GUI，我等结果」⇒ 暂不动 main）。
3. ghostty 后端剩余项（鼠标拖选复制 / 滚回缓冲行数 / 自定义配色 / 滚到底
   隐藏指针）——用户说「你说了我再动」。

### 1.4.1 ⭐ 阶段 C7 / C8 / C9 干了什么（2026-11-08）

| 阶段 | 提交 | 内容 |
|---|---|---|
| C7 | `fdc2054` … `f24098a` | **Windows 终端改成进程内 SSH**（`backend/shell.rs`）⇒ 不再依赖 `ssh.exe`/`plink`；修 SFTP 面板 adjustment 是 NULL；文档；CI 装 GTK4 加固 |
| C8 | `b64f067` | 修用户真机报的**三个**问题：① **终端打不出字**（`ghostty_view` 的 `emit` 只调 `set_output_handler`，而生产代码**从没设过它** ⇒ 按键字节被丢弃）② **密码要输三遍**（SFTP/终端/分屏是三条独立连接）③ **密码存不住**（`vault::save_all` 用 `create()` 重写库 ⇒ 每次换 salt ⇒ 缓存的解锁密钥作废 ⇒ **刚存的密码读不回来**；只在**设了主密码**的库上犯） |
| C9 | `25bb24f` | **两个平台的密码统一成一套**（共享层 `src/pwcache.rs`）：进程内缓存 + 密码框去重 + **会话级密码文件**（让 spawn 出去的 ssh 也问**我们**的助手 ⇒ Linux 上终端的密码提示从 tty 移到对话框） |
| C9-二 | `71aec46` | Windows 本地开发环境脚本（`win_env.ps1` / `win_msvc_env.ps1`） |
| C9-三 | `b44a48a` | Windows 开发环境搭建指南（逐条命令）+ 一键安装脚本 `win_setup.ps1` |
| C10 | `c7742fe` | **Windows 上也能用 ssh-agent**（`\\.\pipe\openssh-ssh-agent` 命名管道）⇒ **这才是「两个平台像两个软件」的真正根因**：以前 Windows 上 `try_agent` 直接返回 false ⇒ 密钥在 agent 里时 Linux 免密、Windows 却要密码。认证逻辑已抽成共享的 `try_agent_with`，只有连接方式按平台不同 |
| C10-二 | `83af040` | Windows 上怎么开 ssh-agent 服务 + `ssh-add` + 自定义文件名要写 `IdentityFile`（写进 `docs/Windows-开发环境搭建.md` 第 7 节） |
| C11 | `29bc8cf` | 光标不再骑在上一行（根因是**文字被画低了一行**，不是光标位置算错） |
| C11-二 | `7ff371b` | `tools/*.ps1` 补 **UTF-8 BOM**（中文 Windows 上没 BOM 的脚本根本跑不起来） |
| C11-三 | `5cab53a` | `win_build` 的 GTK 头文件路径改成**追加**，别整个替换 `INCLUDE` |
| C11-四 | `4757e41` | `win_env` 写 `INCLUDE`/`LIB` 先把管道结果包成数组（否则空值下 `+=` 变成字符串拼接）——修的是我写的脚本的 bug |
| C12 | `66116fa` | 密码在凭证库里时连接**不再要** ⇒ 组装连接参数那一刻**现查**凭证库 |
| C13 | `7e22186` | 光标会闪了 ⇒ 内核默认改闪 + 渲染层**相位定时器** |
| C14 | `c41b5c2` | 点了文件面板后点回终端**立刻能打字** ⇒ 点击聚焦显式做 + 无焦点时光标画空心。⚠️ **用户自己定位到「终端不能打字」的另一个根因是焦点问题**（我 C8 修的 `emit` 是另一个真 bug，两条都必要） |
| 修-1 | `6f903b1` | **设 GLib 程序名**（`util::init_prgname`）：不设的话 **Linux 上每开一个终端都喷一条 GLib-CRITICAL**（`g_utf8_make_valid: assertion 'str != NULL' failed`）—— 根因在 VTE 0.80：`SpawnContext` 的 `m_systemd_scope` 默认 true ⇒ 每 spawn 子进程都走 systemd-scope 那条路 ⇒ 调 `g_get_prgname()` ⇒ 没设过就是 NULL。调用点收口在 `PtySession::new()`（一处设，处处生效） |
| **合支** | `6f903b1` | ⚠️ **2026-11-09：`feature/cross-platform` 已快进合入 `main`**（41 个提交，86 文件，+10941/-530）⇒ `main` == `feature/cross-platform` == `6f903b1`。**按仓库惯例走快进、不造 merge commit**（这个仓库的 `main` 从来没有过合并提交，历史全线性）。⇒ **接下来**：`main` 上的版本号仍是 **0.4.4**（而 0.4.4 已经发过版了）⇒ 要发版得再补一个「版本号升到 0.5.0 + 发布说明」的提交（仓库惯例是**单独的**发版提交）。 |
| **发版 0.5.0** | `791545e` | 版本号 0.4.4 → **0.5.0**（`Cargo.toml` + `Cargo.lock`）+ `debian/changelog` 加 0.5.0-1 条目 + `docs/发布说明/v0.5.0.md`（正文用 `{{VERSION}}` 占位，发版时按 tag 注入）。校验：`python3 tools/check_version.py v0.5.0` ⇒ 版本号一致。⚠️ **打 tag `v0.5.0` 才会真正发布**（`release.yml` 是 `push: tags: ['v*']` 触发的 ⇒ 会编 amd64/arm64 的 deb 与 tar.gz、建 GitHub Release）。 |
| 凭据库徽标 | `1bc8a57` | 用户 2026-11-09 看截图说「这个按钮感觉太大了，换成一个图标吧」＋「点了还会弹一个对话框显示所有凭据库里面有什么东西，这个没有必要，菜单里有个『凭据库』，那里显示就好了」⇒ ① 徽标**只留图标**（状态文字挪到 tooltip 与**无障碍标签** `gtk::accessible::Property::Label`）；② 点徽标**只解锁**（`vaultui::badge_clicked`），**不开凭据库管理器**（管理器只在菜单「凭据库…」里）；③ 连接表单里那颗「解锁凭据库…」也**不再**顺手弹管理器。判定逻辑抽成纯函数 `vaultui::badge_action_of(exists, protected, unlocked)`。⚠️ **判定顺序**：先看「已解锁」再看「有没有主密码」——明文库在进程刚起来时**也是未解锁**（`is_unlocked` 只看进程内缓存的密钥）。 |
| 字体 / 语言实时生效 | `833fd71` | 用户 2026-11-09：「首选项-终端 那里增加字体设置，而且字体设置和字号修改后应该实时生效」「切换语言也应该实时生效」。① **终端字体设置**：首选项→「终端」页新开一节「终端字体」（**字体名**下拉 = 本机等宽字体，pango `FontMap` 枚举 + `is_monospace` 过滤；**字号**沿用 SpinButton）；字体规则（字体名+字号+优先级）**两个后端共用一份** `termview::font_description()`。② **实时生效**：新增 `tabs::sync_term_fonts()`（与 `sync_term_palettes()` 同路子）⇒ 改完立刻刷所有已开终端；**连接里设过的字号仍压过全局默认**。③ **语言实时切换**：`i18n` 加钩子表（`add_lang_hook`/`notify_lang_changed`）⇒ 语言一变就重建**界面外壳**（顶栏+菜单栏+侧栏）与**首选项窗口的页签**；⚠️ **终端标签页不动**（在跑的 SSH 会话不能重建）⇒ 标签页里零散文案保持旧语言。⚠️ **重建必须延到 idle**（`idle_add_local_once`）：改语言的回调**就在那个下拉框里**，回调里销毁它 = use-after-free。④ 顺带把 `app_menubar`/`build_header`/`toggle_search_for` 从 `src/main.rs` **搬进 `src/topbar.rs`** ⇒ 它们以前住在 bin 里 ⇒ **example 测不到** ⇒ 搬进 lib 后新冒烟 `examples/settings_live_smoke.rs`（9 条）能真的建顶栏、切语言、再建一次比菜单标题。⑤ 英文表补 7 条（`tools/i18n.py gen` 重新生成，别手改）。⚠️ 页签重建**顺序**踩过坑：`remove_page` 会让后面的页往前挪 ⇒ 边摘边挂顺序全乱 ⇒ 改成**先全摘、再按序挂**。 |

⚠️ **C9 有一处 Linux 主线的用户可见变化**：终端里 `ssh` 的密码提示**不再
出现在 tty 上**（改成我们的对话框）。**要密码的场景（堡垒机 launcher 传
`-pw`）完全不受影响**（那时密码早就有 ⇒ 走 `PUTTYPLUS_PW` 直接应答）。

⚠️ **C9 顺带修了一个冒烟的竞态**：`ghostty_view_smoke` 原来自己起了第二
个 pty 读线程，而 `attach_pty` 已经起了一个 ⇒ 两个读者抢同一个 master
fd ⇒ 断言时红时绿 ⇒ 改成只看 ghostty 屏幕（单一读者）。

### 1.5 Linux 侧基线（现在全绿）

```bash
export PATH=$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH
cargo test --lib                        # 646 绿
bash tools/cli_contract_check.sh        # PASS=73 FAIL=0
bash tools/cli_contract_check.sh --selftest   # FAIL=0
bash tools/win_cross_check.sh           # Windows 目标零错误
```

- release 产物：`release/puttyplus`（7,402,216 字节，`.gitignore` 忽略 `/release/`）。
  ⚠️ **别把 C 版候选名 `puttyplus` 放在仓库根**（compat_check.sh 会拿它当 C 版基线）。
- ghostty 三条冒烟（**每个 cargo 命令都要带 ZIG**）：
  ```bash
  ZIG=$HOME/.cache/puttyplus-zig/bin/zig xvfb-run -a cargo run --features ghostty       --example ghostty_vt_smoke     # 含 VTE 对拍
  ZIG=… xvfb-run -a cargo run --features ghostty --example ghostty_view_smoke
  ZIG=… xvfb-run -a cargo run --features ghostty --example ghostty_session_smoke
  ```
- 本地测试 sshd：`/usr/sbin/sshd -f ~/.cache/puttyplus-test-sshd/sshd_config`（2233）。

### 1.6 本机环境要点

- **Zig 0.17.1**：`~/.cache/puttyplus-zig/bin/zig`（PyPI `ziglang` wheel，官方源太慢）。
  ⚠️ 不带 `ZIG=` 跑 ghostty 相关命令 ⇒ build.rs 找不到 zig 会 **panic**。
- `rustup target add x86_64-pc-windows-msvc` **已装**（`tools/win_cross_check.sh` 用）。
- ⚠️ **仓库级 Windows 交叉编译不行**（`glib-sys` 要 Windows sysroot）⇒
  整个 GTK4 程序只能在 Windows/CI 上编；Linux 侧只有 `win_cross_check.sh` 能查
  不依赖 GTK 的那部分（`localpath.rs` / `perm.rs`）。
- ⚠️ GitHub API **未认证限流**；`gh` CLI 本机**没装**（装它要先请示）。

### 1.7 跨平台分支的坑（别再踩）

- **`glib::Pid` 在两个平台上内部类型不一样**（Unix `c_int` / Windows `*mut c_void`）
  ⇒ 平台差异要落到**自己的类型别名**（`platform::pty::ChildPid`），别让调用方碰底层类型。
- **跨平台路径别手搓 `+ "/"`** ⇒ 用 `Path::join`，或把分隔符收进**可注入的参数**
  （`config::expand_tilde_for(p, home, windows)`）⇒ **Windows 的行为在 Linux 上就能测**。
- **「像不像绝对路径」的形状判定**要与「当前平台的本地路径判定」分成两个函数：
  `localpath::is_abs`（按运行平台）vs `localpath::is_abs_shape`（不分平台）⇒
  否则 Linux 上永远测不到 Windows 的盘符形态。
- **远端路径恒 POSIX，本地路径平台原生** ⇒ `crate::util::path_*` vs
  `crate::platform::localpath::*`，**别混用**。
- **Windows runner 没有桌面会话** ⇒ `gtk::init()` 起不来 ⇒ **GUI 冒烟在 CI 上跑不了**；
  单测只跑 `cargo test --lib`（11 个 example 直接用 vte4，Windows 上编不过）。
- **Windows 的 MAX_PATH 是 260** ⇒ CI 里 `CARGO_TARGET_DIR=D:/t`。
- ⚠️ **契约脚本（`cli_contract_check.sh`）的 RBIN 必须认 `CARGO_TARGET_DIR`**：
  写死 `target/release/...` ⇒ CI（target 挪到短路径）上找不到 exe
  ⇒ 契约步骤直接 exit 2 ⇒ **而且前几轮都在这一步之前就挂了，这个错一直藏着**。
  ⇒ 脚本现在认 `CARGO_TARGET_DIR`（并把 `D:\t` 归一化成正斜杠，Git Bash 里
  混合分隔符不稳）+ 允许 `RBIN=` 覆盖 + 找不到时报错带完整路径。
- ⚠️ **别拿字面量路径去比路径字符串**（分隔符是平台相关的）⇒ 要么先归一化
  反斜杠，要么用平台感知的 `Path` API（后者在 Linux 上**没法验**，优先选前者）。

## 二、阶段 19 / 20 收尾情况（都已提交）

### 2.1 阶段 19（ER 图）

- 代码全在 `0007132`：后端 `backend/ermodel.rs`（纯模型 + 布局，10 项单测）、
  `dbmeta::meta_schema_foreign_keys`（PG 按位置配对列）、`db.rs::er_graph`、
  界面 `panel/erview.rs` + `dbui.rs` 的 ErTab + `dbtree.rs` 的右键「ER 图…」。
- **右键菜单路径**在 `f704ee3` 补的测试覆盖：
  `dbtree.rs` 的 `MenuBtns` / `update_menu_sensitivity` / `popup_menu_for`
  （按 TreePath 逐层走找行）+ `dbui.rs` 的 `tree_er_menu_now` 钩子；
  e2e 6.15 段断言「右键 → ER 图…」真开一个页签，模式节点上也可点。
  e2e 输出：`[6.15.3] 右键菜单开 ER 图：页签 2 → 3`。

### 2.2 顺手修的真 bug（`f704ee3`）

**「CSV 导入后对象树永久空白」**：根因是 `DbTree::clear()` 把**根节点也抹掉**
（`store.clear()` + `nodes.clear()`），之后 `reload()` 找不到根节点直接 return，
树再也长不回来——刷新按钮都不管用。修法：根节点标签单独记一份（`root_label`），
`reload()` 找不到时按它重建；导入完改用 `reload()` 而不是 `clear()`。
教训已写进 `AGENTS.md`。

### 2.3 阶段 20（列显示设置持久化，`6078bb8`）

- 新增 `src/backend/dbcols.rs`（8 项单测）：存
  `~/.config/puttyplus/columns.ini`（0600，`PUTTYPLUS_COLS_INI` 可改路径）。
  组名 `<连接标识>/<模式>.<表名>`，值 `列名:1,列名2:0`（列名转义 `%` `,` `:`）。
- **按列名存、不按下标**；套用时：存过的按名字找回下标，**后加的列追加到
  末尾且默认显示**，删掉的列丢掉。
- **只存表浏览页签**（查询结果的列每次都不一样，存了没意义）；按连接分区。
- e2e 6.16 段：`[6.16] 藏掉「name」后` → `[6.16.1] 重开表后可见列`
  （重开表后那一列还是藏着的）。

### 2.4 阶段 21（导出格式扩展，`82a3814`）

- 工具栏的导出入口从**两颗图标按钮**（导出 CSV / 导出 SQL）改成**一个
  「导出…」菜单按钮**，菜单里选 CSV / INSERT SQL / JSON / XML / HTML。
- 新增 `src/backend/dbexport.rs`（6 项单测）：纯函数序列化。
- 新增 `DbGrid::visible_values_opt()`：保留 NULL 与未保存的改动
  （原来的 `visible_values()` 把 NULL 转成了文本 "NULL"）。
- ⚠️ **NULL 与空串是两回事**：CSV 里写 "NULL"，JSON 里是 `null`、
  XML 里是空元素、HTML 里是空单元格。
- ⚠️ **XML 列名放属性里**（`<col name="user id">`）——列名可能不是合法的
  XML 元素名（`user id`、`2nd_col`）。
- 截图 `/tmp/export_menu.png`。

### 2.5 这轮踩到的坑（别再踩）

- **e2e 跑挂了会留下真库的残留**：探针表 `pp_er_child` / `pp_er_parent`
  下轮直接 "relation already exists" 挂掉。现在 e2e 开头先
  `DROP TABLE IF EXISTS` 自愈；真库残留检查：
  PG `select count(*) from pg_tables where tablename like 'pp_%'` 应为 0；
  MySQL `SHOW TABLES FROM demo LIKE 'pp%'` 应为空。
- ⚠️ **`MenuButton::set_popover()` 自己就会给 popover 设父级**：别再手动
  `pop.set_parent(&btn)`——会报 `gtk_widget_set_parent: assertion
  'gtk_widget_get_parent (widget) == NULL' failed`（e2e 里真踩到了）。
- **启动时有 5 条 `gtk_css_node_insert_after` 的 Gtk-CRITICAL**：**既有问题**，
  与阶段 21 无关（stash 掉改动跑基线一样有）。出在程序启动、装主题那会儿，
  不影响功能，暂时没查。
- **e2e 的树遍历改成按路径走**了（`TreePath::from_indices` + `append_index`）：
  原来的 `iter_next` 深遍历会走一半，看着像"数据没加载出来"。
- **拆提交**：混合文件（dbui.rs / db_ui_e2e.rs / AGENTS.md）先**备份完整版**
  再 `git checkout HEAD --`，只重放第一个提交要的那几处，提交完再把备份
  拷回来。备份**必须在第一次 checkout 之前**做。

## 二之二、本轮做的阶段 22~28（提交 b6b233d）

### 22 双击即连 + 工作台去连接信息

- `dbui::use_profile` 改成「选中 + 立即连」；工作台整行删掉
  「当前连接 [下拉] [+] [连接] [断开]」，连接名写进**页签标题**
  （「数据库 · <连接名>」，`SubTabs::set_title` 要同步 `width_chars`，
  否则标题被压成省略号）。
- 拆成两条入口：`panel::on_open_db()`（开 + 连当前）与
  `panel::on_open_db_page()`（只开）——双击时走后者，免得"先连错一条
  再断开重连"。
- e2e 新增 2.1 段「断开 → 双击 → 已连接」，PG / MySQL 都绿。

### 23 文件列表双击 = 编辑

- `panel::on_row_activated`：文件双击 → `fileedit::open_remote/open_local`
  （可编辑可保存）；目录双击仍进入目录。下载/上传保留在工具栏与右键菜单。
- `fileedit_ui_smoke` 新增 `dblclick.txt` + 真 `row-activated` 断言。

### 24 在当前会话上打开数据库

- 标签头常驻「数据库」图标按钮 + 标签头右键「增加数据库连接…」。
  （**阶段 27 把标签头图标撤了**，入口挪到右键菜单；子标签栏有「+」。）

### 26 终端可重建（用户连报三次的那件事）

用户原话：「如果不小心关了终端，应该有办法重新建立」→
「右键中的重新连接是连sftp，可以新增连接终端」→
「关掉终端tab之后，右键点重新连接终端，并没有重建终端tab」。

- 终端区显式摆「**会话已结束 · 点此重新连接**」条（`child_exited` 时出现）；
- 标签头右键菜单新增「**重新连接终端**」（与「重新连接 SFTP」并列——
  **两条链路**：SFTP 走进程内通道，终端是另起的 ssh 子进程）；
- 终端子页签被 × 关掉后，「重新连接终端」**重建**一个终端页签
  （否则重连的是看不见的会话，点了像没反应）；
- 会话树右键也有「重新连接终端」：标签还在就切过去重连，标签没了就用
  同一份参数重建一个标签（`Tabs::reconnect_term_with_opts`）。
- ⚠️ **顺手修的真 bug**：状态条（广播/日志/录制指示条）的显示判断用了
  `is_visible()`——那是**有效可见性**（自身 + 祖先都可见），而状态条整体
  在没提示时是藏着的 → **永远算不出"有提示"** → 这几个指示条**从来没显示过**。
  改成读自己的标志 `get_visible()`，并把每个 chip 的初始标志置 false。
- 新增 `examples/term_reconnect_smoke.rs`（对本地 sshd 的真会话，8 步）。

### 28 工作台去按钮 + 图标修正

- 工作台里**一个连接按钮都不留**：删掉「断开」「新建数据库连接…」。
  新建走子标签栏「+」，断开走**关掉页签自动断开**（`dbui::on_tab_closed`；
  独立窗口路径关窗口同样断）。
- ⚠️ **当场输入的密码不能跟工作台一起没**：密码原来在 `DbUi.session_pass`
  里，页签一关就没了（e2e 实测："重新打开工作台后应当能再连上"失败，
  状态是"数据库要求密码"）。挪到 `PanelState.db_pass`（会话级）。
- `SubTabs` 的关闭钩子改成**可注册多个**（`add_close_hook`）：终端页签收
  会话、数据库页签断连接，谁的东西谁注册；**钩子一律抓弱引用**（子标签挂
  在 PanelState 上，抓强的成环）。
- ⚠️ **`database-symbolic` / `postgresql-symbolic` /
  `preferences-system-database-symbolic` / `view-columns-symbolic` 系统里
  根本不存在** → GTK 回退成"图标缺失"（用户看到的**禁止符号**）。
  换成 Adwaita 里确定有的 `drive-harddisk-symbolic` /
  `drive-multidisk-symbolic` / `view-list-symbolic`。
- 文件列表右键「上传…」原用 `document-open-symbolic`，跟同一菜单的「打开」
  **撞图标** → 改成跟工具栏一致的 `go-up-symbolic` / `go-down-symbolic`。
- ⚠️ **查图标存在性的办法**（下次换图标前先查）：
  ```bash
  grep -rhoE '"[a-z0-9-]+-symbolic"' src/ | tr -d '"' | sort -u |
  while read -r n; do
    find /usr/share/icons -name "$n.svg" -print -quit | grep -q . || echo "缺失: $n"
  done
  ```

### 27 标签头瘦身（图标挪到子标签）

用户原话：「aly tab上不要放图标了，图标放到下面终端和数据库的tab上」／
「文件打开后，tab上不要有"编辑"两个字，可以改成一个图标」／
「可以在下面tab头同一行的最右边……放一个按钮，用于建立新的term或者数据库连接」。

- 主标签头只剩 `[状态点][标题][×]`（撤掉「快速命令」「数据库」两个图标按钮；
  两个功能的入口都还在：主菜单 / 快捷键 / 右键菜单）。
- 子标签带图标：终端 `utilities-terminal-symbolic`、数据库 `database-symbolic`、
  文件编辑器 `document-edit-symbolic`（只读 `text-x-generic-symbolic`）；
  文件编辑器页签标题只留 `● 文件名`。
- 子标签栏右端一个「**+**」：菜单「新建终端」（当前连接里再开一条独立
  SSH 会话，平级的另一个终端页签）/「新建数据库连接…」。
- ⚠️ **「+」不能排成独立一列**：用户明确说"软件右边整整新增了一列，
  太占地方"。用 `gtk::Overlay` 把按钮**浮**在页签栏右端（halign=End,
  valign=Start），不占布局宽度，内容能铺到窗口右边缘。
  `SubTabs::root()` 返回的是这个 Overlay，**外部要挂 `root()` 而不是裸的 `nb`**
  （裸 notebook 已有父级，塞进 Paned 会被 GTK 拒绝 → 整块高度 0）。

## 二之二之二、本轮做的阶段 29 / 30（提交 `34f4a77` / `f08ece6`）

### 29 「+」新建数据库连接固定在当前会话（堡垒机场景）

用户连提三轮：「+号新增数据库连接的时候，弹出的窗口中还是有个"在哪个会话上连"
的下拉框，但在堡垒机场景下，没有保存的会话，怎么弄」→「做成始终在当前 tab
所在的会话上打开，**任何情况下都不需要那个下拉框**，要换连接就切到别的 tab
再点 +」。

- `dbconn::ui_new` 的签名从 `preset_session: Option<&str>` 改成 `&str`
  ——**所有入口都预置死会话**，「在哪个会话上连」的下拉框**彻底删掉**
  （它已经是死代码，留着等于留一个"堡垒机场景下框空着还锁着"的坑）。
- 堡垒机场景（当前会话不在连接列表里）→ 一行说明「这条连接挂在当前会话上」。
  ⚠️ **不劝用户去连接管理器把会话存一条**——他没保存是**刻意的**
  （堡垒机端口/密码下次就变，存下来是过期垃圾）。
- **撤掉连接管理器里的「未保存的会话」分组**（本来打算给"孤儿"数据库连接
  造的）：用户明确不要。连带删掉 `dbstore::list_sessions` / `list_sessions_from`。
- **不做"从 `user@host:port` 反推连接参数"的兜底**——按旧地址重连只会连到
  一台已经不是的机器上。
- **按钮从「保存」改成「连接」**（用户附截图提的）：所有入口建完都**立刻连上
  并打开工作台**，写「保存」是误导。提示也不再弹「连接已保存」（还没连上，
  且弹窗压在刚打开的工作台上），只在**密码没记住**时弹一句。
  ⚠️ **照样落盘**：工作台按会话键从 `databases.ini` 列连接
  （`use_profile` → `dbstore::get`），不落盘这条连接建完就"找不到"、
  点了「连接」也连不上。落的是**连接定义**，不是 SSH 会话。

### 30 凭据库「忘了主密码」有个出路（重置）

用户：「我忘记了凭据库的密码…但得有个手段重置凭据库，这样我哪怕重新输入那些
凭据，而不是永远无法再保存密码。」

- `vault::reset(new_master)`：丢掉旧库、用新主密码建一个**空**库。旧密码
  **永久丢失**（Argon2id + AES-GCM 没有后门），但**连接列表 / 数据库连接定义
  / 「记住密码」勾选都不受影响** → 用户只要**重新输入**。
- 解锁框里多一颗「**忘记主密码？重置凭据库**」；**密码输错时对话框不关**
  （就地显示红色错误 + 指路）——老实现是弹个报错框然后**死路一条**。
- 重置分两步：先摆清"会丢什么/不会丢什么"再要新主密码。
- **顺手修的真 bug**：凭据库写入改成**原子替换**（老实现 `fs::write` 直接
  截断重写，写到一半被打断就只剩半截文件，**整个库连同全部密码一起没了**）。
  临时文件建时就是 0600（不留窗口），文件名带 pid（程序允许多开）。
- 主题补 `label.pp-error`：GTK 内置 `.error` 被本项目的
  `label { color: @pp_fg; }` 盖掉了，错误提示渲染成正常前景色。

## 二之二之三、本轮做的阶段 31 / 32 / 33 / 34

### 31 凭据库管理器（看库里有什么 / 改主密码 / 明文模式）

用户：「按你的意见来做」——补齐阶段 30 暴露的三个缺口。

- **凭据库管理器窗口**（`vaultui::open_vault_window`）：列出**库里有哪些
  连接名 + 各自属于哪个会话**（`db:<会话键>/<连接名>`，`dbstore::split_vault_name`）
  + 四个动作（改主密码 / 重置 / 锁定 / 关闭）+ 状态行。
  解锁完原先只提示一句"已解锁"，用户**什么也看不到**。
- **改主密码**（`vault::change_master(old, new)`）：凭据一条不少地搬过去；
  旧密码错则拒绝且文件不动；⚠️ **不许降级成明文**（新密码留空被拒）。
- **空主密码 = 明文模式**：文件格式升到 `version=2`，文件头明写
  `protected=true|false`——一眼看出这份库**没加密**；⚠️ **不假装加密**
  （空串派生出的密钥谁都能算出来）。老库按加密处理。
- `vaultui::MasterOpts` **改成 pub**：冒烟要直接开这个对话框验
  "密码输错时不关门"（只有真对话框才测得出来）。

### 32 右键「在此处打开终端」+ 拖拽上传 + 传输完不弹窗

用户原话：「1. 文件列表右键菜单增加一个『在此处打开终端』2. 支持文件拖拽上传，
可以通过拖拽到远程文件列表或者拖拽到终端来开启上传 3. 文件传输完不要弹窗，
可以发个通知。」

- **「在此处打开终端」**（`tabs::open_term_at`）：在当前标签**新开终端页签**
  （新 SSH 会话）并 `cd` 到那个目录。⚠️ **不复用当前终端**（会打断用户正跑
  的命令）。目标目录由纯函数 `term_target_dir` 决定（5 项单测）。
  `cd` 靠**就绪钩子**（`TermPane::set_ready_hook`）在 spawn 成功后才写 pty；
  ⚠️ 钩子传 `&vte::Terminal` **而不是** `TermPane`（捕获自己 = 自引用环）。
- **拖拽上传**：拖到远程列表 / 拖到终端都能传（拖到终端 = 传到面板当前
  浏览的远程目录）。⚠️ 终端的 DropTarget **只接 `text/uri-list`**（VTE 自己
  的拖放接 String，不抢）。⚠️ **顺手修了真 bug**：`drag_from` 原来只在
  `prepare` 里设**从不重置** → "程序里拖过一次"之后，再从文件管理器拖进来
  会被当成"同面板自拖"**直接拒掉**（用户看到"拖进来没反应"）。
  ⚠️ DropTarget 闭包一律抓**弱引用**（否则 = 引用环，面板释放不掉）。
- **传输完只发通知，不弹窗**：⚠️ **通知发不出去时也绝不弹窗**——桌面没有
  通知守护进程是最常见的情况，那时兜底成模态窗 = "每传完一批弹一次"，
  正是用户要摆脱的。改为**状态栏**写一句。失败明细在通知正文里，
  **点通知**才展开汇总窗（`ActionInvoked` 信号）。
  ⚠️ `Notify` 是**异步**的，"守护进程拒收"稍后才回主线程——**最容易弹窗的
  正是那一刻**，所以两个单测都**泵主循环 3 秒**再数顶层窗口。

### 33 凭据库状态看得见（顶栏常驻标志）

用户：「界面上应该有明显的标志，指示凭据库是否已经开启。否则用户可能会忘记
或糊涂。」

- **问题**：凭据库是"开了才生效"的开关，**开了没有全靠用户记得**——忘了就
  会以为"记住密码"把密码加密存起来了，其实它**明文躺在 `connections.ini`**。
- `vault::State`（`Off`/`Unlocked`/`Locked`/`Plain`）+ `vaultui::badge_button`：
  顶栏常驻 chip（图标 + 文字 + 配色 + tooltip），点它直接打开凭据库管理器。
  连接管理器顶部工具栏里也有同一个标志；两个菜单项标题也带状态。
- `vaultui::notify_state_changed()`：解锁/锁定/改主密码/重置/新建库五个点
  通知，标志自己跟着变。钩子抓**弱引用** + 控件销毁时注销。
- ⚠️ **别给标志加 `flat`**：Adwaita 的 `button.flat` 是 transparent，优先级
  比 `.pp-chip` 高，chip 底色会没了。⚠️ 换状态类时**基础类 `pp-chip` 要留**
  （底色/圆角/内边距都在它身上）。

### 34 新建数据库连接：当场输入的密码这次连接就要用

用户（附截图）：「这个对话框下面的按钮是『连接』，并没有让保存，即使不保存
应该也能连接吧，但现在点了连接会弹出一个对话框，让再次输入数据库密码。
这行为显然是不对的。」

- **根因**：对话框里敲的密码**只**在勾「记住密码」时才进凭据库；没勾时
  被**直接丢掉**。工作台连库按「内存口袋（`PanelState.db_pass`）→ 凭据库」
  找密码，两处都没有 → **再问一遍**用户。
- **修法**：`dbconn::stash_password(session_key, name, pw)` 把当场输入的
  密码放进**那个会话**的内存口袋（按连接名存）。
  - 新增 `tabs::panel_state_by_session_key`（按 `dbstore::session_key`
    对齐）——用户可能在连接管理器/会话树里建连接，而真正连库的是某个标签
    页里的工作台。
  - **不勾「记住密码」也放**（不落盘是对的，但这次连接要用它）。
  - **密码框留空不放**（塞空密码只会让它拿空密码去连）。
  - **新建（`ui_new`）与编辑（`ui_edit`）两个入口都放**。
  - 落盘行为不变（连接定义照旧进 `databases.ini`）。

## 二之二之四、本轮做的阶段 35 / 36（菜单统一）

### 35 所有菜单统一到终端右键菜单（用户报「划过不高亮」）

用户（2026-10-28）：「所有的菜单风格都要像终端右键菜单看齐。目前数据库连上
以后，左侧列表中菜单鼠标划过菜单项没有高亮，也需要修复」。

- **项目里早有一套统一构件** `src/menu.rs`（`Popover` + `Button` +
  `pp-menu-*` CSS）：终端 / 侧边栏 / 文件面板 / 队列 / 标签 / 连接管理器 /
  快速命令 / 子标签都在用。但**数据库工作台的两处菜单是手搓的**：
  - `panel/dbtree.rs` 对象树右键菜单（11 项）：`Button::with_label` + `flat`
  - `panel/dbui.rs` 导出格式菜单（5 项）：同上
  - **「划过不高亮」的根因**：普通 `gtk::Button` **没挂 Motion 控制器**。
    Popover 里按钮拿到 PRELIGHT 状态后 CSS `:hover` 常常只更新状态不刷背景，
    高亮一直是靠 `menu::attach_hover()` **显式加 `pp-menu-hover` 类**实现的。
- **修法**：两处都改用 `crate::menu`（图标 + 卡片 + 显式 hover + 分组线），
  「删除表」走 `menu::danger()` 标红。
- **`menu.rs` 新增**：`popover_detached()`（**不挂父**，给
  `MenuButton::set_popover()` 用）、`item_label()` / `set_item_label()`。
- **`DbTree` 新增测试钩子**：`menu_items()` / `menu_popover()` /
  `expand_row_by_name()`。
- **验证**：`examples/menu_style_smoke.rs` —— 假后端填出对象树 → 弹真实
  右键菜单 → 逐项断言类/图标/hover 控制器 → **用 xdotool 把真指针挪到菜单
  项上**，确认拿到 `pp-menu-hover`（走的是"真指针 → GTK 事件 → Motion
  控制器"，不是自己给自己加类）。截图 `/tmp/pp_tree_menu.png`。

### 36 顶栏菜单栏 + 连接管理器 ⋯ 菜单也统一

- **顶栏**：原来是 `gtk::PopoverMenuBar` + `gio::Menu` 模型。现在是**一排
  普通 `gtk::Button`**，各带一个 `crate::menu` 建的弹层；菜单项内容抽成
  `main.rs::TOP_MENUS` 常量表（图标 + 文字 + action 名 + 是否危险项）。
- **连接管理器 ⋯ 菜单**：原生 `PopoverMenu` → `crate::menu` 自绘。
- ⚠️ **三个坑**（都写进代码注释了）：
  1. 给 `MenuButton::set_popover()` 用的弹层要用 `popover_detached()`——
     那个方法自己会 `gtk_popover_set_parent`，再设一次报
     `assertion 'parent == NULL' failed`。
  2. 顶栏用**普通 Button**而不是 `MenuButton`：后者自带的小三角在
     GTK 4.18 里不是普通 image 节点，CSS 压不掉；普通 Button 的弹层要
     **自己 `set_parent(&btn)`**。
  3. **CSS 后代选择器会漏进弹层**：弹层是按钮的子控件，
     `button.pp-menubar-item image { … }` 会把弹层里的**菜单图标一起
     命中**——图标凭空消失（实测踩过）。
- ⚠️ **e2e 的 `find_button_by_label` 改了**：菜单项按钮的 child 是
  Box（图标 + 文字 Label），**没有**设 button 的 `label` 属性，
  只看 `b.label()` 会找不到（e2e 曾挂在「导出菜单里应当有『导出为 CSV…』」）。
  现在两种都查。

## 二之二之五、本轮做的阶段 37（终端分屏 / 关闭当前终端修复）

用户（2026-10-28）：「终端窗口右键菜单，分屏和关闭当前终端按钮不起作用了」。

- **根因 1（分屏）**：`split_index` 要求「目标面板的父容器本身是个
  `Paned`」（要把目标面板塞进新 `Paned`，再把新 `Paned` 换到目标面板原来的
  位置），拿不到就**静默 `return`**（不报错、不 panic）。阶段 12 把终端改成
  **子标签**后，终端面板的父容器变成 notebook 的页面容器（`Box`）→ 分屏
  永远失败。**与阶段 35/36 的菜单改动无关**，是子标签化时漏掉的。
- **根因 2（关闭当前终端）**：连带受害者。`close_pane_index` 里「只剩一块
  面板时不动」（`count() <= 1` 直接 `return false`）本意是"没有另一半可以
  顶上来"，但分屏坏掉后**永远只有一块** → 这条路永远 no-op。
- **修法**：
  1. `tabs.rs::term_container()`：终端子页签内容套一层**单子控件的
     `gtk::Paned`**（GTK4 里单子控件的 Paned 会让子控件占满全区，外面看
     不出来），分屏那条路径不用改。
  2. **布局树按终端子页签分组**：`Tab.layout`（一棵树）→ `Tab.layouts`
     （`Vec<(u64, PaneLayout)>`，每个终端子页签一棵）。之前只有 Tab 级一棵，
     **额外终端子页签**（子标签栏「+」新建的）的面板不在任何树里 → 在那些
     页签上点分屏也是"点了没反应"。
  3. `close_pane_index` 分三种：分屏中→关掉**这一半**；只剩一块→**关掉
     那个终端子页签**（跟子页签上的 × 一个意思）；目标是额外子页签→关掉
     那个额外子页签。
  4. ⚠️ 关子页签时要把它的**布局树一并摘掉**，并对树里分屏出来的副面板
     `shutdown()`——它们**不在 `term_pages` 里**，漏了就留下一堆野 ssh 进程。
- **验证**：新增 `examples/split_smoke.rs`（四段断言：分屏 → 关一半 →
  单面板时关子页签 → 额外子页签也能分屏）+ GUI 实测（xdotool 点**真菜单
  项**：右键 →「左右分屏」→ 两栏；右键右半 →「关闭当前终端」→ 只剩一栏；
  单面板再点 → 终端子页签消失）+ 截图 `/tmp/pp_s37_split.png` 等。

## 二之二之六、本轮做的阶段 38（顶栏菜单栏悬停切换）

用户（2026-10-28）：「我打开文件菜单然后鼠标移动到别的菜单上，别的菜单会自动
打开。而我们现在的软件必须先关闭现在的菜单，才能打开别的菜单，就是说需要点
两下，而常用的软件都是根本不用点，只要移动鼠标就行了。」

- **根因**：阶段 36 把顶栏菜单栏改成"一排普通 `gtk::Button` + 各自的自绘弹层"
  时，**只接了 `clicked`**——压根没人监听"鼠标移过来"，所以只能点两下。
- ⚠️ **别去改弹层的模态属性**：GTK4 的 `GtkPopover` **没有 modal 概念**
  （那是 GTK3 `GtkMenu` 的东西，也没有指针 grab）。弹层开着时 motion 照常
  派发到别的按钮上，悬停切换做得到；`autohide` 默认开着，点别处仍会自动收起。
  （gtk4-rs 里也**没有** `set_modal` 绑定——查 `gtkpopover.h` 才发现没有
  这个 C 函数，一度以为该用 `set_modal(false)`。）
- **修法**：`menu.rs` 新增 `MenuBar`——标题按钮挂 `EventControllerMotion`，
  `enter`（鼠标移入）时切到那个菜单；点击是 toggle 语义；弹层 `closed` 时
  清掉"当前打开着的是哪个"（不然下次切换会对关掉的弹层调 `popdown`）。
  `main.rs` 的顶栏菜单栏改用它（`TOP_MENUS` 表不动）。
- **验证**：menu 模块新增 2 项不变量单测（标题按钮**必须挂 Motion 控制器**、
  切菜单时旧的必须关上）+ 新增 `examples/menubar_hover_smoke.rs`——xdotool
  把**真指针**移到另一个标题上，断言菜单**自动打开**且旧的自动收起
  （`EventControllerMotion` 没有"手动派发事件"的 API，只能靠真指针）。

## 二之二之七、本轮做的阶段 39（主界面按钮条图标化）

用户（2026-10-28）：「主界面上所有的按钮都改成图标按钮，尽量少用文字」。

- **范围**（用户确认）：**只改按钮条**（顶栏 / 文件面板 / 工作台 SQL 行 /
  表设计器工具栏 / 文件编辑器标题栏），**弹窗对话框保持文字**——那些是
  表单动作，「保存修改」和「放弃修改」光看图标分不出来，图标化反而更
  容易点错。欢迎页的空状态主按钮同理保留文字。
- **做法**：文字一律挪进 **tooltip**，按钮只剩图标。工作台复用已有的
  `dbui::icon_button()`（带 `pp-icon-btn` 类），文件面板复用 `pane::icon_btn()`，
  顺手删掉只剩一个调用者的 `pane::text_btn()`。
- ⚠️ **说明文字里提到按钮名的地方要跟着改**：工作台空状态原来写「写 SQL
  点「执行」」→ 改成「点 ▶」；表设计器 tip 从「改完点「保存修改」」→
  「改完点工具栏的保存图标」。按钮变图标了，提示还指着旧文字就指空了。
- **图标名要挑对**：`edit-select-all-symbolic`（全选）当「执行选中」用会被
  误解成"全选"，换成 `edit-select-symbolic`（选中）✅；「取消」用
  `media-playback-stop-symbolic`（■），「历史」用 `document-open-recent-symbolic`。
  改之前先查 `/usr/share/icons/Adwaita/symbolic/` 确认图标**真的存在**
  （不存在会回退成"图标缺失"的禁止符号）。
- **验证**：新增 `examples/icon_btn_smoke.rs`——断言按钮条上的按钮
  **child 是 `Image`（纯图标）**且**都有非空 tooltip**（少一条界面就悄悄
  退化回"一排字"，还不报错）。为此加了两个测试钩子：
  `dbui::sql_row_buttons_now()`、`Pane::transfer_btn`。
  ⚠️ 写钩子时踩了 E0716：`st.db_ui.borrow().as_ref()` 的临时值在语句结束
  就没了，借用要**落成具名变量**（`let slot = st.db_ui.borrow();`）。

## 二之二之八、本轮做的阶段 40（顶栏菜单栏收窄）

用户（2026-10-28）：「主菜单宽度有些太大」。

- **根因**：菜单项宽度 = 标题（两个汉字约 27px）+ **左右 padding 各 8px**。
  padding 是"看不见但占地方"的大头——7 项就是 112px 纯空白，项间隔 16px
  （GNOME 大约 6~8px）。
- **改法**：`padding: 2px 8px` → `1px 5px`；标题字号 `font-size: 0.92em`
  （菜单栏是"入口"，不该跟正文抢字号）。
- **实测**：每项 43px → 35px，总宽 **301px → 245px**（-19%）。顶栏上实际
  观感从 ~357px 收到 ~265px。
- ⚠️ **样式调整要量，别凭感觉**：`examples/ui_shot.rs` 新增 `menubar` 模式
  逐项打印按钮宽度（`cargo run --example ui_shot -- menubar`）。
  ⚠️ **ui_shot 不会自行退出**（结尾是 `MainLoop::new(None, false).run()`），
  要用 `timeout 30 cargo run ...` 跑，否则会一直挂着。

## 二之二之九、本轮做的阶段 41（顶栏菜单栏减顶级菜单 7 → 5）

用户（2026-10-28）接着阶段 40 报：「我看不出变化啊」→ 卸掉系统里
10-21 装的旧 deb 包、改跑最新 release build 后又问「为什么还不行呢」。

- **根因（两层）**：① 阶段 40 只调了 padding 与字号，总宽 301 → 245px
  （-19%）——**19% 肉眼分辨不出来**；② 用户当时跑的还是**旧 deb 包**，
  阶段 18~40 的改动他一个都没看到过。
- **关键判断**：**要让用户一眼看出差别，只能动结构，不能只调样式**。
  样式微调有天花板（收到"挤成一团"之前顶多 -20%）。
- **做法**：顶级菜单 **7 → 5**。「编辑」（复制/粘贴/终端查找…）并进
  「终端」开头，「帮助」（快捷键一览/关于）并进「工具」末尾。功能项
  一个没少（复制/粘贴在终端右键菜单里本来就有，查找有 Ctrl+F，
  快捷键一览有 F1）。样式再收一档：`padding: 1px 4px`、`font-size: 0.88em`。
- **实测**：`examples/ui_shot.rs -- menubar` 逐项量宽——**301 → 160px**
  （5 项 × 32px），相对最初 **-47%**。
- ⚠️ **菜单表挪到 lib 模块 `src/topmenus.rs`**：原来在 `main.rs` 里，
  `ui_shot` 量宽度只能**手抄一份标题列表**——抄本会漂移（菜单从 7 减到
  5 时抄本还是 7 项，量了个寂寞）。现在 `top_menu_titles()` 给真实标题。
- ⚠️ **新增「菜单项 ↔ action」对账**（`topmenus::missing_actions`）：
  菜单并来并去最容易出的错是"**菜单项还在、action 没了**"——点了没
  反应、界面不报错。`main.rs` 启动时对一遍，缺了往 stderr 打一行。
  （已端到端验证：故意把 action 名改坏，启动时确实打出告警。）
- ⚠️ **交付前记得重新打包安装**：用户跑的是 `/usr/bin/puttyplus`（dpkg
  装的 deb），改完源码不 `build_deb.sh` + `dpkg -i`，用户看到的还是旧版
  ——这正是上一轮"看不出变化"的直接原因。

## 二之二之十、本轮做的阶段 42（数据库「保存修改 / 放弃修改」按钮一直禁用）

用户（2026-10-28）：「数据库的提交和回滚按钮都是禁用的，改了数据之后也
点不了」。

- **根因**：`ResultTab` 的「保存修改 / 放弃修改」建出来时是
  `sensitive(false)`（没进编辑模式时不该能点），而 `set_edit_toolbar()`
  **只切了 `visible`、忘了切 `sensitive`** → 进编辑模式后两个按钮变可见，
  却还是**灰色**，点了没反应，界面还不报错。
- ⚠️ **为什么一直没被发现**：e2e 里这段流程走测试钩子 `dbui::grid_save()`
  **直接调内部函数 `do_save()`**，**从来不点真实按钮**——钩子绕过了整个
  按钮层。跟「e2e 查模型 ≠ 查视图」是一类毛病：**测的是内部函数，不是
  用户点的那颗按钮**。
- **修法**：`set_edit_toolbar()` 里同时切 sensitive。⚠️ **别去动「编辑 /
  插入行 / 删除行」的 sensitive**——那三个能不能点取决于"目标是不是可编辑
  的表"（视图表不能编辑），由别处按 `editable` 设，覆盖掉会让视图表也能
  进编辑模式。
- **新增防线**：纯函数 `edit_toolbar_visible()` / `save_discard_sensitive()`
  + 2 项单测；测试钩子 `dbui::edit_toolbar_buttons()` 交出**真实按钮**；
  e2e 断言「保存 / 放弃必须 `is_sensitive()`」。
- ⚠️ **验证过这条断言真的抓得住**：临时回退修复跑 PG e2e → 断言失败，
  打出「「保存修改」按钮是**禁用**的 —— 改了数据点不动」；还原后 e2e 全绿
  （`[3.9.1.1] 保存/放弃按钮可点：save=true discard=true`）。

## 二之二之十一、本轮做的阶段 63（统一关闭确认，审计 P0-1）

用户：UI/UX 审计 P0-1——同一个「未保存的工作」，从不同入口关闭得到不同结果。

- **新模块 `src/panel/closeguard.rs`**：
  - `collect_dirty(st)`：只读地收集三类脏状态（文件编辑器的未保存修改 /
    数据库待保存的改动（数据行 + 表结构）/ 正在跑的传输）；
  - `request_close(st, proceed)`：弹**同一个**确认框（保存并关闭 / 不保存关闭 /
    取消），**默认焦点落在「取消」**；`request_close_many` /
    `request_close_window` 是多面板版本。
- **关闭路径全部改走它**：标签 × / 右键「关闭标签」/ 中键 / 「关闭其它标签」
  （一次问完所有标签）/ 窗口 × / 文件→退出 / **数据库子标签 ×**
  （以前 `add_page_icon(..., None)` ⇒ 按 × 直接把改动丢掉）。
- ⚠️ **GTK4 移除了 `gtk_dialog_run()`**：同步模态自己跑**嵌套主循环**
  （`dialogs::run_modal`）。
- ⚠️ **「保存并关闭」是异步的**：关窗会把 worker 通道丢掉 ⇒ 等回包齐了再关
  （`SaveBarrier`）；**存失败就不关**。
- ⚠️ **本地文件的保存是同步完成的** ⇒ 批量保存的"完成报告"必须延后一拍。
- ⚠️ **数据库的保存要排队**（`dbui::save_chain`）；回包里正借着 `st.db_ui`
  ⇒ "保存完成"的报告也要延后。
- ⚠️ **关窗口时"存完再关"不能顺手 `w.close()`**（递归回 close-request 处理器）。
- ⚠️ **`style_dialog` 按文案认主操作**：「不保存关闭」里也有"保存" ⇒ 两个蓝
  按钮 ⇒ 关闭确认框里自己定角色。
- **防回归**：`examples/close_confirm_smoke.rs`（真的开文件、真的插字、真的点
  按钮；**验证过回退后会失败**）。
- **验证**：476 单测 + compat 80/0 + 四个相关冒烟全绿 + 实机截图。

## 二之二之十二、本轮做的阶段 64（查询结果带真实列类型，审计 P0-3）

- **问题**：查询结果的列头一律写"文本" ⇒ 数字列按**字典序**排序（10 在 2 前）、
  布尔列显示 `t`/`f`、类型不知道时也写"文本"（在骗人）。
- **改法**：`DbRows` 带 `types: Vec<Option<ColType>>`；`ColType::from_pg_oid()`
  映射表；`ColType::Other` = 「类型不知道」⇒ 显示「—」；网格里数字按数值排、
  布尔按真假排、布尔显示「是/否」（**只改显示**，原始数据不动）。
- ⚠️ **计划里的前提落空了**：tokio-postgres 的**简单查询协议**把类型 OID
  **丢掉了**（`SimpleColumn` 只有列名）⇒ 改用 `prepare` 一次问类型
  （不执行不传行），**代价是每条 SELECT 多一个来回**。MySQL 不用绕
  （线协议就带类型）。
- **防回归**：单测 6 项 + 新冒烟 `examples/coltype_smoke.rs`（不连真库，
  **验证过回退后会失败**）。
- ⚠️ **真库 e2e 未跑**（要装 PG 服务端 ⇒ 得先请示用户）。

## 二之二之十三、本轮做的阶段 65（标签状态聚合，审计 P0-4）

- **问题**：标签上的状态点只反映**文件面板**（SFTP）⇒ 终端会话早结束了
  （敲了 `exit`、网络抖了），点还是**绿的**，点进去才发现是"死画面"。
- **改法**：`active.rs` 加 `TermState{Unknown,Alive,Dead}`（**三态**：
  没上报 ≠ 已结束）+ 纯函数 `aggregate(sftp, term, db)`（取最严重的）；
  标签点 / 侧边栏 / 连接管理器**共用**它；tooltip 说清哪一部分出问题
  （「文件面板：已连接 / 终端：已结束 / 数据库：未连接」）。
  ⚠️ 终端掉了映射成**黄点**（Err 在两棵树里不画点 ⇒ 会与"没连"混淆）；
  ⚠️ 数据库是**可选**功能 ⇒ 只进 tooltip，不进颜色。
- **接线**：`TermPane` 加**存活钩子**（子进程起来 / 退出各一次）⇒
  `panel::note_term_alive()` ⇒ `tabs::refresh_dots()`。闭包只抓**弱引用**。
- **防回归**：单测 3 项 + 新冒烟 `examples/state_agg_smoke.rs`
  （含**生产路径**回归：PATH 前置一个**假 ssh** 脚本 ⇒ 起来报「运行中」、
  退出报「已结束」；**验证过摘掉钩子后会失败**）。
- ⚠️ **一条假警报**：`pty.spawn_async` 是**异步**的 ⇒ 我以为"钩子装晚了
  ⇒ 启动事件会漏"，实测不漏 ⇒ 补报逻辑改成防御性；**回退验证的对象也换了**
  （见 AGENTS.md「状态点与聚合状态的坑」）。

## 二之二之十四、本轮做的阶段 66（DB 页签独占宽度 + 对象树可折叠，审计 P0-2）

- **问题**：DB 工作台是连接页的子标签，而连接页左边常驻 SFTP 面板（340px）
  ⇒ 1200 宽时主区只剩 300px，主 CTA 被裁掉（审计 P0-2）。
- **改法**：
  - `subtabs.rs`：`SubPage` 增 `kind`（`PageKind`），新增
    `add_page_icon_kind()`（保留旧签名）；
  - `tabs.rs`：监听子标签的 **`switch-page`** ⇒ 当前页是 DB 就
    `sftp.set_visible(false)`（**只切 visible，不改 Paned 结构**），
    切回终端页就恢复；
  - 子标签栏「＋」旁加「文件面板」开关（只在 DB 页出现），选择**被记住**
    （`settings.ini` 的 `ui/db_hide_sftp`，默认隐藏）；
  - `dbui.rs`：对象树可折叠（树头「‹」+ 顶部「对象树」开关）。
- **防回归**：新冒烟 `examples/db_wide_smoke.rs`（6 条断言，含几何断言），
  **两次回退验证都确认会失败**。
- ⚠️ **GTK4 的 `GtkNotebook:current-page` 改了也不发通知** ⇒ 监听切页要用
  `switch-page` 信号；且它在**切换之前**发 ⇒ 得按页序号查种类。

## 二之二之十五、本轮做的阶段 67（容器与窗口生命周期规范，审计 P2-17/P2-16）

- **问题**：同一个工具能从多个入口开出**多个副本**；关闭行为不统一；
  有的记尺寸有的不记；**新连接对话框没有父窗口**。
- **改法**：新模块 `src/windows.rs`：
  - 三类容器：**页面**（子标签）/ **工具窗**（非模态 + 单例 + 记尺寸）/
    **对话框**（模态 + 有父窗口）；
  - `open_or_focus(kind, build)`：同类只开一个，关掉即注销 + 记尺寸；
  - `dialog(title, parent, buttons)`：必定有父窗口（兜底到主窗口）；
  - 父窗口收成一份（connmgr / about / shortcutsui / replayui 各存一份的
    时代结束）；
  - 首批归类：连接管理器 / 私钥 / 凭据库 / 目录镜像 ⇒ 工具窗；
    一批模态对话框 ⇒ 走 `windows::dialog`。
- **防回归**：新冒烟 `examples/window_lifecycle_smoke.rs`（**走真实入口**），
  **两次回退验证都确认会失败**。
- ⚠️ 数顶层窗口要**只数可见的**（`gtk_window_list_toplevels()` 混着 GTK
  内部的隐藏窗口）。

## 二之二之十六、本轮做的阶段 68（统一「新建」入口，审计 P1-2/P1-6）

- **问题**：同一个"新建"有 **5 个入口**（顶栏 `+` / 侧栏 `+` / 子标签 `+` /
  欢迎页按钮 / 文件菜单），**各写各的** ⇒ 从哪个入口进来看到的东西不一样；
  两个 `+` 长得一模一样、干的活完全不同。
- **改法**：
  - **动作表**（`topmenus::NEW_ITEMS`，唯一来源）4 项：新建连接… /
    新建终端 / 新建数据库连接… / 导入 PuTTY 会话…（各带 icon+label+action+scope）；
  - **动作的实现**只有一份（`newmenu::run_in` / `run_in_session`），各入口
    只是"从表的哪个角度看"（`topmenus::NewEntry`）；
  - 会话内的入口只给会话内的动作；**没有会话时会话内的动作置灰**
    （`newmenu::NewCtx`）；
  - **两个 `+` 换不同图标**（顶栏/侧栏 `list-add-symbolic`，子标签
    `tab-new-symbolic`），tooltip 写清作用域。
- **防回归**：新冒烟 `examples/newmenu_smoke.rs`（26 条断言），**两次回退
  验证都确认会失败**。
- ⚠️ **踩到的坑**：
  - `MenuButton` 的弹层走 `set_popover()` ⇒ 内容一次性建好、上下文变了
    不跟着变 ⇒ 改用**普通 `gtk::Button`** + `menu::popover()`，每次点击
    重建菜单；
  - 弹层是 `set_parent` 挂上去的，**不进 children 链** ⇒ 冒烟要拿它得走
    **测试钩子**（`Sidebar::plus_popover()`）；
  - 弹层必须在**真实 toplevel** 里才能 `popup()`，否则**段错误**；
  - ⚠️ **欢迎页上顶栏「＋」是隐藏的**（既有行为）——别当成 bug。
- **验证**：488 单测 + compat 80/0 + 6 个冒烟全绿；截图
  `/tmp/newmenu_session.png`、`/tmp/newmenu_welcome.png`
  （脚本 `tools/shot_newmenu.sh`）。

## 二之二之十七、本轮做的阶段 69（首选项中心，审计 P1-4）

- **问题**：设置项散在**四个地方**（「外观」窗口 / 「会话选项」/ 连接
  编辑器 / 传输队列窗口顶部），终端外观还拆成三处 ⇒ 改一个设置得先知道
  它在哪。
- **改法**：
  - 新模块 `src/settingsui.rs`：**一个窗口六个页**（外观 / 终端 / 传输 /
    安全 / 快捷键 / 高级），工具窗（同类只开一个 + 记尺寸）；
  - **注册表** `SETTINGS`：每个设置项一行（id/label/page/from）⇒
    **有且只有一个归属页**；
  - **原「外观」窗口删掉**（`themeui.rs` 整个没了）：「视图 → 外观…」打开
    首选项中心的**外观页**；「工具」菜单加总入口「首选项…」；
  - **传输设置搬走**到「传输」页，队列窗口顶部改成只读摘要 + 「去设置」；
  - **终端配色挪到「终端」页**，新增**全局终端字号**（`ui/term_font`）。
- **防回归**：单测 2 项 + 冒烟 `examples/settingsui_smoke.rs`（22 条断言），
  **回退验证过**。
- ⚠️ **踩到的坑**：
  - GTK4 的 `Notebook::tab_label` 认的是**页控件**不是页序号 ⇒ 得先
    `nth_page()` 拿控件；
  - `gtk::Label::new` 收 `&str`，`format!()` 出来的是 `String` ⇒ 要 `&`；
  - `Switch::state-set` 的参数是 **bool**（不是 `StateType`）；
  - 用户可见文字里**别留 Markdown 的 `**`**（GTK 标签不渲染，会原样显示）——
    截图时才发现「传输」页那句提示里全是星号。
- **验证**：490 单测 + compat 80/0 + 7 个冒烟全绿；截图
  `/tmp/settings_*.png`（六页各一张）。

## 三、下一步候选（阶段 28+）

按"对标 Navicat / DBeaver / XTerminal"的价值排序：

1. **导入向导增强**（阶段 18 的后续）：目标表**不存在时自动建表**
   （按 CSV 表头猜类型）、**映射模板保存**、xlsx 支持（要引依赖，先掂量）。
2. **ER 图增强**：导出成图片 / SVG、手工摆表、从图上直接建外键。
3. **列宽 / 窗口布局持久化**（阶段 20 只做了"哪些列、什么顺序"）。
4. **导出向导**：勾字段、选编码、分文件（阶段 21 只做了格式选择）。
5. **查一查启动时那 5 条 `gtk_css_node_insert_after` 警告**（不影响功能，
   但看着不舒服）。

## 四、常用命令

```bash
cd /home/fwt/workspace/projects/putty
export PATH=$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH
xvfb-run -a cargo test --lib                 # 423 项
xvfb-run -a cargo run --example fileops_smoke         # 阶段 32：5 步
xvfb-run -a cargo run --example vault_reset_smoke      # 凭据库重置 4 步
xvfb-run -a cargo run --example vault_mgr_smoke        # 凭据库管理器 3 步
xvfb-run -a cargo run --example dbconn_dialog_smoke    # 新建连接对话框 3 步
xvfb-run -a -s "-screen 0 1920x1200x24" cargo run --example db_ui_e2e -- \
    182.92.244.132 22 root 127.0.0.1 5432 postgres postgres mysecretpassword pg --shot-er
xvfb-run -a -s "-screen 0 1920x1200x24" cargo run --example db_ui_e2e -- \
    182.92.244.132 22 root 127.0.0.1 3306 root demo my-secret-pw mysql --shot-er
bash tools/compat_check.sh             # PASS=80 FAIL=0

# 本地测试 sshd（阶段 32 的 fileops_smoke 要它；⚠️ 主机密钥固定放这里，
# 别放 /tmp —— /tmp 会被清理，host key 一变 ssh 会直接拒连）
D=$HOME/.cache/puttyplus-test-sshd
/usr/sbin/sshd -f "$D/sshd_config" -E "$D/sshd.log"
cargo build --release
```

**真库**（跳板机 `182.92.244.132`，SSH 密钥 `~/.ssh/id_rsa`，
容器 `some-postgres` / `some-mysql`）：

| 类型 | 地址 | 端口 | 用户 | 库 | 密码 | 版本 |
|---|---|---|---|---|---|---|
| PostgreSQL | 127.0.0.1 | 5432 | postgres | postgres | mysecretpassword | 18.6 |
| MySQL | 127.0.0.1 | 3306 | root | demo | my-secret-pw | 26.7.0 |

## 五、别踩的坑（补充）

- **拆提交**：`git apply --cached --unidiff-zero` 会把后续 hunk 插到错位置。
  正确做法是 `git diff -U3` 拆成**带上下文**的 patch，再 `git apply`。
  ⚠️ 备份"完整版"要在**第一次 `git checkout HEAD --` 之前**做。
- **e2e 查模型 ≠ 查视图**：模型有节点不代表屏幕上画出来了。
- **包列表/树的 ScrolledWindow 一律要 `set_vexpand(true)`**。
- **深遍历 `GtkTreeStore` 不能用 `iter_next`**：一律按路径走。
- **清树别把根节点抹掉**：`clear()` 之后 `reload()` 就长不回来了。
- **RefCell 的借用别跨语句撑着**：`self.x.borrow().as_ref()` 那样写，
  临时值在语句结束就没了（E0716）；要 `let v = self.x.borrow();` 落变量。
- **普通 Label 不解析 markdown**：`dialogs::confirm_with` / `info` 里是
  `gtk::Label`（不是 markup label），`**` 会**原样显示**出来。强调要么改用
  中文措辞，要么自己建 markup label。
- **GTK 内置的 `.error` 在本项目里没用**：主题 BASE 段有一条
  `label, button, … { color: @pp_fg; }`，把 Adwaita 的 `label.error` 盖掉了
  → 错误提示渲染成正常前景色。自带 `label.pp-error`。
- **跑示例 / 冒烟前用 `PUTTYPLUS_DB_INI` / `PUTTYPLUS_CONN_INI` /
  `PUTTYPLUS_HIST_INI` / `PUTTYPLUS_COLS_INI` / `PUTTYPLUS_VAULT` 指到临时
  文件**，别写用户真实配置（凭据库尤其不能碰）。
  （校验：`~/.config/puttyplus/databases.ini` 的 md5 应为
  `70f6a18c7c08a29a5e1525c7a15fbb22`；跑完不该出现 `columns.ini`。）
- **python 批量替换会静默失败**：改完必须 grep 校验落地。
- **改容器结构前先查"谁依赖某个控件的父容器类型"**：阶段 12 把终端改成
  子标签后，终端面板的父容器从 `Paned` 变成 notebook 页面容器（`Box`），
  而分屏要求"父容器本身是 `Paned`"，拿不到就**静默 return** → 「左右分屏」
  点了跟没点一样。父容器类型是**隐式契约**，改结构时要一起改。
- **「只剩一块就不动」的守卫会变成永远 no-op**：分屏坏掉后永远只有一块，
  「关闭当前终端」跟着一起废掉。**一个功能坏了、另一个跟着坏且症状不同**
  时，先怀疑共享前提（阶段 37）。
- **容器树按"拥有者"分组**：布局树改成"每个终端子页签一棵"
  (`Tab.layouts`)，额外终端子页签才能分屏。关子页签时要把它的树摘掉，
  树里分屏出来的副面板也要 `shutdown()`（否则留野 ssh 进程）。
- ⚠️ **`TreeViewColumn::set_widget()` 只换列头、不动列内容**（2026-10-29 实测，
  探针截图 `/tmp/tvc_probe*.png`）：可以放心用它做「列名 + 类型小字 + ⋮」这种
  dbx 风格列头。⚠️ 但 **`set_title()` 也要留着**——`set_widget` 只影响显示，
  `title` 属性还得有值（导出 / 过滤条件编辑器 / 列类型映射都靠 `col.title()`
  拿列名，少写一句那里全是空串）。
  ⚠️ 自定义列头里点子控件**仍会冒泡**到列头按钮（= 排序照样触发），
  得自己记标志把那次排序吃掉。
- ⚠️ **e2e 等回包不能只看状态栏关键词**：上一次查询留下的「查询完成：3 行」
  还挂着，光看"有没有查询完成"会**立刻返回**拿到旧结果——网络慢的时候必踩
  （看着像"过滤没生效"，其实是没等到新回包）。要跟发查询**之前**的状态比。
  同理，双击表 / 建探针例程这类**固定 sleep** 也得改成轮询等真结果。
- ⚠️ **e2e 会往真库留脏数据**：中途崩掉时探针行留在库里，下一轮"行数回到
  原样"的断言就挂（看着像功能坏了，其实是上一轮的脏数据）。e2e 开头先清
  自己插的行（`id >= 900000`）才幂等。
- ⚠️ **`menu::tests::菜单项文字可读可改` / `theme::tests::跟随系统也能拿到修复规则`
  偶发失败**（并行跑时依赖 GTK 全局状态）：全量 `cargo test --lib` 偶尔 437/439。
  单独跑必过——**不是功能坏了**，别去改代码追这个。
- ⚠️ **重建 model 会丢选中行**（`store.clear()` 把选择清了）：多选一改就"失选"，
  用户没法接着改第二行。重建前记住选中的 `RowRef`、重建后选回去。
- ⚠️ **GTK4 的 Popover 打开时会 grab 指针**：后面的右键全被它吃掉
  （点哪儿都是"关菜单"）。测试里连续右键前**先按 ESC** 关掉弹层。
- ⚠️ **不装主题（`theme::apply()`）时，自定义 CSS 类不生效**：菜单会变成
  "没上妆"的默认样式——图标和文字挤在一起，看着像布局坏了，其实是主题没装。
- ⚠️ **`ComboBoxText::append(id, text)` 的顺序别搞反**：`id` 是程序里认的，
  `text` 是**给用户看的**。写成 `append("并且", "AND")` 的话，界面上显示的是
  "AND"，而 `active_id()` 回来的是"并且"——判断 `"OR"` 永远不成立 →
  **「或者」这个选项形同虚设**（看着能选，选了没反应）。阶段 48 踩过。
- ⚠️ **`scrot` 不覆盖同名文件**：文件已存在时它会自动加 `_000` / `_001`
  后缀另存——下次去看 `/tmp/xxx.png` 看到的是**上一次**的图，白忙一场。
  截图前先 `rm -f` 目标文件。（阶段 48 排查"截图一片黑"时在这上面绕了半天。）
- ⚠️ **工作台嵌在连接页的子标签里时，截图截不到它**：工作台不是独立窗口，
  光"建了面板"不等于"工作台可见"。要么**不给面板挂子标签容器**
  （`dbui::ensure` 的兜底路径会自己开一个 1200x760 的独立窗口），要么
  `sub.switch_to(tab_id)` 切过去再截。
- ⚠️ **类型映射要按"列名"配对，别按"第几列"**：`col_type_map` 原来拿网格的
  **显示位置**配元数据的类型，隐藏过列以后就错位——int 列被当成 text 列，
  条件里的值多套一层引号（PG 报类型错）。现在列名与类型都取自**元数据**
  （同序配对），网格的 `column_type_map()` 只作兜底。
- ⚠️ **`gtk::Stack::add_named()` 不会设 title**：`GtkStackSwitcher` 显示的是
  **title** 不是 name——用 `add_named` 加的页，按钮全是**空白**（看着像
  "分段控件没画出来"）。要 `add_titled(child, name, title)`。（阶段 49 踩过。）
- ⚠️ **GTK4 的 `GtkStackSwitcher` 内部按钮是 homogeneous 的**：每个按钮平分
  整行宽度（5 个按钮摊满 640px，像一排导航链接，不像分段控件），而且当前段
  没有底色高亮。要紧凑的分段控件就**自己画一排 `ToggleButton`**。
- ⚠️ **分段控件要双向同步**：Stack 的 `visible-child-name` 变了要更新按钮，
  按钮点了要更新 Stack。同步时**先比再设**——`set_active` 会发 `toggled`
  → 又去设 visible_child_name → 再 notify，不比的话会绕成死循环。
- ⚠️ **改 `RefCell` 里的集合后要重建视图**：借用**要在花括号里结束掉**再调
  重建函数——重建函数里还要读同一个 `RefCell`，借用还挂着就是**双重借用 →
  panic**（阶段 50 在 `DbGrid::set_column_editable` 踩过）。
- ⚠️ **同一套网格被两种语义共用时，菜单要能关**：`DbGrid` 的右键菜单是按
  **结果网格**做的（设为 NULL / 克隆为新行 / 转置…），字段表 / 索引表 /
  外键表右键弹那一套就是错的（"把字段名设为 NULL"听着就危险）——加了
  `set_row_menu_enabled(false)`。
- ⚠️ **`ComboBoxText` 的选项要从 model 里取**：它的界面结构里只有一个 Label
  （显示当前选中的那个）；要拿**全部**选项得读 `model()`（返回的是
  **TreeModel**，不是 ListModel）。
- ⚠️ **树上挂的控件回调一律抓弱引用**：工作台的控件都挂在 `ui.page` 上，而
  `ui.page` 归 `PanelState` 管——回调里抓 `Rc<PanelState>` 就是
  `PanelState → 页 → 控件 → 回调 → PanelState` 的环，面板永远释放不掉。
  阶段 51 的空态按钮、树工具行都抓 `Rc::downgrade`。（老代码里的
  `wire()` 抓的是强引用，靠 `on_tab_closed` 把 `db_ui` take 出来断开环。）
- ⚠️ **动树的回调里别拿着 `db_ui` 的借用**：`tree.set_filter` / `reload` 会
  **同步**触发「row-expanded」→ 发元数据请求 → 回调里又要借 `db_ui`。
  拿着借用去调就是**双重借用 → panic**。先把 `Rc<DbTree>` 克隆出来、
  借用一结束再动树。
- ⚠️ **`GtkTreeStore` 没法把某一行隐藏起来**：过滤只能"当初就别添加"。
  所以对象类型过滤一变就得**重建整棵树**（`reload()`），代价是展开的层级
  会收起来——这是 GTK 的限制，总比"下拉显示「表」、树上还挂着几百个存储
  过程"骗人强。
- ⚠️ **PG 没有"表最后修改时间"**：PG 只有 analyze 时间（语义不是一回事），
  `pg_class` 里也没有这一项。表统计的"修改时间"在 PG 上**恒空**——
  **宁可空着也不编一个**；MySQL 有 `information_schema.TABLES.UPDATE_TIME`。
- ⚠️ **表统计要"一次查一个模式"**：`meta_object_stats` 是一张表一条 SQL，
  一个模式下两百张表就是两百次往返，慢得没法用。新增的
  `DbMetaReq::TableStats { schema }` 一次查完整个模式。
- ⚠️ **本机跑 `cargo test --lib` 会撞 fcitx5 的 GTK IM 断言**（SIGABRT，
  `fcitximcontext5.cpp:48`）：加 `GTK_IM_MODULE=xim` 跑就正常。
  这跟代码无关，是本机装了 fcitx5 的 GTK 输入法模块。
- ⚠️ **GTK4 没有窗口位置 API**：`gtk_window_move` / `gtk_window_get_position`
  在 GTK4 里**被删了**（GTK3 才有），窗口摆哪儿由**窗口管理器**决定。
  没有 WM 的 Xvfb 下新开的对话框会**顶到屏幕外头**（只露出底下一条）——
  截图要用**更大的虚拟屏**（`xvfb-run -s "-screen 0 1920x1200x24"`）兜住，
  别的办法没有。
- ⚠️ **`gtk::Dialog::with_buttons` 不会自动放内容**：它只给标题栏 + 按钮区，
  中间那块是空的——必须 `dlg.content_area().append(&box_)`，不然窗口里
  **什么都没有**（看着像"点了没反应"）。
- ⚠️ **ComboBoxText 一个都不选显示的是空白**（看着像没内容）：填完选项要
  `set_active(Some(0))` 默认选中第一个。
- ⚠️ **普通 gtk::Label 里 markdown 星号是字面量**：「**整张表**」会原样
  显示成「\*\*整张表\*\*」。要用真控件 + 纯文本。
- ⚠️ **菜单项按钮的 child 是一层 Box**（图标 + 文字 Label）：取文字要
  `menu::item_label(&btn)`（它会钻进去找带 `pp-menu-label` 的那个），
  直接 `child().downcast_ref::<Label>()` 取不到。
- ⚠️ **本机跑 `cargo test --lib` 要带 `env GTK_IM_MODULE=xim XMODIFIERS=@im=fcitx`**：
  只设 `GTK_IM_MODULE` 仍会撞 fcitx5 的 GTK IM 断言（SIGABRT）。
- 💡 **真库 e2e 不用等远端 SSH**：本机装个 PG 服务端
  （`sudo apt-get install -y postgresql-16`）+ 用**本机测试 sshd**
  （`~/.cache/puttyplus-test-sshd`，端口 2233），隧道就把
  `127.0.0.1:5432` 转到**这台机器**上的 PG——e2e 参数用
  `127.0.0.1 2233 fwt 127.0.0.1 5432 postgres postgres mysecretpassword pg`，
  **真库全流程能跑**（远端 182.92.244.132 的 SSH 一直卡在 banner 交换）。
  探针表要自己建（e2e 写死了 `demo_users`，第一行得叫「张三」）。
- ⚠️ **装东西前先请示用户**（2026-10-29 用户定的硬规矩）：apt/dpkg/pip/
  cargo install 一律先问。已写进 AGENTS.md 核心规则第 9 条。
- ⚠️ **「打包」≠「装进系统」**（2026-10-29 用户补充）：`build_deb.sh`
  **打包可以直接做**；但 `sudo dpkg -i` 装进系统**必须先问用户**。
  ⛔ 2026-10-29 交付阶段 54 时打完包直接 `sudo dpkg -i` 装了，被用户当场
  指出「不是说不让你自己安装吗」——当时 AGENTS.md 里"交付前记得重新打包
  安装"那句话写得含糊，被当成了授权。**已改成"重新打包"并写明边界**。
- ⚠️ **进程内 -R 的方向和 -L 相反**（阶段 53，最容易写错的一处）：监听端在
  **远端**、目标在**客户端侧**解析——SSH 协议的 tcpip-forward 请求只带绑定
  地址/端口、**不带目标**，所以 `forwarded-tcpip` 通道里也没有目标地址，得
  客户端自己记一份「哪个绑定对应哪个目标」（`forward::RfwdTable`，查表要
  **按端口兜底**：服务器回报的绑定地址可能是 0.0.0.0）。
- ⚠️ `Handle::tcpip_forward` **只在请求端口为 0 时**回实际端口；端口非 0 时
  回 0 表示成功（别把 0 当成失败）。
- ⚠️ **停用 -R 要发 `cancel_tcpip_forward`**：光把登记表摘掉，服务器那边还
  占着端口。
- ⚠️ **进程内转发 ≠ CLI 的 -L/-R/-D**：命令行参数仍由终端 spawn 的 ssh/plink
  承接（compat 第 3 段逐字节对照，改了必挂）；进程内规则是**另一份**
  （connections.ini 的 `inprocess_forwardings` 键，另一条路径）。
- ⚠️ **`st.fwd_ui.borrow().as_ref()` 写一行会 E0716**（临时值被释放）——必须
  落成具名变量 `let slot = st.fwd_ui.borrow(); let Some(ui) = slot.as_ref()`。
- ⚠️ **GtkPopover 的四个脾气**（阶段 54 顶栏菜单左对齐时全踩了一遍）：
  1. **默认居中对着锚点**，没有左对齐开关——`set_pointing_to` 只换锚点、
     不改对齐方式（给它窄矩形，弹层照样以它为中心）。左对齐只能自己算
     `set_offset`。
  2. **`translate_coordinates` 量到的位置含已施加的偏移**（偏移加在表面
     上）——拿它当"基准位置"会把菜单又拉回去（我第一版就这么错的：
     量到 4 就当成基准，实际基准是 -107，菜单被拽到按钮左边）。
     **校准时必须减掉已施加的偏移**。
  3. **表面在 X11 上不透明**：没画到的像素是**黑块**。所以弹层背景不能写
     `transparent`（圆角外出黑角），`box-shadow` 也不能要（GTK4 画成不透明
     黑块，整张菜单被 34px 黑框包住——用户截图里一直有）。
  4. **只有真实指针事件才会让弹层 map**：`emit_clicked()`、直接 `popup()`
     都弹不出来（`is_visible()` 一直 false，`pop.width()` 一直 0）→
     测弹层位置必须用 xdotool 挪**真指针**点。
- ⚠️ **量几何别靠肉眼估**（阶段 54 的教训）：弹层表面宽、卡片左边缘、
  标题文字左边缘全部用 `xwininfo` + ImageMagick 逐像素量（`convert ...
  -crop ... gray:-` 再按段扫），人眼估能差 10~40px。第一版"错位了"就是
  差在「弹层表面 vs 可见卡片」那 34px 上——靠看截图根本看不出来。
- ⚠️ **GTK4 弹层居中基准的公式**（实测）：基准 = `按钮宽/2 - (表面宽-9)/2`
  （表面 250 → -107；318 → -141，两点都对）。表面宽 = 菜单体自然宽 + 14
  （padding 6×2 + border 1×2）。⚠️ 改了 `theme.rs` 里 `.pp-menu > contents`
  的 padding/border，要同步改 `menu.rs` 的 `CARD_CHROME`。
- ⚠️ **标题文字的排版盒起点 = 按钮左 + `padding-left`**（不是 Label 控件
  的原点——Label 是按钮的 child，`translate_coordinates` 量出来是 0；
  真正决定文字从哪儿开始的是按钮的内边距）。文件/连接/终端/视图/工具
  五个菜单交叉验证过。
- ⚠️ **转发规则「保存到连接配置」原来只认自建条目**（阶段 55 修）：ssh config
  里的别名在连接管理器里是**只读**的（本体在 ~/.ssh/config），没有地方写回
  → 点按钮弹「保存失败」。而用户平时连的就都是别名 → 功能等于废的。
  现在别名的规则存 `connections.ini` 的 `[ssh_alias_forwards]` 段（**键 = 别名**，
  不是会话键——会话键是展开后的 `user@host:port`，两个别名指向同一台机器会撞）。
  ⚠️ **不写 ~/.ssh/config**：那是 ssh 自己的 `LocalForward`（交给 ssh 进程做
  转发），跟本应用的**进程内**转发不是一回事，混着会互相干扰。
- ⚠️ **规则列表为空 ≠ 没有规则**（阶段 55）：列表内容来自 worker 的 Forwards
  事件；**连接没建立时规则加不进去**（worker 回「未连接」），列表于是**一直是
  空的**。这时点「保存」原来会**静默清空**已存的规则 → 现在先弹确认框。
  另：保存成功提示里的条数要数**实际写入的**，别出现「已把 0 条规则写进」。
- ⚠️ **`app.windows()` 列不出普通 `gtk::Dialog`**（阶段 55）：它只列应用窗口
  （GtkApplicationWindow）。要找弹出的对话框用 `gtk::Window::list_toplevels()`。
  另外 `Window → Dialog` 的 downcast 方向要对（父 → 子）。
- ⚠️ **按钮的位置就是它的语义**（阶段 56）：转发窗口的「保存到连接配置」
  贴着**下面的表单**，但它保存的是**上面的列表** → 用户理所当然以为
  「保存 = 存我刚填的那条」，点完才发现存的是另一份东西。修法不是改功能，
  是**把按钮挪到它操作的那个东西旁边**（列表标题行右侧）、改名说清
  「保存列表到连接配置」、tooltip 写明「整份列表、覆盖式」。表单区只管
  「添加」，列表区才管「保存」。⚠ 冒烟要**走真实控件树**判定位置
  （CSS 类 `pp-fwd-list-head`），别断言内部变量——位置本身就是这次要修的东西。
- ⚠️ **GTK4 的 `TreePath` 没有 `parent()`**（GTK3 有）：找父行得用
  `path.indices()` 拿下标，再 `TreePath::from_indices(&idxs[..len-1])` 重建
  父路径。写 `child.parent()` 会编译到 `MountOperation::parent` 上去
  （报"方法名不匹配"）。
- ⚠️ **转发规则有两套机制，树里要标清来源**（阶段 56）：`inproc_forwards`
  跑在应用自己那条 SSH 连接上，`forwards` 交给终端 spawn 的 ssh/plink
  承接——只列一套，用户会以为另一套没生效。所以灰字标「进程内」/「终端」；
  且**终端承接的规则永远不标「运行中」**（它的运行态在 ssh 进程里）。
- ⚠️ **别跨 RefCell 借状态**（阶段 57）：活动状态想读工作台的
  `db_ui.connected`，那要借 `db_ui` 的 RefCell——而工作台的事件处理
  **正借着**它（`on_connected` 全程持着 `st.db_ui.borrow()`）→ 借两次
  直接 panic。解法：在面板状态里存一份**副本** `db_active`，谁改谁写、
  别的模块只读。**凡是"别的模块要读的状态"，就别让它住在别人的 RefCell 里。**
- ⚠️ **树要重画就别轮询**（阶段 57）：重建整树会 `store.clear()`，**把用户
  的选中行冲掉**。轮询刷新（每 2 秒）会让用户点着点着选中就没了 → 只能
  **事件驱动**：钩在会话状态变化 / 转发事件 / 数据库连接状态变化这三处。
- ⚠️ **两棵树要同一份快照**（阶段 57）：各写一份必然走样（一边写「已连接」、
  一边写「未连接」，用户没法信）。新模块 `src/active.rs` 收一份跨窗口快照
  （连接管理器是独立对话框，不属于某个窗口 → `tabs.rs` 维护所有窗口的
  `Weak<Tabs>` 表）。
- ⚠️ **`cargo test --lib` 里的 GTK 用例会静默跳过**（阶段 57 实测）：测试跑在
  libtest 的子线程上，`gtk::init()` 在非主线程起不来 → 菜单/CSS/通知那几个
  用例在无头环境下全是「跳过：本机没有显示服务」。所以**GTK 相关的断言必须
  放进 example 冒烟**（冒烟在主线程跑，`gtk::init()` 成功），别指望单测兜底。
- ⚠️ **窄树里，短标签放前面**（阶段 57 实测）：会话树只有 ~235px 宽，规则描述
  又长（「本地 127.0.0.1:9000 → 10.0.0.1:80」）→ 行尾**必然被截断**。来源
  标签（「进程内」/「终端」/「运行中」）要是跟在后面，就**整个看不见**，
  用户分不清这条规则由哪套机制承接 → 短标签挪到**前面**，被截断的细节留给
  tooltip。⚠ 截图能直接看出这件事：文字看着"在"（被截断了），但标签没了。
- ⚠️ **菜单项只建一次，文字/图标要自己改**（阶段 58）：右键菜单的项在
  `install_row_menu` 里建好反复用，状态变了（「启用转发」↔「停用转发」）只能
  **后改文字**——`gtk::Button` 的 child 是一层 Box，得钻进去找带
  `pp-menu-label` / `pp-menu-icon` 类的那个 Label / Image（`menu::set_item_label`
  / `menu::set_item_icon`）。
- ⚠️ **转发规则的启停要分「配置」和「运行」两层**（阶段 58）：只改配置 →
  用户得断开重连才看得出生效；只改运行 → 下次连接又按老配置起来了。正确做法
  是**两层都改**：配置层写回 connections.ini，运行层给 worker 发
  `ForwardSetEnabled`。⚠ 终端承接的规则（-L/-R/-D 交给 ssh 进程）运行态改不了
  ——别骗用户说「立刻生效」。
- ⚠️ **开关类菜单项用勾选式，别用"文字变来变去"**（阶段 59 实测）：文字在
  「启用转发 / 停用转发」之间切换，用户还得在脑子里翻译一遍才知道现在是开
  还是关；**右侧一个勾**才是一眼可见。勾选标记**要占位**（停用时只是不显示，
  位置留着），否则菜单在开/关两种状态下宽度不同、重新弹出时"跳一下"。
- ⚠️ **勾选菜单项的动作挂 `clicked`，不是 `toggled`**（阶段 59）：程序自己
  `set_active()` 刷新状态时**也会发 `toggled`** → 挂 `toggled` 会在"刷新"
  时误触发动作（用户没点，规则却被翻了）。顶栏菜单用 `pop.connect_map`
  每次弹出刷新勾选态。
- ⚠️ **写死 px 的 `min-height` 会把中文削掉底下一横**（阶段 60 实测，用户
  2026-10-29 报的「左右分屏」的「左」字少一横）：GTK4 的 `GtkLabel` 在**分配
  高度恰好等于**文字自然高度时，最后一行字形正好压在绘制边界上 → 被裁；
  而分配高度来自写死的 px（顶栏 20px / 菜单项 24px）→ 字号一变（中文字体
  fallback 的 ascent/descent 比西文大）自然高度追上来、**余量归零** → 削字。
  ⇒ **高度和字形余量一律用 `em`**（`theme.rs` 里 `.pp-menu-item` /
  `button label` 那几条）。
- ⚠️ **量"字形余量"不能直接看 Label 的 natural**（阶段 60）：给 Label 设了
  `min-height` 之后，GTK 报的 natural 就被**抬高成 min-height 那个值**，
  "分配高度 = natural"恒成立 → 余量无从量起。得另造一个**同字号、且不受
  任何 min-height 规则影响**的对照 Label（放在普通 Box 里、不在 button 内）
  量纯文本高度（`examples/menu_fit_smoke.rs`）。
- ⚠️ **这类问题在开发机上复现不了**（阶段 60）：本机字体的余量恰好够、看着
  好好的，用户办公机换个字体就不够。只能靠"多档字号 + 几何断言"钉死，而且
  **新断言必须验证它真抓得住**（把修复回退一遍，确认测试会失败）。
- ⚠️ **GTK4 的 CSS 没有 `max-width` / `max-height`**（阶段 62 实测）：写了会报
  `Theme parser error: No property named "max-width"`，**整条规则被丢弃**
  （后面几条规则也可能跟着不生效），还会刷 `gtk_css_node_insert_after` 断言。
  ⇒ 想限制宽度只能**让内容自己收窄**：Label 用 `set_max_width_chars()`，
  一排按钮改**竖排**（自然宽 = 最宽的那个按钮）。
- ⚠️ **`wrap` 单独救不了长文字**（阶段 62 实测）：`wrap` 是按**分配到的宽度**
  折行的，而父容器默认按**自然宽**排版 ⇒ 自然宽就是"不折行时那行字的长度"
  ⇒ wrap 永远不触发，文字照样被裁。**必须同时给宽度上限**。
- ⚠️ **窄容器要按真机宽度搭场景再测**（阶段 62）：DB 工作台被挤在 SFTP
  面板右边，主区只剩 296px；开发机上按"整页"搭场景永远看不出来。新冒烟
  `examples/db_narrow_smoke.rs` 按「侧栏 230 + SFTP 340 + 子标签区 626」
  搭，并断言场景本身的宽度（防场景搭歪导致断言空转）。
- ⚠️ **测试阶段不打包、不安装**（用户 2026-10-30 明确要求，为了加快速度）：
  默认动作是「改代码 → 跑测试/冒烟 → 截图 → 提交」。要交付时用户会另行
  通知，那时才 `bash tools/build_deb.sh`；`sudo dpkg -i` 仍然要先问。
