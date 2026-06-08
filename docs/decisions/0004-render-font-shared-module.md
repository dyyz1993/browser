# 0004. 文本渲染：render::font 共享模块

- **状态：** accepted
- **日期：** 2026-06-08
- **触发里程碑：** M34

## 背景（Context）

M12 引入截图功能时，`screenshot.rs`（CLI crate）内联实现了 fontdue 矢量文本渲染——
Font::from_bytes + metrics + rasterize + M25 坐标公式（`y_origin = baseline - ymin - height + 1`）。

M5-M7 GUI crate（gui/window.rs）用的是独立的 5×7 手写 bitmap_font。两套渲染逻辑完全割裂：

1. **字形质量割裂**：GUI 用粗糙的 5×7 点阵，screenshot 用 16pt 矢量抗锯齿
2. **代码重复**：fontdue 初始化 + 坐标公式逻辑只在 screenshot.rs，gui 无法复用
3. **严重 bug（M34 发现）**：gui bitmap_font.rs 的 `draw_text` 对所有字符都调
   `rasterize_char('X')`——**所有字符都画成 'X' 的字形**，GUI 窗口无法辨认任何文字

M25 的坐标公式曾连续失败 5 次（猜公式 + 肉眼降采样"看起来能辨认"），最终用
文档语义推导 + 像素级 PIL 对照锁定。这套来之不易的逻辑不应只服务 screenshot.rs。

## 选项（Options）

### 选项 A：保持现状（screenshot.rs 保留，gui 单独修 bitmap_font bug）
- **优点**：改动最小，不破坏 screenshot.rs 已验证逻辑
- **缺点**：
  - gui 仍需独立实现/维护字形定位（M25 公式需复制）
  - 代码重复，未来两套逻辑可能漂移（M25 教训：字形坐标极易写错）
  - bitmap_font 整个文件质量低，维护负担

### 选项 B：render::font 共享模块（提取 fontdue 到 render crate）
- **优点**：
  - **单一真相源**：M25 公式只在一处，screenshot + gui 共用
  - gui 直接获得矢量抗锯齿质量（无需重写）
  - FontRenderer 封装 Font + metrics + glyph cache，API 干净
  - render crate 作为渲染层，font 模块归属合理
- **缺点**：
  - render crate 需加 fontdue 依赖（+ font.ttf 资源）
  - screenshot.rs 需迁移（回归风险：已有 M25 锁定测试）
  - gui 加 browser-render 依赖（编译时间略增）

### 选项 C：gui crate 内联 fontdue（各自复制一份）
- **优点**：无跨 crate 依赖
- **缺点**：最严重的代码重复，M25 公式双份维护

## 决策（Decision）

**选 选项 B：提取到 `render::font` 共享模块。**

理由优先级：
1. **M25 教训驱动**：坐标公式曾失败 5 次，最不应重复维护——单一真相源防漂移
2. **GUI bug 根治**：不是打补丁修 bitmap_font，而是用已验证的矢量渲染替换它
3. **架构归属合理**：render crate 是渲染层，font 模块天然属于这里

## 后果（Consequences）

**好处：**
- **单一真相源**：M25 公式 + metrics 测量只在 render::font，8 单元测试锁定
- **GUI 质量飞跃**：从 5×7 点阵 + 'X' bug → 16pt 矢量抗锯齿，每字符真实字形
- **API 清晰**：`FontRenderer::new()` + `render_text_to_rgba(text, link_spans)`
- **screenshot 不退化**：M25 公式锁定测试迁移到 render::font，端到端验证通过

**代价：**
- render crate 加 fontdue 依赖（~2s 编译增量）+ font.ttf（~340KB 嵌入资源）
- screenshot.rs 迁移（已验证不退化：483 tests + 端到端 PNG 生成正常）
- gui 加 browser-render 依赖（编译时间略增，可接受）

**验证：**
- render::font: 8 单元测试（含 M25 公式锁定 `glyph_vertical_position_formula_is_correct`
  + metrics 合理性 + link 蓝色渲染）
- screenshot: 端到端不退化（Pre link (https://x.com) post → 7.7K PNG）
- gui: `cargo build -p browser-gui` 编译通过（GUI 需显示器，CI 无法 e2e）

**后续要补的事：**
- 无：此决策是终局。screenshot + gui 都已迁移，M25 公式单一来源。

**commit 记录：**
- `195a0fc` feat(render): M34 GUI 像素级图像渲染（render::font 共享）
