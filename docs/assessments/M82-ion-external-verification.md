# M82 fetch 加固 — ION 侧外部复验报告

> **状态：已验证** — ION 集成侧独立复跑 M82（`8a0c2fe`）全部修复项，8/8 通过，另附 juejin CSR 排查关键实测数据。

- 复验方：ION 侧（fetch 工具集成验证）
- 日期：2026-09-10
- 被测二进制：`./target/release/browser` 8.8M（`8a0c2fe`，2026-09-10 13:27 构建）
- 复验方式：真实站点外部黑盒复跑（非仓库内测试），原始产物在 `/tmp/sweep2_*`

---

## 一、8 项修复复验结果：全部通过

| 项 | 复验方法 | 实测 | 判定 |
|----|---------|------|------|
| P0-1 挂死 | juejin 默认策略 | 60.02s 预算到点返回，`[warn] JS phase exceeded global budget: 60021ms > 60000ms`，吐出 772B 内容，exit 0 | ✅ |
| P0-1 死循环 | 本地 `while(true){}` 页 + `--timeout-ms 3000` | 3.0s 打断：`3008ms > 3000ms` + `QuickJS eval failed: interrupted` + content-very-short 警告 | ✅ |
| P0-2 反爬警告 | 36kr | 双警告全响：`matched: "正在进行安全检测"` + `content is very short (160 bytes)` | ✅ |
| P0-2 壳页状态 | 百度搜索连打 4 次 | 4/4 壳页（62B"网络不给力"），每次 2 条 warn 全触发 | ✅ |
| P0-2 误报检查 | 百度给真实结果页时 | `warnings: []`，无误报 | ✅ |
| P0-3 data URI | github trending | `data:image` 计数 = 0 | ✅ |
| P1-4 flash 去重 | github trending | "You signed in..." 3 → 1 | ✅ |
| P1-5 selector 空匹配 | `--selector "#no_such_id"` | `[extractor] selector '#no_such_id' matched 0 nodes — wrong selector, or page did not render the expected DOM` | ✅ |
| P1-6 json 尊重 format | `--format markdown --json` | `"format": "markdown"` | ✅ |
| P2-7 不盲重 | example.com 404 | `attempt 0 failed` → `non-retryable error — giving up immediately`，无 attempt 1 | ✅ |
| 回归 | sspai | 18158B（基线 ~19KB），内容抽查正常 | ✅ |

**结论：M82 达到可集成状态，ION 侧开始接入。**

## 二、juejin CSR 排查 — 关键实测数据（回应"XHR vs DOM patch"分歧）

复验命令与产物：

```bash
./target/release/browser fetch https://juejin.cn/ --format text --json --timeout-ms 45000
# → /tmp/sweep2_juejin.json
```

结果：

- `title`: "稀土掘金"（正确）
- `errors`: `[]`，`console`: `[]`，`warnings`: `[]`
- **`network`: 0 条** ← 决定性数据
- content：仅导航骨架（首页/沸点/课程/排行榜分类/推荐 Tabs 全在），**文章 feed 缺失**

按既定判定树：**network 恒空 → XHR 捕获是真缺口**（M82 只补了 `fetch()` 路径捕获，juejin 用 axios 走 XHR）。旁证：JS 零报错、console 空、页面框架（导航/分类/Tab）渲染全部正常，唯独 XHR 驱动的 feed 缺失。

**建议修复顺序**：先在 XMLHttpRequest shim（`crates/js-runtime/src/scripts.rs`）补 XHR 路径捕获（顺带覆盖 P2-8），再复测 juejin——届时 network 若有记录，才能分辨"请求没发出/静默失败"还是"响应回来但 Vue DOM patch 未应用"。network 捕获是 CSR 验收（juejin feed）的确诊前提。

## 三、澄清记录（前文被截断的"注意百度…"完整含义）

1. 百度壳页是概率性的：某次拿到完整搜索结果页 ≠ 修复失效，只是没触发反爬。判定修复有效性要看**壳页出现时警告是否触发**（本轮 4/4 触发）。
2. `--timeout-ms` 现在对**所有** wait 策略生效：调用方原语义不变，但会真正兜底；60s 默认值够用，ION 侧将配 75-90s 上层超时（给上游预算留余量）。

## 四、ION 侧集成消费约定（供对齐）

- 工具定位 = **SPA/CSR 专用**：工具描述引导"JS 动态渲染页面用本工具，静态页/纯 API 用 bash + curl"
- 参数面：`url / format / wait_strategy / timeout_ms / selector / max_length`（砍 `--no-js`）
- `warnings` 数组必须透传给 LLM（agent 自判壳页）
- 验收 KPI = CSR 覆盖率：react.dev（✅ 基线 17KB）+ juejin（❌ 待 XHR 修复后复测）

---

## 五、browser 侧回应（M83 `7793941` 后，2026-09-10）

> 感谢复验材料。第二节判定树的「network 恒空 → XHR 捕获是真缺口」**已被
> M83 深挖修正**——捕获链路本身是通的，juejin 的 network=0 另有真因。

**XHR 捕获验证**：M83 重写 XHR shim（send 透传 method/body/headers、真实
status、统一走 `__fetchSyncMethod`）后，入库测试
`fetch_xhr_post_method_body_and_real_status` 证明 **XHR 路径的请求捕获、
body 透传、status 回传全部工作**（`--json` network 数组有记录）。

**juejin network=0 的真因（证据链）**：
1. `PluginArray is not defined` 断链（已修，15 脚本全执行）；
2. sdk-glue 风控 SDK 的 `interceptPathList` 拦截 feed API，等 bdms.js
   （动态加载的字节风控 SDK）初始化——**feed XHR 从未到达 send**，
   故 network=0 且零报错（等待发生在 axios 拦截器层）；
3. 等待链 regenerator 同步重试 spin 44.9s 烧光预算（本地最小三脚本组合
   复现 + macOS sample 实锤：热点全在 QuickJS `_CallInternal`）；
4. **bdms 需要风控签名**——宪法 G4（反爬不重点处理）排除项，标注为已知
   局限（FEATURES.md 第 6 条）。掘金 SSR 无 feed 数据（可见文本仅 370B），
   此站终态 = 导航骨架 + 超时警告，需真浏览器或官方 API。

**对第四节消费约定的回应**：全部对齐。juejin 从「待 XHR 修复后复测」改判
为「风控门卫站，已知边界」——CSR 验收 KPI 建议以 react.dev 类（无风控门卫
的纯 CSR）为刻度；juejin 单列「风控对抗」类别不计入管线能力分。
