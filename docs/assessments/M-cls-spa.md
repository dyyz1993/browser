# M-cls: cls.cn/telegraph SPA 渲染评估报告

> 日期：2026-06-17
> 目标：保证 `https://www.cls.cn/telegraph` 能通过本浏览器 SPA 渲染出正文，
> 且运行内存可控（用户要求"内部自愈，免得有 40 多 GB"）。
> 实现步骤见 [docs/plans/M-cls-spa.md](../plans/M-cls-spa.md)。
> PROGRESS 入口见 [PROGRESS.md](../../PROGRESS.md)。

## 1. 现象与根因

### 1.1 cls.cn/telegraph 是 Next.js **CSR**（不是 SSR）

抓取 `https://www.cls.cn/telegraph`（17.8KB HTML）检查 `__NEXT_DATA__`：

```json
{"props":{"pageProps":{"initialState":{"chooseNav":"telegraph"}},"__N_SSP":true},
 "page":"/telegraph","query":{},...}
```

`pageProps.initialState` **只有 `{chooseNav}`，没有电报正文**。HTML body 全是
`-.--` 占位符 + `电报持续更新中` + `加载更多` + 空 `<div></div>`。正文由 JS
运行时通过 XHR 拉取后动态渲染——典型 CSR。

### 1.2 正文数据 API 带签名，无法直接调

正文真实来源是 `https://www.cls.cn/v1/roll/get_roll_list`，但实测：

```bash
$ curl ".../get_roll_list?app=CailianpressWeb&os=web&sv=8.4.6&rn=5"
{"errno":"10012","msg":"签名错误"}
```

签名算法混淆在 vendor bundle 里（`webpackChunk_N_E` 多 chunk），逆向不稳定。
（旧的 `/nodeapi/telegraphList` 已下线返回 SPA shell。）

### 1.3 main.js 在 boa 引擎里 eval 内存暴涨到 6.6GB → OOM 被杀

执行 `main-8d59394e.js`（142KB Next.js bundle）时内存实测：

| 阶段 | 峰值 RSS |
|------|----------|
| 修复前（boa 默认 + 250K 循环上限） | **~6.6 GB**（VSZ ~450GB） |
| 修复后（子进程 + RLIMIT_AS + RSS 监控） | **~415 MB** |

进程在 ~57s 被 macOS OOM-killer SIGKILL，渲染永不完成。

### 1.4 boa 0.20 无内存配额 API

boa 0.20 的 `RuntimeLimits` 只有：
- `loop_iteration_limit`（原 250_000）
- `recursion_limit`（默认 512）
- `stack_size_limit`（默认 10240）

**没有分配/内存配额 API**。runaway 循环内的累积分配无法在引擎层封顶。

### 1.5 架构约束

- `js-runtime` 全 crate `#![forbid(unsafe_code)]` → `setrlimit`（需 unsafe）
  不能放这里，只能放 cli crate。
- `SharedTree = Rc<RefCell<Tree>>` 是 `!Send`（thread-local）→ 线程内内存隔离
  不可行；只能 **子进程 + OS rlimit**。

## 2. 决策

### 2.1 内存自愈护栏：子进程 + RLIMIT_AS + 父进程 RSS 监控（纵深防御）

**两层**：
1. **子进程内 `setrlimit(RLIMIT_AS)`**（cli crate，rlimit crate）—— Linux 强制；
   macOS 不强制（返回 EINVAL），仅作尽力。
2. **父进程轮询子进程 RSS**（`ps -o rss= -p <pid>`，50ms 间隔）—— 跨平台
   可靠，超上限立即 SIGKILL 子进程。这是 macOS 上的实际护栏。

**为什么不用线程内方案**：`SharedTree` `!Send`，且 boa 无内存 API。
**为什么 RLIMIT_AS 而非 RLIMIT_RSS**：RSS 在多数内核是"建议"非强制；AS 是强制。
**子进程怎么通信**：隐藏子命令 `browser js-render`（clap `hide(true)`），
父进程 stdin 传 `base_url \x1f width \x1f html`，stdout 收渲染纯文本。

**子进程被杀后怎么办**：父进程 `Ok(None)` → **不重跑会 OOM 的 JS**，
改为 `run_js=false` 渲染静态壳 + CSR 数据兜底（见 2.2）。这是"内部自愈"的
核心——JS 挂了不死磕，换数据源。

### 2.2 数据源：m.cls.cn/telegraph 移动版 SSR（无签名）

发现 `https://m.cls.cn/telegraph`（移动版）是 **SSR**：HTML 直接内嵌
`initialState: {... roll_data: [...]}`，每条含 `brief`/`content`/`ctime`/`level`，
**无需签名、无需 JS**。比带签名的 API 稳定得多。

`spa_fallback::fetch_cls_cn_telegraph`：
1. 拉 `m.cls.cn/telegraph`（同步，复用 spawn+tokio 模式）。
2. `extract_roll_data_json`：按括号深度配对抠出 `roll_data[...]` JSON（处理
   字符串内的 `]`，不误判闭合）。
3. 自研极简 JSON 解析器（无 serde 依赖，遵循自研优先）解析。
4. 每条 → `[YYYY-MM-DD HH:MM] [等级] 正文` 注入 `<body>`。

实测注入 20 条真实电报（带日期/等级/正文）。

### 2.3 host→fetcher 注册表（可扩展）

`HOST_FETCHERS: &[(HostMatcher, Fetcher)]`。新增别的 CSR 站点只加一项，
不动 `try_csr_fallback` 主逻辑。cls.cn 先接，匹配 `www.cls.cn`/`cls.cn`/`m.cls.cn`。

### 2.4 触发条件：注册表即权威信号

不做 body 文本长度启发式（静态壳含导航等长文本会误判"已渲染"）。触发条件
= **base_url 的 host 在注册表里**。fetcher 自己决定数据在不在。

## 3. 纵深防御第二层：收紧 boa 运行时限制

`JS_LOOP_ITERATION_LIMIT` 250_000 → **40_000**，加 `stack_size_limit(4096)`
和 `recursion_limit(256)`。`while(true)` 现在会在限制内抛错（0.2s 结束）而非
挂起。这不是根治（cls 的 OOM 不是简单循环，是循环内分配），但能早死早省 CPU，
且对别的 SPA 更安全。

## 4. 实测验收

```
$ ./target/release/browser render-url "https://www.cls.cn/telegraph" --width 80
[sandbox] RLIMIT_AS not enforced on this OS (EINVAL); parent RSS monitor is the active guard
[sandbox] child RSS 415008KB > cap 409600KB, killing (self-healing)
[sandbox] JS render failed/OOM → static shell + CSR fallback (no JS re-run)
[csr-fallback] injected data for https://www.cls.cn/telegraph
```

- **峰值 RSS ~415MB**（修复前 6.6GB，降 98.4%）
- **wall time ~2s**（修复前 ~57s 后被杀）
- **输出含 20 条真实电报**（日期+等级+正文，非 `-.--` 占位符）

## 5. 不做的事

- 不实现完整 Next.js hydration（boa 引擎限制，PROGRESS 已记，ADR-0002 决策用 boa）。
- 不逆向 cls 签名（移动版 SSR 已满足，签名不稳定）。
- 不引入 V8/deno_core。
- 不改 `js-runtime` 的 `forbid(unsafe_code)`（unsafe 限定在 cli crate）。
