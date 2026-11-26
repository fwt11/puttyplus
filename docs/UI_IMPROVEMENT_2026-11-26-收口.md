# UI 提升改造 · 收口对照（2026-11-26 → 2026-11-27）

> **输入**：[`UI_REVIEW_2026-11-26-全界面.md`](UI_REVIEW_2026-11-26-全界面.md)
> （审核）＋ [`UI_IMPROVEMENT_PLAN_2026-11-26.md`](UI_IMPROVEMENT_PLAN_2026-11-26.md)
> （改造计划）。
> **执行**：阶段 174–176，共 **15 项**（UI-00 ～ UI-14）＋ 1 个计划外的
> 真缺陷修复（`build_ddl` 的主键约束名）。
> **结论**：**P0 全关**、**P1 全关**、**P2 全做**；没有遗留的 P0/P1。

---

## 1. 逐项对照（审核问题 → 改动 → 证据）

| 审核项 | 问题 | 改动 | 提交 | 证据 |
|---|---|---|---|---|
| **P0-1** | Docker 列表列宽随内容伸缩 ⇒ 列被挤没 | 列宽**分四档**（宽≥1170 / 中≥940 / 窄≥530 / 极窄<530），每档自带 min_width；改听 `hadjustment.page-size`（`notify::width` 在 GTK4 根本不触发） | `87bd908` | 4 条新单测 |
| **P0-2** | 实心按钮 / 错误色对比度不够 | 按钮与错误色按**实际颜色**算对比度并钉住 | `05334ed` | `contrast_smoke` |
| **P1-1** | 表单窗口高度一刀切 ⇒ 短页空、长页被裁 | **每页各自套 `ScrolledWindow`**（GTK4 的 `GtkNotebook` 最小高度 = 最高页 ⇒ 外套滚动容器缩不下去，**notebook 最小高度从 567 降到 85**）；高度跟当前页走 | `b57f8ef` | `dlgsize_smoke` |
| **P1-2** | SFTP 失败只弹一次弹窗，没了 | 持久失败态：面板头常驻「重试」「详情」 | `f0144f9` | `sftp_fail_state_smoke` |
| **P1-3** | 数据库空态是品牌区，没动作 | 品牌区**整个删掉**；未连接态主 CTA「新建数据库连接…」**走既有流程**；三种状态各说各的 | `d241761` | `db_workspace_empty_smoke` |
| **P1-4** | Docker 文件错误只给原文 | 错误**分类**（7 类）＋ 说人话 ＋ 原文进 tooltip | `e24f004` | 4 条新单测 |
| **P1-5** | 回放窗重复 Separator | 拆成 `sep_top` / `sep_bottom` | `4fe6a22` | `replay_win_smoke` |
| **P2-1** | 欢迎页能力卡「看着能点、点了没反应」 | 能力区改成**说明卡**（无底色/边框/hover ＋ 小标题「这个应用能干什么」）；内容列限宽 ＋ 垂直重心稳定 | `eb2ae4a` | `welcome_semantics_smoke` |
| **P2-2** | 表面/间距/节奏不统一 | 浅色主题**补第三层表面**（面板与卡片原本同色）；补「输入面」「选中面」令牌；五档间距 `theme::space::*` | `ba7b423` | 2 条主题新测 |
| **P2-3** | ER 图无关系时一片空白 | 三种状态各说各的 ＋ **主 CTA 有可见结果** ＋ 单表带字段摘要 | `da473be` | `er_state_smoke` |
| **P2-4** | 通知平铺、队列失败项没法处理 | 通知**按级别分组**（文字＋图标）；队列**摘要/筛选/失败动作**；「重新传输」独立且**不隐式覆盖** | `7e84c47` | `queue_group_smoke` |
| **P2-5** | 目录镜像时间列表头被截断 | 每列语义 min_width ＋ 默认宽 960 ＋ 完整值进 tooltip | `f545ab7` | `mirror_width_smoke` |
| **P2-6** | 快速命令空态是列表里的一行（会被选中） | 空态**移出 ListBox**（GTK4 会把非 row 子控件包成行） | `c33dd16` | `quickcmd_empty_smoke` |
| **P0 前置** | 缺统一的几何/主题/GTK 健康口径 | `uitest` 公共口径 ＋ `ui_baseline_smoke`（三主题×三尺寸 = 36 条） | `bfcb3ce` | 18 条反向验证变红 |
| **收口** | 性能回归 / 文档 / CI | `perf_regress_smoke`（500 行刷新 8ms、行数不变）；CI 挂 6 个新冒烟；`UI_CHECKLIST.md` §9–§11 | 本提交 | 反向验证变红 |
| **计划外** | `build_ddl` 的主键约束名是**猜的** | 用**服务器给的真名**；MySQL 写 `PRIMARY KEY (…)`（不带 CONSTRAINT 子句） | `70b60a3` | 2 条新单测 |

---

## 2. 前后对照（几何与状态，可复现）

| 场景 | 改前 | 改后 | 怎么复现 |
|---|---|---|---|
| Docker 列表（窄面板） | 列被挤没，看不到「资源」 | **四档分档**，中档仍保「资源」列 | `docker_actioncol_smoke` |
| 连接管理器（长页「代理」） | 底部被裁 | 每页各自滚 | `dlgsize_smoke` |
| SFTP 失败后 | 弹窗关掉就没了 | 面板头常驻「重试」「详情」 | `sftp_fail_state_smoke` |
| 数据库工作台（未连接） | 大图标 ＋ 产品名 ＋「数据库工作台」 | 标题 ＋ 解释 ＋ **主 CTA** | `db_workspace_empty_smoke` |
| 欢迎页能力区 | 三张**有边框的卡**（看着能点） | 三块**说明** ＋「这个应用能干什么」 | `welcome_semantics_smoke` |
| 浅色主题表面 | 面板与卡片**同色**（`#ffffff` / `#ffffff`） | `#eceef1` / `#f7f8fa` / `#ffffff` 三层 | `cargo test --lib theme::` |
| 通知中心 | 一条平铺的时间列表 | **失败 → 警告 → 信息 → 成功**分组 | `queue_group_smoke` |
| 传输队列 | 只有一列列表 | 顶部**摘要** ＋ **全部/进行中/失败**筛选 | `queue_group_smoke` |
| ER 图（单表） | 一片空白画布 | 标题 ＋ **字段摘要** ＋ 两个 CTA | `er_state_smoke` |

**截图**（人工证据，不做像素 diff）：
```bash
xvfb-run -a cargo run --example design_gallery      # 三主题令牌画廊
xvfb-run -a cargo run --example ui_baseline_smoke   # 三主题 × 三尺寸
```

---

## 3. 未解决差异清单（**已知限制 / 有意不做**）

| # | 项 | 为什么没做 | 影响 | 后续 |
|---|---|---|---|---|
| 1 | **GTK4 没有「最大宽度」机制** | 实测 `ScrolledWindow.max-content-width` **不生效**（内容自然宽 1600 时照样分到 1600；四种策略组合全试过）⇒ 只能用「最小宽 ＋ 内容自身宽度上限」 | 欢迎页等页面的内容列宽度靠**构造**封顶，不是靠 CSS 硬限 | 将来 GTK 支持真正的 max-width 时可换 |
| 2 | **「查看表」/「新建关系」这类跨页动作没做** | 审核计划写的是「**等**真实动作」；ER 图里**没有**「新建关系」的现成实现 ⇒ 硬做会引入第二套建外键流程 | ER 图空态只给了「打开画布」「查看表数据」 | 等产品侧决定「新建关系」的交互 |
| 3 | **通知的「查看明细」仍开汇总窗** | 计划说「打开队列并定位失败」；但**通知回调拿不到 `PanelState`**（`report_batch` 的签名里没有）⇒ 改成「汇总窗里有失败清单」，队列侧用筛选实现「定位失败」 | 用户要多点一步 | 下一轮把 `report_batch` 改成收 `st` |
| 4 | **「重新传输」只覆盖文件传输** | 建目录 / 删除**没有可重放的原语** ⇒ 菜单项灰掉并说明原因 | 合理 | — |
| 5 | **高对比主题仍无第三层表面** | 高对比的取舍就是「黑就是黑，靠边框分」（已有测试钉住这个决定） | 卡片靠 1px 白边区分 | — |
| 6 | **GTK4 的 CSS 节点插入断言**（`gtk_css_node_insert_after`） | GTK 自身的问题（审核文档 §9.2 已记为测试侧）⇒ 列入 `uitest::ENV_NOISE` 白名单（**逐条写明理由**） | 基线冒烟里被滤掉 | 上游 GTK 修复 |
| 7 | **Windows 版的人工复验** | 本轮在 Linux 上做；Windows 需在 Windows 机器上按 `docs/跨平台-构建说明.md` §10 复验 | Windows 侧的视觉收口未验证 | 按跨平台清单人工过一遍 |

---

## 4. 总验收清单（对照计划 §UI-14）

- [x] 审核报告中的 **P0 全部关闭**（P0-1、P0-2 ＋ P0 前置 UI-00）。
- [x] **P1 全部关闭**（P1-1 ～ P1-5）。
- [x] **P2 全部完成**（P2-1 ～ P2-6）。
- [x] **没有新增 GTK critical** ⇒ `ui_baseline_smoke` 在三主题 × 三尺寸下断言（36 条全绿）。
- [x] **没有整表重画回归** ⇒ `perf_regress_smoke`（500 行刷新 8ms、行数不变）。
- [x] **默认/窄窗口、三主题、键盘路径全部通过** ⇒ `ui_baseline_smoke`（1200×780 / 1024×700 / 1366×768 × 三主题）。
- [x] **测试未写入用户真实配置** ⇒ `uitest::isolate_configs` ＋ `cleanup_temp_configs`（§10.4）。
- [x] **未提交真实主机、IP、密码、内网域名** ⇒ 新冒烟里的地址全是 `127.0.0.1` / `10.0.0.9`（RFC1918 占位）/ `/tmp`。
- [x] **未打包、未安装、未发布** ⇒ 本轮只出 `release/puttyplus`（本地可执行文件），**没有**打 deb、没有安装、没有推 release。

---

## 5. 怎么复现这一轮

```bash
export ZIG=$HOME/.cache/puttyplus-zig/bin/zig

cargo test --lib                      # 单测（含 i18n 守门与主题令牌）
cargo check --examples                # 全部 example 都能编（签名漂移本地就红）

# 新冒烟（都不连服务器 ⇒ 本机与 CI 都跑得动）
for ex in ui_baseline_smoke welcome_semantics_smoke er_state_smoke \
          db_workspace_empty_smoke queue_group_smoke perf_regress_smoke; do
  cargo build --example "$ex" && xvfb-run -a "./target/debug/examples/$ex"
done

# 老冒烟（回归）
for ex in welcome_cards_smoke db_empty_smoke replay_win_smoke \
          sftp_fail_state_smoke quickcmd_empty_smoke mirror_width_smoke \
          docker_actioncol_smoke dlgsize_smoke settingsui_smoke; do
  cargo build --example "$ex" && xvfb-run -a "./target/debug/examples/$ex"
done

# 命令行契约（73 条黄金基准）
bash tools/cli_contract_check.sh
```
