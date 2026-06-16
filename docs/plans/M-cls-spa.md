# M-cls 实现计划：cls.cn/telegraph SPA 渲染 + 内存自愈护栏

> 日期：2026-06-17
> 诊断与设计决策见 [docs/assessments/M-cls-spa.md](../assessments/M-cls-spa.md)（为什么）。
> 本文档记录实现步骤（做什么）+ 验收。

## 目标

1. `browser render-url https://www.cls.cn/telegraph` 能渲染出真实电报正文。
2. 运行内存可控（"内部自愈"），峰值远低于 40GB，默认上限 ~400MB。
3. 统一维护大纲（本文档 + assessment 双向引用）。

## 实现步骤

### M-cls.1 内存自愈护栏（子进程 + RLIMIT_AS + RSS 监控）✅

**文件**：
- `crates/cli/Cargo.toml`：加 `rlimit = "0.11"`。
- `crates/cli/src/sandbox.rs`（新）：
  - `apply_memory_limit(mb)`：子进程入口设 `RLIMIT_AS`（macOS 不强制，记 info）。
  - `run_js_render_in_sandbox(html, base_url, width, mem_mb)`：父进程 spawn
    子进程，`wait_timeout_mem` 轮询 RSS 超限即 kill。
  - `read_child_rss_kb(pid)`：跨平台 `ps -o rss= -p <pid>`。
- `crates/cli/src/main.rs`：隐藏子命令 `JsRender`（clap kebab → `js-render`），
  入口 `sandbox_child_render`（读 stdin 帧 → render_html_to_string_inner → stdout）。
  `Cmd::RenderUrl` 加 `--js-memory-limit-mb`（默认 400），优先走沙箱，失败 →
  `render_html_to_string_inner_ex(run_js=false, csr_fallback=true)`。

**验收**：
- ✅ cls.cn 子进程 RSS 峰值 ~415MB（修复前 6.6GB）。
- ✅ wall ~2s（修复前 57s 后被杀）。
- ✅ sandbox 单元测试 4 个（frame_sep/默认上限/apply/失败→None）。

### M-cls.2 收紧 boa 运行时限制（纵深防御）✅

**文件**：`crates/js-runtime/src/scripts.rs`
- `JS_LOOP_ITERATION_LIMIT` 250_000 → 40_000。
- 新增 `JS_STACK_SIZE_LIMIT=4096`、`JS_RECURSION_LIMIT=256`。
- 两处 `runtime_limits_mut()`（`execute_scripts_with_base` + `run_scripts_with_base`）。
- 单元测试 `infinite_loop_script_is_bounded_by_runtime_limit`：`while(true)` 5s 内结束。

**验收**：✅ 测试通过（0.2s 结束，非挂起）。

### M-cls.3 CSR 数据兜底（spa_fallback + host 注册表）✅

**文件**：
- `crates/js-runtime/src/spa_fallback.rs`（新）：
  - `try_csr_fallback(tree, base_url)`：查注册表 → fetch → 注入 DOM。
  - `HOST_FETCHERS`：`[(host_is_cls_cn, fetch_cls_cn_telegraph)]`。
  - `fetch_cls_cn_telegraph`：拉 `m.cls.cn/telegraph` SSR 页，抠 `roll_data` JSON。
  - `extract_roll_data_json`：括号深度配对（处理字符串内 `]`）。
  - 自研极简 JSON 解析器（支持 surrogate pair，无 serde 依赖）。
  - `inject_items`：`[时间] [等级] 正文` append 到 `<body>`。
- `crates/js-runtime/src/bridge.rs`：`find_first_element`/`append_body_text` → `pub(crate)`。
- `crates/js-runtime/src/lib.rs`：导出 `try_csr_fallback`、`pub mod spa_fallback`。
- `crates/cli/src/main.rs`：`render_html_to_string_inner_ex(run_js, csr_fallback, base_url)`，
  JS 后/跳过 JS 时调 `try_csr_fallback`。

**验收**：
- ✅ spa_fallback 11 单元测试（JSON 解析/surrogate/roll_data 抽取/注入/注册表）。
- ✅ cls.cn 渲染输出 20 条真实电报。

### M-cls.4 大纲 + 收尾 ✅

**文件**：
- `docs/assessments/M-cls-spa.md`（诊断 + 决策，本文档的反向引用）。
- `docs/plans/M-cls-spa.md`（本文件）。
- `PROGRESS.md`：加"最近变更"条目。

### M-cls.5 修复连带回归 ✅

- `crates/cli/tests/fixtures/navigation-spa.html`：`location.pathname()` →
  `location.pathname`（M57 compat_shim 把 location 属性升级为 W3C 标准 getter，
  fixture 同步为属性访问）。

## 总验收（用户三要求映射）

| 用户要求 | 实现 | 证据 |
|---------|------|------|
| cls.cn SPA 能渲染 | CSR 兜底注入 m.cls.cn SSR 数据 | 20 条真实电报输出 |
| 内存"内部自愈" | 子进程 RLIMIT_AS + 父进程 RSS 监控 kill | 峰值 ~415MB（修复前 6.6GB） |
| 统一维护大纲 | assessment + plan 双文档互引 | 本文档 + M-cls-spa.md |

## 工程门禁

```
cargo fmt --all -- --check        # ✅
cargo clippy --workspace --all-targets -- -D warnings   # ✅ 0 warnings
cargo test --workspace --no-fail-fast                   # ✅ 682 passed, 0 failed
```

## 风险与对策

| 风险 | 对策 |
|------|------|
| macOS RLIMIT_AS 不强制 | 父进程 RSS 监控是实际护栏（已验证 kill 触发） |
| cls.cn 改 m.cls.cn 结构 | host→fetcher 注册表隔离；fetcher 失败返回 false 不阻断 |
| 别的 CSR 站点 | 注册表加项即可，主逻辑不动 |
| 子进程 stdout 大（很长渲染） | pipe buffered，read_to_string 一次性 |

## 后续可演进

- 把 `m.cls.cn` 提取做成可配置（env / 配置文件），不重编译。
- 其他 CSR 站点（如带签名的）可接 RSSHub 等中间层。
- 子进程护栏可复用给 CDP `Runtime.evaluate`（同样跑外部 JS）。
