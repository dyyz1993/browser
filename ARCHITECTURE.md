# Architecture

## Crate 依赖图

```
                        ┌─────────────┐
                        │  cli (bin)  │
                        └──┬──────────┘
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
        ┌──────────┐ ┌───────────┐ ┌──────────┐
        │   page   │ │   render  │ │   ...    │
        └──┬───┬───┘ └─────┬─────┘ └──────────┘
           │   │           │
           │   │     ┌─────▼─────┐
           │   │     │  layout   │
           │   │     └─────┬─────┘
           │   │           │
           │   │     ┌─────▼─────┐
           │   │     │css-engine │
           │   │     └─────┬─────┘
           │   │           │
           ▼   ▼           ▼
        ┌──────────┐  ┌──────────────┐
        │js-runtime│  │ html-parser  │
        └────┬─────┘  └──────┬───────┘
             │               │
             ▼               ▼
              ┌──────────────┐
              │     dom      │  ◄── 所有上层 crate 共享
              └──────────────┘
                     │
                     ▼
              ┌──────────────┐
              │     net      │  ◄── page 直接用
              └──────────────┘
```

## 数据流（M1 完成态）

```
URL ─▶ net::get() ─▶ Vec<u8>
                       │
                       ▼
              html_parser::parse()
                       │
                       ▼
                   dom::Tree
                       │
                       ▼
              dom::pretty_print()
                       │
                       ▼
                     stdout
```

## 数据流（M4 完成态：SPA 渲染）

```
URL
 │
 ▼
page::Page::new()
 │
 ├─▶ net::get(url) ──▶ HTML
 │                       │
 │                       ▼
 │             html_parser::parse()
 │                       │
 │                       ▼
 │                   dom::Tree
 │                       │
 │                       ▼
 │             js_runtime::evaluate(<scripts>)
 │                       │
 │                       │  ┌──────────────────────┐
 │                       │  │ JS 调 DOM API         │
 │                       │  │ JS 调 fetch()         │
 │                       │  │   └─▶ net::get(api)   │
 │                       │  │       ─▶ json         │
 │                       │  │ JS 写 DOM             │
 │                       │  └──────────────────────┘
 │                       ▼
 │              (等待 networkidle)
 │                       │
 │                       ▼
 │              dom::pretty_print() / dom::serialize()
 │                       │
 ▼                       ▼
"渲染后 HTML" ────────────▶ 调用方
```

## Crate 职责边界

| Crate | 职责 | 不负责 |
|-------|------|--------|
| `net` | HTTP/HTTPS/WS 客户端、Cookie jar、URL 解析 | HTML 内容解析 |
| `dom` | DOM 数据结构（arena）、Node/Element API、事件、遍历 | 解析、JS |
| `html-parser` | 把 HTML 字符串转成 dom::Tree | DOM 内部数据结构 |
| `css-engine` | 解析 CSS、选择器匹配、计算样式 | 布局 |
| `layout` | 把 dom::Tree + 计算样式 → 布局树 | 绘制 |
| `render` | 布局树 → 像素（tiny-skia） | 窗口管理 |
| `js-runtime` | 嵌入 JS 引擎（boa/deno_core）、JS ↔ DOM 桥 | JS 引擎本身 |
| `page` | Page/Frame/Navigation 编排、生命周期 | 具体子能力 |
| `cli` | 命令行入口、子命令分发 | 业务逻辑 |

## 关键决策（ADR 摘要）

- **ADR-0001（待写）：** DOM 用 arena，不用 `Rc<RefCell>`。理由：JS 桥借用循环、对齐 Servo。

> 完整决策列表见 `docs/decisions/`
