# 自研浏览器项目 — 全程进度大纲

> 执行状态：**M0-M8 完成**，M9.1 进行中
> 当前 HEAD: 6bd8a24 (M8.5 e2e form interaction)
> Workspace: 217 tests, 0 clippy warnings

---

## 总览

| 里程碑 | 状态 | Commit | 测试 | 备注 |
|--------|------|--------|------|------|
| M0 | ✅ | 5147b17 | — | 项目骨架 + CI（9 crate） |
| M1 | ✅ | 多个 | — | HTML 获取 + DOM 树打印 |
| M2 | ✅ | 多个 | 111 | 文本流渲染 + 最小布局引擎 |
| M3 | ✅ | 多个 | 140 | JS 执行 + JS↔DOM 桥 |
| M4 | ✅ | 多个 | 160 | SPA 渲染（**项目目标达成**） |
| M5 | ✅ | 4 commits | 179 | GUI 窗口（winit + softbuffer） |
| M6 | ✅ | 3 commits | 211 | 渲染质量修复 + 黄金对比 + CI |
| M7 | ✅ | 22 commits | 217 | 渲染质量 + 交互（CSS margin + DOM API + 真实字体 + URL 栏 + 滚动） |
| M8 | ✅ | 4 commits | 217 | 表单交互（input/textarea/button/form） |
| M9 | 🟡 | 进行中 | — | 图片渲染（M9.1 进行中） |
| M10 | ⚪ | 未开始 | — | 性能优化 |

---

## M0 — 项目骨架 + CI ✅

**目标**：10 crate workspace + CI pipeline（tests + clippy + fmt）

**验收**：
- `cargo test --workspace` → 0 failed
- `cargo clippy --workspace -- -D warnings` → 0 warnings
- `cargo fmt --all -- --check` → no diff

**Commits**:
- 5147b17: 初始 skeleton

---

## M1 — HTML 获取 + DOM 树打印 ✅

**目标**：curl + parse + 打印 DOM

**验收**：
- `./browser get https://example.com` → HTML 打印
- DOM 树结构正确（head/body/div nested）

**关键决策**：
- `html5ever` wrapper
- arena DOM (`Vec<Node>` + `NodeId(usize)`)

---

## M2 — 文本流渲染 ✅ (111 tests)

**目标**：渲染 HN fixture 标题列表

**验收**：
- `./browser render-file tests/fixtures/news.ycombinator.com.html --width 400` → 包含 4 条标题/点数/用户名/时间

**子任务**：
- M2.1: CSS 解析（cssparser wrapper）
- M2.2: 选择器匹配
- M2.3: compute_styles
- M2.4: layout tree construction
- M2.5: block layout
- M2.6: inline layout + 字符级折行
- M2.7: ASCII renderer
- M2.8: `render-file` 子命令
- M2.9: e2e HN snapshot
- M2.10: postmortem

---

## M3 — JS 执行 ✅ (140 tests)

**目标**：动态元素能进 DOM

**验收**：
- `./browser render-script tests/fixtures/spa-blog.html --width 120` → 输出 "Welcome to my blog / Recent posts"
- stderr: "3 script(s) executed"

**关键决策**：
- `boa_engine` 0.20（学习阶段）
- `NativeFunction::from_fn_ptr` + thread-local bridge
- 同步 fetch（spawn + tokio current_thread）

---

## M4 — SPA 渲染 ✅ (160 tests) — **项目目标达成**

**目标**：React/Vue SPA 可爬（至少能看到内容）

**验收**：
- `./browser render-url https://spa-blog.example.com` → 输出标题 + 动态内容

**关键决策**：
- 同步 fetch + relative URL resolve
- JS 在渲染前执行

---

## M5 — GUI 基础 ✅ (179 tests)

**目标**：跨平台开窗浏览

**验收**：
- `./browser open https://example.com` → GUI 窗口弹出
- `--check` flag 无显示器模式 + 4 个 e2e 测试

**关键决策**：
- `winit` + `softbuffer`
- 5x7 bitmap_font（手写 90 个 ASCII glyph）
- 放弃 `tiny-skia`（0.11 API break）

---

## M6 — 渲染质量修复 ✅ (211 tests)

**目标**：长段落折行 + `<head>` 不渲染 + `<li>` 前缀

**验收**：
- `python3 tests/snapshots/compare.py` → 6 个对比素材弹出
- `UPDATE_SNAPSHOTS=1 cargo test -p browser-cli --test integration_snapshot` → 重生成黄金对比

**子任务**：
- M6.0a: 修长段落折行（renderer 读 `box.words`）
- M6.0b: `<head>` 子树不渲染
- M6.0c: `<li>` 前缀 `• ` + 段后空行 heuristic
- M6.0d: 对比图脚本 + 重生成
- M6.1: 黄金对比测试（integration_snapshot.rs）
- M6.2: 跨平台 CI（Linux GUI deps + 移除 tiny-skia）
- M6.3: 收尾

---

## M7 — 渲染质量 + 交互 ✅ (217 tests)

**目标**：CSS margin/padding + DOM API + 真实字体 + URL 栏 + 滚动

**验收**：
- `./browser render-file tests/fixtures/dom-api.html --width 80` → 包含 "Hello from JS"（DOM API）
- `./browser render-file tests/fixtures/chinese-font.html --width 80` → 中文渲染成功
- URL 栏显示 "https://" placeholder，键盘输入可更新

**子任务**：
- M7.1 ✅ (8 commits): CSS margin/parsing/LayoutBox/construct/block/collapsing/horizontal/UA defaults/<style> extraction/anon skip/e2e tests/snapshots
- M7.2 ✅ (3 commits): DOM API bridges (`__createEl/__appendChild/__setAttr/__getElById/__qs/__setText/__getTag/__getBody`) + selector enhancement (`*` / `.class` / compound)
- M7.5 ✅ (6 commits): URL 栏渲染 + 键盘输入 + MouseWheel/PageUp/PageDown/Home/End + long-scroll fixture
- M7.4 ✅ (5 commits): fontdue 集成 + DejaVuSans.ttf 757KB 嵌入 + 中文支持验证
- M7.3 🟡 defer: 异步 JS（MicrotaskQueue 写完但 boa 0.20 API 复杂）

**关键改进**：
- 真实 CSS margin/padding 支持（UA defaults + collapsing）
- 完整 DOM API 桥
- GUI URL 栏 + 键盘输入 + 滚动
- 真实字体（DejaVuSans 757KB，fontdue 抗锯齿，中文支持）

---

## M8 — 表单交互 ✅ (217 tests)

**目标**：input/textarea/button/form 支持

**验收**：
- `./browser render-script tests/fixtures/form-interaction.html --width 80` → 日志 `[dom-click] #15` + `[dom-submit] #11` + 输出 "Form submitted"

**子任务**：
- M8.1 ✅ (4010ff0): `__getValue/__setValue` 桥
- M8.2 ✅ (7bc4b76): textarea 支持（通过 `__getValue`）
- M8.3 ✅ (7bc4b76): `__click` 桥
- M8.4 ✅ (2dbae55): `__submit` 桥
- M8.5 ✅ (6bd8a24): e2e 测试（form-interaction.html）

**Note**: onclick/onsubmit handler execution pending（需要 JS 事件系统）

---

## M9 — 图片渲染 🟡 (进行中)

**目标**：<img> 标签渲染（占位符 → 真实图像）

**验收**：
- `<img src="logo.png">` → 渲染 `[IMG: logo.png]` 占位符（M9.1）
- `<img src="https://example.com/logo.png">` → fetch 二进制图像（M9.2）
- 图像解码 + 绘制（M9.3）

**子任务**：
- M9.1: <img> 标签占位符渲染（进行中）
- M9.2: fetch 二进制图像（png/jpg）
- M9.3: 图像解码 + 绘制
- M9.4: img src 属性解析
- M9.5: e2e 测试

---

## M10 — 性能优化 ⚪

**目标**：layout 缓存 + 增量渲染

---

## SPA 爬虫满足条件（M4 已达成）

**当前能力**（M8 完成）：
- ✅ HTTP 获取（hyper + rustls）
- ✅ HTML 解析（html5ever wrapper）
- ✅ DOM 树构建（arena pattern）
- ✅ JS 执行（boa_engine，<script> 标签）
- ✅ 布局引擎（block + inline + 折行 + margin/padding collapsing）
- ✅ 文本渲染（ASCII + GUI 窗口 + 真实字体）
- ✅ CSS 选择器（tag/class/id/compound）
- ✅ DOM API 桥（createEl/appendChild/setAttribute/innerHTML/getValue/setValue/click/submit）
- ✅ 异步 fetch 桥（__fetchSetBody/__fetchAppendBody）
- ✅ 表单交互（input/textarea/button/form）

**待完成**：
- 🟡 图片渲染（M9）
- ⚪ 性能优化（M10）

**结论**：M4 已满足 SPA 爬虫核心需求，M7-8-9 为增强功能。

---

## 下一步

**当前执行**：M9.1（<img> 占位符渲染）
**下一个**：M9.2（fetch 二进制图像）