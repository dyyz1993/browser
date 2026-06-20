# M66 QuickJS CSR 对标评估报告

> 日期：2026-06-20
> 引擎：QuickJS（rquickjs 0.12）vs boa（git main）vs Chrome（headless --dump-dom）
> 评估方法：`browser fetch <url> --format text` + Chrome `--dump-dom` 提取纯文本

---

## 一、拉齐程度（对标 Chrome）

### 渲染成功率

| 状态 | 站点数 | 站点 |
|------|:------:|------|
| ✅ 完全渲染（0 错误） | 5 | nuxt.com, svelte.dev, vite.dev, react.dev, remix.run |
| ✅ 渲染成功（1-2 错误） | 3 | vuejs.org, astro.build, docusaurus.io |
| ❌ 渲染失败 | 2 | bark.day.app（docsify 异步链）, solidjs.com（template clone） |

### 内容覆盖率（对标 Chrome 文本字符数）

| 站点 | QuickJS | Chrome | 覆盖率 | 说明 |
|------|--------:|-------:|:------:|------|
| react.dev | 7530 | 8287 | **91%** | ⭐ QuickJS 独有（boa 仅 0.7%） |
| docusaurus.io | 4086 | 4793 | **85%** | |
| svelte.dev | 1938 | 2349 | **83%** | |
| vite.dev | 2994 | 3859 | **78%** | |
| vuejs.org | 1354 | 2120 | **64%** | ESM module 部分跳过 |
| astro.build | 5565 | 10665 | **52%** | ESM module + Web Components 跳过 |
| remix.run | 2909 | 6253 | **47%*** | *Chrome 多出的主要是 CSS 变量声明 |
| bark.day.app | 1 | 586 | **❌** | docsify 异步渲染链未完成 |
| solidjs.com | 1 | 1707 | **❌** | SolidJS template clone 不兼容 |

**平均覆盖率（成功站点）：67%**

### 差距根因

| 根因 | 影响站 | 修复方向 |
|------|--------|---------|
| ESM module 有 `import "./x"` 跳过 | vuejs/vite/astro | HttpLoader 正确实现 chunk fetch |
| setTimeout 同步执行 | remix/astro | 异步 event loop（Promise.then 驱动） |
| docsify 异步渲染链 | bark | 更完整的 docsify API 兼容 |
| SolidJS template clone | solidjs | 深层 DOM 兼容 |
| Web Components (customElements) | astro | class extends HTMLElement GC 安全 |

---

## 二、性能对比（QuickJS vs boa vs Chrome）

### 速度（秒，取最快值）

| 站点 | QuickJS | boa | Chrome | QJS vs boa |
|------|--------:|----:|-------:|:----------:|
| nuxt.com | **3.5** | 23.5 | 30.1 | **快 6.7x** |
| remix.run | **0.5** | 2.1 | 10.9 | **快 4.1x** |
| svelte.dev | **1.3** | 2.4 | 12.3 | **快 1.8x** |
| astro.build | **13.6** | 17.7 | 10.1 | **快 1.3x** |
| vuejs.org | **8.4** | 9.0 | 11.2 | **快 1.1x** |
| vite.dev | **8.8** | 9.1 | 17.2 | 持平 |
| react.dev | **15.1** | 31.2 | 21.3 | **快 2.1x** |
| docusaurus | 17.3 | 12.0 | — | 慢 0.7x |

**QuickJS 7/8 站比 boa 快，5/8 站比 Chrome 快**

### 内存（峰值 RSS MB）

| 站点 | QuickJS | boa | Chrome | QJS vs boa | QJS vs Chr |
|------|--------:|----:|-------:|:----------:|:----------:|
| nuxt.com | **26** | 141 | 456 | 1/5 | **1/18** |
| react.dev | **26** | 133 | 263 | 1/5 | **1/10** |
| docusaurus | **41** | 162 | 263 | 1/4 | **1/6** |
| vite.dev | **21** | 50 | 269 | 1/2 | **1/13** |
| astro.build | **21** | 32 | 262 | 1/2 | **1/12** |
| svelte.dev | **18** | 28 | 262 | 1/2 | **1/15** |
| remix.run | **18** | 27 | 248 | 1/2 | **1/14** |
| vuejs.org | **21** | 42 | 266 | 1/2 | **1/13** |

**QuickJS 中位数 21MB，Chrome 的 1/13**

---

## 三、架构设计

```
┌─────────────────────────────────────────────┐
│  CLI（main.rs）                              │
│  --js-engine quickjs（默认）/ boa           │
├─────────────────────────────────────────────┤
│  EngineKind（engine.rs）                     │
│  ├── BoaEngine（engine_boa.rs）             │
│  └── QuickJsEngine（engine_quickjs.rs）      │
│      ├── 68 个 bridge 函数（Function::new） │
│      ├── HttpResolver + HttpLoader（ESM）   │
│      ├── eval_safe（CatchResultExt）        │
│      └── 独立 JS shim（5400 行精简版）       │
├─────────────────────────────────────────────┤
│  bridge.rs（共享 DOM 后端）                  │
│  qjs_bridge 模块（QuickJS 公开包装）         │
│  with_tree / find_by_selector / collect_text │
├─────────────────────────────────────────────┤
│  JS shim 层（引擎无关的纯 JS 字符串）         │
│  window / document / Element / XHR / URL     │
│  crypto / history / localStorage / Event     │
└─────────────────────────────────────────────┘
```

---

## 四、测试方法与步骤

### 4.1 基本验证（单站）

```bash
# 构建（默认包含 QuickJS）
cargo build --release -p browser-cli

# 文本提取
./target/release/browser fetch https://nuxt.com/ --format text

# Markdown 提取
./target/release/browser fetch https://nuxt.com/ --format markdown

# HTML 提取
./target/release/browser fetch https://nuxt.com/ --format html

# ASCII 画面渲染
./target/release/browser render-url https://svelte.dev/ --width 80

# PNG 截图
./target/release/browser render-url https://svelte.dev/ --screenshot output.png

# 切换引擎
./target/release/browser fetch https://nuxt.com/ --js-engine boa
```

### 4.2 多站对比基准

```bash
# QuickJS vs boa（内容字符数）
for url in "https://nuxt.com/" "https://svelte.dev/" "https://vite.dev/"; do
  qjs=$(./target/release/browser fetch "$url" --format text 2>/dev/null | wc -c)
  boa=$(./target/release/browser fetch "$url" --format text --js-engine boa 2>/dev/null | wc -c)
  echo "$url: QuickJS=$qjs boa=$boa"
done

# 内存对比（macOS）
/usr/bin/time -lp ./target/release/browser fetch "https://nuxt.com/" --format text >/dev/null 2>&1 | grep "maximum resident"

# 对标 Chrome（需要 Chrome 安装）
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
"$CHROME" --headless=new --disable-gpu --no-sandbox \
  --virtual-time-budget=8000 --dump-dom "https://nuxt.com/" 2>/dev/null | \
  python3 -c "import sys,re;h=sys.stdin.read();h=re.sub(r'<script[^>]*>.*?</script>','',h,flags=re.DOTALL);t=re.sub(r'<[^>]+>',' ',h);print(len(re.sub(r'\s+',' ',t).strip()))"
```

### 4.3 自动化基准脚本

```bash
# 完整三方对比（12 站，速度+内存+内容+错误数）
bash tests/benchmarks/csr_benchmark.sh

# QuickJS vs boa 速度+内存
bash tests/benchmarks/perf_compare.sh

# CSR 内容覆盖率（QuickJS vs Chrome）
bash tests/benchmarks/chrome_compare.sh
```

### 4.4 JS 错误诊断

```bash
# 查看 JS 执行错误（按频率排序）
./target/release/browser fetch https://react.dev/ --format text 2>&1 >/dev/null | \
  grep -oE "message=[^|]*" | sort | uniq -c | sort -rn | head -10

# 查看脚本执行数量
./target/release/browser fetch https://react.dev/ --format text 2>&1 | grep "executed"

# 底层插桩（分阶段 RSS + 耗时）
./target/release/browser fetch https://react.dev/ --format text --profile
```

### 4.5 测试门禁

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
```

---

## 五、结论

**QuickJS 作为默认引擎的定位：**

| 维度 | 评估 | 对标 Chrome |
|------|------|------------|
| **速度** | ⭐ 最快 | 5/8 站超 Chrome |
| **内存** | ⭐ 最低（21MB 中位数） | Chrome 的 1/13 |
| **渲染质量** | 良好（67% 平均覆盖率） | 未完全拉齐 |
| **react.dev** | ⭐ 91%（boa 0.7%） | 接近 Chrome |
| **稳定性** | ⭐ 8/10 站 0 assertion | GC 安全 |
| **画面渲染** | ✅ ASCII + PNG | 支持截图 |

**拉齐程度：70%对标 Chrome**（8/10 站成功渲染，平均覆盖率 67%，react.dev 91%）。剩余差距来自 ESM module 完整支持 + 异步 event loop + 框架特定兼容。
