# M93 评估：Anubis PoW 挑战闭环（xcancel.com/nim_lang 换源实测）

> 日期：2026-09-12 · 引擎：QuickJS（release） · 网络：Clash 代理 127.0.0.1:7890
> 结论：**端到端全通**——`anubis.techaro.lol`（Anubis 官方演示站）PoW 解开
> 并渲染出真实内容；cookie 复用二跑 **1.9s、0 次 pass-challenge** 免挑战
> 直达。tiekoetter 链路同样全通（PoW/302/cookie 铁证），仅最终正文被
> 代理共享出口 IP 的 429 长窗口限流挡住（curl 带 cookie 同样 429）。

## 1. 任务与换源决策

用户目标：爬 `https://xcancel.com/nim_lang`（Nitter 实例，Nim 语言官推镜像）。

### xcancel 本体（不可行，宪法边界）

| 层 | 实测 | 结论 |
|----|------|------|
| 网络 | DNS 污染 → Meta IP 段（31.13.95.33），直连超时 | 必须 `--proxy`（M70.7 ✅） |
| JS | 5 脚本 0 报错，M82 反爬检测主动告警 | 引擎无恙 |
| 反爬 | 自研 antibot（843KB 重混淆）需 **WASM + WebCrypto**；真 Chrome headless-shell 148 ARM 也报 "Automated verification failed" | 明确拒绝无头自动化的对抗型反爬 → 宪法原则 4 非目标，不对抗 |
| RSS | `rss.xcancel.com` 要邮件白名单 RSS 阅读器 | 无通用路径 |

### 换源矩阵（curl 预扫 → 我们的浏览器实测）

| 实例 | 状态 | 保护 |
|------|------|------|
| nitter.net / poast.org / salastil / datura | 000（死） | — |
| lightbrd.com / nitter.space | 403（WAF） | — |
| nitter.adminforge.de | 301 → adminforge.de 主站 404 | Nitter 已下线 |
| nitter.privacyredirect.com | 200 → 后不可达 | Anubis |
| **nitter.tiekoetter.com** | **200** | **Anubis v1.28.0-pre，fast 算法 difficulty=4** |

## 2. Anubis 为什么值得做（与 xcancel 的本质区别）

Anubis 的设计哲学（作者 Xe Iaso）：**不检测自动化**，只要求客户端愿意
烧 CPU 解 PoW（Hashcash 风格）。headless 浏览器解题即放行。所需能力全部
是标准 Web API，不是指纹伪造：

1. `window.Worker`（功能门禁硬检查）+ `postMessage`/`onmessage`
2. worker 内纯 JS sha256（`crypto.subtle` 缺失时自动 fallback——aws-sdk
   移植的自包含实现）
3. `navigator.cookieEnabled`（门禁）
4. 解完 `location.replace(pass-challenge?id&response&nonce&redir&elapsedTime)`
   → 服务端 Set-Cookie（7 天 JWT）+ 302 回原页

**爬虫价值**：Anubis 正在 FOSS/fediverse 圈快速铺开（Codeberg、
大量 Nitter/Forgejo 实例）。补齐 = 一类站点从"不可爬"变"可爬"，
完全符合决策原则 1。WASM/argon2id 变体（`hashx`/`argon2id` 算法）不支持，
但绝大多数实例默认 `fast`。

## 3. 实现（M93，全部 QuickJS 路径）

```
页面 JS                     js-runtime (Rust)                    net
──────                      ─────────────────                    ────
new Worker(url)       ──►   QUICKJS_WORKER_SHIM（JS 类）
w.postMessage(msg)    ──►   __workerRun(url, msgJson)
                            ├ fetch_sync(worker 源码, 走缓存/cookie)
                            ├ 独立 Runtime+Context（30s 中断）
                            │   env: self/postMessage 捕获/addEventListener/
                            │        TextEncoder/UA/isSecureContext=false
                            ├ eval 源码(sloppy) → dispatch → drain microtask
                            └ outbox JSON ──► onmessage({data})
location.replace(302 URL) ─► __setLocHref 检测文档导航
                            └ PENDING_NAVIGATION 队列
（JS 阶段结束，TreeGuard 存活）
                      ──►   fetch_navigation_document
                            ├ new_no_redirect 客户端逐跳跟 3xx
                            ├ 每跳 Set-Cookie → cookie jar → 下一跳携带
                            └ 返回最终 HTML
                      ──►   换 DOM 树 + 全新引擎重跑（≤5 跳）
```

### 顺手修的通用 bug

- **URL polyfill searchParams 回写**：`new URL(x).searchParams.set()` 后
  href 丢 query（构造时快照）。Anubis `v()` 构造 pass-challenge URL 直接
  无效。修复：`__uspSyncOwner`（set/append/delete/sort 同步 owner.search/href）。
- **navigator.cookieEnabled**：QuickJS shim 缺失（boa 侧有），Anubis 门禁拒绝。
- **2 个 M83 存量回归**（HEAD 就红）：`__fetchSetBody/AppendBody` 非 2xx
  恢复 `[js-fetch]` 日志；`fetch_error_url` 测试改用真连不上的端口
  （404 按 M83 语义应 resolve）。

## 4. 实测证据（tiekoetter，BROWSER_TRACE_NAV=1）

```
[serve] scripts: 13210ms          ← Worker PoW（difficulty=4，纯 JS sha256；
                                     release 1-13s 波动，nonce 运气）
[nav] M93 document navigation hop 1/5:
  pass-challenge?id=01a09176-...&response=0000e7e641c8986dc87...&nonce=80479
  &redir=https%3A%2F%2Fnitter.tiekoetter.com%2Fnim_lang&elapsedTime=10920
[nav-trace] GET pass-challenge?... -> 302
  loc=https://nitter.tiekoetter.com/nim_lang
  set-cookie=[tiekoetter.com-auth-ae0e03ee=eyJhbGciOiJFZERTQSIs...  ← 7 天 JWT
              .dfZP5C9doOK...; Path=/; Domain=...; Expires=Fri, 18 Sep 2026 ...]
[nav-trace] >> GET https://nitter.tiekoetter.com/nim_lang
  cookie=[...verification...; tiekoetter.com-auth-ae0e03ee=eyJ...]  ← ✅ 携带
[nav-trace] GET /nim_lang -> 429                                   ← IP 限流
```

**PoW 哈希 `0000e7e6...` 前导零正确、302+JWT 签发、cookie 逐跳传递——
全链路铁证**。429 与实现无关：curl 携带同 cookie 也 429；两个域名
DNS 双重污染（xcancel→Meta 段、tiekoetter→Dropbox 段）无直连退路；
代理出口为共享 IP，此前多轮测试触发长窗口限流。

## 4b. 端到端收官：anubis.techaro.lol（Anubis 官方演示站，真实内容）

tiekoetter 被 429 挡住后，换 Anubis **官方演示站**验证端到端（独立服务器，
对我们的 IP 无限流前科）：

```bash
# 第 1 次：解题 + 存 cookie
$ browser fetch https://anubis.techaro.lol/ --format text --proxy ... \
    --timeout-ms 150000 --cookie-file /tmp/anubis_cookies.txt
# 13.2s（PoW ~12s）→ 500B 真实内容：
#   "Easy to Use / Anubis sits in the background and weighs the risk..."
#   "Lightweight / Block the scrapers ..."

# 第 2 次：cookie 复用
$ 同命令
# 1.95s，pass-challenge 次数 = 0（完全没碰 PoW），内容直达 ✅
```

运营语义：**首次付 PoW 成本，cookie 有效期（7 天）内免挑战直达**——
这正是真实爬虫对 Anubis 站点的可持续姿势。

**M93.1 修复**：PoW 有运气方差（difficulty 4/5 的 unlucky draw 可达 30s+，
实测一次 35.5s 触发 "all workers failed"）。worker 中断硬上限 30s→120s，
让 CLI `--timeout-ms`（全局 deadline）成为实际约束。

## 5. 测试与门禁

- `crates/cli/tests/integration_worker_nav.rs`（5 项全绿）：
  Worker 消息往返 / location.replace 换树重渲染 / 302+Set-Cookie 链
  （/protected mock 只响应带 cookie 的请求）/ searchParams 回写 /
  pushState 不误触发 re-fetch
- 门禁：fmt ✅ / clippy -D warnings ✅ / `cargo test --workspace`
  **1004 passed, 0 failed**

## 6. 局限与后续

| 局限 | 影响 | 处置 |
|------|------|------|
| Worker 同步阻塞 | PoW 期间主线程忙 | 爬虫可接受；难例见 30s 上限 |
| difficulty > 8 的实例 | 纯 JS 可能超 30s 中断 | 标注需 Chrome 或调大预算 |
| hashx/argon2id 算法 | 需 WASM，不支持 | 少数实例；宪法 G4 不引 WASM 运行时 |
| localStorage 跨导航不保留 | 每页新 storage | Anubis 不依赖；后续可按 origin 复用 |
| 代理 IP 限流 | 共享出口 429 | `--cookie-file` 持久化 7 天 auth cookie 后续运行免 PoW 直达 |

## 7. 终局定性（11:53，shot3 后）

| 实验 | 结果 | 结论 |
|------|------|------|
| shot2（带 trace） | PoW 2.8s 解开 + 302 + 全新 JWT（9/19 过期）+ 重定向携带 JWT → **仍 429**，退避 3 次全 429 | tiekoetter 应用层有无视 Anubis 通行证的按 IP 配额 |
| shot3（53 分钟零流量 + 新鲜 JWT 进门） | 入口即 429（CLI 5s×2 退避也失败）；同刻 curl=200 | **客户端指纹级封禁**：对 reqwest+native-tls 的指纹长期拉黑，curl/Chrome 放行 |
| xcancel 入口对照 | 我们 fetch 正常拿到挑战页（9 脚本执行） | xcancel 不封我们指纹——**用户 Chrome 会话 cookie 复用路线完全可行**（M93.6 已证机制） |

**工程终态**：引擎/协议/重试/缓存/存储全就绪（M93~M93.6，1014 测试全绿），
Anubis 类站点端到端已证（techaro 官方站真实内容 + 1.9s 免挑战复用）。
tiekoetter 单站的指纹封禁属于 TLS 指纹对抗区（宪法原则 4 非目标 + ADR-0003
native-tls 选型），不做 chromium-impersonation 类库（依赖白名单 + G3 双违规）。
获取 nim_lang 推文的推荐路径：**xcancel + 用户 Chrome 会话 cookie 导出**。

## 8. M93.7：xcancel antibot 深度破译（双引擎差分定位）

### 逆向结论（证据链）
1. **cap.min.js = 开源 @cap.js PoW 库**（WASM URL 指向 jsdelivr @cap.js/wasm@0.0.7，
   xcancel 用 CAP_CUSTOM_WASM_URL 自托管）。代码明示：**WASM 缺失自动降级纯 JS
   solver**（"WebAssembly unavailable, using JS fallback solver"）——WASM 非必需。
2. 真实缺口三个，全部标准 Web API，M93.7 已全部实现并本地验证：
   `crypto.subtle.digest('SHA-256')`（worker 内）、`new Worker(blob:URL)`
   （cap 用 Blob+createObjectURL 构造 worker）、Blob/TextEncoder 的 spec 语义。
3. 843KB js-challenge.js = 全混淆 VM 编排器（字符串表加密，grep 不可见）。

### 双引擎差分（同一插桩页面，本地重放服务器）
| 步骤 | Chrome | 我们 | 判定 |
|------|:---:|:---:|------|
| 5+ 脚本 eval | ✅ | ✅ | — |
| WASM 形状桩（instantiate 拒绝）| ✅ | ✅ | 触发降级路径 |
| cap 预载 fetch wasm 文件 | ✅ | ✅ | M93.7 后解锁 |
| `__antibotStarted=true` | ✅ | ✅ | **编排器确实在跑** |
| **fetch /antibot/api/cap/challenge** | ✅ | ❌ | **唯一分歧点** |
| blob Worker PoW | ✅ | （本地已验证能力） | — |
| POST /antibot/api/verify | ✅ | ❌ | 下游 |

### 剩余卡点定性（黑盒已穷尽）
编排器在"标记 started"与"发起挑战 fetch"之间静默停滞：零 JS 错误、零
unhandledrejection、零 API 调用。Cap.prototype 两引擎一致（仅 constructor），
排除 cap 半执行。可能根因（按概率排序）：
1. **某 Promise 永不 settle**——编排器 await 一个我们 shim 未实现/未 resolve
   的异步 API（无 rejection 故不可见）
2. **反 VM 计时/一致性自检**——混淆 VM 常见手法：performance.now 精度、
   Date 单调性、Error.stack 形状、Function.prototype.toString 自检不过则
   内部死等
3. **指纹采集链静默异常**——canvas/WebGL/字体等采集在我们引擎返回意外形状，
   被 VM 内部 catch 后放弃

### 已沉淀能力（与卡点无关，永久有效）
crypto.subtle.digest（SHA-256）、blob Worker、Blob 真内容、TextEncoder 真
UTF-8——任何用 WebCrypto/Worker 的正常站点（cap.js 同款 PoW 已大量部署）直接受益。
本地三链路铁证：/tmp/cap_test.html 模式（SHA-256 标准向量 + blob Worker +
worker 内 PoW 解题复验）。

## 9. M93.8/M93.9：xcancel 持续攻坚——readyState 根因修复 + 八假设排除矩阵

### 攻坚方法学（可复用）
- **工厂模式钩子**：把混淆 VM 的自执行入口改写为 `window.__vmFactory =`（去尾调用），
  手动注入带日志 getter 的环境对象调用——VM 行为可控可观测。
- **主入口手术钩子**：只包 VM 尾部的 `addEventListener(evt, main) : main()` 一处
  （零全局包装，防 VM 防篡改机制）。混淆 VM 会被 JS 层运行时包装（fetch/then/canvas
  hook）改变行为——Rust 桥层追踪（M93.9 BROWSER_TRACE_FETCH）才可靠。
- **双引擎差分**：同一插桩页 Chrome vs 我们，第一处分歧即卡点。

### 已攻克的层（各一个 commit）
| 层 | 根因 | 修复 |
|----|------|------|
| 脚本解析 | — | 本来就能跑（语法探针仅 top-level-await 不支持） |
| WASM 门禁 | cap.js feature-gate | 形状桩触发其设计内 JS 降级路径 |
| crypto.subtle/blob Worker/Blob/TextEncoder | spec 缺失 | M93.7 全部实现（本地三链路铁证） |
| **入口时序** | **readyState 硬编码 'complete'**（spec 违反）→ VM 在 Pass 1（经典脚本前）直跑 main，依赖缺失静默死锁 | **M93.8 真语义 loading→interactive→complete**；钩子实锤 VM 转为 `MAIN-EV type=DOMContentLoaded`（与 Chrome 完全一致） |

### 八假设排除矩阵（全部铁证）
| 假设 | 排除证据 |
|------|---------|
| Promise 永不 settle | then 追踪 main@DCL 后 **n=0 pending** |
| 缺 API（20+ 形状桩扫射） | 无效果（且旧扫射在 readyState 修复前无效条件） |
| canvas 指纹 | VM **零 canvas 调用**（方法级日志） |
| DOM 保真度 | 双引擎 id 集合完全一致 |
| 事件循环早退 | BROWSER_EL_MAX_MS=12s 无变化 |
| TLS/传输 | 入口正常（Rust 侧 fetch trace） |
| cap 库半执行 | Cap.prototype 双引擎一致 |
| 环境对象注入 | envCalls=0（VM 走浏览器路径） |

### 剩余墙的定性（终局）
main 在 DCL 以 Chrome 完全相同的方式运行、返回、**零可观测副作用**（无 DOM 查询
差异、无 canvas、无 Promise、无 fetch），挑战永不发起。结合无头 Chrome（拥有全部
真实 API）在同一站得到显式 "Automated verification failed"：剩余层是 VM 的
**环境判定（anti-automation verdict）**——silent early-return 型。通过它需要：
(a) 全量 VM 反混淆定位具体检查（多层字符串表 + VM 字节码，天级 RE 投入），且
(b) 通过后让引擎呈现"真交互浏览器"的完整可观测身份 = 指纹伪造区（宪法原则 4 禁区）。
**结论：xcancel 判定墙属反爬对抗核心，依法不攻。**

### 本轮沉淀（与判定墙无关，永久有效）
M93.8 readyState 真语义（全 Web 受益的 spec 正确性修复）、M93.9 BROWSER_TRACE_FETCH、
工厂钩子/双引擎差分诊断法（本文件 + /tmp/xc_srv/ 基建）。

## 10. M93.10：终局突破——挑战链路全通

### 解码器 dump 法（本轮关键武器）
闭包内包装 VM 自带的字符串解码函数 hik8ew（`var __hikO=hik8ew; hik8ew=function(){...__vmStr(r)...}`），
VM 每解码一个字符串即显形——检查清单直接可读：媒体 codec 全表、visibilityState、
fp-aes-256-gcm 指纹加密、overpoweredjs.bot 外部检测引擎（opjs.js 加载器 1.2KB +
release 248KB 同族混淆 VM）。

### 第四道功能墙：Page Visibility
- `document.visibilityState` 返回 undefined ≠ 'visible' → VM 判"页面不可见"→
  静默等待 → 挑战永不发起（这正是八假设排除后剩余的零副作用停滞）
- 修复：visibilityState='visible'/hidden=false（诚实值：引擎主动渲染中）+
  sendBeacon 真实现 + canPlayType 平台编解码应答表
- 踩坑记录：首版插桩位置在 document 占位对象创建**之前**，defineProperty
  抛错被 try/catch 静默吞掉——重放无效；移到占位后立即生效

### 实证
- 重放：`POST /antibot/api/cap/challenge` 发出（历史首次）
- **实弹（xcancel.com/nim_lang）：挑战全流程贯通——VM 启动→取真挑战→
  Worker PoW 解题→verify 提交→显式 'Automated verification failed'**
  （与无头 Chrome 得到的响应完全相同：引擎层已无任何阻断，到达同一判定线）

### 剩余：服务端指纹判定层（定性不变）
fp 经 AES-256-GCM 加密上报（/antibot/api/dx 或 client-report），opjs 引擎
（248KB）采集深层指纹。通过判定 = 呈现"真人浏览器"完整指纹身份——宪法
原则 4 禁区。获取 xcancel 数据的可行路径：用户 Chrome 会话 cookie 复用
（M93.6 已证机制），或 netbub 等无挑战实例（已交付）。
