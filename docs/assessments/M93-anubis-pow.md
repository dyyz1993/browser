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
