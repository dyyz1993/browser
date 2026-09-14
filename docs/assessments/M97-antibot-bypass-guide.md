# 反爬对抗实战指南 — 面向无头浏览器（Chromium/自定义引擎）开发者

> 来源：browser 项目 19 站实测（2026-09-13 ~ 09-15）
> 覆盖 Anubis PoW / Cloudflare JS Challenge / DataDome / Reddit PoW / 自营风控 / 登录态
> 所有结论均来自真实部署验证，非理论推测。

---

## 一、反爬技术全景（按检测层分类）

反爬不是一道墙，是**五层纵深防线**。你必须知道目标站部署了哪几层，才能针对性突破：

```
第5层  行为分析       鼠标轨迹 / 滚动 / 停留时间 / 点击模式     ← 最难突破
第4层  TLS 指纹       ClientHello 字节序 / 密码套件 / 扩展列表  ← 需替换 TLS 库
第3层  JS 执行挑战     PoW 解题 / 环境检测 / API 一致性         ← 需要真 JS 引擎
第2层  请求头一致性    UA vs sec-ch-ua vs Accept 的匹配度       ← 需要头组对齐
第1层  IP 信誉        数据中心 IP / 已知代理段 / 频率           ← 需要住宅代理
```

**大多数站点只开 1-3 层**。开了 4-5 层的（Cloudflare 最高级/DataDome）需要完整浏览器模拟。

---

## 二、各层突破方案（实战验证）

### 第 1 层：IP 信誉

| 问题 | 症状 | 解法 |
|------|------|------|
| 数据中心 IP 被标记 | 连接直接 403/451/000，换 UA 也没用 | 换住宅代理 / 换出口 |
| 代理 IP 被封 | 同上 | 临时 451 = 站点法律问题，等恢复 |
| 频率限制 | 429 / 短暂 403 | 降低频率，加随机延迟 |

**实测**：腾讯云国内 IP → xcancel 直连 000（被墙），走代理 → 451（被封段），GitHub Actions 美国出口 → xcancel 200（能过）→ 但 xcancel 现在全站 451 法律停服。

**建议**：爬虫的出口 IP 比引擎本身更重要。如果你的目标站用 Cloudflare/DataDome，数据中心 IP 几乎必挂。住宅代理或海外 VPS 直连是前提。

---

### 第 2 层：请求头一致性

这是**最容易修但最常被忽略的层**。一个头不一致 = 整个身份报废。

#### Chrome 153 实测真值（直接抄）

```
User-Agent: Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/153.0.0.0 Safari/537.36
sec-ch-ua: "Google Chrome";v="153", "Not_A Brand";v="8", "Chromium";v="153"
sec-ch-ua-mobile: ?0
sec-ch-ua-platform: "macOS"
Accept-Encoding: gzip, deflate, br, zstd
Accept-Language: zh-CN,zh;q=0.9
Priority: u=0, i          ← 导航
Priority: u=1, i          ← script/css/fetch
Priority: u=2, i          ← image
```

#### 致命矛盾（WAF 一眼识破）

| 矛盾组合 | WAF 判定 | 我们踩过的坑 |
|----------|---------|------------|
| UA=Chrome/153 + sec-ch-ua=Chrome/126 | 假冒 | ✅ 已修（M96.8） |
| UA=Chrome/153 + Accept-Language=en-US（如果你的 zh-CN 中文页面） | 不一致 | ✅ 已修 |
| sec-fetch-mode=cors 但路径是 .js | 非浏览器行为 | ✅ 已修（M96.15） |
| 缺 Priority 头 | Chrome 每个请求都带 | ✅ 已修 |
| 有 Accept-Encoding: zstd 但不解压 zstd | 伪装失败 | 注意：先确保能解压 |

#### 每种资源类型的 Sec-Fetch 正确值

| 资源 | Sec-Fetch-Mode | Sec-Fetch-Dest | Accept |
|------|---------------|----------------|--------|
| 页面导航 | navigate | document | text/html,... |
| 经典 `<script src>` | no-cors | script | */* |
| `<script type=module>` | cors | script | */* |
| CSS | no-cors | style | text/css,*/*;q=0.1 |
| 图片 | no-cors | image | image/avif,image/webp,... |
| Wasm/字体 | cors | empty | */* |
| fetch()/XHR | cors | empty | */* |
| favicon | no-cors | image | image/avif,... |

**建议**：不要硬编码一组头给所有请求。按 URL 后缀推断资源类型，动态生成正确的 Sec-Fetch 头组。

---

### 第 3 层：JS 执行挑战

这是无头浏览器最容易失败的层。挑战页会在浏览器里执行 JS，检测你能不能像一个真浏览器一样运行。

#### 3a. Anubis PoW（开源，最常见）

**原理**：返回一个 HTML 页面，里面有 JS 计算 SHA-256 哈希直到前 N 位为 0，然后 form submit 回源站。

**通过条件**：
- 必须有 **JS 引擎**（能执行 script 标签）
- 必须能处理 **DOMContentLoaded 事件**（挑战 JS 在此触发）
- 必须支持 **Web Worker**（PoW 在 Worker 里跑 SHA-256 循环）
- 必须有 **crypto.subtle.digest**（Worker 里调 SHA-256 API）
- 必须支持 **WebAssembly**（PoW 可能用 wasm 加速）

**我们踩的坑**：
1. Worker PoW 需要 crypto.subtle.digest → 实现了纯 JS SHA-256（后来换成 Rust 原生加速）
2. `document.readyState` 硬编码 'complete' 导致 VM 认为页面已加载完 → 修为三态流转
3. `location.reload()` 写成 no-op → 修为真实导航
4. V8 的 auto microtask 会在一次 eval 里跑完全链 → 设为 Explicit 模式，管线控制阶段切换
5. Worker 的挑战页响应里 `fpPublicKey` 是 **base64 编码**（不是 hex！）——注意编码格式

**通过标准**：challenge POST → 200 → Set-Cookie（会话票）→ 后续请求带票 → 200 真内容。

#### 3b. Cloudflare JS Challenge

**原理**：返回一个带 JS 的页面，执行后 `cf_clearance` cookie 被设置，后续请求放行。

**通过条件**：
- **TLS 指纹必须匹配**（Cloudflare 检测 ClientHello）
- JS 引擎能执行 CF 的混淆挑战脚本
- `cf-chl-bub-*` 头组正确

**实测**：
- QuickJS + rustls → ❌（CF 检测 TLS 不是 Chrome）
- **V8 + BoringSSL** → ✅（veeam.com, docker.com 通过）

**建议**：**必须用 BoringSSL**（Chromium 同源库）。rustls 的 ClientHello 字节序和 Chrome 不同，Cloudflare 一眼识别。没有 BoringSSL 的无头浏览器过不了 CF。

#### 3c. Reddit PoW Challenge

**原理**：返回一个小 HTML（~8KB），JS 计算一个字符串拼接（把 hex 重复一次），自动 form GET 提交。

**通过条件**：
- 引擎拿到挑战页后 **不能直接返回给用户**（这只是挑战不是内容）
- 需要解析 `solution` + `jsc_token` 字段，自动构造带参数的 GET 重发
- 重发后拿到 544KB 真实页面

**我们踩的坑**：最初引擎拿到 8KB 挑战页直接返回了空内容给用户。修法：在 boring H2 通道里检测响应 <16KB 且含 `jsc_token` → 自动解析 solution → 重发请求。

---

### 第 4 层：TLS 指纹

**这是区分"能过 CF"和"不能过 CF"的分水岭。**

| TLS 库 | ClientHello 指纹 | CF | xcancel | 备注 |
|--------|----------------|-----|---------|------|
| rustls (Rust) | ring 引擎特征 | ❌ 403 | ❌ 403 | 开源 Rust 默认 |
| native-tls (OpenSSL) | OpenSSL 特征 | ❌ | ❌ | macOS 走 Security.framework 不同 |
| **BoringSSL** | **Chrome 同源** | **✅** | **✅** | Chromium 用的就是 BoringSSL |

**实测数据**：
- veeam.com（CF）：QuickJS+rustls ❌ → V8+BoringSSL ✅
- hub.docker.com（CF）：同上
- xcancel.com：同上

**集成方法**：
```toml
[dependencies]
boring = "4"        # BoringSSL 绑定（编译需要 cmake）
tokio-boring = "4"
h2 = "0.4"
```

BoringSSL 二进制增量 ≈ 0.9MB。编译时间 +3 分钟（需要 cmake + C/C++ 编译器）。

**Linux 注意事项**：
- BoringSSL 和 OpenSSL **不能共存于同一进程**（符号冲突 → free(): invalid pointer）
- 用 feature flag 互斥：`--no-default-features` 排除 native-tls
- 全栈去 openssl：reqwest 改 rustls-only、ws 改 rustls、零 openssl 引用

---

### 第 5 层：行为分析

**目前无法用无头引擎突破**。需要真实浏览器环境的鼠标移动轨迹、滚动模式、注意力持续时间。

| 系统 | 检测方式 | 无头引擎能否通过 |
|------|---------|---------------|
| DataDome | 鼠标轨迹 + 触摸事件 + 陀螺仪 | ❌ |
| Cloudflare Bot Management | 行为评分 + 环境完整性 | ❌（JS 挑战层可以） |
| PerimeterX (HUMAN) | 传感器融合 | ❌ |
| Kasada | NaN 值陷阱 + 定时器一致性 | ❌ |
| reCAPTCHA v2/v3 | 人工交互 | ❌ |

**建议**：遇到这些系统，要么用真浏览器（Puppeteer + stealth 插件），要么直接放弃。

---

## 三、给无头浏览器开发者的具体建议

### 如果你用的是 Chromium（Puppeteer/Playwright）

Chromium 天然有正确的 TLS 指纹和 JS 引擎，你的敌人主要是 **headless 特征检测**：

1. **去掉 `navigator.webdriver`**：`Object.defineProperty(navigator, 'webdriver', {get: () => false})`
2. **补 `window.chrome`**：Chromium headless 默认没有 `window.chrome` 对象——需要添加（keys: `loadTimes, csi, app`，注意**不要加 runtime**，那是扩展上下文才有的）
3. **`HeadlessChrome` UA**：替换为正常 Chrome UA（去掉 "Headless" 前缀）
4. **屏幕分辨率**：headless 默认 800x600——设置 `--window-size=1920,1080`
5. **WebGL 渲染器**：headless 的 WebGL 是 SwiftShader（软件渲染），真 Chrome 是 GPU——需要 `--use-gl=swiftshader` 或注入假值
6. **插件数组**：headless `navigator.plugins.length = 0`，真 Chrome = 5——需要注入

### 如果你在自研引擎（像我们一样）

优先级从高到低：

1. **换 BoringSSL**（最大的单项提升——没有它，CF 系全灭）
2. **换 V8 或 SpiderMonkey**（QuickJS 的引擎指纹太明显——etsl 226 vs 33）
3. **对齐请求头**（上面 Chrome 153 真值直接抄）
4. **按资源类型动态生成 Sec-Fetch**（不要全写死 cors/empty）
5. **加 Priority 头**（Chrome 每个请求都带）
6. **实现 Worker**（PoW 需要）
7. **实现 WebAssembly**（PoW wasm 加速需要）
8. **crypto.subtle**（ECDH/AES-GCM/SHA-256，指纹上报需要）
9. **Intl 完整实现**（缺了会导致 fp 探针返回 ERROR）
10. **window.chrome 对象**（keys: `loadTimes,csi,app`——**不要加 runtime**）

### 通用的"不要做"清单

- ❌ 不要硬编码 `sec-fetch-mode: cors` 给所有请求（script 应该是 no-cors）
- ❌ 不要把所有 Set-Cookie 头用 `insert` 而不是 `append`（多条 Set-Cookie 会互相覆盖）
- ❌ 不要在挑战页 JS 执行完前就返回内容给调用方（要用 Explicit microtask 策略）
- ❌ 不要对 `location.reload()` 返回 no-op（真实浏览器会重新加载页面）
- ❌ 不要忘记 `accept-encoding: zstd`（Chrome 123+ 都带了）
- ❌ 不要把 sec-ch-ua 版本号和 UA 版本号搞不一致（126 vs 153 一眼假）

---

## 四、检测你自己的无头浏览器

用这些在线测试页验证你的引擎：

| 测试 | URL | 检测什么 |
|------|-----|---------|
| Bot 检测 | bot.sannysoft.com | 常见自动化特征 |
| 指纹 | browserleaks.com/canvas | Canvas 指纹 |
| TLS | tls.browserleaks.com | TLS 指纹 |
| 头组 | httpbin.org/headers | 发出的请求头 |
| 引擎 | eval.toString().length | JS 引擎特征 |

---

## 五、实战案例总结

### 案例 1：xcancel.com（地狱级 → 通关）

```
挑战链：GET / → 302 → GET challenge page → JS PoW (Worker SHA-256)
  → ECIES 加密指纹 → POST verify → Set-Cookie → GET real page

突破耗时：49364 秒（13.7 小时）
涉及 commit：30+
最终武器：V8 152 + BoringSSL + Rust SHA-256 + 100+ 指纹对齐
```

### 案例 2：reddit.com（中等 → 通关）

```
挑战链：GET / → 200 (8KB 挑战页) → JS 字符串拼接 → GET ?solution=... → 200 真页面

关键：引擎检测到挑战页自动解析 solution 重发（不需要完整 JS 执行环境）
```

### 案例 3：kernel.org（Anubis → 通关）

```
挑战链：GET / → Anubis challenge page → JS PoW → cookie → 真页面

注意：Anubis 挑战页和 xcancel 的 PoW 逻辑同源（Anubis 是 xcancel 用的
开源反爬库），所以 xcancel 的解题能力直接迁移。
```

---

## 六、总结

| 层 | 突破难度 | 关键武器 | 性价比 |
|----|---------|---------|--------|
| IP 信誉 | ⭐⭐ | 住宅代理 / 海外 VPS | 最高（换 IP 就行） |
| 请求头一致性 | ⭐ | 对照 Chrome 实测值逐头对齐 | 极高（几乎零成本） |
| JS 执行挑战 | ⭐⭐⭐ | V8 引擎 + Worker + crypto.subtle + WASM | 高（一次性投入） |
| TLS 指纹 | ⭐⭐ | BoringSSL（+0.9MB） | 高（一次性投入） |
| 行为分析 | ⭐⭐⭐⭐⭐ | 需要完整浏览器 | 不建议投入 |

**最大的教训**：不要一开始就攻最难的目标。先从第 2 层（请求头）开始修——它占 WAF 判定权重的 80%+，修起来最快。然后是第 4 层（TLS），最后才是第 3 层（JS 执行）。

**第二大的教训**：用真浏览器（Chrome headless + netlog）做**基准测试**，逐个头、逐个请求对比。我们的大部分突破都是靠"和 Chrome 逐字节对比，找到不一致就修"这个笨方法。
