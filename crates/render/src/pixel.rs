//! M80 路线 B：像素化近似渲染器（2D 画布，非 ASCII 字符网格）。
//!
//! 把 [`LayoutTree`] 直接光栅化为 RGBA 位图：背景色矩形 + border 边线 +
//! 按真实字号（UA font-size 阶梯，h1=2em 等）用 fontdue 逐字形绘制的文本。
//! 目标是简单站点（example.com 级别）的截图接近 Chrome 观感。
//!
//! ## 坐标系（cell → px 映射）
//!
//! layout 产出的坐标是"字符格"单位（1 x 单位 = 1 字符列，1 y 单位 = 1 文本行），
//! 不是像素。像素渲染的映射规则：
//! - `x_px = x_cells * CELL_W`，`CELL_W` = 基准字号下可打印 ASCII 的**平均**
//!   advance（≈9px @16px）。用平均而非最大值：布局按"每字符 1 格"折行，
//!   平均 advance 让真实绘制的文本宽度与折行预估一致（行尾不溢出）。
//! - `y_px = y_lines * LINE_H`，`LINE_H` = ascent+descent+LINE_GAP（同
//!   font.rs `measure_layout` 公式，≈25px @16px）。
//! - `scale` 全局乘子（1.0 = 1 格 1 px；文本过小时可传 2.0）。
//!
//! pixel 模式下 CLI `--width` 语义是 **CSS px**：先用
//! [`layout_columns_for_px`] 换算成布局列数喂给 `run_layout`，再原样把
//! px 宽度传给 [`render_pixel`]。ASCII 模式的 width（字符列）语义不变。
//!
//! ## 与 ASCII 路径的关系
//!
//! ASCII 输出是爬虫契约，本模块**不触碰** `ascii.rs`；仅复用
//! `font.rs` 的内嵌字体资产与 WCAG 对比度保障（`ensure_contrast`）。

use std::collections::HashMap;
use std::sync::OnceLock;

use browser_css_engine::{parse_color, parse_length, BoxEdges, Declaration, Length};
use browser_dom::NodeId;
use browser_layout::{BoxType, LayoutBox, LayoutTree};

use crate::font::{ensure_contrast, is_cjk_char, CJK_FONT_BYTES, FONT_BYTES, LINE_GAP};

/// M80: computed styles map（`compute_styles` 产物，含 UA 阶梯）。
pub type StyleMap = HashMap<NodeId, Vec<Declaration>>;

/// 基准字号（px）。UA 阶梯的 1em；与 `font.rs::FONT_SIZE` 对齐。
pub const BASE_FONT_PX: f32 = 16.0;
/// W3C link 蓝 #0000EE（与 font.rs LINK_BLUE 一致）。
const LINK_BLUE: (u8, u8, u8) = (0, 0, 238);
/// 画布像素上限（防御性：16M px ≈ 64MB RGBA）。超限截断高度。
const MAX_CANVAS_PIXELS: usize = 16_000_000;
/// 隐式白底（画布初始化色，与 font.rs WHITE 一致）。
const WHITE: (u8, u8, u8) = (255, 255, 255);

/// M80.1: 带透明度的颜色。
///
/// 背景：css-engine 的 `parse_color` 是 ASCII 路径契约（返回 `(u8,u8,u8)`，
/// **丢弃 alpha 字节**），`#0000000a`（4% 黑色阴影，vuejs.org 导航/hero 大量
/// 使用）经它一转就成了不透明黑 —— 首秀验证里"页面左侧/顶部大片黑色实块"
/// 的根因。pixel 模式必须从 styles 原始声明重新解析 alpha。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    /// 0.0（全透明）..=1.0（不透明）。
    pub a: f32,
}

impl Rgba {
    fn opaque(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    /// 叠在 `under`（视觉上最近的祖先背景，None = 白底）上的最终观感色。
    /// 用于文字对比度判定：半透明遮罩下的文字要按合成后的底色判断。
    fn composite_over(self, under: Option<(u8, u8, u8)>) -> (u8, u8, u8) {
        let u = under.unwrap_or(WHITE);
        if self.a >= 0.999 {
            return (self.r, self.g, self.b);
        }
        if self.a <= 0.001 {
            return u;
        }
        let a = (self.a * 255.0).round() as u32;
        (
            blend_channel(self.r, u.0, a),
            blend_channel(self.g, u.1, a),
            blend_channel(self.b, u.2, a),
        )
    }
}

/// 带 alpha 的颜色解析：8 位 hex / `rgba(r,g,b,a)` / `transparent`；
/// 其余交给 `parse_color` 兜底（不透明）。
fn parse_color_rgba(value: &str) -> Option<Rgba> {
    let lower = value.trim().to_ascii_lowercase();
    if lower == "transparent" {
        return Some(Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 0.0,
        });
    }
    if let Some(hex) = lower.strip_prefix('#') {
        let d = |t: &str| u8::from_str_radix(t, 16).ok();
        let expand = |t: &str| d(&format!("{0}{0}", &t[..1]));
        return match hex.len() {
            3 => Some(Rgba::opaque(
                expand(&hex[0..1])?,
                expand(&hex[1..2])?,
                expand(&hex[2..3])?,
            )),
            4 => {
                let a = expand(&hex[3..4])?;
                Some(Rgba {
                    a: f32::from(a) / 255.0,
                    ..Rgba::opaque(
                        expand(&hex[0..1])?,
                        expand(&hex[1..2])?,
                        expand(&hex[2..3])?,
                    )
                })
            }
            6 => Some(Rgba::opaque(d(&hex[0..2])?, d(&hex[2..4])?, d(&hex[4..6])?)),
            8 => {
                let a = d(&hex[6..8])?;
                Some(Rgba {
                    a: f32::from(a) / 255.0,
                    ..Rgba::opaque(d(&hex[0..2])?, d(&hex[2..4])?, d(&hex[4..6])?)
                })
            }
            _ => None,
        };
    }
    let func = |prefix: &str| {
        let rest = lower.strip_prefix(prefix)?;
        let inner = rest.strip_suffix(')')?;
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        let px = |t: &str| t.parse::<u8>().ok();
        let r = px(parts.first()?)?;
        let g = px(parts.get(1)?)?;
        let b = px(parts.get(2)?)?;
        let a = match parts.get(3) {
            Some(t) => parse_alpha_token(t)?,
            None => 1.0,
        };
        Some(Rgba { r, g, b, a })
    };
    func("rgba(")
        .or_else(|| func("rgb("))
        .or_else(|| parse_color(&lower).map(|(r, g, b)| Rgba::opaque(r, g, b)))
}

/// alpha 分量：`0.5` / `50%`，clamp 到 0..=1。
fn parse_alpha_token(t: &str) -> Option<f32> {
    let t = t.trim();
    let v = if let Some(p) = t.strip_suffix('%') {
        p.trim().parse::<f32>().ok()? / 100.0
    } else {
        t.parse::<f32>().ok()?
    };
    Some(v.clamp(0.0, 1.0))
}

/// 元素自身的背景声明（含 alpha），按声明顺序 fold、最后一个生效
/// （镜像 construct.rs::apply_box_style 的 last-write-wins）。
/// 找不到声明时由调用方回退 `bx.style.background`（不透明，M39 路径）。
/// M80.4: 本盒自身 font-style 声明是否 italic（UA 表 em/i 系声明）。
fn own_font_style_italic(styles: &StyleMap, id: Option<NodeId>) -> bool {
    let Some(id) = id else { return false };
    let Some(decls) = styles.get(&id) else {
        return false;
    };
    decls.iter().any(|d| {
        d.property.eq_ignore_ascii_case("font-style")
            && d.value.trim().eq_ignore_ascii_case("italic")
    })
}

/// M80.3: 本盒自身 font-weight 声明（不继承）——"bold"/">=600" → 700，
/// normal/lighter/数值 → 400。UA 表对 strong/b/th/h1-h6 都有声明。
fn own_font_weight(styles: &StyleMap, id: Option<NodeId>) -> Option<u16> {
    let id = id?;
    let decls = styles.get(&id)?;
    for d in decls.iter().rev() {
        if d.property.eq_ignore_ascii_case("font-weight") {
            let v = d.value.trim().to_ascii_lowercase();
            return Some(match v.as_str() {
                "bold" | "bolder" => 700,
                "normal" | "lighter" => 400,
                _ => {
                    let n: u16 = v.parse().unwrap_or(400);
                    if n >= 600 {
                        700
                    } else {
                        400
                    }
                }
            });
        }
    }
    None
}

/// M80.3: 本盒自身 text-align:center（近似居中用）。
fn own_text_align_center(styles: &StyleMap, id: Option<NodeId>) -> bool {
    let Some(id) = id else { return false };
    let Some(decls) = styles.get(&id) else {
        return false;
    };
    decls.iter().any(|d| {
        d.property.eq_ignore_ascii_case("text-align")
            && d.value.trim().eq_ignore_ascii_case("center")
    })
}

fn own_background(styles: &StyleMap, id: Option<NodeId>) -> Option<Rgba> {
    let decls = styles.get(&id?)?;
    let mut out = None;
    for d in decls {
        let p = d.property.to_ascii_lowercase();
        if p == "background-color" || p == "background" {
            if let Some(c) = parse_color_rgba(&d.value) {
                out = Some(c);
            }
        }
    }
    out
}
/// 内嵌主字体（进程内只解析一次）。
static MAIN_FONT: OnceLock<fontdue::Font> = OnceLock::new();
/// 单元格度量缓存。
static CELL_METRICS: OnceLock<(f32, f32)> = OnceLock::new();

fn main_font() -> &'static fontdue::Font {
    MAIN_FONT.get_or_init(|| {
        fontdue::Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default())
            .expect("embedded font.ttf must parse")
    })
}

/// 返回 `(cell_w, line_h)`：基准字号下 1 个布局 x 单位 / y 单位的像素宽度。
/// `cell_w` = 可打印 ASCII 平均 advance；`line_h` = ascent+descent+LINE_GAP。
#[must_use]
pub fn cell_metrics() -> (f32, f32) {
    *CELL_METRICS.get_or_init(|| {
        let font = main_font();
        let cell_w = {
            let mut total = 0.0_f32;
            let mut n = 0_u32;
            for c in (32u8..=126).map(|b| b as char) {
                total += font.metrics(c, BASE_FONT_PX).advance_width;
                n += 1;
            }
            (total / n as f32).max(1.0)
        };
        let line_h = font
            .horizontal_line_metrics(BASE_FONT_PX)
            .map(|lm| lm.ascent.ceil() + (-lm.descent).ceil() + LINE_GAP as f32)
            .unwrap_or(BASE_FONT_PX * 1.55)
            .max(1.0);
        (cell_w, line_h)
    })
}

/// pixel 模式布局列数换算：CSS px 宽度 → `run_layout` 需要的字符列数。
/// 布局按"每字符 1 格"折行，`cell_w` px/格，所以 `cols = px / cell_w`。
#[must_use]
pub fn layout_columns_for_px(px_width: usize) -> usize {
    let (cell_w, _) = cell_metrics();
    ((px_width as f32 / cell_w).round() as usize).max(1)
}

/// M81: 屏幕坐标命中测试——CSS px 坐标 → 最深层带 `element_id` 的布局盒。
///
/// 入参是 CSS px（与 pixel 模式 `--width` 语义一致，scale=1.0）；布局坐标
/// 是字符格单位，按 [`cell_metrics`] 的 `cell_w`/`line_h` 换算（与
/// [`render_pixel`] 相同的映射，保证"看到的盒子"和"命中的盒子"一致）。
///
/// 算法：深度优先遍历盒树，**子盒优先**（更深的 DOM 节点接收事件——CSS
/// 命中语义的最内层元素规则）；子树都不含点时本盒若包含点且带
/// `element_id` 则命中（Anonymous 等无 id 盒自动跳过，命中落到其祖先）。
/// 右/下边缘半开区间（`x < x0+w`），零宽/高盒不命中。
///
/// 供 CLI `--click` 与 CDP `Input.dispatchMouseEvent`（坐标→NodeId）复用。
#[must_use]
pub fn hit_test(tree: &LayoutTree, x_px: f32, y_px: f32) -> Option<NodeId> {
    let (cell_w, line_h) = cell_metrics();
    if !x_px.is_finite() || !y_px.is_finite() || cell_w <= 0.0 || line_h <= 0.0 {
        return None;
    }
    let xc = x_px / cell_w;
    let yc = y_px / line_h;
    deepest_hit(&tree.root, xc, yc)
}

/// [`hit_test`] 的递归体：`(x, y)` 已换算为格单位。子树有命中（含跨越
/// 无 id 盒落到祖先的命中）时以子树为准，否则本盒自匹配。
fn deepest_hit(bx: &LayoutBox, xc: f32, yc: f32) -> Option<NodeId> {
    for child in &bx.children {
        if let Some(id) = deepest_hit(child, xc, yc) {
            return Some(id);
        }
    }
    let d = &bx.dimensions;
    if d.width > 0.0
        && d.height > 0.0
        && xc >= d.x
        && xc < d.x + d.width
        && yc >= d.y
        && yc < d.y + d.height
    {
        return bx.element_id;
    }
    None
}

/// M80: 把布局树光栅化为 RGBA 位图。
///
/// - `styles`：`compute_styles` 产物（含 UA 阶梯），用于读每元素的
///   `font-size` 比率、`color`、`background`；text 叶子节点不在 map 里
///   → 沿盒树继承父元素的生效值。
/// - `viewport_width`：CSS px（pixel 模式 `--width` 语义）。
/// - `scale`：全局像素乘子（默认 1.0）。
///
/// 返回 `(width, height, rgba)`。
#[must_use]
pub fn render_pixel(
    tree: &LayoutTree,
    styles: &StyleMap,
    viewport_width: usize,
    scale: f32,
) -> (usize, usize, Vec<u8>) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let (cell_w, line_h) = cell_metrics();
    let cw = cell_w * scale;
    let lh = line_h * scale;

    // 画布尺寸：宽 = viewport px；高 = 树的最大 bottom（含背景盒），
    // 再加大字号行（h1=2em 等）向下溢出行槽的余量——布局按基准行高
    // 计数，32px 字形的 descender 会超出 (sy+1)*line_h。
    let img_w = ((viewport_width.max(1) as f32) * scale).round() as usize;
    let mut max_bottom = 1.0_f32;
    let max_ratio = collect_extent(&tree.root, styles, 1.0, &mut max_bottom);
    let max_px = BASE_FONT_PX * max_ratio.clamp(1.0, 10.0);
    let (a_max, d_max) = line_extents(max_px);
    let overflow = (a_max + d_max - lh).max(0.0);
    let mut img_h = ((max_bottom * lh + overflow).ceil() as usize).max(1);
    // 防御性截断：避免病态大树撑爆内存。
    if img_w * img_h > MAX_CANVAS_PIXELS {
        img_h = (MAX_CANVAS_PIXELS / img_w).max(1);
    }

    let mut canvas = Canvas::new(img_w, img_h);
    {
        let mut painter = Painter {
            canvas: &mut canvas,
            styles,
            cell_w: cw,
            line_h: lh,
            glyphs: GlyphCache::default(),
        };
        paint_box(&mut painter, &tree.root, &Inherited::default());
    }
    // 裁掉底部因动态扩高留下的空白行（纯 255；声明的白底视觉相同）。
    while canvas.h > 1 {
        let row = &canvas.buf[(canvas.h - 1) * canvas.w * 4..canvas.h * canvas.w * 4];
        if row.iter().any(|&v| v != 255) {
            break;
        }
        canvas.h -= 1;
    }
    canvas.buf.truncate(canvas.w * canvas.h * 4);
    (canvas.w, canvas.h, canvas.buf)
}

/// 渲染中继承下来的上下文（CSS 继承属性的近似）。
#[derive(Clone, Copy)]
struct Inherited {
    /// 生效文字色（style.color 继承链；None = 默认黑）。
    color: Option<(u8, u8, u8)>,
    /// 视觉上最近的祖先背景色（用于文字对比度判定；None = 白底）。
    bg: Option<(u8, u8, u8)>,
    /// 生效 font-size 比率（vs [`BASE_FONT_PX`]；h1=2.0 等）。
    font_ratio: f32,
    /// M80.3: 生效字重（700 = 粗体——strong/b 或 font-weight:>=600）。
    /// fontdue 无多字重字形，粗体用 ±1px 双描边近似（笔画加厚）。
    weight: u16,
    /// M80.3: text-align:center 继承（text-align 是继承属性；声明盒的
    /// 后代文本叶都居中——paint_text 用该标志 + 盒宽做居中偏移）。
    centered: bool,
    /// M80.4: font-style:italic 继承（UA 表 em/i/cite/var/dfn 声明）。
    /// 无真斜体字形 → 逐行右移的斜切变换近似（ shear ≈ 高度的 1/6）。
    italic: bool,
}

impl Default for Inherited {
    fn default() -> Self {
        Self {
            color: None,
            bg: None,
            font_ratio: 1.0,
            weight: 400,
            centered: false,
            italic: false,
        }
    }
}

impl Inherited {
    fn font_px(&self) -> f32 {
        (BASE_FONT_PX * self.font_ratio.clamp(0.05, 10.0)).min(160.0)
    }
}

struct Painter<'a> {
    canvas: &'a mut Canvas,
    styles: &'a StyleMap,
    cell_w: f32,
    line_h: f32,
    glyphs: GlyphCache,
}

/// M80: 遍历并绘制。顺序：本盒背景/border → 子盒子 → 本盒文字
/// （文字最后画，保证文字覆盖在子背景之上；对齐 ascii.rs 的层级直觉）。
fn paint_box(p: &mut Painter, bx: &LayoutBox, inh: &Inherited) {
    // 1) 本盒自身样式 + 继承合并。
    //    背景：优先从 styles 原始声明解析（保留 alpha，M80.1 —— css-engine
    //    的 parse_color 丢 alpha 字节）；无声明时回退 bx.style.background
    //    （不透明，construct.rs::apply_box_style 的 M39 产物）。
    //    color 走 bx.style；font-size 只在 styles map 里。
    let box_bg: Option<Rgba> = own_background(p.styles, bx.element_id)
        .or_else(|| bx.style.background.map(|c| Rgba::opaque(c.r, c.g, c.b)));
    // 生效观感底色 = 本盒背景叠在最近祖先背景上（半透明遮罩下文字的
    // 对比度必须按合成后的底色判断）。
    let eff_bg: Option<(u8, u8, u8)> = box_bg.map(|c| c.composite_over(inh.bg));
    // M80.3: 字重判定——纯 CSS 语义（UA 表对 strong/b/th 和 h1-h6 都声明
    // font-weight: bold；作者样式同名规则覆盖之）。>=600 视为粗体。
    let weight = match own_font_weight(p.styles, bx.element_id) {
        Some(w) => w,
        None => inh.weight,
    };
    let cur = Inherited {
        color: bx.style.color.map(|c| (c.r, c.g, c.b)).or(inh.color),
        // 背景本身不继承，但"文字背后的最近背景"沿祖先链走（对比度判定用）。
        bg: eff_bg.or(inh.bg),
        font_ratio: font_ratio_of(p.styles, bx.element_id, inh.font_ratio),
        weight,
        // M80.3: text-align 继承——本盒声明或父级已居中。
        centered: own_text_align_center(p.styles, bx.element_id) || inh.centered,
        // M80.4: font-style 继承——本盒 italic 声明或父级已斜体。
        italic: own_font_style_italic(p.styles, bx.element_id) || inh.italic,
    };

    // 2) 背景 + border（先父后子）。全透明（a=0）的盒不画（否则 #00000000
    // 会画出黑块；不透明画回退见上）。
    let d = &bx.dimensions;
    if d.width > 0.0 && d.height > 0.0 {
        let x0 = (d.x * p.cell_w).round() as i32;
        let y0 = (d.y * p.line_h).round() as i32;
        let w = (d.width * p.cell_w).round().max(0.0) as i32;
        let h = (d.height * p.line_h).round().max(0.0) as i32;
        if let Some(bg) = box_bg {
            p.canvas.fill_rect_rgba(x0, y0, w, h, bg);
        }
        let b = &bx.style.border;
        if b.top || b.bottom || b.left || b.right {
            let ink = contrast_ink((40, 40, 40), eff_bg.or(inh.bg));
            p.canvas.draw_border(x0, y0, w, h, b, ink);
        }
    }

    // 3) 子盒子。
    for child in &bx.children {
        paint_box(p, child, &cur);
    }

    // 4) 本盒文字（仅 Inline，同 ascii.rs 的规则）。
    if bx.box_type == BoxType::Inline {
        let ink = if bx.link {
            contrast_ink(LINK_BLUE, cur.bg)
        } else {
            contrast_ink(cur.color.unwrap_or((0, 0, 0)), cur.bg)
        };
        paint_text(p, bx, &cur, ink);
    }
}

/// 生效墨色：对比度不足时按 WCAG 翻转黑白（复用 font.rs 批 29 策略）。
fn contrast_ink(ink: (u8, u8, u8), bg: Option<(u8, u8, u8)>) -> (u8, u8, u8) {
    ensure_contrast(ink, bg.unwrap_or((255, 255, 255)))
}

/// 一个词按 `font_px` 字形 advance 累计的绘制宽度（换行判断用）。
fn word_width(p: &mut Painter, word: &str, font_px: f32) -> f32 {
    word.chars()
        .filter(|&c| c != '\n' && c != '\r')
        .map(|c| p.glyphs.get(c, font_px).0.advance_width)
        .sum()
}

/// 文本绘制：`words` 逐词按 (sx, sy) 定位，字形间用真实 advance
/// （不再对齐字符网格）。链接加下划线。`[IMG`/`[SVG` 占位符跳过
/// （ASCII 后处理专属产物，像素图里是噪声）。
/// M80.5: 居中偏移按当前字号实测行宽计算（shrink-to-fit 缩字号后行宽
/// 变窄，eager 版用旧字号 slack 为负 → h1 大字号居中失效）。
#[allow(clippy::too_many_arguments)]
fn center_shifts_for(
    entries: &[(String, f32, f32)],
    centered: bool,
    box_left: f32,
    avail: f32,
    p: &mut Painter,
    font_px: f32,
) -> Vec<f32> {
    if !centered {
        return Vec::new();
    }
    let space_w = p.glyphs.get(' ', font_px).0.advance_width;
    let mut row_w = 0.0_f32;
    let mut rows: Vec<(f32, f32)> = Vec::new(); // (sy, row_width)
    let mut prev: Option<f32> = None;
    for (word, _sx, sy) in entries {
        if prev != Some(*sy) {
            if let Some(pw) = prev {
                rows.push((pw, row_w));
                row_w = 0.0;
            }
            prev = Some(*sy);
        }
        row_w += word_width(p, word, font_px) + space_w;
    }
    if let Some(pw) = prev {
        rows.push((pw, row_w));
    }
    entries
        .iter()
        .map(|(_, sx, sy)| {
            let rw = rows
                .iter()
                .find(|(psy, _)| (psy - sy).abs() < 0.5)
                .map(|(_, w)| *w)
                .unwrap_or(0.0);
            let slack = (avail - rw).max(0.0);
            (slack / 2.0 - (sx * p.cell_w - box_left)).max(0.0)
        })
        .collect()
}

fn paint_text(p: &mut Painter, bx: &LayoutBox, inh: &Inherited, ink: (u8, u8, u8)) {
    let mut font_px = inh.font_px();
    let entries: Vec<(String, f32, f32)> = if !bx.words.is_empty() {
        bx.words.clone()
    } else if let Some(t) = &bx.text {
        // 未走 word-wrap 的兜底（同 ascii.rs：直接从 box 原点排布）。
        vec![(t.clone(), bx.dimensions.x, bx.dimensions.y)]
    } else {
        Vec::new()
    };

    // 同一行内的词按"布局位置 vs 前一词末端+空格"取 max 定位：
    // 布局的词位按"每字符 1 格"计算，而真实字形按字号放大（h1=2em 时
    // 字宽翻倍），直接跳布局位会让同一盒内的词互相叠印（example.com
    // 标题 "Example Domain" 变 "EXAMIPLEDOMAIN" 的根因）。
    // IMG/SVG 占位符是 ASCII 后处理专属产物：按盒整体跳过（占位符
    // 是盒的全部文本，词级判断会漏掉被折行拆开的 "100x100]" 残段）。
    if entries
        .first()
        .is_some_and(|(w, _, _)| w.starts_with("[IMG") || w.starts_with("[SVG"))
    {
        return;
    }
    let space_w = p.glyphs.get(' ', font_px).0.advance_width;
    // M80.1: 盒内行距 pitch —— 大字号行（h1=2em 等）字形高约 1.5 个布局
    // 行槽，同盒多行仍按 25px 布局行距画会自我叠印（docusaurus hero 两行
    // 大标题糊成一团的根因）。盒内行距按字形实际高度展开。
    let (mut a_px, mut d_px) = line_extents(font_px);
    let mut pitch = (a_px + d_px + 4.0).max(p.line_h);
    let mut pitch_extra = pitch - p.line_h; // >0 仅当字号 > 基准（标题类）
                                            // M80.1: 行绘制宽超页宽 → 整盒按比例缩小字号以适配页宽（Chrome
                                            // 会重新布局折行；我们的布局按格数算无法重排，截断丢字、折行又
                                            // 会与下方内容叠印——vuejs hero 教训。缩放保住"完整 + 不叠印"。
                                            // M80.2: 从"仅大字号（pitch_extra>0）"扩展到**任何宽超限盒**——
                                            // CJK 布局按 1 格/字但字形 advance ≈1.7 格，普通字号中文行同样
                                            // 溢出画布（右边界截断）。Latin 正文天然不超（1 词/格≈advance），
                                            // 不受影响。
    let page_right = p.canvas.w as f32;
    // M80.3: text-align:center —— 布局不消费该属性，这里按"盒内容宽 -
    // 行已排宽"的一半右移首词起点（近似：整盒内容一起居中，多行时每行
    // 都以盒左缘为基准 → 与 Chrome 逐行居中有差，但标题/单行场景正确）。
    let centered = inh.centered;
    // M80.3: 居中实现——按行实测文字 advance 总宽，行首词起点右移
    // "可用宽 - 行宽" 的一半（可用宽 = 盒右缘 - 盒左缘）。
    // M80.5: 惰性计算——必须在 shrink-to-fit 之后调（字号被缩小后行宽
    // 变窄，旧 eager 版用缩小前字号算 slack，h1 大字号场景 slack 为负
    // 被吃掉 → 居中失效）。
    let box_left = bx.dimensions.x * p.cell_w;
    let avail = (bx.dimensions.width * p.cell_w).max(1.0);
    let mut centered_shifts: Vec<f32> =
        center_shifts_for(&entries, centered, box_left, avail, p, font_px);
    if !entries.is_empty() {
        let mut max_need = 0.0_f32;
        let mut min_start = f32::MAX;
        let mut prev: Option<f32> = None;
        let mut cur_start = 0.0_f32;
        let mut cur_w = 0.0_f32;
        for (word, sx, sy) in &entries {
            if prev != Some(*sy) {
                if prev.is_some() {
                    max_need = max_need.max(cur_start + cur_w);
                }
                cur_start = sx * p.cell_w;
                cur_w = 0.0;
                min_start = min_start.min(cur_start);
                prev = Some(*sy);
            }
            cur_w += word_width(p, word, font_px) + space_w;
        }
        max_need = max_need.max(cur_start + cur_w);
        let avail = (page_right - min_start.min(page_right)).max(1.0);
        if max_need > avail {
            let f = avail / max_need;
            font_px *= f;
            let (a2, d2) = line_extents(font_px);
            a_px = a2;
            d_px = d2;
            pitch = (a_px + d_px + 4.0).max(p.line_h);
            pitch_extra = pitch - p.line_h;
            // M80.5: 字号变了 → 居中偏移按新字号重算。
            centered_shifts = center_shifts_for(&entries, centered, box_left, avail, p, font_px);
        }
    }
    let mut prev_sy: Option<f32> = None;
    let mut extra_y = 0.0_f32;
    let mut pen_end = 0.0_f32;
    for (entry_idx, (word, sx, sy)) in entries.into_iter().enumerate() {
        let same_line = prev_sy == Some(sy);
        if !same_line {
            if let Some(ps) = prev_sy {
                extra_y += (sy - ps) * pitch_extra;
            }
        }
        // 同一布局行内的词按"布局位置 vs 前一词末端+空格"取 max 定位：
        // 布局的词位按"每字符 1 格"计算，而真实字形按字号放大（h1=2em
        // 时字宽翻倍），直接跳布局位会让同盒的词互相叠印。
        let cshift = centered_shifts.get(entry_idx).copied().unwrap_or(0.0);
        let mut pen_x = if same_line {
            (sx * p.cell_w + cshift).max(pen_end + space_w)
        } else {
            sx * p.cell_w + cshift
        };
        prev_sy = Some(sy);
        let baseline = (sy * p.line_h + extra_y + a_px).round() as i32;
        p.canvas
            .ensure_height((baseline + d_px.ceil() as i32 + 2).max(0) as usize);
        let word_start = pen_x.round() as i32;
        for ch in word.chars() {
            if ch == '\n' || ch == '\r' {
                continue;
            }
            let (m, mask) = p.glyphs.get(ch, font_px);
            if m.width > 0 && m.height > 0 {
                let gx = pen_x.round() as i32 + m.xmin;
                // M25 公式（font.rs 锁定）：y_origin = baseline - ymin - height + 1
                let gy = baseline - m.ymin - m.height as i32 + 1;
                // 斜率 0.2（≈11° oblique），总位移 = 0.2 × 字形高。
                let shear = if inh.italic { 0.2 } else { 0.0 };
                p.canvas.blend_glyph(gx, gy, &m, &mask, ink, shear);
                // M80.3: 粗体近似——weight>=700 时同一字形在 x+1 再压一次
                // （笔画加厚 1px；fontdue 单字重，无真粗体字形可用的近似）。
                if inh.weight >= 700 {
                    p.canvas.blend_glyph(gx + 1, gy, &m, &mask, ink, shear);
                }
            }
            pen_x += m.advance_width;
        }
        pen_end = pen_x;
        if bx.link {
            // 链接下划线：baseline 下方 2px。
            let uw = (pen_x.round() as i32 - word_start).max(1);
            p.canvas.fill_rect(word_start, baseline + 2, uw, 1, ink);
        }
    }
}

// ── 样式提取 ──

/// font-size 比率的两种语义：em/%（相对父字号）与 px/rem（相对根）。
#[derive(Clone, Copy)]
enum FontRatio {
    Em(f32),
    Absolute(f32),
}

/// 某元素自身的 font-size 声明（无声明 = None → 纯继承）。
fn own_font_size(styles: &StyleMap, id: Option<NodeId>) -> Option<FontRatio> {
    let decls = styles.get(&id?);
    decls.and_then(|ds| {
        ds.iter()
            .rev()
            .find(|d| d.property.eq_ignore_ascii_case("font-size"))
            .map(|d| parse_font_ratio(&d.value))
    })
}

/// 合并继承得到生效比率：em/% 乘父比率，px/rem 直接替换。
fn font_ratio_of(styles: &StyleMap, id: Option<NodeId>, inherited: f32) -> f32 {
    match own_font_size(styles, id) {
        Some(FontRatio::Em(r)) => inherited * r,
        Some(FontRatio::Absolute(r)) => r,
        None => inherited,
    }
}

/// font-size 值 → 比率。`2em`→Em(2.0)，`150%`→Em(1.5)，
/// `32px`→Absolute(2.0)，`1rem`→Absolute(1.0)。镜像 construct.rs::font_size_ratio。
fn parse_font_ratio(value: &str) -> FontRatio {
    match parse_length(value) {
        Some(Length::Em(v)) => FontRatio::Em(v),
        Some(Length::Percent(v)) => FontRatio::Em(v / 100.0),
        Some(Length::Px(v)) => FontRatio::Absolute(v / BASE_FONT_PX),
        Some(Length::Zero) => FontRatio::Absolute(0.0),
        _ => FontRatio::Em(1.0),
    }
}

// ── 画布 ──

struct Canvas {
    w: usize,
    h: usize,
    buf: Vec<u8>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            buf: vec![255u8; w * h * 4],
        }
    }

    /// 动态扩高：大字号折行/行距展开会把文字画到初始高度以下。按 1.5 倍
    /// 或 min_h+64 增长，受 [`MAX_CANVAS_PIXELS`] 约束。新增区域填白。
    fn ensure_height(&mut self, min_h: usize) {
        if min_h <= self.h || self.w.saturating_mul(self.h) >= MAX_CANVAS_PIXELS {
            return;
        }
        let mut new_h = (self.h + self.h / 2).max(min_h + 64);
        if self.w.saturating_mul(new_h) > MAX_CANVAS_PIXELS {
            new_h = (MAX_CANVAS_PIXELS / self.w).max(1);
        }
        if new_h <= self.h {
            return;
        }
        self.buf.resize(self.w * new_h * 4, 255);
        self.h = new_h;
    }

    /// 填充实心矩形（不透明，裁剪到画布）。border / 链接下划线用。
    fn fill_rect(&mut self, x0: i32, y0: i32, w: i32, h: i32, c: (u8, u8, u8)) {
        self.fill_rect_rgba(x0, y0, w, h, Rgba::opaque(c.0, c.1, c.2));
    }

    /// 填充带透明度的实心矩形：`out = c*a + dst*(1-a)`（dst 已含下层背景）。
    /// a=1 走快速覆盖路径；a=0 直接跳过（全透明不画）。
    fn fill_rect_rgba(&mut self, x0: i32, y0: i32, w: i32, h: i32, c: Rgba) {
        if w <= 0 || h <= 0 || c.a <= 0.001 {
            return;
        }
        let x_start = x0.max(0) as usize;
        let y_start = y0.max(0) as usize;
        let x_end = (x0 + w).min(self.w as i32).max(0) as usize;
        let y_end = (y0 + h).min(self.h as i32).max(0) as usize;
        if c.a >= 0.999 {
            for py in y_start..y_end {
                for px in x_start..x_end {
                    let idx = (py * self.w + px) * 4;
                    self.buf[idx] = c.r;
                    self.buf[idx + 1] = c.g;
                    self.buf[idx + 2] = c.b;
                    self.buf[idx + 3] = 255;
                }
            }
            return;
        }
        let a = (c.a * 255.0).round() as u32;
        for py in y_start..y_end {
            for px in x_start..x_end {
                let idx = (py * self.w + px) * 4;
                self.buf[idx] = blend_channel(c.r, self.buf[idx], a);
                self.buf[idx + 1] = blend_channel(c.g, self.buf[idx + 1], a);
                self.buf[idx + 2] = blend_channel(c.b, self.buf[idx + 2], a);
            }
        }
    }

    /// 画矩形 border（有标记的边各画 1px 线；盒过小时跳过，同 ascii.rs）。
    fn draw_border(
        &mut self,
        x0: i32,
        y0: i32,
        w: i32,
        h: i32,
        b: &BoxEdges<bool>,
        ink: (u8, u8, u8),
    ) {
        if w < 2 || h < 2 {
            return;
        }
        let x1 = x0 + w - 1;
        let y1 = y0 + h - 1;
        if b.top {
            self.fill_rect(x0, y0, w, 1, ink);
        }
        if b.bottom {
            self.fill_rect(x0, y1, w, 1, ink);
        }
        if b.left {
            self.fill_rect(x0, y0, 1, h, ink);
        }
        if b.right {
            self.fill_rect(x1, y0, 1, h, ink);
        }
    }

    /// alpha 合成一个字形：`out = ink*a + dst*(1-a)`（dst 已含背景）。
    fn blend_glyph(
        &mut self,
        gx: i32,
        gy: i32,
        m: &fontdue::Metrics,
        mask: &[u8],
        ink: (u8, u8, u8),
        shear: f32,
    ) {
        for dy in 0..m.height {
            let py = gy + dy as i32;
            if py < 0 || py >= self.h as i32 {
                continue;
            }
            // M80.4: 斜切（oblique 近似）——总水平位移 = shear px，
            // 按 dy 相对字形中线归一分布（顶部 -s/2 → 底部 +s/2），
            // 避免逐行累加导致字形散架/出界。
            let mid = m.height as f32 / 2.0;
            let rel = dy as f32 - mid;
            let row_shift = (shear * rel).round() as i32;
            for dx in 0..m.width {
                let alpha = u32::from(mask[dy * m.width + dx]);
                if alpha == 0 {
                    continue;
                }
                let px = gx + dx as i32 + row_shift;
                if px < 0 || px >= self.w as i32 {
                    continue;
                }
                let idx = (py as usize * self.w + px as usize) * 4;
                self.buf[idx] = blend_channel(ink.0, self.buf[idx], alpha);
                self.buf[idx + 1] = blend_channel(ink.1, self.buf[idx + 1], alpha);
                self.buf[idx + 2] = blend_channel(ink.2, self.buf[idx + 2], alpha);
            }
        }
    }
}

/// 单通道 alpha 合成（同 font.rs::blend_channel）。
fn blend_channel(ink: u8, dst: u8, a: u32) -> u8 {
    ((u32::from(ink) * a + u32::from(dst) * (255 - a)) / 255) as u8
}

/// 收集树的最大 bottom（cell 单位）与最大生效 font-size 比率（含继承）。
fn collect_extent(
    bx: &LayoutBox,
    styles: &StyleMap,
    inherited_ratio: f32,
    max_bottom: &mut f32,
) -> f32 {
    let ratio = font_ratio_of(styles, bx.element_id, inherited_ratio);
    let bottom = bx.dimensions.y + bx.dimensions.height;
    if bottom > *max_bottom {
        *max_bottom = bottom;
    }
    let mut best = ratio;
    for child in &bx.children {
        best = best.max(collect_extent(child, styles, ratio, max_bottom));
    }
    best
}

/// (ascent, descent) at `font_px`，descent 取正值。
fn line_extents(font_px: f32) -> (f32, f32) {
    match main_font().horizontal_line_metrics(font_px) {
        Some(lm) => (lm.ascent.ceil(), (-lm.descent).ceil()),
        None => ((font_px * 0.9).ceil(), (font_px * 0.25).ceil()),
    }
}

// ── 字形缓存（按 (char, size) 缓存，CJK 回退同 font.rs）──

#[derive(Default)]
struct GlyphCache {
    cache: HashMap<(char, u32), (fontdue::Metrics, Vec<u8>)>,
    cjk: Option<fontdue::Font>,
}

impl GlyphCache {
    fn get(&mut self, ch: char, size: f32) -> (fontdue::Metrics, Vec<u8>) {
        let key = (ch, (size * 4.0).round() as u32); // 0.25px 粒度
        if let Some(hit) = self.cache.get(&key) {
            return hit.clone();
        }
        let (m, mask) = if is_cjk_char(ch) {
            self.cjk_font().rasterize(ch, size)
        } else {
            let m = main_font().metrics(ch, size);
            if m.width == 0 || m.height == 0 {
                self.cjk_font().rasterize(ch, size)
            } else {
                main_font().rasterize(ch, size)
            }
        };
        self.cache.insert(key, (m, mask.clone()));
        (m, mask)
    }

    fn cjk_font(&mut self) -> &fontdue::Font {
        self.cjk.get_or_insert_with(|| {
            fontdue::Font::from_bytes(CJK_FONT_BYTES, fontdue::FontSettings::default())
                .expect("embedded cjk.ttf must parse")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_layout::{Dimensions, RgbColor};

    fn decl(prop: &str, value: &str) -> Declaration {
        Declaration {
            property: prop.into(),
            value: value.into(),
            important: false,
        }
    }

    fn inline_text(text: &str, x: f32, y: f32) -> LayoutBox {
        let mut b = LayoutBox::new(BoxType::Inline).with_text(text.into());
        b.dimensions = Dimensions::new(x, y, text.chars().count() as f32, 1.0);
        b.words = vec![(text.into(), x, y)];
        b
    }

    /// 模拟布局的 word-wrap：max_cols 列折行，1 格/字符 + 1 格词间空格，
    /// 产出 (word, sx, sy) 列表（镜像 layout_inline_run::place_word）。
    /// 真实管线的 words 是按词拆开的——测折行/行距必须用它，否则整行是
    /// 一个"超长词"条目，触发行首词不折行的保护。
    fn inline_wrapped_text(text: &str, x: f32, y: f32, max_cols: f32) -> LayoutBox {
        let mut b = LayoutBox::new(BoxType::Inline).with_text(text.into());
        let mut cx = x;
        let mut cy = y;
        let mut words = Vec::new();
        for w in text.split_whitespace() {
            let wl = w.chars().count() as f32;
            let at_start = cx == x;
            if !at_start && 1.0 + wl > x + max_cols - cx {
                cy += 1.0;
                cx = x;
            }
            if cx > x {
                cx += 1.0;
            }
            words.push((w.to_string(), cx, cy));
            cx += wl;
        }
        b.words = words;
        b.dimensions = Dimensions::new(x, y, max_cols, cy - y + 1.0);
        b
    }

    fn block_with(children: Vec<LayoutBox>) -> LayoutBox {
        let mut b = LayoutBox::new(BoxType::Block);
        let height: f32 = children.iter().map(|c| c.dimensions.height).sum();
        b.dimensions = Dimensions::new(0.0, 0.0, 40.0, height.max(1.0));
        b.children = children;
        b
    }

    fn count_dark(buf: &[u8]) -> usize {
        buf.chunks_exact(4)
            .filter(|px| px[0] < 128 && px[1] < 128 && px[2] < 128)
            .count()
    }

    fn count_exact(buf: &[u8], c: (u8, u8, u8)) -> usize {
        buf.chunks_exact(4)
            .filter(|px| (px[0], px[1], px[2]) == c)
            .count()
    }

    #[test]
    fn cell_metrics_are_sane() {
        let (cw, lh) = cell_metrics();
        assert!((6.0..=14.0).contains(&cw), "cell_w out of range: {cw}");
        assert!((18.0..=32.0).contains(&lh), "line_h out of range: {lh}");
    }

    #[test]
    fn layout_columns_for_px_round_trips() {
        let (cw, _) = cell_metrics();
        let cols = layout_columns_for_px(400);
        // 400px / ~9px ≈ 44 列；换算回 px 误差 < 1 格。
        assert!(((cols as f32 * cw) - 400.0).abs() <= cw);
        assert_eq!(layout_columns_for_px(0), 1);
    }

    #[test]
    fn background_rect_painted_exact_color() {
        // Block 盒 40x1 格，背景红。格子内任一点必须是精确 (255,0,0)。
        let mut root = block_with(vec![]);
        root.style.background = Some(RgbColor { r: 255, g: 0, b: 0 });
        let tree = LayoutTree { root };
        let (w, h, buf) = render_pixel(&tree, &StyleMap::new(), 400, 1.0);
        assert!(w > 0 && h > 0);
        assert!(count_exact(&buf, (255, 0, 0)) > 100, "red rect expected");
    }

    #[test]
    fn nested_child_background_over_parent() {
        // 父蓝底、子绿底（子区域被绿色覆盖）。
        let mut child = block_with(vec![]);
        child.style.background = Some(RgbColor { r: 0, g: 200, b: 0 });
        child.dimensions = Dimensions::new(1.0, 1.0, 5.0, 1.0);
        let mut root = block_with(vec![child]);
        root.style.background = Some(RgbColor { r: 0, g: 0, b: 200 });
        let tree = LayoutTree { root };
        let (_, _, buf) = render_pixel(&tree, &StyleMap::new(), 400, 1.0);
        assert!(count_exact(&buf, (0, 0, 200)) > 100, "parent blue expected");
        assert!(count_exact(&buf, (0, 200, 0)) > 50, "child green expected");
    }

    #[test]
    fn border_edges_drawn_dark() {
        let mut root = block_with(vec![]);
        root.dimensions = Dimensions::new(2.0, 2.0, 10.0, 3.0);
        root.style.border = BoxEdges::all(true);
        let tree = LayoutTree { root };
        let (w, _h, buf) = render_pixel(&tree, &StyleMap::new(), 400, 1.0);
        let (cw, lh) = cell_metrics();
        let at = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            (buf[i], buf[i + 1], buf[i + 2])
        };
        let x0 = (2.0 * cw).round() as usize;
        let y_mid = (3.0 * lh).round() as usize; // 盒内竖直中段
                                                 // 左边缘 = 暗色；盒内部 = 白。
        assert!(at(x0, y_mid).0 < 100, "left border pixel dark");
        assert!(at(x0 + 2, y_mid).0 > 200, "interior stays white");
    }

    #[test]
    fn text_positioned_at_word_coords() {
        // 文本词定位在 x=5 格：前 4 格（x<4*cell_w）应全白（无墨）。
        let root = block_with(vec![inline_text("hello", 5.0, 1.0)]);
        let tree = LayoutTree { root };
        let (w, _h, buf) = render_pixel(&tree, &StyleMap::new(), 400, 1.0);
        let (cw, lh) = cell_metrics();
        let gutter_w = ((4.0 * cw) as usize).min(w);
        let mut lead_dark = 0usize;
        for y in 0..(lh as usize) {
            for x in 0..gutter_w {
                let i = (y * w + x) * 4;
                if buf[i] < 128 {
                    lead_dark += 1;
                }
            }
        }
        assert_eq!(lead_dark, 0, "no ink allowed in the leading gutter");
        // 词区域有墨。
        assert!(count_dark(&buf) > 10, "text glyphs expected");
    }

    #[test]
    fn heading_ratio_2em_draws_taller_glyphs() {
        // 同文本，font-size 2em 的墨迹行数显著多于 1em（UA h1 阶梯生效）。
        let styles_for = |v: &str| {
            let mut m = StyleMap::new();
            m.insert(1, vec![decl("font-size", v)]);
            m
        };
        let tree_with = || {
            let mut b = inline_text("Example", 0.0, 0.0);
            b.element_id = Some(1);
            LayoutTree {
                root: block_with(vec![b]),
            }
        };
        let ink_rows = |styles: &StyleMap| {
            let (w, h, buf) = render_pixel(&tree_with(), styles, 400, 1.0);
            let mut rows = 0;
            for y in 0..h {
                let row = &buf[y * w * 4..(y + 1) * w * 4];
                if row.chunks_exact(4).any(|px| px[0] < 128) {
                    rows += 1;
                }
            }
            rows
        };
        let h1 = ink_rows(&styles_for("2em"));
        let h2 = ink_rows(&styles_for("1em"));
        assert!(h1 >= h2 * 2, "2em glyphs must be ~2x taller: {h1} vs {h2}");
    }

    #[test]
    fn link_text_is_blue_with_underline() {
        let mut b = inline_text("go", 0.0, 0.0);
        b.link = true;
        let tree = LayoutTree {
            root: block_with(vec![b]),
        };
        let (_, _, buf) = render_pixel(&tree, &StyleMap::new(), 400, 1.0);
        // 核心 link 像素 = 精确 (0,0,238)。
        assert!(
            count_exact(&buf, (0, 0, 238)) > 0,
            "saturated blue expected"
        );
    }

    #[test]
    fn css_color_is_used_for_text() {
        // color 由 construct.rs::apply_box_style 折进 bx.style.color（M70 路径），
        // pixel 渲染直接读它。font-size 走 styles map。
        let styles = {
            let mut m = StyleMap::new();
            m.insert(1, vec![decl("font-size", "1em")]);
            m
        };
        let mut b = inline_text("hi", 0.0, 0.0);
        b.element_id = Some(1);
        b.style.color = Some(RgbColor { r: 200, g: 0, b: 0 });
        let tree = LayoutTree {
            root: block_with(vec![b]),
        };
        let (_, _, buf) = render_pixel(&tree, &styles, 400, 1.0);
        assert!(count_exact(&buf, (200, 0, 0)) > 0, "CSS red ink expected");
    }

    #[test]
    fn img_placeholder_skipped() {
        let b = inline_text("[IMG: foo.png]", 0.0, 0.0);
        let tree = LayoutTree {
            root: block_with(vec![b]),
        };
        let (_, _, buf) = render_pixel(&tree, &StyleMap::new(), 400, 1.0);
        assert_eq!(count_dark(&buf), 0, "placeholder must not draw ink");
    }

    #[test]
    fn empty_tree_renders_white_canvas() {
        let tree = LayoutTree {
            root: LayoutBox::new(BoxType::Block),
        };
        let (w, h, buf) = render_pixel(&tree, &StyleMap::new(), 400, 1.0);
        assert!(w >= 1 && h >= 1);
        assert!(buf.iter().all(|&v| v == 255));
    }

    #[test]
    fn scale_multiplies_canvas() {
        let tree = LayoutTree {
            root: block_with(vec![inline_text("hi", 0.0, 0.0)]),
        };
        let (w1, h1, _) = render_pixel(&tree, &StyleMap::new(), 400, 1.0);
        let (w2, h2, _) = render_pixel(&tree, &StyleMap::new(), 400, 2.0);
        assert!((w2 as i64 - 2 * w1 as i64).abs() <= 2);
        assert!(h2 >= h1);
    }

    #[test]
    fn font_ratio_inherits_to_text_leaves() {
        // text 叶子不在 styles map 里 → 继承 h1 盒的 2em。
        // h1(2em) > 文本叶子：叶子盒 element_id=99 不在 map 中。
        let styles = {
            let mut m = StyleMap::new();
            m.insert(7, vec![decl("font-size", "2em")]);
            m
        };
        let mut h1 = LayoutBox::new(BoxType::Block);
        h1.element_id = Some(7);
        let mut leaf = inline_text("Big", 0.0, 0.0);
        leaf.element_id = Some(99);
        h1.children.push(leaf);
        h1.dimensions = Dimensions::new(0.0, 0.0, 40.0, 1.0);
        let tree = LayoutTree { root: h1 };
        let (_, _, buf) = render_pixel(&tree, &styles, 400, 1.0);
        // 2em 墨迹高度 > 1 行槽（line_h ≈ 25px），说明字号继承了 2em。
        let (w, _h, _) = (400usize, 0usize, ());
        let mut top = usize::MAX;
        let mut bottom = 0usize;
        for y in 0..buf.len() / 4 / w {
            let row = &buf[y * w * 4..(y + 1) * w * 4];
            if row.chunks_exact(4).any(|px| px[0] < 128) {
                top = top.min(y);
                bottom = bottom.max(y);
            }
        }
        assert!(top != usize::MAX, "ink expected");
        assert!(
            bottom - top > 20,
            "2em inherited glyph must span >20px, got {}",
            bottom - top
        );
    }

    // ---- M80.1: alpha 背景（黑色实块修复）----

    #[test]
    fn fully_transparent_bg_draws_nothing() {
        // rgba(0,0,0,0) 是"全透明"，绝不能画出黑块。
        let styles = {
            let mut m = StyleMap::new();
            m.insert(1, vec![decl("background-color", "rgba(0, 0, 0, 0)")]);
            m
        };
        let mut root = block_with(vec![]);
        root.element_id = Some(1);
        let tree = LayoutTree { root };
        let (_, _, buf) = render_pixel(&tree, &styles, 400, 1.0);
        assert_eq!(
            count_exact(&buf, (0, 0, 0)),
            0,
            "transparent must not paint black"
        );
    }

    #[test]
    fn hex8_alpha_blends_over_white() {
        // #0000001a（a=26/255≈0.10）→ 白底上 ≈ (229,229,229)，不是黑。
        let styles = {
            let mut m = StyleMap::new();
            m.insert(1, vec![decl("background-color", "#0000001a")]);
            m
        };
        let mut root = block_with(vec![]);
        root.element_id = Some(1);
        let tree = LayoutTree { root };
        let (_, _, buf) = render_pixel(&tree, &styles, 400, 1.0);
        let gray = count_exact(&buf, (229, 229, 229));
        assert!(
            gray > 100,
            "expected ~10%% black blend, got {gray} px of (229,229,229)"
        );
        assert_eq!(
            count_exact(&buf, (0, 0, 0)),
            0,
            "alpha byte must not be dropped"
        );
    }

    #[test]
    fn rgba_semi_transparent_blends() {
        // rgba(0,0,0,0.5) over white → ≈128 灰。
        let styles = {
            let mut m = StyleMap::new();
            m.insert(1, vec![decl("background", "rgba(0, 0, 0, 0.5)")]);
            m
        };
        let mut root = block_with(vec![]);
        root.element_id = Some(1);
        let tree = LayoutTree { root };
        let (_, _, buf) = render_pixel(&tree, &styles, 400, 1.0);
        let any = buf
            .chunks_exact(4)
            .filter(|px| (100..=156).contains(&px[0]) && px[0] == px[1] && px[1] == px[2])
            .count();
        assert!(any > 100, "expected mid-gray blend pixels, got {any}");
    }

    #[test]
    fn text_contrast_uses_composited_bg() {
        // rgba(0,0,0,0.95) 遮罩上的黑字必须翻成白字（按合成后底色判定）。
        let styles = {
            let mut m = StyleMap::new();
            m.insert(1, vec![decl("background-color", "rgba(0, 0, 0, 0.95)")]);
            m
        };
        let mut b = inline_text("hi", 0.0, 0.0);
        b.element_id = Some(1);
        let tree = LayoutTree {
            root: block_with(vec![b]),
        };
        let (_, _, buf) = render_pixel(&tree, &styles, 400, 1.0);
        let bright = buf
            .chunks_exact(4)
            .filter(|px| px[0] > 200 && px[1] > 200 && px[2] > 200)
            .count();
        assert!(
            bright > 10,
            "ink must flip to white on near-black bg, bright={bright}"
        );
    }

    #[test]
    fn parse_color_rgba_variants() {
        assert_eq!(
            parse_color_rgba("#0000001a"),
            Some(Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 26.0 / 255.0
            })
        );
        assert_eq!(
            parse_color_rgba("#11223344"),
            Some(Rgba {
                r: 0x11,
                g: 0x22,
                b: 0x33,
                a: 68.0 / 255.0
            })
        );
        assert_eq!(
            parse_color_rgba("#ff0000"),
            Some(Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 1.0
            })
        );
        assert_eq!(
            parse_color_rgba("transparent"),
            Some(Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 0.0
            })
        );
        assert_eq!(
            parse_color_rgba("rgba(10, 20, 30, 0.25)").map(|c| c.a),
            Some(0.25)
        );
        assert_eq!(parse_color_rgba("rgb(1, 2, 3)").map(|c| c.a), Some(1.0));
        assert_eq!(parse_color_rgba("black").map(|c| c.a), Some(1.0));
        assert_eq!(parse_color_rgba("garbage"), None);
    }

    // ---- M80.1: 大字号行距 + 绘制期折行（hero 叠印修复）----

    /// 收集有墨的行号列表。
    fn ink_rows(buf: &[u8], w: usize) -> Vec<usize> {
        let h = buf.len() / 4 / w;
        (0..h)
            .filter(|&y| {
                buf[y * w * 4..(y + 1) * w * 4]
                    .chunks_exact(4)
                    .any(|px| px[0] < 128)
            })
            .collect()
    }

    #[test]
    fn two_line_heading_stacks_without_self_overlap() {
        // 2em 两行标题、行宽不超页（不触发缩放）：绘制行距必须按字形高度
        // 展开，两行墨迹之间要有 ≥4 行无墨间隙（旧行距 25px 时两行 32px
        // 字形互相叠印 —— docusaurus hero 根因）。
        let styles = {
            let mut m = StyleMap::new();
            m.insert(1, vec![decl("font-size", "2em")]);
            m
        };
        let mut b = inline_wrapped_text("HEADING LINE ONE TWO THREE", 0.0, 0.0, 20.0);
        b.element_id = Some(1);
        let tree = LayoutTree {
            root: block_with(vec![b]),
        };
        let (w, _h, buf) = render_pixel(&tree, &styles, 400, 1.0);
        let rows = ink_rows(&buf, w);
        assert!(rows.len() >= 2, "two inked lines expected");
        // 找最大连续无墨间隙。
        let mut best_gap = 0;
        let mut run = 0;
        for y in rows[0]..=*rows.last().unwrap_or(&0) {
            if rows.contains(&y) {
                run = 0;
            } else {
                run += 1;
                best_gap = best_gap.max(run);
            }
        }
        assert!(
            best_gap >= 4,
            "two heading lines must be separated, max gap={best_gap}"
        );
    }

    #[test]
    fn oversized_heading_shrinks_to_fit_page() {
        // 布局 2 行、每行 2em 绘制宽 ~790px > 200px 页宽 → 整盒字号缩小，
        // 文字完整落在页宽内（不截断、不折行、不与下方叠印）。
        let styles = {
            let mut m = StyleMap::new();
            m.insert(1, vec![decl("font-size", "2em")]);
            m
        };
        let mut b = inline_wrapped_text(
            "aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii jjjj kkkk",
            0.0,
            0.0,
            43.0,
        );
        b.element_id = Some(1);
        let tree = LayoutTree {
            root: block_with(vec![b]),
        };
        let (w, _h, buf) = render_pixel(&tree, &styles, 200, 1.0);
        let rows = ink_rows(&buf, w);
        // 数"墨带"（连续有墨行的段数）：2 条布局行都要画出来。
        let mut bands = 0;
        let mut prev: Option<usize> = None;
        for &y in &rows {
            let cont = prev.map(|p: usize| y == p + 1).unwrap_or(false);
            if !cont {
                bands += 1;
            }
            prev = Some(y);
        }
        assert!(bands >= 2, "both layout lines must be drawn, got {bands}");
        // 完整性：墨迹最右缘接近页宽（缩放后恰好适配），且不越界。
        let mut max_x = 0usize;
        for (i, px) in buf.chunks_exact(4).enumerate() {
            if px[0] < 128 {
                max_x = max_x.max(i / 4 % w);
            }
        }
        assert!(max_x < w, "ink must stay inside the page");
        assert!(
            max_x >= w * 7 / 10,
            "text must fill the shrunken width, max_x={max_x}"
        );
    }

    #[test]
    fn body_text_unaffected_by_pitch_and_wrap() {
        // 正文（1em）行为不变：行距 pitch = line_h，无绘制期折行。
        let root = block_with(vec![inline_text("hello world foo", 0.0, 0.0)]);
        let tree = LayoutTree { root };
        let (w, _h, buf) = render_pixel(&tree, &StyleMap::new(), 400, 1.0);
        let rows = ink_rows(&buf, w);
        assert!(!rows.is_empty());
        // 全部墨迹在第一个行槽内（≤ line_h）。
        let (_, lh) = cell_metrics();
        assert!(
            rows.iter().all(|&y| (y as f32) < lh * 1.05),
            "1em text must stay in its line slot"
        );
    }

    #[test]
    fn parse_font_ratio_variants() {
        assert!(matches!(parse_font_ratio("2em"), FontRatio::Em(v) if (v - 2.0).abs() < 1e-6));
        assert!(matches!(parse_font_ratio("150%"), FontRatio::Em(v) if (v - 1.5).abs() < 1e-6));
        assert!(
            matches!(parse_font_ratio("32px"), FontRatio::Absolute(v) if (v - 2.0).abs() < 1e-6)
        );
        assert!(matches!(parse_font_ratio("garbage"), FontRatio::Em(v) if (v - 1.0).abs() < 1e-6));
    }

    // ---- M81: hit_test 屏幕坐标命中测试 ----

    fn box_at(id: Option<NodeId>, x: f32, y: f32, w: f32, h: f32) -> LayoutBox {
        let mut b = LayoutBox::new(BoxType::Block);
        b.element_id = id;
        b.dimensions = Dimensions::new(x, y, w, h);
        b
    }

    #[test]
    fn hit_test_hits_box_and_converts_px() {
        // 盒 (0,0,10,2) 格；格中心 (5,1) 换算成 px 必须命中 element_id=7。
        let tree = LayoutTree {
            root: box_at(Some(7), 0.0, 0.0, 10.0, 2.0),
        };
        let (cw, lh) = cell_metrics();
        assert_eq!(hit_test(&tree, 5.0 * cw, 1.0 * lh), Some(7));
        // 右缘外（x >= 10*cw）不命中。
        assert_eq!(hit_test(&tree, 10.0 * cw + 1.0, 1.0 * lh), None);
        // 下缘外（y >= 2*lh）不命中。
        assert_eq!(hit_test(&tree, 5.0 * cw, 2.0 * lh), None);
    }

    #[test]
    fn hit_test_skips_anonymous_to_ancestor() {
        // 无 id 的中间盒（Anonymous）：点落在其中时命中其带 id 的祖先。
        let anon = box_at(None, 0.0, 0.0, 20.0, 5.0);
        let mut root = box_at(Some(3), 0.0, 0.0, 40.0, 10.0);
        root.children.push(anon);
        let tree = LayoutTree { root };
        let (cw, lh) = cell_metrics();
        assert_eq!(hit_test(&tree, 1.0 * cw, 0.5 * lh), Some(3));
    }

    #[test]
    fn hit_test_none_on_empty_tree_and_nan() {
        let tree = LayoutTree {
            root: LayoutBox::new(BoxType::Block),
        };
        assert_eq!(hit_test(&tree, 10.0, 10.0), None);
        let tree2 = LayoutTree {
            root: box_at(Some(1), 0.0, 0.0, 10.0, 2.0),
        };
        assert_eq!(hit_test(&tree2, f32::NAN, 1.0), None);
        assert_eq!(hit_test(&tree2, -1.0, -1.0), None);
    }

    #[test]
    fn hit_test_zero_size_box_never_hits() {
        // 零宽/高盒不命中（退化盒防御）。
        let tree = LayoutTree {
            root: box_at(Some(9), 5.0, 5.0, 0.0, 1.0),
        };
        let (cw, lh) = cell_metrics();
        assert_eq!(hit_test(&tree, 5.0 * cw, 5.0 * lh), None);
    }

    #[test]
    fn hit_test_deepest_child_wins_over_parent() {
        // 父 id=1 (0,0,40,10)，子 id=2 (2,1,6,1)：重叠区命中更深的子。
        let child = box_at(Some(2), 2.0, 1.0, 6.0, 1.0);
        let mut root = box_at(Some(1), 0.0, 0.0, 40.0, 10.0);
        root.children.push(child);
        let tree = LayoutTree { root };
        let (cw, lh) = cell_metrics();
        assert_eq!(hit_test(&tree, 3.0 * cw, 1.5 * lh), Some(2));
        // 父独占区域命中父。
        assert_eq!(hit_test(&tree, 20.0 * cw, 8.0 * lh), Some(1));
    }

    #[test]
    fn hit_test_first_matching_sibling_wins() {
        // 两个同级盒重叠时，树序在前的先命中（绘制顺序近似）。
        let a = box_at(Some(1), 0.0, 0.0, 10.0, 2.0);
        let b = box_at(Some(2), 0.0, 0.0, 10.0, 2.0);
        let mut root = box_at(None, 0.0, 0.0, 40.0, 10.0);
        root.children.push(a);
        root.children.push(b);
        let tree = LayoutTree { root };
        let (cw, lh) = cell_metrics();
        assert_eq!(hit_test(&tree, 1.0 * cw, 1.0 * lh), Some(1));
    }
}
