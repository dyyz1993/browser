# JS 引擎自研可行性分析（boa 重写 vs 替代方案）

> 日期：2026-06-18
> 触发：bark/vue-playground 等纯 CSR 站因 boa 引擎层 bug 跑不动，
> 维护者问"能不能自己重写一个 JS 引擎"。

## 一句话结论

> **重写 boa 不可行（13.6 万行，4 倍我们整个项目）。但有三条务实路径：
> ① 持续升 boa（跟上游）；② 换 QuickJS（轻量但完整，需 FFI）；③ 混合架构
> （boa 快速路径 + Chrome CDP 兜底）。推荐 ①+③ 组合。**

## boa 是什么

- **纯 Rust 的 JS 引擎**（github.com/boa-dev/boa），和我们一样是"造轮子学原理"
- 我们用它（ADR-0002）：省 300MB V8，纯 Rust 符合自研原则
- 现状：v0.21，test262 ~94%，但不完整（bark/vue 崩在引擎层 bug）
- 活跃项目：几十贡献者，持续迭代（0.21 刚落地 async/await）

## 重写可行性分析（为什么不行）

### 工作量

| 指标 | boa | 我们整个项目 |
|------|-----|-------------|
| 代码行数 | **136,820** | 32,231 |
| 模块 | 词法/语法/VM/字节码编译器/内置对象/realm/module/gc... | 16 crate 爬虫管线 |
| 倍数 | — | boa 是我们的 **4.2 倍** |

**重写 boa = 从零写 4 个我们现在这么大的项目。** 且 boa 已有几十人几年积累，
我们重写大概率写不过它（尤其是 VM/字节码/GC 这类工业级组件）。

### 自研原则的边界

GOALS.md 原则二「自研优先」有白名单：**「≥10 万行底层库」允许引入**。
boa 13.6 万行，正是白名单设定的场景——自研原则**允许**用 boa。
（白名单还有 html5ever/hyper/fontdue 等，都是这个量级。）

**重写 boa 违背「复杂度门槛」决策原则**——成本（13.6 万行）远超收益。

## 三条务实路径

### 路径 ①：持续升 boa（推荐，零成本）

boa 活跃迭代，很多 bug 上游会修：
- v0.21 落地了 async/await（我们 M60 已升）
- bark 的 `cannot convert null` 可能是已知 bug，上游未来修
- vue 的 `SyntaxError` 是解析器 bug，上游在补

**行动**：定期跟进 boa 版本，每升一版跑 CSR 对比看改善。

### 路径 ②：换 QuickJS（中等成本）

| 维度 | QuickJS（quickjs-ng） | boa 0.21 |
|------|----------------------|----------|
| 语言 | C（需 FFI） | 纯 Rust |
| ES 一致性 | ~100%（完整 ES2023） | ~94% |
| async/await | ✅ 完整 | ✅（刚落地） |
| 体积 | ~1MB（很轻） | 适中 |
| Rust 绑定 | rquickjs（成熟） | 原生 |
| 问题 | FFI = unsafe（违背 forbid(unsafe_code)） | 引擎 bug |

**问题**：QuickJS 是 C 写的，FFI 绑定需要 unsafe。我们 `#![forbid(unsafe_code)]`
全 workspace 强制（AGENTS.md 硬性约定 #5）。**除非专门开个 `js-engine-quickjs`
crate 放 unsafe**（像 cli/sandbox.rs 那样特批），否则不能用。

### 路径 ③：混合架构（推荐组合）

**boa 快速路径 + Chrome CDP 兜底**：
```
fetch <url>
  → --smart 先试 SSR（秒出，80% 站）
  → 不够跑 boa JS（简单 SPA 能跑，如 todomvc-react）
  → boa 崩/空 → 标注「需 Chrome」或未来接 CDP 兜底
```

我们已经有了 `--smart` + catch_unwind + `--no-js`，这构成了**分层兜底**。
纯 CSR 无 SSR 的硬骨头（bark/vue-playground）留给未来 CDP 兜底（opt-in，
用户自己装 Chrome，默认不用）。

## 推荐：① + ③ 组合

1. **短期**：跟 boa 上游版本（0.21→0.22+），每升一版跑 CSR 对比
2. **中期**：实现 CDP 兜底（M63+，opt-in 重型路径，保默认轻量）
3. **不重写 boa**：13.6 万行 + 工业级 VM/GC，自研不现实，违背复杂度门槛

## 什么情况下值得重新评估

- boa 项目停滞（半年无更新）→ 考虑 QuickJS（接受 unsafe 特批）
- 纯 CSR 站占比超过 40%（目前 ~20%）→ CDP 兜底变刚需
- 有专项预算/团队 → 可以评估完整自研（学习价值极高，但产出周期以年计）
