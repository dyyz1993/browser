# M78 — 标准兼容性评分基线 + 自优化循环

> **目标（本循环的总目标）**：把 AGENTS.md「浏览器兼容性评分、优化闭环与停止条件」章节
> 落地为可执行的测量工具与修复循环，并**持续迭代直到达成基线分数**。
>
> **本文档是循环的目标锚点**：Agent 每轮循环前重读本文档，确认目标未变。

---

## 一、目标定义（SMART）

| 项 | 目标值 | 说明 |
|----|--------|------|
| **标准兼容性分** | **≥ 0.85**（1.0 制，对应 AGENTS 公式五类加权） | `20% Test262 + 25% HTML/DOM + 15% CSS/Selector + 25% Web API/Network/EventLoop + 15% Storage/Navigation/CDP` |
| 单类下限 | 每类 ≥ 0.50 | 防偏科：不允许一类 1.0 掩盖另一类 0.0 |
| SPA task score | = 1.00 | 既有 35 bug-hunt fixtures + 全量 `cargo test --workspace` 必须保持全绿 |
| 性能护栏 | 二进制大小 & 冷启动耗时任一项恶化 < 10% | 每轮循环记录，回退超 10% 视为该轮失败 |
| **停止条件** | 上述全部满足 → 循环停止 | 对应 AGENTS.md「什么时候可以停止一个阶段」的入门基线（95/100 是阶段完成线，0.85 是本轮循环的"一定的基础"） |

## 二、测量方法（harness 规格）

### 工具

- `tests/compat/run_compat.py` —— 单一入口：
  1. 从 `tests/compat/suites/`（锁定版本）读测试清单（manifest）
  2. 生成 wrapper HTML（test262 → 三段式 script 块哨兵；WPT → 注入 testharness 完成回调采集）
  3. 起本地 HTTP 服务（127.0.0.1，python 标准库）
  4. 并发调 `browser fetch <url> --format html`（release 二进制）
  5. 解析结果 → `tests/compat/results/latest.json` + `report.md`（五类分数 + 加权总分 + 失败频率表）

### 计分

- `PASS=1.0 / FAIL=TIMEOUT=CRASH=NOT_RUN=0 / OUT_OF_SCOPE 不入分母`（对齐 AGENTS.md）
- test262：三段式 wrapper（含 harness includes 内联），哨兵 div `data-outcome`；
  负向测试（frontmatter negative）按期望错误判定
- WPT：只选**纯 `.html` 且引用 testharness.js** 的用例；依赖 `?pipe`/stash/worker/https
  的测试**选例时排除**并在 manifest 登记为 infra-excluded（非我们能力问题）
- CDP：以 `cargo test -p browser-cdp` + 18 个 Puppeteer e2e 为代理（manifest 声明）

### manifest 已声明的 PARTIAL / 限制（预先声明，不许事后降级）

1. test262 只跑单模式（默认 sloppy；`onlyStrict` 用 strict）。双模式跑分留待后续。
2. test262 排除 `flags: [async, module, CanBlockIsFrozen, CanBlockIsNotFrozen, RawJSON]`
   （QuickJS 引擎/宿主集成限制，登记为 excluded）。
3. CSS 布局类只测 selector/匹配（testharness 型）；reftest（视觉对比）为非目标。
4. WPT 选例上限：每类 ≤ 80（锁定清单写入 manifest，保证分母稳定）。

## 三、循环协议（每轮固定六步，对齐 AGENTS.md）

```
while 标准兼容性分 < 0.85 或 任一类 < 0.50:
  1. 读 results/latest.json 失败频率表，选失败数最多的缺口
  2. 归因到 crate（js-runtime / css-engine / dom / html-parser / eventloop / net / storage / navigation / cdp）
  3. 先写最小回归测试（L1/L2，先红）
  4. 修复（纯 JS polyfill 优先，不引 Rust 依赖）
  5. 重跑该类测试 → 全量重评分 → 记录分数变化
  6. 三门禁（fmt/clippy/test）→ commit → 更新 PROGRESS.md
```

**每轮必须记录**：分数变化（前→后）、失败数变化、二进制大小、冷启动耗时。

## 四、验收

```bash
# 一键跑分
python3 tests/compat/run_compat.py            # 输出五类分数 + 总分 + 失败频率表

# 达标判定（harness 自动判定并输出 VERDICT: PASS/FAIL）
python3 tests/compat/run_compat.py --verdict  # 总分≥0.85 且每类≥0.50 且 spa_task=1.0
```

## 决策记录：跨 realm iframe 定性为非目标（2026-08-19，用户决策）

- **决策**：拒绝"每个 iframe 一个独立 JS realm + postMessage 跨上下文路由"的工程投入。
- **理由**：与 North Star（SPA 爬虫渲染）不符——真实 SPA 站点极少依赖 iframe 双向
  通信完成内容渲染；该能力是完整浏览器的架构件而非爬虫必需。对齐 AGENTS.md
  决策原则 1（爬虫价值优先：否）与 3（复杂度门槛：3-5 个十轮成本超收益）。
- **影响面**：~55-110 页 WPT no-results（storage 30 / html_dom 20 / webapi 5）
  定性 OUT_OF_SCOPE，不计入后续修复目标；评分天花板相应锁定在
  ~0.55-0.60 区间（长尾 + 已修复项的守护）。
- **替代策略**：循环转长尾扫荡 + 回归守护（每轮跑评分确认无退化）。

## 五、风险与边界

- WPT 体量大：`--depth 1 --filter=blob:none --sparse` 只拉选例目录的 blob。
- suites/ 目录不入库（体积），manifest 记录 git revision + 文件清单保证可复现
  （`run_compat.py --fetch` 可重建）。
- testharness.js 本身可能在我们引擎上跑不起来 → 这正是循环的第一批燃料（修 DOM API）。
- 不为提分做网站专用 hack；修的都是标准 API 缺口（AGENTS 第九章）。
