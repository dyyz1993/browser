# Weekly JS Code Module Review

> **目的**：确保 JS shim 代码按模块组织，不积累在单一大文件中。
> **频率**：每周一次（推荐周一）。
> **工具**：`python3 tools/js-module-review.py`

---

## 检查清单

### 1. 运行模块映射工具

```bash
python3 tools/js-module-review.py
```

输出应显示每个模块在预期范围内。如有 ⚠️ 警告 → 执行拆分。

### 2. 检查文件大小阈值

| 文件 | 阈值 | 当前 |
|:-----|:----:|:----:|
| `scripts.rs` | < 4000 行 | ~3950 |
| `bridge.rs` | < 3500 行 | ~3400 |
| `engine_quickjs.rs` | < 600 行 | ~520 |

超出阈值 → 考虑拆分子模块。

### 3. 检查新增 JS Shim 代码位置

新增的 JS shim 代码（`QUICKJS_*_SHIM` 常量）应按功能放入对应常量。
如果新功能不属于现有 4 个 shim（Global/Element/Document/XHR）：

| 功能 | 推荐位置 |
|:-----|:---------|
| 全局 API（Event/Blob/Proxy/TextEncoder） | `QUICKJS_GLOBAL_SHIM` |
| DOM 元素方法 | `QUICKJS_ELEMENT_SHIM` |
| document 方法 | `QUICKJS_DOCUMENT_SHIM` |
| 网络请求 | `QUICKJS_XHR_SHIM` |
| CSSOM | 新建 `QUICKJS_CSS_SHIM` |
| WebSocket | 新建 `QUICKJS_WS_SHIM` |
| Performance | 维持现有 |

### 4. 检查 Bridge 函数归属

Rust bridge 函数（在 `bridge.rs` 中的 `fn *_bridge`）应按功能分组：

| 功能 | 前缀 |
|:-----|:-----|
| DOM 操作 | `__setBody`, `__appendBody`, `__getAttr`, `__setAttr` |
| 选择器 | `__qs`, `__qsAll`, `__qsMatch`, `__qsClosest` |
| 网络请求 | `__fetchSync`, `__fetchSyncMethod` |
| 存储 | `__getCookie`, `__setCookie` |
| 计时器 | `__setTimeout`, `__setInterval`, `__drainDueTimers` |

新增 bridge 函数 → 确认前缀一致、注释标注功能组。

### 5. 检查 test fixture 模块归属

`tests/bug-hunt/categories/` 下的 fixture 应按类别放入正确目录：

| 目录 | 内容 |
|:-----|:-----|
| `A-js-core/` | ES2020+ 语法、内置对象 |
| `B-dom-api/` | DOM 操作（createElement/querySelector 等） |
| `C-spa/` | SPA 路由、存储、异步 |
| `D-network/` | XHR、fetch、Cookie |
| `E-console/` | console.log/error/warn |
| `F-edge/` | Blob、FileReader、FormData 等 |

---

## 模块架构参考

```
crates/js-runtime/src/
├── scripts.rs              # QuickJS JS shim（4 个常量 + 编排）
│   ├── QUICKJS_GLOBAL_SHIM   (~858行)  全局对象/API
│   ├── QUICKJS_ELEMENT_SHIM  (~647行)  Element 原型
│   ├── QUICKJS_DOCUMENT_SHIM (~116行)  document 对象
│   └── QUICKJS_XHR_SHIM      (~?行)    XMLHttpRequest
├── bridge.rs               # Rust ↔ JS bridge 函数
│   ├── qjs_bridge模块         QuickJS 桥
│   └── boa_bridge (cfg)      Boa 桥
├── engine_quickjs.rs       # QuickJS 引擎 + ESM loader
│   ├── HttpLoader            Module loader
│   └── HttpResolver          URL resolver
├── engine_boa.rs (cfg)      # Boa 引擎
├── compat_shim.rs (cfg)     # Boa compat shim
├── document_shim.rs (cfg)   # Boa document shim
└── ...                      # 其他 Boa shim（可选）
```

---

## 何时需要拆分

- `scripts.rs` > 4500 行 → 拆分的候选
- `bridge.rs` > 4000 行 → 拆分的候选
- 同一模块有超过 100 行的注释/文档 → 考虑独立文件
- 任何文件超过 80KB → 必须拆分
