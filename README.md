# Browser

跨平台、L1 级自研浏览器，核心用途：**渲染并爬取 SPA 页面，控制内存占用**。

## 状态

🚧 M0（项目骨架）阶段。详见 [ROADMAP.md](./ROADMAP.md)。

## 快速开始

```bash
# 验证骨架
cargo build --workspace
cargo test --workspace

# M1 完成后可以这样用：
# cargo run -p browser-cli -- get https://example.com
# cargo run -p browser-cli -- parse path/to/file.html
```

## 项目结构

```
browser/
├── PLAN.md           # 总体计划
├── ROADMAP.md        # 里程碑路线图
├── ARCHITECTURE.md   # 架构与数据流
├── crates/
│   ├── net/          # HTTP/WS/TLS
│   ├── dom/          # DOM 树 + 事件
│   ├── html-parser/  # html5ever 封装
│   ├── css-engine/   # 选择器 + 样式
│   ├── layout/       # 布局引擎
│   ├── render/       # tiny-skia 光栅化
│   ├── js-runtime/   # JS 引擎 + DOM 桥
│   ├── page/         # Page/Frame/Navigation
│   └── cli/          # 二进制入口
└── docs/
    ├── decisions/    # 架构决策记录（ADR）
    └── postmortems/  # 里程碑复盘
```

## 文档导航

- [PLAN.md](./PLAN.md) — 总体计划（里程碑、step、验收）
- [ROADMAP.md](./ROADMAP.md) — 进度路线图
- [ARCHITECTURE.md](./ARCHITECTURE.md) — 架构与数据流
- [docs/decisions/](./docs/decisions/) — 架构决策记录（ADR）
- [docs/postmortems/](./docs/postmortems/) — 里程碑复盘

## License

MIT
