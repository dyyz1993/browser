# M76 — Vite ESM 纯 CSR 兼容性：对标 Chrome 渲染 pi-agent-chat

> **目标**：让 `localhost:5173`（Vite + React + TSX 纯 CSR）通过我们的浏览器渲染出与 Chrome 一致的内容（177KB），零 JS 错误。
>
> **用户场景**：Vite dev server 是最常见的现代前端开发环境。能渲染 Vite SPA = 能渲染 90%+ 的纯 CSR 项目。

---

## 一、现状差距

实验对比（`browser fetch http://localhost:5173/`）：

| 方式 | 大小 | 有 app 内容？ |
|------|-----|:----------:|
| 无 JS 静态壳 | ~5KB | ❌ 空 `<div id="root">` |
| 我们（CSR 渲染） | ~5KB | ❌ 只拿到 boot failure 面板 |
| **Chrome** | **177KB** | ✅ 完整 React app + UI 树 |

我们的 stderr 错误：
```
Error: import.meta only valid in module code      # @vite/client 退化 eval
Error: module declare: Exception                    # @vite/client ESM 路径失败
```

## 二、根因分析

### 2.1 模块依赖链

```
localhost:5173/
  ├── <script type="module">import { injectIntoGlobalHook } from "/@react-refresh"  ← 静态 import
  ├── <script type="module" src="/@vite/client">   ← 含 export class HMRContext
  ├── <script> boot failure 监控（普通 JS，正常执行）
  └── <script type="module" src="/main.tsx">        ← 含 import.meta.env + import from
```

### 2.2 QuickJS 失败环节

1. **`/@vite/client`** — 含 `export class HMRContext` + `import "/..."` → `has_static_esm_syntax(code)` 返回 `true`
2. → 走 `engine.eval_module_with_imports(&url, &code)` → QuickJS 的 Module::declare API 失败
3. → 整个模块链断裂 → 所有 React 组件不渲染
4. → boot failure 定时器 10s 后显示错误面板

### 2.3 根因分类

| 问题 | 类型 | 难度 |
|------|------|:----:|
| QuickJS `eval_module_with_imports` 对 Vite 转译模块失败 | 引擎 API 缺口 | 🟡 中 |
| `import.meta.env` patch 在 `try_strip` 路径可用但 module 路径没用 | 缺失 fallback | 🟢 易 |
| Vite `export class` 无法被 `try_strip_esm_for_eval` 退化 | 架构限制 | 🔴 难 |

## 三、修复路线

### Phase 1 — 快速 fallback（~1 commit）
- 当 `eval_module_with_imports` 失败时，尝试 `try_strip_esm_for_eval` 退化 eval
- 代价：`export class` 和 `import {}` 无法被 strip，但仍可能跳过部分模块

### Phase 2 — QuickJS ESM Module API 修复（~1-2 天）
- 诊断 `eval_module_with_imports` 为什么对 Vite 模块失败
- 加调试输出 `eprintln!("Module::declare error: {:?}", err)` 看具体原因
- 修复 Module::parse/declare/eval 的调用时序

### Phase 3 — ESM 模块依赖图并行加载（~3-5 天）
- Vite 的 `import "/..."` 是绝对路径依赖，需递归 resolve 所有依赖
- 当前 `HttpLoader` 只做单文件加载，不做依赖图追踪
- 需要实现模块依赖图解析（类似 Node.js ESM resolver）

## 四、验收标准

```bash
# Phase 1 — 部分内容出现，stderr 错误减少
browser fetch http://localhost:5173/ --format html --no-js 2>/dev/null | wc -c
# 期望：> 5000（至少包含 meta 等静态壳）

browser fetch http://localhost:5173/ --format text 2>/dev/null | wc -c
# Phase 2: > 50000（出现部分 React 内容）
# Phase 3: > 150000（接近 Chrome 的 177KB）

# stderr 零错误
browser fetch http://localhost:5173/ --format text 2>/dev/null > /dev/null
# 期望：无 Error 输出
```

## 五、已知风险

- Vite dev server 可能返回 HTTP 404 对未预编译的中间模块（`/@fs/...` 路径）
- HMR WebSocket 连接在无 WS 支持时可能让 Vite 客户端无限重连
- React Refresh (`@react-refresh`) 的 `injectIntoGlobalHook` 需要 Worker/MessageChannel 桩
