# 自研浏览器项目 — 进度日志（活跃）

> 本文档记录每次重要变更，每 commit 后更新。
> **目标/非目标/验收** 见 [docs/GOALS.md](./docs/GOALS.md)（单一事实来源）。
> **能力清单** 见 [docs/FEATURES.md](./docs/FEATURES.md)。
> **里程碑** 见 [docs/ROADMAP.md](./docs/ROADMAP.md)。

---

## 当前状态快照

| 指标 | 值 |
|------|-----|
| HEAD | `c193486`（M38 XHR status/readyState/onreadystatechange） |
| 总 commits | 168 |
| 测试 | 538 passed, 0 clippy warnings |
| Crates | 15 |
| CLI 子命令 | 8 |
| 核心目标 G1（SPA 爬虫）| ✅ 达成（M4） |
| 截图 G2 | ✅ 达成（M12.1） |
| 跨平台 G3 | ✅ 达成 |

---

## 最近变更（倒序）

### M-cls — cls.cn/telegraph SPA 渲染 + 内存自愈护栏 ✅
- M-cls.1 ✅ 内存自愈护栏（子进程 + RLIMIT_AS + 父进程 RSS 监控 kill）
- M-cls.2 ✅ 收紧 boa 运行时限制（loop 250K→40K, stack 4096, recursion 256）
- M-cls.3 ✅ CSR 数据兜底（spa_fallback + host→fetcher 注册表，cls.cn 接 m.cls.cn SSR）
- M-cls.4 ✅ 大纲文档（assessment + plan）
- M-cls.5 ✅ 连带回归修复（navigation fixture location getter）

用户诉求：cls.cn/telegraph 能 SPA 渲染 + 内存"内部自愈"（免得 40GB）+ 统一大纲。

**根因**：cls.cn/telegraph 是 Next.js **CSR**，`__NEXT_DATA__` 只有 `{chooseNav}`
无正文，正文需带签名 XHR（`get_roll_list` errno 10012）。执行 `main.js`(142KB)
在 boa 0.20 里 eval 内存暴涨到 **6.6GB 被 OOM 杀**，渲染永不完成。

**解法**（纵深防御 + 数据双管）：
1. 危险 JS 跑在子进程，父进程轮询 RSS（50ms）超 ~400MB 立即 SIGKILL（self-healing）。
   macOS RLIMIT_AS 不强制，RSS 监控是实际护栏。子进程被杀 → **不重跑 JS**，
   改 `run_js=false` 渲染静态壳 + CSR 兜底。
2. CSR 兜底：发现 `m.cls.cn/telegraph` 是 **SSR**，内嵌 `roll_data[]`（20 条
   brief/ctime/level，无签名）。`spa_fallback` 抠 JSON（自研解析器）注入 body。
3. host→fetcher 注册表，新 CSR 站点加项即可。

**实测**：峰值 RSS **~415MB**（修复前 6.6GB，降 98.4%），wall **~2s**（修复前
57s 被杀），输出 20 条真实电报（日期+等级+正文，非 `-.--` 占位符）。

文档：[docs/assessments/M-cls-spa.md](docs/assessments/M-cls-spa.md)（诊断+决策）、
[docs/plans/M-cls-spa.md](docs/plans/M-cls-spa.md)（步骤+验收）。
工程门禁：fmt ✅ clippy 0 warnings ✅ **682 tests pass** ✅。

### M42-M48 — CDP 协议层 + Puppeteer 端到端全链路打通 ✅
本项目是**通用 SPA 爬虫浏览器 + 兼容 CDP**。M42 起逐步补齐 CDP 协议，
M48 用真实 **Puppeteer 25.1** 验证全链路。

- M42-M43 ✅ CDP server 骨架：WebSocket + JSON-RPC + `/json` 发现端点。
- M44 ✅ `Page` 域：navigate（fetch→parse→render→存 PageState）+ captureScreenshot。
- M45 ✅ `Runtime` 域：evaluate。
- M46 ✅ `DOM` 域：getDocument / getOuterHTML / querySelector（基于 PageState.tree）。
- M47 ✅ `Network` 域：getResponseBody + enable/disable。
- M49 ✅ `Emulation` 域（未知方法统一 ack，puppeteer 握手必需）。
- M50-M53 ✅ `Page` lifecycle 事件 + `Target` 域（flatten session）。

**M48 Puppeteer e2e 实测全链路打通**（`run-all.js` 3/3 scenarios、12/12 断言）：
握手 + newPage + navigate + title（走 isolated world）+ screenshot（PNG）
+ page.evaluate（含 document/location 真实 DOM 访问）+ DOM 取数。
6 个真实修复：targetId 字段、createTarget 事件去重、catch-all 扩展、
executionContextCreated、isolated world worldName（**title 卡点根因**）、
Runtime.callFunctionOn + `eval_in_tree`（**evaluate 接真实 DOM**）。
详见 [docs/assessments/M48-puppeteer-e2e.md](docs/assessments/M48-puppeteer-e2e.md)。
工程门禁：fmt ✅ clippy 0 warnings ✅ cdp 80 tests + js-runtime 135 tests ✅。
**已知边界**：`page.$()`/`$()` 受 boa 0.20 不支持 ES2018+（async generator/
for-await/using）限制；爬虫用 `page.evaluate(() => document.querySelector(...))`
完全够用（dom-via-evaluate.js 4/4 验证）。

### M29-M30 — ADR 文档 + 截图优化 + <a> 蓝色渲染 ✅
- M29.1 ✅ ADR-0003 TLS 后端切换（hyper-rustls → native-tls）
- M29.2 ✅ wss:// TLS 验证（代码层面确认 ws crate 只支持 ws://）
- M29.3 ✅ 截图尺寸优化（--max-height 参数，限高避免超长截图）
- M30 ✅ <a> 标签蓝色渲染（screenshot ANSI + W3C #0000EE）

ADR-0003 记录 M24.2 决策：hyper-rustls 连百度报 AlertReceived(ProtocolVersion)，
但 curl 能秒连。根因是 TLS 客户端兼容性，非网络环境问题。切换到 native-tls 解决。

截图尺寸优化：screenshot.rs render_text_to_png 加 max_height 参数，从顶部截断。
CLI render-file/render-script/render-url 加 --max-height flag。

<a> 蓝色渲染（关键设计）：LayoutBox 加 link: bool，CharBuffer 加 links mask，
render_ascii 双 API（plain + colored）。stdout 保持纯 ASCII（爬虫安全），
screenshot 用 colored（ANSI \x1b[4;34m...\x1b[0m）。screenshot.rs
strip_ansi_and_track_links 解析 ANSI → link span，渲染蓝色 (#0000EE)。
render_html_to_string_inner 返回 (plain, colored) tuple，parse+layout+JS 只跑一次
（避免重复执行 JS 的副作用风险）。

验证：3 render 单元测试 + 4 screenshot ANSI 单元测试 + 百度截图 555K（max-height 2000）。

---

### 2026-06-06 — 文档体系建立（wiki 重构）🟡 进行中
**变更**：建立结构化 wiki 文档体系，解决多文档冗余 + 过时问题。
- ⭐ `docs/GOALS.md`：NORTH STAR（目标/非目标/验收，单一事实来源）
- `docs/FEATURES.md`：能力清单（CLI/CSS/JS 桥/Web API 矩阵）
- `docs/ARCHITECTURE.md`：更新到 M14（含 storage/navigation）
- `docs/CONVENTIONS.md`：工程规范（提交/自研边界/依赖白名单）
- `docs/DIRECTORY.md`：目录结构 + crate 职责
- `docs/TESTING.md`：三级测试分层 + 验收命令
- `docs/ROADMAP.md`：M0-M14 + 前瞻
- `README.md`：重写（快速开始 + SPA 爬虫示例 + 文档导航）
- 删除根目录 `ROADMAP.md` / `ARCHITECTURE.md`（移入 docs/）
- `docs/PLAN.md`：加 superseded 标注（历史归档）

### M28 — JS 全局对象补齐（对齐 W3C/Chrome 基础子集）✅
- M28.1 ✅ navigator_shim.rs（userAgent/platform/language/languages/onLine/cookieEnabled/vendor）
- M28.2 ✅ window_shim.rs（window===globalThis 自引用 + innerWidth/innerHeight/视口数据）
- M28.3 ✅ document_shim.rs（getElementById/querySelector/createElement 包装 __* 桥 + body/head/cookie/title/location）
- M28.4 ✅ screen_shim.rs（width/height/colorDepth/orientation，响应式布局特性检测）
- M28.5 ✅ 文档同步

补齐 SPA 反爬/特性检测最常读的四大全局对象，对齐 W3C/Chrome 基础子集。
所有对象用纯 JS 对象字面量（非 NativeFunction getter），避免跨 eval this 绑定丢失。

设计决策（M28.1 关键教训）：navigator 值全是静态的（不依赖运行时 DOM），
与 location/history（值依赖运行时需方法调 __locationHref()）不同——用 JS 对象字面量
一次构造，是真正的**数据属性**，符合 W3C（`navigator.userAgent` 无括号），
且代码更简单。第一版用 ObjectInitializer::function() 注册，访问返回函数对象
而非字符串（ua=空），修为纯 JS 对象字面量解决。

window = globalThis 自引用（不复制 navigator/location 到 window，经 globalThis 自动可见），
document 方法包装现有 __* 桥，screen 全部静态默认值（无显示器环境）。

验证：4 个 shim 共 29 单元测试，端到端全部验证（navigator userAgent/platform、
window.innerWidth/self===window、document.readyState/body、screen.width/orientation），
百度 JS 错误减少（document/window/navigator 不再未定义）。workspace 481 tests。

---

### M27 — `<a href>` 链接目标渲染（G1 爬虫核心）✅
- M27.1 ✅ construct.rs inject_a_href（`text` → `text (url)`，参照 inject_li_bullet）
- M27.2 ✅ integration_anchor.rs 3 e2e（有文本/空链接/无 href）+ example.com 快照更新
- M27.3 ✅ 文档同步

让爬虫从渲染文本直接看到链接指向，无需解析 DOM。ASCII 模式无颜色概念，
内联 URL 比颜色/下划线对爬虫更直接可用。空链接（无文本子节点）seed 文本
叶子显示 href（爬虫不丢链接）。

验证：受控实验 + layout 23 单元测试 + 3 e2e + example.com 快照（预期更新）。

---

### M26 — 真实站点渲染修复（百度截图可用）✅
- M25.1+M25.2 ✅ 截图字形坐标修复（fontdue metrics 精确测量 + 坐标公式不翻转，乱码→可读）
- M26.1 ✅ textarea 加入非渲染黑名单（修复百度 CSS 泄漏，截图 50MB→1.3MB，降幅 78%）
- M26.2 ✅ 文档同步

让百度等真实站点截图真正可用。两大修复：
1. fontdue 字形坐标：旧版用硬编码 COL_WIDTH/LINE_HEIGHT 且翻转公式导致字形重叠+垂直镜像。
   改用 fontdue Metrics 精确测量（advance_width/ascent/descent）+ 正确坐标公式
   （y_origin = baseline - ymin - height + 1，不翻转）+ 纯数学锁定测试。
2. textarea CSS 泄漏：百度把 CSS 藏在 `<textarea style="display:none">` 做延迟加载，
   旧版把 textarea 当普通元素渲染导致 CSS 泄漏（69% 噪音）。加入非渲染黑名单。

验证：百度截图 2270×57100(50MB) → 2736×10600(1.3MB)，内容干净（百度首页/新闻/hao123 全在）。

---

### M24 — TLS 后端切换（百度可连）✅
- M24.1 ✅ 诊断（curl 能秒连百度，hyper-rustls 报 AlertReceived(ProtocolVersion)）
- M24.2 ✅ net crate hyper-rustls→reqwest(native-tls)，公开 API 不变
- M24.3 ✅ 实测百度可连可截图 + commit + 文档

推翻 memory 旧误判：'真实 HTTPS 连接失败 = 网络环境问题'是错的。真相是
hyper-rustls 的 ring provider 与百度 CDN TLS 不兼容。换 reqwest(native-tls，
curl 同款系统 TLS 库）解决。

---

### M23 — WebSocket（手写 RFC 6455，实时 SPA）✅
- M23.1 ✅ browser-ws crate：RFC 6455 帧编解码纯算法
  （OpCode/Frame/apply_mask/encode_frame/decode_frame，7/16/64-bit 长度，控制帧校验）
- M23.2 ✅ 握手 sha1 + base64 + handshake（纯算法，RFC 6455 §4.2.2 经典向量验证）
- M23.3 ✅ tokio TCP 连接 + 握手 + 帧读写（ws://，XorShift64 PRNG）
- M23.4 ✅ WsManager 多连接管理器（后台线程 + 命令/事件队列，修复 await_holding_lock）
- M23.5 ✅ JS WebSocket 全局对象（纯 JS 原型，修复 Close 不 push 事件 + recv_buf 丢失）
- M23.6 ✅ 文档同步

解决实时 SPA（聊天/推送）的最后一块拼图。**手写 RFC 6455**，不引入 tungstenite
（遵循 GOALS.md 自研优先）。ws:// 全链路验证（echo server e2e）。

真实 bug 修复（2 个，记录教训）：
1. manager.rs WsCmd::Close：发 Close 帧后必须 push Closed 事件，否则 pump
   ws_connection_count 永不归零 → idle 超时。
2. client.rs recv_message：recv_buf 必须是 struct 字段而非局部变量。一次
   socket.read 可能读到多帧 TCP 数据，局部 buf return 时 drop 会丢失后续帧字节。

---

### M22 — 真实图像渲染（<img> → ASCII art）✅
- M22.1 ✅ render crate image 模块（image_to_ascii_from_img + resolve_local_image_src，10 测试）
- M22.2 ✅ cli render_html_to_string post_process_images（跨行扫描，3 e2e）
- M22.3 ✅ 文档同步

解决 M9 的 [IMG: src] 纯文本占位符问题：爬虫/CLI 现在能看到 <img> 图像内容。
本地图像（file:// / 绝对路径 / 相对 cwd）解码成 ASCII art，http(s) URL 不下载
（避免渲染管线引入网络）。M12.3 的 image_to_ascii 提升到 render crate。
post_process_images 跨行扫描（src 可能因折行被拆，] 可能被 width 截断丢失）。

---

### M21 — Cookie 持久化（跨进程保留登录态）✅
- M21.1 ✅ cookie crate serialize/deserialize（TSV，不引入 serde，10 测试）
- M21.2 ✅ cli --cookie-file flag + CookieFileGuard（RAII，持有 jar owned clone + sync_jar）

解决"爬虫重启要重新登录"痛点：启动时 load cookie 文件，退出时 save。
TSV 纯文本格式（自研优先，不引入 serde）。
CookieFileGuard 持有 owned jar clone（避开 TreeGuard::drop 清空 thread-local）。

---

### M20 — fetch 增强（POST/PUT/DELETE + 真实 status code）✅
- M20.1+M20.2 ✅ net 通用 request（POST/PUT/DELETE wiremock 测试）
- M20.3 ✅ fetch(url, {method, body, headers}) JS 端 + request_full（真实 status 201）
- M20.4 ✅ 文档同步

fetch 现支持完整 HTTP method 集合：表单提交 / REST API 调用场景。
request_full 返回真实 status code（不再被 is_success() 吞掉 201/204）。
POST/PUT/DELETE 自动带 cookie jar（复用 M15）+ networkidle 计数（复用 M18）。

---

### M19 — 标准 fetch API（现代 SPA 核心）✅
- M19.1 ✅ __fetchSync 桥 + fetch_shim（Response 对象 + .text()/.json()）
- M19.2 ✅ e2e（fetch.then(text/json/catch)，5 tests）
- M19.3 ✅ 文档同步 + 真实 SPA 手动验证（res.json 用户列表渲染）

标准 fetch：fetch(url) → Promise<Response> → res.text()/json() → Promise。
React/Vue/Next.js 等 SPA 核心数据获取模式。复用 M16 Promise + M15 cookie jar +
M18 networkidle。网络错误 reject TypeError（标准行为）。

---

### M18 — networkidle 算法（爬虫渲染完整性信号）✅
- M18.1 ✅ pending_requests 计数器 + is_network_idle（5 e2e）
- M18.2 ✅ cli --assert-network-idle flag（render-script / render-url，2 e2e）

networkidle = pending_timers==0 && pending_requests==0。
爬虫用此信号判断 SPA 是否渲染完（Playwright/Puppeteer 同款能力）。
fetch_sync 用 RequestGuard（Drop）确保计数器即使失败也 -1。

---

### M17 — XMLHttpRequest（老 SPA 依赖）✅
- M17.1 ✅ XhrState + thread-local + 4 桥（__xhrCreate/Open/Send/GetResponseText）
- M17.2 ✅ XMLHttpRequest 全局构造器（纯 JS 原型，闭包捕获 self）
- M17.3 ✅ e2e（wiremock 真实 fetch + onload 渲染，3 tests）
- M17.4 ✅ 文档同步 + 真实 SPA 手动验证（Users 表格渲染）

异步 XHR 支持：new XMLHttpRequest() / open / send / onload / responseText。
设计：同步 fetch（复用 fetch_sync + cookie jar）+ setTimeout(0) 异步触发 onload。
教训：boa native fn 跨 eval 传 this 不可靠，纯 JS 原型更稳。

---

### M16 — 异步 JS（setTimeout + Promise，用 boa 自研）✅
- M16.0 ✅ ADR-0002 决策（调研推翻 M7.3 defer，用 boa 而非 deno_core）
- M16.1 ✅ browser-eventloop crate（TimerWheel 纯算法，14 tests）
- M16.2 ✅ setTimeout/clearTimeout 桥 + 全局名（thread-local + drain API）
- M16.3 ✅ event loop 接入 run_scripts（pump_event_loop，6 集成测试）
- M16.4 ✅ Promise.then 触发（ctx.run_jobs，5 集成测试）
- M16.5 ✅ e2e fixture（timer-spa.html，setTimeout+Promise 驱动 SPA 渲染）

异步 JS 支持：setTimeout(fn, 0) / Promise.resolve().then() / 递归 setTimeout
链 / Promise 链 + microtask 与 macrotask 交错。无需 300MB V8。

---

### M15 — Cookie jar（跨请求会话保持）✅
- M15.1 ✅ browser-cookie crate（RFC 6265 子集，21 tests）
- M15.2 ✅ net::get_with_headers（带 Cookie 头 + 返回 Set-Cookie）
- M15.3 ✅ JS fetch_sync 接入 jar（主线程读写，新线程传 String）
- M15.4 ✅ cli get/render-url/open 主请求共享 jar（fetch_with_jar）
- M15.5 ✅ e2e（cookie jar 跨请求会话保持，2 tests）

SPA 爬虫增强：解决百度等登录态反爬。主请求设的 cookie → JS fetch 带上。

---

### M14 — Navigation（history / location）✅
- M14.1 ✅ browser-navigation crate（HistoryStack + Location 解析，11 tests）
- M14.2 ✅ `__history*` / `__location*` bridges + install_navigation
- M14.3 ✅ history/location JS 对象 shim + 接入 run_scripts（8 tests）
- M14.4 ✅ e2e fixture navigation-spa.html（5 tests）
- M14.5 ✅ PROGRESS/memory 同步

### M13 — Web Storage（localStorage / sessionStorage）✅
- M13.1 ✅ browser-storage crate（`Rc<RefCell<HashMap>>`，6 API，8 tests）
- M13.2 ✅ `__storage*` bridges + install_storage
- M13.3 ✅ localStorage/sessionStorage JS 对象 shim（6 tests）
- M14.4 ✅ e2e fixture storage-spa.html（2 tests）

### M12 — 截图 + 图像 ASCII ✅
- M12.1 ✅ PNG screenshot（`--screenshot` flag，fontdue + png）
- M12.3 ✅ image-ascii 子命令（image crate + 10 级灰阶 ramp）

### M11 — Bug 修复 ✅
- vh/vw/vmin/vmax → Zero / rem → 16px / pt → 4/3 px

### M10 — 性能优化 ✅
- LayoutCache（get_or_compute）+ DirtyTracker（mark/mark_subtree/is_dirty/clear）

### M9 — 图片占位符 ✅
- `[IMG: src]` 占位符注入（construct.rs build_box）

### M8 — 表单交互 ✅
- `__getValue` / `__setValue` / `__click` / `__submit` 橋 + e2e

### M7 — 渲染质量 + 交互 ✅
- M7.1 CSS margin/padding（真实解析 + UA defaults + collapsing）
- M7.2 完整 DOM API（`__createEl`/`__appendChild`/`__qs`/...）
- M7.4 真实字体（fontdue + DejaVuSans 739KB + 中文）
- M7.5 URL 栏 + 键盘输入 + 滚动
- M7.3 异步 JS 🟡 defer（boa 0.20 JsObject::call 私有）

### M0-M6 — 核心管线 ✅
- M0 骨架 + CI / M1 HTTPS+DOM / M2 CSS+布局+ASCII 渲染
- M3 JS 执行（boa）/ M4 SPA 渲染（**项目目标达成**）
- M5 GUI 窗口 / M6 渲染质量修复 + 跨平台 CI

---

## 前瞻（按爬虫价值排序，见 GOALS.md 决策原则）

1. **切 deno_core** ⭐⭐⭐⭐⭐ — 解锁 setTimeout/Promise/async-await
2. **Cookie jar** ⭐⭐⭐⭐ — 跨请求会话，解决登录态反爬
3. **XMLHttpRequest** ⭐⭐⭐ — 老 SPA 依赖
4. **真实图像渲染进 GUI** ⭐⭐
5. **WebSocket** ⭐⭐
6. **networkidle 算法** ⭐⭐⭐
7. **资源拦截器** ⭐⭐⭐（省 60% 内存）

---

## 文档维护规则

- **每 commit 后**：更新本文件"最近变更"
- **目标变更**：改 docs/GOALS.md + 写 ADR
- **新增功能**：改 docs/FEATURES.md
- **架构变化**：改 docs/ARCHITECTURE.md
- **里程碑完成**：改 docs/ROADMAP.md + 写 docs/postmortems/M<n>.md
- **冲突优先级**：GOALS > FEATURES > ARCHITECTURE > 其他
