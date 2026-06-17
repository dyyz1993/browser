# M62 SPA 真实站点对比：browser fetch vs Chrome headless

> 日期：2026-06-18
> 对比脚本：[`tests/benchmarks/spa_compare.sh`](../../tests/benchmarks/spa_compare.sh)
> 方法：15 个 SPA 站点，browser fetch --smart（我们）vs Chrome --headless --dump-dom（真渲染基线），四维度公平对比。

## 一句话结论

> **内容质量持平率 73%（11/15 + 1 赢 = 80% 有效覆盖），速度平均快 5.8 倍（14/15 站更快），
> 内存仅 Chrome 的 1/11（27MB vs 311MB）。** 这是 M60-M62 升级 + 28 项 JS 补齐后的真实数据。

## 四维度总成绩（15 站）

| 维度 | browser fetch | Chrome headless | 对比 |
|------|--------------|-----------------|------|
| **内容质量**（关键词命中） | 12/15 拿到 | 14/15 拿到 | 持平 73%，我们赢 1（掘金），Chrome 赢 3 |
| **速度**（平均 wall ms） | **4,717ms** | 27,356ms | **快 5.8 倍，14/15 站更快** |
| **内存**（平均 peak RSS） | **27MB** | 311MB | **省 11 倍** |
| **稳定性** | 15/15 不崩溃 | 15/15 | 持平（M62 catch_unwind 兜底后） |

## 详细结果（15 站逐项）

### ✅ 持平 + 我们更优（11 站）

| 站点 | 我们 ms/MB/B | Chrome ms/MB/B | 速度比 |
|------|-------------|----------------|--------|
| todomvc-react | 1.9s/64MB/95B | 5.9s/263MB/16B | 3.1x |
| todomvc-vue | 1.8s/39MB/95B | 4.0s/253MB/355B | 2.2x |
| todomvc-preact | 2.9s/28MB/95B | 7.6s/259MB/508B | 2.7x |
| caniuse | **1.0s/14MB**/3.8KB | 13.6s/262MB/3.4KB | **13x** |
| jsonplaceholder | 1.0s/14MB/2.4KB | 8.9s/267MB/2.8KB | 8.8x |
| npmtrends | 2.1s/14MB | **166s**/280MB | **79x** |
| cls（财联社） | **0.4s/14MB** | 35.3s/308MB | **91x** |
| svelte-repl | 5.0s/16MB | 13.9s/271MB | 2.8x |
| solid-playground | 3.1s/31MB | 47.5s/**574MB** | 15x |
| firecrawl-docs | **1.7s/21MB** | 43.7s/**507MB** | **26x** |
| owid-grapher | **1.9s/16MB**/27KB | 16.0s/322MB/41KB | 8.4x |

> 注：owid-grapher 之前触发 boa panic 崩溃，M62 catch_unwind 兜底后稳定降级拿 27KB。

### 🏆 我们赢 Chrome（1 站）

| 站点 | 我们 | Chrome | 原因 |
|------|------|--------|------|
| **掘金** | **0.36s/15MB** ✅ 拿到"稀土" | 23.5s/270MB ❌ 没拿到 | Chrome headless 的 `--dump-dom` 在 8s virtual-time 内没渲染完掘金的复杂 JS；我们 `--smart` 检测 SSR 充足直接跳过 JS |

### ❌ Chrome 赢我们（3 站）

| 站点 | 我们 | Chrome | 原因 |
|------|------|--------|------|
| bundlephobia | 5.3s/**8B** ❌ | 5.5s/270MB ✅ | JS 报错（ boa 兼容），SSR 内容太稀疏 |
| bark | 4.3s/**1B** ❌ | 4.6s/264MB ✅ | 纯 CSR（docsify），SSR 空壳，boa 跑不动 |
| vue-playground | **38s**/1B ❌ | 14.4s/301MB ✅ | Vue REPL 纯 CSR，boa 跑 JS 38s 仍空 |

**规律**：3 个输的都是**纯 CSR 无 SSR 兜底**的站——JS 渲染拿不到数据，SSR 也没内容。这是 boa 引擎天花板，已知局限。

## 关键发现

### 1. `--smart` 模式是杀手级特性
掘金/cls/firecrawl 这种有 SSR 的重 JS 站，Chrome 要 23-44s 跑完整 JS，我们 `--smart` 检测 SSR 充足直接跳过，**0.4-1.7s 秒出**。

### 2. 内存优势巨大且稳定
- 我们平均 27MB，Chrome 平均 311MB（**11 倍差距**）
- 最极端：solid-playground 我们 31MB vs Chrome **574MB**（18 倍）
- 原因：不启动 Chromium 引擎，单进程 arena DOM

### 3. 速度优势主要来自 `--smart` 跳过 JS
纯 JS 渲染（非 smart）我们不一定比 Chrome 快（boa 比 V8 慢）。但 `--smart` 让有 SSR 的站跳过 JS 执行，速度碾压。

### 4. 覆盖率瓶颈确认：纯 CSR 无 SSR
3 个输的站全是纯 CSR（bark/vue-playground/bundlephobia）。这印证了 M60 路线图的判断——**纯 CSR 无 SSR 兜底是 boa 天花板，需真 Chrome**。

## 推广话术（有数据支撑）

> **「15 站实测：内容覆盖率 80%，速度平均快 6 倍，内存仅 Chrome 的 1/11。
> 有 SSR 的站秒出（smart 模式），纯 CSR 站诚实标注需 Chrome。
> 13MB 单文件，scp 上服务器当 curl 用。」**

## 复现

```bash
# 编译
cargo build --release -p browser-cli

# 跑全量对比（约 10 分钟）
./tests/benchmarks/spa_compare.sh --group all

# 或分组跑
./tests/benchmarks/spa_compare.sh --group main   # 8 站较快
./tests/benchmarks/spa_compare.sh --group stress  # 7 站含重 JS
```

## 已知局限（诚实）

1. **纯 CSR 无 SSR 站**（3/15 = 20%）：boa 跑不出数据，需 Chrome
2. **内容字节数普遍少于 Chrome**：我们 noise filter 去得狠 + markdown 紧凑（爬虫优势，非劣势）
3. **vue-playground 38s 仍空**：boa 跑 Vue REPL 超时，可考虑加更激进的超时降级
