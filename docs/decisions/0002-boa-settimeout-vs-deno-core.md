# ADR-0002: 用 boa 自研 setTimeout/Promise，而非切 deno_core

**状态：** Accepted
**日期：** 2026-06-07
**里程碑：** M16 规划期
**决策者：** AI 调研 + 用户确认方向（A：异步 JS）

---

## 背景

M7.3 当初 defer 异步 JS（setTimeout/Promise/async-await），理由记录为：

> "boa 0.20 `JsObject::call` 是私有 API，实现 setTimeout 需要 JobQueue/NativeObject
> 跨边界，复杂度远超 MVP 价值。推迟到切 deno_core。"

M15 完成后，用户选择按 A→C→D 推进，A = 切 deno_core。按 GOALS.md 决策原则
第 3 条（复杂度门槛）+ 第 4 条（自研优先），切 300MB V8 的 deno_core 是重决策，
必须先做可行性 spike + 验证 M7.3 的 defer 理由是否仍然成立。

## 调研发现

对 boa 0.20 源码（`~/.cargo/registry/src/.../boa_engine-0.20.0/`）的核查推翻了
M7.3 的 defer 前提：

| API | 位置 | 可见性 | 用途 |
|-----|------|--------|------|
| `JobQueue` trait | `src/job.rs:190` | **pub** | Promise/microtask 后端 |
| `SimpleJobQueue` | `src/job.rs:284` | pub（参考实现） | FIFO 队列模板 |
| `Context::enqueue_job` | `src/context/mod.rs:467` | **pub** | JS job 入队 |
| `Context::run_jobs` | `src/context/mod.rs:473` | **pub** | JS job 出队执行 |
| `NativeFunction::call` | `src/native_function.rs:141` | **pub** | 调用 native 回调 |
| `JsFunction::call` | `src/object/builtins/jsfunction.rs:65` | **pub** | setTimeout 回调 JS 函数 |
| `NativeFunction::from_copy_closure` | `src/native_function.rs:105` | pub | 闭包捕获 |

**结论：** M7.3 defer 理由的"JsObject::call 是私有"前提**不成立**。
`NativeFunction::call` 和 `JsFunction::call` 都是 pub，`JobQueue` trait 也 pub。
setTimeout/Promise 完全可以用现有 boa 自研。

## 对比两个方案

### 方案 A：切 deno_core（M16 原始计划）

| 维度 | 评估 |
|------|------|
| 引擎 | V8（~300MB 预编译 binary，从 storage.googleapis.com 下载） |
| 网络风险 | 高（spike 显示 fetch OK，但 300MB binary 下载在受限网络环境不稳定） |
| 工作量 | **大**：重写全部 28 个 `__*` JS 桥（DOM 14 + Storage 6 + Navigation 8） |
| 二进制体积 | +300MB（违反 G1 低资源初衷，GOALS.md 未明说但隐含） |
| 白名单 | 需新增 deno_core + v8 到 G4 白名单 |
| 解锁能力 | setTimeout / Promise / async-await / 真实 SPA bundle / Future-based fetch |

### 方案 B：用 boa 自研 setTimeout/Promise（本次发现）

| 维度 | 评估 |
|------|------|
| 引擎 | 现有 boa 0.20（已 in 白名单） |
| 网络风险 | 无 |
| 工作量 | **中**：新增一个 event loop crate，重用现有 28 个 `__*` 桥不变 |
| 二进制体积 | +0 |
| 白名单 | 无新增 |
| 解锁能力 | setTimeout / Promise / microtask / async function（V8 的 async-await 完整语义仍有差距，但 SPA 常用子集够用） |
| 自研深度 | 高（符合 G4 学习目的） |

## 决策

**采用方案 B：用 boa 0.20 自研 setTimeout/Promise/microtask。**

理由（按 GOALS.md 决策原则排序）：
1. **自研优先（原则 4）**：核心 API 都是 pub，无技术阻塞。
2. **复杂度门槛（原则 3）**：方案 B 是中等工程，方案 A 是大工程（重写 28 桥）。
3. **不破坏既有（原则 5）**：方案 B 保留全部 28 个 `__*` 桥 + 291 tests。
4. **学习价值（原则 2）**：自研 event loop 比调 V8 API 学得更深。

## 副作用与边界

- **放弃**：完整 ES 异步语义（V8 才有的某些边界情况）。
  对 SPA 爬虫场景够用（多数 SPA 用 `fetch().then()` + `setTimeout`，这些都能实现）。
- **保留**：deno_core 作为 M16+ 的 fallback——若未来发现 boa 的某个异步语义
  无法绕过（如 top-level await 与模块系统的复杂交互），再评估切 deno_core。

## 验收

本 ADR 不引入代码，仅记录决策。后续 M16 实现以本 ADR 为依据：

```
M16.1: browser-eventloop crate（TimerWheel + JsJobQueue impl JobQueue）
M16.2: __setTimeout/__clearTimeout 桥 + 全局 setTimeout/clearTimeout shim
M16.3: run_scripts_with_base 接入 event loop（drain timers + run_jobs 循环）
M16.4: Promise 基础支持（enqueue_job + run_jobs 已具备，验证 + 测试）
M16.5: e2e fixture（SPA 用 setTimeout + Promise 渲染内容）
M16.6: ADR 引用更新 + PROGRESS + ROADMAP 同步
```

## 参考

- M7.3 defer 记录：`docs/ROADMAP.md` M7.3 行
- GOALS.md 决策原则：`docs/GOALS.md` §决策原则
- boa 0.20 源码：`~/.cargo/registry/src/index.crates.io-*/boa_engine-0.20.0/`
