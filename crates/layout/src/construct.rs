//! Build a [`LayoutTree`] from a DOM [`Tree`] + computed styles.
//!
//! Strategy (the "formatting context" decision):
//! - Block-level tags (html, body, div, p, h1-h6, ul, li, header,
//!   footer, section, article, main, nav, aside, blockquote, pre,
//!   hr) → [`BoxType::Block`]
//! - Everything else (a, span, em, strong, ...) → [`BoxType::Inline`]
//! - Text nodes become anonymous inline boxes carrying their text
//! - Anonymous block wrappers wrap mixed inline content inside a
//!   block parent (per CSS spec)

use std::collections::HashMap;

use browser_css_engine::{parse_box_lengths, parse_length, BoxEdges, Declaration, Length};
use browser_dom::{NodeData, NodeId, Tree};

use crate::boxes::{
    AlignItems, BoxType, FlexDirection, FlexWrap, JustifyContent, LayoutBox, LayoutTree, RgbColor,
};

/// Default block-level tag set. Conservative; can grow as fixtures demand.
const BLOCK_TAGS: &[&str] = &[
    "html",
    "body",
    "div",
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "header",
    "footer",
    "section",
    "article",
    "main",
    "nav",
    "aside",
    "blockquote",
    "pre",
    "hr",
    "br",
    "table",
    "form",
    "figure",
    "figcaption",
];

fn is_block_tag(tag: &str) -> bool {
    BLOCK_TAGS.contains(&tag)
}

fn box_type_for_element(tag: &str) -> BoxType {
    if is_block_tag(tag) {
        BoxType::Block
    } else {
        BoxType::Inline
    }
}

/// M72.1: width (in characters) of the `<hr>` rule line.
const HR_LINE_WIDTH: usize = 80;
/// M72.1: font-size ratio (vs 16px base) at which text is rendered
/// UPPERCASE — the ASCII proxy for "visibly larger glyphs".
const UPPERCASE_RATIO_THRESHOLD: f32 = 1.5;

/// M80.1: options for layout tree construction.
///
/// `ascii_visuals` gates the three ASCII-only text proxies applied during
/// construction (font-size ≥ 1.5em → UPPERCASE, strong/b → `**…**`,
/// em/i/cite/var/dfn → `*…*`). These rewrites are destructive to the text
/// content and cannot be undone downstream, so the pixel renderer needs
/// them disabled to see the raw text (it renders real font size / weight
/// itself from the computed styles).
///
/// Default is `ascii_visuals = true` — the historical ASCII crawler output
/// contract stays byte-identical.
#[derive(Debug, Clone, Copy)]
pub struct ConstructOptions {
    /// Apply the ASCII visual proxies to text leaves (`true` for ASCII
    /// rendering, the default; `false` for pixel rendering).
    pub ascii_visuals: bool,
    /// M80.6: margin/padding 的格换算分母（px 值 ÷ scale = 格数）。
    /// ASCII 模式 1.0（历史行为：50px→50 格）；像素模式 = 布局行高 px
    /// （50px→2 格）——否则 50px margin 在像素画布上被放大成 50 行巨隙。
    pub unit_scale: f32,
}

impl Default for ConstructOptions {
    fn default() -> Self {
        Self {
            ascii_visuals: true,
            unit_scale: 1.0,
        }
    }
}

impl ConstructOptions {
    /// Options with the ASCII visual proxies disabled (pixel rendering).
    #[must_use]
    pub fn pixel() -> Self {
        Self {
            ascii_visuals: false,
            unit_scale: 1.0,
        }
    }
    /// M80.6: pixel + margin/padding 格换算分母（传布局行高 px——
    /// 50px margin ÷ 25 = 2 格，像素画布上即 50px）。
    #[must_use]
    pub fn pixel_with_scale(unit_scale: f32) -> Self {
        Self {
            ascii_visuals: false,
            unit_scale,
        }
    }
}

/// Build a [`LayoutTree`] from a DOM tree and its computed styles.
///
/// Styles include the UA defaults injected by
/// `browser_css_engine::compute_styles`; `construct` maps a few of them to
/// visible ASCII treatments (font-size ≥ 1.5em → UPPERCASE) while page CSS
/// overrides stay effective through the same cascade.
///
/// Equivalent to [`construct_layout_tree_with`] with the default
/// [`ConstructOptions`] (ASCII visual proxies enabled).
#[must_use]
pub fn construct_layout_tree(
    tree: &Tree,
    styles: &HashMap<NodeId, Vec<Declaration>>,
) -> LayoutTree {
    construct_layout_tree_with(tree, styles, ConstructOptions::default())
}

/// Build a [`LayoutTree`] with explicit [`ConstructOptions`].
///
/// With `opts.ascii_visuals == false` the three destructive ASCII text
/// rewrites are skipped and text leaves keep their original content —
/// required by the pixel renderer (`--render-mode pixel`), which renders
/// real font size / bold / italic instead of the ASCII proxies.
#[must_use]
pub fn construct_layout_tree_with(
    tree: &Tree,
    styles: &HashMap<NodeId, Vec<Declaration>>,
    opts: ConstructOptions,
) -> LayoutTree {
    // The DOM root is Document; we model the layout root as an
    // anonymous block that contains whatever Document's children produce.
    let mut root = LayoutBox::new(BoxType::Anonymous);
    root.element_id = Some(tree.root());
    for &child in tree.children_of(tree.root()) {
        build_box(tree, child, "", None, styles, opts, &mut root.children);
    }
    LayoutTree { root }
}

/// Lowercased tag name of an Element node, `None` for other node kinds.
fn element_tag(tree: &Tree, id: NodeId) -> Option<String> {
    match tree.data(id) {
        NodeData::Element { tag, .. } => Some(tag.to_ascii_lowercase()),
        _ => None,
    }
}

fn build_box(
    tree: &Tree,
    id: NodeId,
    parent_tag: &str,
    list_index: Option<usize>,
    styles: &HashMap<NodeId, Vec<Declaration>>,
    opts: ConstructOptions,
    out: &mut Vec<LayoutBox>,
) {
    match tree.data(id) {
        NodeData::Element { tag, attrs, .. } => {
            if is_non_rendered_tag(tag) {
                return;
            }
            let tag_lower = tag.to_ascii_lowercase();
            let mut bt = box_type_for_element(&tag_lower);
            // M81: `position: absolute | fixed` → out of flow. Detected
            // here, consumed by block.rs / flex.rs / grid.rs layout.
            let mut positioned = false;
            // M32: CSS `display` 声明覆盖 tag-based 默认值。
            // 支持 display:flex / display:block / display:inline。
            if let Some(decls) = styles.get(&id) {
                for d in decls {
                    if d.property.eq_ignore_ascii_case("display") {
                        let v = d.value.trim().to_ascii_lowercase();
                        match v.as_str() {
                            "flex" | "inline-flex" => bt = BoxType::Flex,
                            "grid" | "inline-grid" => bt = BoxType::Grid,
                            "block" => bt = BoxType::Block,
                            "inline" => bt = BoxType::Inline,
                            _ => {}
                        }
                    } else if d.property.eq_ignore_ascii_case("position") {
                        let v = d.value.trim().to_ascii_lowercase();
                        if v == "absolute" || v == "fixed" {
                            positioned = true;
                        }
                    }
                }
            }
            // M81: CSS blockification — absolutely positioned elements
            // compute to block-level, so an inline tag (`<span
            // style="position:absolute">`) becomes a Block box and is
            // routed through the block/anonymous out-of-flow path
            // instead of taking space in an inline run.
            if positioned && bt == BoxType::Inline {
                bt = BoxType::Block;
            }
            let mut bx = LayoutBox::new(bt).with_element(id);
            bx.positioned = positioned;
            // M32: 如果是 flex 容器，从 CSS 读 flex-direction/justify-content/gap。
            if bt == BoxType::Flex {
                apply_flex_props(id, styles, &mut bx);
            }
            // M33: 如果是 grid 容器，从 CSS 读 grid-template-columns/gap。
            if bt == BoxType::Grid {
                apply_grid_props(id, styles, &mut bx);
            }
            // M32: 读 flex-grow 属性（flex item）。
            apply_flex_grow(id, styles, &mut bx);
            // M35.3: 读 grid-column/grid-row 显式定位（grid item）。
            apply_grid_placement(id, styles, &mut bx);
            // M39: 读 background-color/border（视觉样式）。
            apply_box_style(id, styles, &mut bx);
            // M70.3: <svg> 特殊处理——读 width/height + 子元素 attrs，编码成
            // [SVG: ...] 占位符。svg 子元素（circle/rect/...）不进布局树
            // （它们不是 HTML element，是图形描述），由 CLI 后处理替换为 ASCII art。
            // 注意：保持 svg 为 Inline（与 <img> 一致），因为 paint 只输出
            // Inline box 的 text。Block box 的 text 会被忽略。
            if tag_lower == "svg" {
                bx.text = Some(encode_svg_placeholder(tree, id, attrs));
                bx.box_type = BoxType::Inline;
                bx.children = Vec::new();
            } else {
                bx.children = build_children(tree, id, bt, &tag_lower, styles, opts);
            }
            // M7.1.3: fill margin/padding from CSS + UA defaults.
            // M80.6: unit_scale>1 时（像素模式）px 值 ÷ scale 换算成格。
            apply_box_model(&tag_lower, id, styles, opts.unit_scale, &mut bx);
            // M72.1: UA-stylesheet visual mapping (ASCII-only proxies).
            // font-size comes from the computed declarations (the UA sheet
            // provides the h1–h6 ladder; author CSS overrides it), so a page
            // resetting `h1 { font-size: 1em }` also cancels the uppercase.
            // M80.1: gated behind `ascii_visuals` — pixel rendering needs
            // the raw text (real font size / bold / italic), and these
            // rewrites are destructive (cannot be undone downstream).
            if opts.ascii_visuals {
                let ratio = font_size_ratio(styles.get(&id));
                if ratio >= UPPERCASE_RATIO_THRESHOLD {
                    uppercase_text_leaves(&mut bx);
                } else if matches!(tag_lower.as_str(), "strong" | "b") {
                    // ASCII bold: markdown-style **…** markers.
                    wrap_first_last_text(&mut bx, "**", "**");
                } else if matches!(tag_lower.as_str(), "em" | "i" | "cite" | "var" | "dfn") {
                    // ASCII italic: markdown-style *…* markers.
                    wrap_first_last_text(&mut bx, "*", "*");
                }
            }
            // M72.1: <hr> — a horizontal rule line (no children).
            if tag_lower == "hr" {
                bx.box_type = BoxType::Inline;
                bx.children = Vec::new();
                bx.text = Some("─".repeat(HR_LINE_WIDTH));
            }
            // M72.1: <pre> — raw text with whitespace/newlines preserved.
            if tag_lower == "pre" {
                bx.box_type = BoxType::Inline;
                bx.children = Vec::new();
                bx.text = Some(collect_pre_text(tree, id));
                bx.preserve_whitespace = true;
            }
            // M9.1.1/M72: inject placeholder for <img>.
            // 噪声治理（M72）：绝不能把 src URL / data-URI 当文本输出——
            // 真实 SPA 截图里几百行 `[IMG /_next/image?url=...]` / 超长
            // data:image/svg+xml 淹没正文。分两类处理：
            // - http(s):// data: // 协议相对 src → 紧凑占位 `[IMG w×h]`
            //   （有 width/height 属性时）或完全跳过（无尺寸信息）
            // - 本地路径 src → 保留 `[IMG: src]` 标记，供 CLI 的
            //   post_process_images 替换为 M22 真实图像 ASCII art（能力不变）
            if tag.eq_ignore_ascii_case("img") {
                bx.text = img_placeholder_text(attrs);
            }
            // M27.1: <a href> append target URL so crawlers can see link
            // destinations in rendered text. e.g. "News (https://...)".
            // Browsers color/underline links; ASCII mode lacks color, so
            // we surface the href inline (huge value for the G1 crawler goal).
            if tag_lower == "a" {
                bx = bx.with_link(); // M30: mark for colored rendering
                if let Some(href) = attrs
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("href"))
                    .map(|(_, v)| v.as_str())
                    .filter(|h| !h.trim().is_empty())
                {
                    inject_a_href(&mut bx, href);
                }
            }
            // M6.0c: <li> bullet prefix (CSS ::marker placeholder).
            // M72.1: `<ol>` items are numbered "1. / 2. / …" per parent list.
            if tag_lower == "li" {
                let bullet = match (parent_tag, list_index) {
                    ("ol", Some(n)) => format!("{n}. "),
                    _ => "• ".to_string(),
                };
                inject_li_bullet(&mut bx, &bullet);
            }
            out.push(bx);
        }
        NodeData::Text(s) => {
            let bx = LayoutBox::new(BoxType::Inline)
                .with_element(id)
                .with_text(s.clone());
            out.push(bx);
        }
        NodeData::Comment(_) | NodeData::Doctype { .. } | NodeData::Document => {
            // Skip.
        }
    }
}

/// Apply CSS margin/padding + UA defaults to a freshly-built box.
/// CSS overrides non-Zero UA edges. Longhands override shorthand
/// via parse_box_lengths.
/// M80.6: Length::Px 值乘系数（格换算）。Em/Percent 不动（消费端按
/// 字号/容器解析，格语义已对）。
fn scale_length(l: &mut browser_css_engine::Length, f: f32) {
    if let browser_css_engine::Length::Px(v) = l {
        *v *= f;
    }
}

fn apply_box_model(
    tag: &str,
    id: NodeId,
    styles: &HashMap<NodeId, Vec<Declaration>>,
    unit_scale: f32,
    bx: &mut LayoutBox,
) {
    bx.margin = ua_default_margins(tag);
    bx.padding = BoxEdges::default();
    if let Some(decls) = styles.get(&id) {
        let css_margin = parse_box_lengths(decls, "margin");
        let css_padding = parse_box_lengths(decls, "padding");
        // M7.1.6 fix: explicit `0` in CSS must override UA defaults.
        // If the user declared *any* margin property (shorthand or
        // longhand), replace the whole BoxEdges — parse_box_lengths
        // fills missing longhand edges with Zero, which is the right
        // behavior for "user reset to zero".
        let any_margin_decl = decls
            .iter()
            .any(|d| d.property == "margin" || d.property.starts_with("margin-"));
        let any_padding_decl = decls
            .iter()
            .any(|d| d.property == "padding" || d.property.starts_with("padding-"));
        if any_margin_decl {
            bx.margin = css_margin;
        }
        if any_padding_decl {
            bx.padding = css_padding;
        }
    }
    // M80.6: 像素模式（unit_scale>1）——margin/padding 的 px 值是"像素
    // 意图"，而 run_layout 按格消费（1 格 = 1 行高 px）。÷scale 换算：
    // 50px margin ÷ 25px/行 = 2 格，像素画布上即 50px。UA 默认 margin
    //（0.67em→Px）同尺度处理。必须在 CSS 替换之后缩放。
    if unit_scale > 1.0 {
        let inv = 1.0 / unit_scale;
        for edge in [
            &mut bx.margin.top,
            &mut bx.margin.bottom,
            &mut bx.margin.right,
            &mut bx.margin.left,
        ] {
            scale_length(edge, inv);
        }
        for edge in [
            &mut bx.padding.top,
            &mut bx.padding.bottom,
            &mut bx.padding.right,
            &mut bx.padding.left,
        ] {
            scale_length(edge, inv);
        }
    }
}

/// M32: 从 CSS 读 flex-direction / justify-content / gap 填充 FlexProps。
fn apply_flex_props(id: NodeId, styles: &HashMap<NodeId, Vec<Declaration>>, bx: &mut LayoutBox) {
    let Some(decls) = styles.get(&id) else {
        return;
    };
    for d in decls {
        if d.property.eq_ignore_ascii_case("flex-direction") {
            let v = d.value.trim().to_ascii_lowercase();
            match v.as_str() {
                "row" | "row-reverse" => bx.flex.direction = FlexDirection::Row,
                "column" | "column-reverse" => bx.flex.direction = FlexDirection::Column,
                _ => {}
            }
        } else if d.property.eq_ignore_ascii_case("justify-content") {
            let v = d.value.trim().to_ascii_lowercase();
            match v.as_str() {
                "flex-start" | "start" | "left" => bx.flex.justify = JustifyContent::FlexStart,
                "center" => bx.flex.justify = JustifyContent::Center,
                "flex-end" | "end" | "right" => bx.flex.justify = JustifyContent::FlexEnd,
                "space-between" => bx.flex.justify = JustifyContent::SpaceBetween,
                _ => {}
            }
        } else if d.property.eq_ignore_ascii_case("gap") {
            if let Some(Length::Px(v)) = parse_length(&d.value) {
                bx.flex.gap = v;
            }
        } else if d.property.eq_ignore_ascii_case("flex-wrap") {
            let v = d.value.trim().to_ascii_lowercase();
            match v.as_str() {
                "nowrap" => bx.flex.wrap = FlexWrap::Nowrap,
                "wrap" | "wrap-reverse" => bx.flex.wrap = FlexWrap::Wrap,
                _ => {}
            }
        } else if d.property.eq_ignore_ascii_case("align-items") {
            let v = d.value.trim().to_ascii_lowercase();
            match v.as_str() {
                "stretch" | "normal" => bx.flex.align = AlignItems::Stretch,
                "flex-start" | "start" => bx.flex.align = AlignItems::FlexStart,
                "center" => bx.flex.align = AlignItems::Center,
                "flex-end" | "end" => bx.flex.align = AlignItems::FlexEnd,
                _ => {}
            }
        }
    }
}

/// M32: 从 CSS 读 flex-grow 属性（flex item）。
/// 支持 `flex-grow: N` 和 shorthand `flex: N`（取第一个值作为 grow）。
fn apply_flex_grow(id: NodeId, styles: &HashMap<NodeId, Vec<Declaration>>, bx: &mut LayoutBox) {
    let Some(decls) = styles.get(&id) else {
        return;
    };
    for d in decls {
        if d.property.eq_ignore_ascii_case("flex-grow") {
            if let Ok(v) = d.value.trim().parse::<f32>() {
                bx.flex_grow = v;
            }
        } else if d.property.eq_ignore_ascii_case("flex") {
            // `flex: <grow> <shrink> <basis>` — take first token as grow.
            if let Some(first) = d.value.split_whitespace().next() {
                if let Ok(v) = first.parse::<f32>() {
                    bx.flex_grow = v;
                }
            }
        }
    }
}

/// M33: 从 CSS 读 grid-template-columns / gap 填充 GridProps。
fn apply_grid_props(id: NodeId, styles: &HashMap<NodeId, Vec<Declaration>>, bx: &mut LayoutBox) {
    let Some(decls) = styles.get(&id) else {
        return;
    };
    for d in decls {
        if d.property.eq_ignore_ascii_case("grid-template-columns") {
            let tracks = crate::grid::parse_grid_template_columns(&d.value);
            if !tracks.is_empty() {
                bx.grid.columns = tracks;
            }
        } else if d.property.eq_ignore_ascii_case("gap") {
            if let Some(Length::Px(v)) = parse_length(&d.value) {
                bx.grid.gap = v;
            }
        }
    }
}

/// M35.3: 从 CSS 读 grid-column / grid-row 显式定位，填充 grid_placement。
fn apply_grid_placement(
    id: NodeId,
    styles: &HashMap<NodeId, Vec<Declaration>>,
    bx: &mut LayoutBox,
) {
    let Some(decls) = styles.get(&id) else {
        return;
    };
    let mut col_start = None;
    let mut col_span = 1usize;
    let mut row_start = None;
    let mut row_span = 1usize;
    let mut has_grid = false;
    for d in decls {
        if d.property.eq_ignore_ascii_case("grid-column") {
            has_grid = true;
            let (s, span) = crate::grid::parse_grid_placement(&d.value);
            col_start = s;
            col_span = span;
        } else if d.property.eq_ignore_ascii_case("grid-row") {
            has_grid = true;
            let (s, span) = crate::grid::parse_grid_placement(&d.value);
            row_start = s;
            row_span = span;
        }
    }
    if has_grid {
        bx.grid_placement = Some(crate::boxes::GridItemPlacement {
            col_start,
            col_span,
            row_start,
            row_span,
        });
    }
}

/// M39/M70: 从 CSS 读 background-color / border / color 填充 BoxStyle。
/// border 简化为 1 字符宽四边相同，background 和 color 解析颜色填 RgbColor。
fn apply_box_style(id: NodeId, styles: &HashMap<NodeId, Vec<Declaration>>, bx: &mut LayoutBox) {
    let Some(decls) = styles.get(&id) else {
        return;
    };
    for d in decls {
        let prop = d.property.to_ascii_lowercase();
        if prop == "background-color" || prop == "background" {
            // background shorthand 可能含多个值（如 "red url(x)"），
            // 只取第一个 color token 尝试解析。
            if let Some((r, g, b)) = browser_css_engine::parse_color(&d.value) {
                bx.style.background = Some(RgbColor { r, g, b });
            }
        } else if prop == "color" {
            // M70: 文字前景色。对称 background-color，调同一个 parse_color。
            if let Some((r, g, b)) = browser_css_engine::parse_color(&d.value) {
                bx.style.color = Some(RgbColor { r, g, b });
            }
        } else if prop == "border" || prop.starts_with("border-") {
            // border shorthand: "Npx solid color" 或 longhand border-top/right/bottom/left
            // 简化：只要有 border 声明且有宽度值，标记对应边为 true。
            let v = d.value.to_ascii_lowercase();
            if v == "none" || v == "hidden" {
                continue;
            }
            // 检测宽度：含 Npx 或 named thin/medium/thick
            let has_width = v.split_whitespace().any(|tok| {
                tok.ends_with("px")
                    && tok
                        .trim_end_matches("px")
                        .parse::<f32>()
                        .is_ok_and(|n| n > 0.0)
                    || matches!(tok, "thin" | "medium" | "thick")
            });
            if !has_width {
                continue;
            }
            match prop.as_str() {
                "border" => {
                    bx.style.border = BoxEdges::all(true);
                }
                "border-top" => bx.style.border.top = true,
                "border-right" => bx.style.border.right = true,
                "border-bottom" => bx.style.border.bottom = true,
                "border-left" => bx.style.border.left = true,
                _ => {}
            }
        }
    }
}

/// UA default margins for block-level elements. ASCII mode: 1em = 1 line.
#[must_use]
fn ua_default_margins(tag: &str) -> browser_css_engine::BoxEdges<Length> {
    let lower = tag.to_ascii_lowercase();
    match lower.as_str() {
        // M70.1: only <p> has the classic 1em top/bottom margin. <div> is a
        // generic container with UA margin 0 in real browsers — the old code
        // wrongly gave <div> 1em too, causing excessive blank lines in nested
        // layouts (1em = 1 terminal line). Removing <div> collapses the
        // whitespace to realistic levels.
        "p" => browser_css_engine::BoxEdges {
            top: Length::Em(1.0),
            right: Length::Zero,
            bottom: Length::Em(1.0),
            left: Length::Zero,
        },
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => browser_css_engine::BoxEdges {
            top: Length::Em(0.67),
            right: Length::Zero,
            bottom: Length::Em(0.67),
            left: Length::Zero,
        },
        "ul" | "ol" => browser_css_engine::BoxEdges {
            top: Length::Em(1.0),
            right: Length::Zero,
            bottom: Length::Em(1.0),
            left: Length::Zero,
        },
        "hr" => browser_css_engine::BoxEdges {
            top: Length::Em(0.5),
            right: Length::Zero,
            bottom: Length::Em(0.5),
            left: Length::Zero,
        },
        _ => browser_css_engine::BoxEdges::default(),
    }
}

/// Tags whose subtrees produce no visual output. Browsers suppress
/// these completely during layout.
///
/// - `head` and `meta`/`link`/`title`: contain document metadata,
///   not rendered body content. Skipping the whole `<head>` subtree
///   is the cleanest fix — `<title>` text won't leak into output.
/// - `script` / `style` / `noscript` / `template`: already established
///   in M4.1.
/// - `textarea`: form control, content is its initial *value*, not
///   document flow text. **M26**: 百度等大站把 CSS 文本塞进
///   `<textarea id="..." style="display:none">` 做延迟加载，导致
///   渲染时 CSS 泄漏（69% 输出是 CSS 噪音）。真浏览器 textarea 内容
///   不参与渲染。
fn is_non_rendered_tag(tag: &str) -> bool {
    matches!(
        tag.to_ascii_lowercase().as_str(),
        "head"
            | "meta"
            | "link"
            | "title"
            | "script"
            | "style"
            | "noscript"
            | "template"
            | "textarea" // M9.1: img 需要（渲染为 [IMG: src] 占位符），不列入黑名单
    )
}

/// Build the children of an Element, inserting Anonymous block wrappers
/// whenever a Block parent has Inline children mixed with Block children.
///
/// `parent_tag` (lowercased) drives M72.1 `<ol>` numbering: each direct
/// `<li>` child of an `<ol>` gets its 1-based position.
fn build_children(
    tree: &Tree,
    parent_id: NodeId,
    parent_box: BoxType,
    parent_tag: &str,
    styles: &HashMap<NodeId, Vec<Declaration>>,
    opts: ConstructOptions,
) -> Vec<LayoutBox> {
    let dom_children = tree.children_of(parent_id);
    if dom_children.is_empty() {
        return Vec::new();
    }

    // M72.1: number <li> children of <ol> (nested lists restart at 1 —
    // each build_children call owns its own counter).
    let is_ol = parent_tag.eq_ignore_ascii_case("ol");
    let mut li_counter = 0usize;
    let mut list_index_for = |tree: &Tree, child_id: NodeId| -> Option<usize> {
        if is_ol && element_tag(tree, child_id).is_some_and(|t| t.eq_ignore_ascii_case("li")) {
            li_counter += 1;
            Some(li_counter)
        } else {
            None
        }
    };

    if parent_box == BoxType::Block {
        // Group consecutive inline children into anonymous blocks.
        let mut result: Vec<LayoutBox> = Vec::new();
        let mut inline_buf: Vec<LayoutBox> = Vec::new();
        for &child_id in dom_children {
            let is_inline = is_inline_node(tree, child_id);
            if is_inline {
                let mut tmp = Vec::new();
                build_box(
                    tree,
                    child_id,
                    parent_tag,
                    list_index_for(tree, child_id),
                    styles,
                    opts,
                    &mut tmp,
                );
                inline_buf.extend(tmp);
            } else {
                flush_inline_buf(&mut inline_buf, &mut result);
                build_box(
                    tree,
                    child_id,
                    parent_tag,
                    list_index_for(tree, child_id),
                    styles,
                    opts,
                    &mut result,
                );
            }
        }
        flush_inline_buf(&mut inline_buf, &mut result);
        result
    } else if parent_box == BoxType::Flex {
        // M32: Flex 容器直接收集 children，不做 anonymous 包装。
        // Flex items 不管原始 tag 是 block 还是 inline，都直接成为 flex item。
        let mut result: Vec<LayoutBox> = Vec::new();
        for &child_id in dom_children {
            build_box(
                tree,
                child_id,
                parent_tag,
                list_index_for(tree, child_id),
                styles,
                opts,
                &mut result,
            );
        }
        result
    } else if parent_box == BoxType::Grid {
        // M33: Grid 容器同样直接收集 children（auto-placement）。
        let mut result: Vec<LayoutBox> = Vec::new();
        for &child_id in dom_children {
            build_box(
                tree,
                child_id,
                parent_tag,
                list_index_for(tree, child_id),
                styles,
                opts,
                &mut result,
            );
        }
        result
    } else {
        // Inline parent → just collect children inline (no anonymous wrappers).
        let mut result = Vec::new();
        for &child_id in dom_children {
            build_box(
                tree,
                child_id,
                parent_tag,
                list_index_for(tree, child_id),
                styles,
                opts,
                &mut result,
            );
        }
        result
    }
}

fn is_inline_node(tree: &Tree, id: NodeId) -> bool {
    match tree.data(id) {
        NodeData::Text(_) => true,
        NodeData::Element { tag, .. } => !is_block_tag(tag),
        _ => false,
    }
}

fn flush_inline_buf(buf: &mut Vec<LayoutBox>, out: &mut Vec<LayoutBox>) {
    if buf.is_empty() {
        return;
    }
    let drained: Vec<LayoutBox> = std::mem::take(buf);
    let mut anon = LayoutBox::new(BoxType::Anonymous);
    anon.children = drained;
    out.push(anon);
}

/// Prepend a list-marker `prefix` ("• " for `<ul>`, "N. " for `<ol>` —
/// M72.1) to the first text-bearing descendant of an `<li>` layout box.
/// The bullet sits at the same (x, y) as the text would have started, then
/// the text follows after the marker. We implement this by mutating the
/// first inline text leaf's `text`.
/// M27.1: Append ` (href)` to the first text leaf of an `<a>` box.
/// If the `<a>` has no text child (e.g. `<a href="u"></a>`), we create
/// a text leaf carrying just the href so the link is still discoverable
/// by crawlers (matches browser behavior where a linkless anchor still
/// has an href).
fn inject_a_href(bx: &mut LayoutBox, href: &str) {
    let suffix = format!(" ({href})");
    if let Some(leaf) = find_first_text_leaf_mut(bx) {
        if let Some(text) = &mut leaf.text {
            if !text.ends_with(&suffix) {
                text.push_str(&suffix);
            }
        }
    } else {
        // No text leaf: seed one so the link is still visible.
        let mut seed = LayoutBox::new(BoxType::Inline).with_text(href.to_string());
        // Mark as anonymous (no element id) so it doesn't interfere with
        // DOM id mapping downstream.
        seed.element_id = None;
        bx.children.push(seed);
    }
}

fn inject_li_bullet(bx: &mut LayoutBox, prefix: &str) {
    if let Some(leaf) = find_first_text_leaf_mut(bx) {
        if let Some(text) = &mut leaf.text {
            if !text.starts_with(prefix) {
                let mut new_text = String::with_capacity(text.len() + prefix.len());
                new_text.push_str(prefix);
                new_text.push_str(text);
                *text = new_text;
            }
        }
    }
}

/// Recursive mutable search for the first inline leaf with non-empty text.
fn find_first_text_leaf_mut(bx: &mut LayoutBox) -> Option<&mut LayoutBox> {
    if bx.box_type == BoxType::Inline && bx.text.as_ref().is_some_and(|t| !t.is_empty()) {
        return Some(bx);
    }
    for child in bx.children.iter_mut() {
        if let Some(found) = find_first_text_leaf_mut(child) {
            return Some(found);
        }
    }
    None
}

/// M72.1: mirror of [`find_first_text_leaf_mut`] scanning right-to-left.
fn find_last_text_leaf_mut(bx: &mut LayoutBox) -> Option<&mut LayoutBox> {
    if bx.box_type == BoxType::Inline && bx.text.as_ref().is_some_and(|t| !t.is_empty()) {
        return Some(bx);
    }
    for child in bx.children.iter_mut().rev() {
        if let Some(found) = find_last_text_leaf_mut(child) {
            return Some(found);
        }
    }
    None
}

/// M72.1: font-size as a ratio vs the 16px base, from computed
/// declarations (the last `font-size` wins — matches the cascade).
///
/// `2em` → 2.0, `150%` → 1.5, `32px` → 2.0, `1rem` → 1.0.
/// No declaration (or unparseable) → 1.0.
fn font_size_ratio(decls: Option<&Vec<Declaration>>) -> f32 {
    let Some(decls) = decls else {
        return 1.0;
    };
    for d in decls.iter().rev() {
        if d.property.eq_ignore_ascii_case("font-size") {
            return match parse_length(&d.value) {
                Some(Length::Em(v)) => v,
                Some(Length::Percent(v)) => v / 100.0,
                Some(Length::Px(v)) => v / 16.0,
                Some(Length::Zero) => 0.0,
                _ => 1.0,
            };
        }
    }
    1.0
}

/// M72.1: UPPERCASE every text leaf under `bx` (recursive). Applied when
/// computed font-size is ≥ 1.5em — capital letters are the ASCII proxy
/// for larger glyphs (taller cap height in the rasterized screenshot).
fn uppercase_text_leaves(bx: &mut LayoutBox) {
    if let Some(text) = &mut bx.text {
        *text = text.to_uppercase();
    }
    for child in bx.children.iter_mut() {
        uppercase_text_leaves(child);
    }
}

/// M72.1: markdown-style emphasis markers — prepend `prefix` to the first
/// text leaf and append `suffix` to the last. When the emphasis wraps a
/// single leaf the result is `**text**` / `*text*`; across child elements
/// (`<strong><a>x</a></strong>`) the markers land on the outermost leaves.
/// No-op when there is no text leaf at all.
fn wrap_first_last_text(bx: &mut LayoutBox, prefix: &str, suffix: &str) {
    if let Some(leaf) = find_first_text_leaf_mut(bx) {
        if let Some(text) = &mut leaf.text {
            if !text.starts_with(prefix) {
                text.insert_str(0, prefix);
            }
        }
    }
    if let Some(leaf) = find_last_text_leaf_mut(bx) {
        if let Some(text) = &mut leaf.text {
            if !text.ends_with(suffix) {
                text.push_str(suffix);
            }
        }
    }
}

/// M72.1: collect the raw text of a `<pre>` subtree (text nodes only,
/// skipping non-rendered tags), applying two browser behaviors: a single
/// newline immediately after `<pre>` is dropped, and tabs expand to
/// 4 spaces.
fn collect_pre_text(tree: &Tree, id: NodeId) -> String {
    fn walk(tree: &Tree, id: NodeId, out: &mut String) {
        match tree.data(id) {
            NodeData::Text(s) => out.push_str(s),
            NodeData::Element { tag, .. } => {
                if is_non_rendered_tag(tag) {
                    return;
                }
                for &child in tree.children_of(id) {
                    walk(tree, child, out);
                }
            }
            NodeData::Comment(_) | NodeData::Doctype { .. } | NodeData::Document => {}
        }
    }
    let mut raw = String::new();
    walk(tree, id, &mut raw);
    let raw = raw.strip_prefix('\n').unwrap_or(&raw);
    raw.replace('\t', "    ")
}

/// M70.3: 把 `<svg>` 及其子元素编码成 `[SVG: ...]` 占位符字符串。
///
/// 格式：`[SVG: w=<w> h=<h> | <tag> <key>=<val> <key>=<val>; <tag> ...]`
/// 由 CLI 后处理（post_process_svgs）解析回图形列表，渲染成 ASCII art。
///
/// svg 的子元素（circle/rect/line/polygon）通过 tree.children_of 读取，
/// 不经过 build_children（它们不是布局元素）。
fn encode_svg_placeholder(tree: &Tree, svg_id: NodeId, svg_attrs: &[(String, String)]) -> String {
    // 读 viewBox / width / height
    let vb_w = svg_attrs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("width"))
        .and_then(|(_, v)| v.trim_end_matches("px").parse::<f32>().ok())
        .unwrap_or(100.0);
    let vb_h = svg_attrs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("height"))
        .and_then(|(_, v)| v.trim_end_matches("px").parse::<f32>().ok())
        .unwrap_or(100.0);

    // M78.85: 只输出尺寸——Vue/Svelte 等站点有上百个 SVG 图标，
    // 详细占位符淹没正文文本。形状信息由 CLI 后处理按需解析。
    let _ = tree;
    let _ = svg_id;
    format!("[SVG {vb_w}x{vb_h}]")
}

/// M72: `<img>` 的文本占位符决策（噪声治理）。
///
/// 返回值语义：
/// - `Some("[IMG: src]")` —— **本地文件候选**（file:// 或裸相对路径如
///   `logo.png`）。保留 src 标记，CLI 渲染后处理（post_process_images）
///   把可解析的本地图像替换为 M22 ASCII art；不可解析的由后处理丢弃
///   （不再回显路径）。
/// - `Some("[IMG w×h]")` —— **URL 形 src**（http(s):// 协议相对 `//`、
///   站内绝对路径 `/...`）且元素带正数 width/height 属性，输出紧凑占位
///   （无 src，无 URL）。
/// - `None` —— 无 src、data-URI（爬虫视觉无信息量，一律跳过）、或 URL
///   形 src 且无尺寸信息。跳过 = 不向文本流注入任何噪声。
///
/// 注意：站内绝对路径 `/a/b.png` 归入 URL 形（SPA 的 `/_next/...`、
/// `/_app/...` 资产全是这种形态，且 CDP 截图路径不走 CLI 后处理，必须
/// 在 construct 源头掐断）。代价是「文件系统绝对路径的 img 不再进 M22
/// ASCII 管线」——该场景可用 file:// 表达（仍支持），且无测试依赖。
fn img_placeholder_text(attrs: &[(String, String)]) -> Option<String> {
    let src = attrs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("src"))
        .map(|(_, v)| v.trim())
        .unwrap_or("");
    if src.is_empty() {
        return None;
    }
    let lower = src.to_ascii_lowercase();
    // data-URI：超长且对爬虫零信息量，无论有无尺寸一律跳过。
    if lower.starts_with("data:") {
        return None;
    }
    // URL 形：http(s)://、协议相对 //、站内绝对路径 /...。
    // file:// 与裸相对路径同属 M22 可解析候选（resolve_local_image_src 支持）。
    let url_like = lower.starts_with("http://")
        || lower.starts_with("https://")
        || src.starts_with("//")
        || src.starts_with('/');
    if !url_like {
        // 本地文件候选：保留标记给 CLI 的 M22 ASCII 替换管线。
        return Some(format!("[IMG: {src}]"));
    }
    // URL 形 src：紧凑占位（width/height 属性都为正数时），否则跳过。
    let dim = |key: &str| {
        attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .and_then(|(_, v)| v.trim().parse::<u32>().ok())
            .filter(|&n| n > 0)
    };
    match (dim("width"), dim("height")) {
        (Some(w), Some(h)) => Some(format!("[IMG {w}x{h}]")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_dom::Tree;

    /// Document > html > body > p(text)
    fn simple_tree() -> Tree {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let html = t.insert(
            Some(root),
            NodeData::Element {
                tag: "html".into(),
                attrs: vec![],
            },
        );
        let body = t.insert(
            Some(html),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let p = t.insert(
            Some(body),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(p), NodeData::Text("hello".into()));
        t
    }

    #[test]
    fn construct_produces_block_chain() {
        let tree = simple_tree();
        let styles = HashMap::new();
        let layout = construct_layout_tree(&tree, &styles);
        // Root is anonymous wrapping html.
        assert_eq!(layout.root.box_type, BoxType::Anonymous);
        let html = &layout.root.children[0];
        assert_eq!(html.box_type, BoxType::Block);
        assert_eq!(html.element_id, Some(1));
        let body = &html.children[0];
        assert_eq!(body.box_type, BoxType::Block);
        let p = &body.children[0];
        assert_eq!(p.box_type, BoxType::Block);
        // <p> wraps its text in an anonymous block (since <p> is Block
        // and its child is Inline).
        assert_eq!(p.children.len(), 1);
        let anon = &p.children[0];
        assert_eq!(anon.box_type, BoxType::Anonymous);
        let text_box = &anon.children[0];
        assert_eq!(text_box.box_type, BoxType::Inline);
        assert_eq!(text_box.text.as_deref(), Some("hello"));
    }

    /// Document > body > p(text) + a(text)
    /// <a> is inline, so the body's children produce two boxes:
    /// the <p> (block) and an anonymous block wrapping <a>.
    #[test]
    fn construct_inserts_anonymous_block_for_inline_sibling_of_block() {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let body = t.insert(
            Some(root),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let p = t.insert(
            Some(body),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(p), NodeData::Text("hello".into()));
        let a = t.insert(
            Some(body),
            NodeData::Element {
                tag: "a".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(a), NodeData::Text("link".into()));

        let layout = construct_layout_tree(&t, &HashMap::new());
        let anon_root = &layout.root;
        let body_box = &anon_root.children[0];
        assert_eq!(body_box.box_type, BoxType::Block);
        // body's children: 1 block (p), 1 anonymous block wrapping <a>.
        assert_eq!(body_box.children.len(), 2);
        assert_eq!(body_box.children[0].box_type, BoxType::Block);
        assert_eq!(body_box.children[1].box_type, BoxType::Anonymous);
        // The anonymous block wraps <a>.
        assert_eq!(body_box.children[1].children.len(), 1);
        let a_wrap = &body_box.children[1].children[0];
        assert_eq!(a_wrap.box_type, BoxType::Inline);
    }

    #[test]
    fn construct_skips_comments_and_doctype() {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let _ = t.insert(
            Some(root),
            NodeData::Doctype {
                name: "html".into(),
            },
        );
        let _ = t.insert(Some(root), NodeData::Comment("hi".into()));
        let p = t.insert(
            Some(root),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(p), NodeData::Text("x".into()));

        let layout = construct_layout_tree(&t, &HashMap::new());
        // Only <p> should appear at top level.
        assert_eq!(layout.root.children.len(), 1);
        assert_eq!(layout.root.children[0].element_id, Some(p));
    }

    #[test]
    fn construct_skips_head_subtree_including_title() {
        // Document > html > [head > title("page title"), body > p("visible")]
        // Only <p> should produce layout output; <title>'s text must
        // NOT leak through.
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let html = t.insert(
            Some(root),
            NodeData::Element {
                tag: "html".into(),
                attrs: vec![],
            },
        );
        let head = t.insert(
            Some(html),
            NodeData::Element {
                tag: "head".into(),
                attrs: vec![],
            },
        );
        let title = t.insert(
            Some(head),
            NodeData::Element {
                tag: "title".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(title), NodeData::Text("page title".into()));
        let body = t.insert(
            Some(html),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let p = t.insert(
            Some(body),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(p), NodeData::Text("visible".into()));

        let layout = construct_layout_tree(&t, &HashMap::new());
        let html_box = &layout.root.children[0];
        // html's children should be only <body> now (head subtree dropped).
        assert_eq!(html_box.children.len(), 1);
        // Sanity: text "page title" must NOT appear anywhere in the tree.
        let mut found_leak = false;
        fn walk(b: &LayoutBox, found: &mut bool) {
            if let Some(t) = &b.text {
                if t.contains("page title") {
                    *found = true;
                }
            }
            for c in &b.children {
                walk(c, found);
            }
        }
        walk(&layout.root, &mut found_leak);
        assert!(!found_leak, "<title> text leaked into layout tree");
    }

    #[test]
    fn construct_skips_script_and_style_content() {
        // body > [script("..."), p("visible"), style("...")]
        // Only the <p> should produce a layout box.
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let body = t.insert(
            Some(root),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let script = t.insert(
            Some(body),
            NodeData::Element {
                tag: "script".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(script), NodeData::Text("__setBody('x')".into()));
        let p = t.insert(
            Some(body),
            NodeData::Element {
                tag: "p".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(p), NodeData::Text("visible".into()));
        let style = t.insert(
            Some(body),
            NodeData::Element {
                tag: "style".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(style), NodeData::Text("body{color:red}".into()));

        let layout = construct_layout_tree(&t, &HashMap::new());
        let body_box = &layout.root.children[0];
        // Only <p> should survive — script and style subtrees dropped.
        assert_eq!(body_box.children.len(), 1);
        assert_eq!(body_box.children[0].element_id, Some(p));
    }

    #[test]
    fn construct_text_only_body_wraps_in_anonymous() {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let body = t.insert(
            Some(root),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let _ = t.insert(Some(body), NodeData::Text("bare".into()));
        let layout = construct_layout_tree(&t, &HashMap::new());
        let body_box = &layout.root.children[0];
        assert_eq!(body_box.box_type, BoxType::Block);
        // Text directly under body gets wrapped in anonymous block.
        assert_eq!(body_box.children.len(), 1);
        assert_eq!(body_box.children[0].box_type, BoxType::Anonymous);
    }
}

/// M72: img 占位符噪声治理回归测试。
#[cfg(test)]
mod img_placeholder_tests {
    use super::img_placeholder_text;

    fn attrs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn https_src_without_dims_is_skipped() {
        // react.dev 噪声源：`[IMG: /_next/image?url=...]` 之类 URL 不再进文本流。
        assert_eq!(
            img_placeholder_text(&attrs(&[("src", "https://cdn.example.com/logo.png")])),
            None
        );
    }

    #[test]
    fn https_src_with_dims_is_compact() {
        assert_eq!(
            img_placeholder_text(&attrs(&[
                ("src", "https://cdn.example.com/hero.png"),
                ("width", "320"),
                ("height", "240"),
            ])),
            Some("[IMG 320x240]".to_string())
        );
    }

    #[test]
    fn origin_absolute_src_is_url_like() {
        // svelte.dev 噪声源：/_app/immutable/assets/...svg。
        assert_eq!(
            img_placeholder_text(&attrs(&[("src", "/_app/immutable/assets/logo.svg")])),
            None
        );
        assert_eq!(
            img_placeholder_text(&attrs(&[("src", "/_next/image?url=x&w=640")])),
            None
        );
    }

    #[test]
    fn origin_absolute_src_with_dims_is_compact() {
        assert_eq!(
            img_placeholder_text(&attrs(&[
                ("src", "/img/banner.png"),
                ("width", "800"),
                ("height", "600")
            ])),
            Some("[IMG 800x600]".to_string())
        );
    }

    #[test]
    fn protocol_relative_src_is_url_like() {
        assert_eq!(
            img_placeholder_text(&attrs(&[("src", "//cdn.example.com/x.png")])),
            None
        );
    }

    #[test]
    fn data_uri_is_always_skipped() {
        // 有无尺寸都跳过：data-URI 对爬虫视觉零信息量且超长。
        let base = attrs(&[("src", "data:image/svg+xml,%3Csvg%20xmlns")]);
        assert_eq!(img_placeholder_text(&base), None);
        let with_dims = attrs(&[
            ("src", "data:image/png;base64,iVBORw0KGgo="),
            ("width", "100"),
            ("height", "100"),
        ]);
        assert_eq!(img_placeholder_text(&with_dims), None);
    }

    #[test]
    fn missing_or_empty_src_is_skipped() {
        assert_eq!(img_placeholder_text(&attrs(&[])), None);
        assert_eq!(img_placeholder_text(&attrs(&[("src", "  ")])), None);
    }

    #[test]
    fn local_relative_src_keeps_marker_for_m22() {
        // M22 真实图像能力：裸相对路径保留 [IMG: src] 标记，
        // 供 CLI post_process_images 替换为 ASCII art。
        assert_eq!(
            img_placeholder_text(&attrs(&[("src", "logo.png")])),
            Some("[IMG: logo.png]".to_string())
        );
        assert_eq!(
            img_placeholder_text(&attrs(&[("src", "img/photos/cat.jpg")])),
            Some("[IMG: img/photos/cat.jpg]".to_string())
        );
        assert_eq!(
            img_placeholder_text(&attrs(&[("src", "file:///tmp/x.png")])),
            Some("[IMG: file:///tmp/x.png]".to_string())
        );
    }

    #[test]
    fn zero_or_invalid_dims_do_not_compact() {
        assert_eq!(
            img_placeholder_text(&attrs(&[
                ("src", "https://x.com/a.png"),
                ("width", "0"),
                ("height", "abc"),
            ])),
            None
        );
    }
}

/// M72.1: UA-stylesheet visual mapping tests (uppercase headings,
/// ol numbering, hr rule line, pre preservation, emphasis markers).
#[cfg(test)]
mod ua_visual_tests {
    use super::*;
    use browser_dom::NodeData;

    fn elem(tag: &str, text: &str) -> NodeData {
        let _ = text;
        NodeData::Element {
            tag: tag.into(),
            attrs: vec![],
        }
    }

    fn text(s: &str) -> NodeData {
        NodeData::Text(s.into())
    }

    /// Collect all text under a layout box.
    fn all_text(bx: &LayoutBox, out: &mut String) {
        if let Some(t) = &bx.text {
            out.push_str(t);
        }
        for c in &bx.children {
            all_text(c, out);
        }
    }

    fn first_child_text(layout: &LayoutTree) -> String {
        let mut s = String::new();
        all_text(&layout.root, &mut s);
        s
    }

    /// Document > html > body > h1("Title")
    fn heading_tree(tag: &str, content: &str) -> Tree {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let html = t.insert(Some(root), elem("html", ""));
        let body = t.insert(Some(html), elem("body", ""));
        let h = t.insert(Some(body), elem(tag, ""));
        let _ = t.insert(Some(h), text(content));
        t
    }

    #[test]
    fn h1_with_ua_styles_is_uppercased() {
        let tree = heading_tree("h1", "Example Domain");
        // h1 is the 4th inserted node: root=0, html=1, body=2, h1=3.
        let mut styles = HashMap::new();
        styles.insert(
            3,
            vec![Declaration {
                property: "font-size".into(),
                value: "2em".into(),
                important: false,
            }],
        );
        let layout = construct_layout_tree(&tree, &styles);
        assert!(
            first_child_text(&layout).contains("EXAMPLE DOMAIN"),
            "h1 text should be uppercased, got {:?}",
            first_child_text(&layout)
        );
    }

    #[test]
    fn h1_with_page_reset_font_size_stays_mixed_case() {
        // Author CSS `h1 { font-size: 1em }` must cancel the UA 2em:
        // the last font-size declaration wins.
        let tree = heading_tree("h1", "Example Domain");
        let mut styles = HashMap::new();
        styles.insert(
            3,
            vec![
                Declaration {
                    property: "font-size".into(),
                    value: "2em".into(),
                    important: false,
                },
                Declaration {
                    property: "font-size".into(),
                    value: "1em".into(),
                    important: false,
                },
            ],
        );
        let layout = construct_layout_tree(&tree, &styles);
        let s = first_child_text(&layout);
        assert!(s.contains("Example Domain"), "got {s:?}");
        assert!(
            !s.contains("EXAMPLE"),
            "page font-size:1em must cancel uppercase"
        );
    }

    #[test]
    fn font_size_ratio_parses_units() {
        let d = |v: &str| {
            vec![Declaration {
                property: "font-size".into(),
                value: v.into(),
                important: false,
            }]
        };
        assert_eq!(font_size_ratio(None), 1.0);
        assert_eq!(font_size_ratio(Some(&d("2em"))), 2.0);
        assert_eq!(font_size_ratio(Some(&d("150%"))), 1.5);
        assert_eq!(font_size_ratio(Some(&d("32px"))), 2.0);
        assert_eq!(font_size_ratio(Some(&d("1rem"))), 1.0);
        // Last declaration wins.
        let both = vec![
            Declaration {
                property: "font-size".into(),
                value: "2em".into(),
                important: false,
            },
            Declaration {
                property: "font-size".into(),
                value: "1em".into(),
                important: false,
            },
        ];
        assert_eq!(font_size_ratio(Some(&both)), 1.0);
    }

    #[test]
    fn ol_items_are_numbered() {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let ol = t.insert(Some(root), elem("ol", ""));
        for item in ["alpha", "beta"] {
            let li = t.insert(Some(ol), elem("li", ""));
            let _ = t.insert(Some(li), text(item));
        }
        let layout = construct_layout_tree(&t, &HashMap::new());
        let s = first_child_text(&layout);
        assert!(s.contains("1. alpha"), "got {s:?}");
        assert!(s.contains("2. beta"), "got {s:?}");
    }

    #[test]
    fn ul_items_keep_bullet() {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let ul = t.insert(Some(root), elem("ul", ""));
        let li = t.insert(Some(ul), elem("li", ""));
        let _ = t.insert(Some(li), text("item"));
        let layout = construct_layout_tree(&t, &HashMap::new());
        assert!(first_child_text(&layout).contains("• item"));
    }

    #[test]
    fn hr_becomes_rule_line() {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let _ = t.insert(Some(root), elem("hr", ""));
        let layout = construct_layout_tree(&t, &HashMap::new());
        let s = first_child_text(&layout);
        assert!(s.contains('─'), "hr should render a rule line");
        assert_eq!(s.chars().filter(|&c| c == '─').count(), HR_LINE_WIDTH);
    }

    #[test]
    fn pre_preserves_newlines_and_tabs() {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let pre = t.insert(Some(root), elem("pre", ""));
        let _ = t.insert(Some(pre), text("a\tb\nc  d"));
        let layout = construct_layout_tree(&t, &HashMap::new());
        fn find_pre(bx: &LayoutBox, out: &mut Option<String>) {
            if bx.preserve_whitespace {
                if let Some(t) = &bx.text {
                    *out = Some(t.clone());
                }
            }
            for c in &bx.children {
                find_pre(c, out);
            }
        }
        let mut found: Option<String> = None;
        find_pre(&layout.root, &mut found);
        let box_text = found.expect("pre box with preserve_whitespace");
        assert_eq!(box_text, "a    b\nc  d", "tabs expand, leading rules apply");
    }

    #[test]
    fn strong_and_em_get_markdown_markers() {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let body = t.insert(Some(root), elem("body", ""));
        let strong = t.insert(Some(body), elem("strong", ""));
        let _ = t.insert(Some(strong), text("bold"));
        let em = t.insert(Some(body), elem("em", ""));
        let _ = t.insert(Some(em), text("italic"));
        let layout = construct_layout_tree(&t, &HashMap::new());
        let s = first_child_text(&layout);
        assert!(s.contains("**bold**"), "got {s:?}");
        assert!(s.contains("*italic*"), "got {s:?}");
    }

    /// M80.1: pixel mode (`ascii_visuals = false`) must keep raw text —
    /// no UPPERCASE for large headings, no markdown emphasis markers.
    /// The pixel renderer reads real font-size / bold / italic itself.
    #[test]
    fn pixel_mode_keeps_h1_text_raw() {
        let tree = heading_tree("h1", "Example Domain");
        let mut styles = HashMap::new();
        styles.insert(
            3,
            vec![Declaration {
                property: "font-size".into(),
                value: "2em".into(),
                important: false,
            }],
        );
        let layout = construct_layout_tree_with(&tree, &styles, ConstructOptions::pixel());
        let s = first_child_text(&layout);
        assert!(
            s.contains("Example Domain"),
            "pixel mode must keep raw heading text, got {s:?}"
        );
        assert!(
            !s.contains("EXAMPLE"),
            "pixel mode must not uppercase, got {s:?}"
        );
    }

    #[test]
    fn pixel_mode_keeps_strong_em_text_raw() {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let body = t.insert(Some(root), elem("body", ""));
        let strong = t.insert(Some(body), elem("strong", ""));
        let _ = t.insert(Some(strong), text("bold"));
        let em = t.insert(Some(body), elem("em", ""));
        let _ = t.insert(Some(em), text("italic"));
        let layout = construct_layout_tree_with(&t, &HashMap::new(), ConstructOptions::pixel());
        let s = first_child_text(&layout);
        assert!(
            s.contains("bold") && s.contains("italic"),
            "raw text present, got {s:?}"
        );
        assert!(!s.contains("**"), "no bold markers, got {s:?}");
        assert!(!s.contains('*'), "no italic markers, got {s:?}");
    }

    #[test]
    fn default_options_match_legacy_wrapper() {
        // The legacy construct_layout_tree must stay byte-identical to
        // construct_layout_tree_with(default) — existing callers rely on it.
        let tree = heading_tree("h1", "Example Domain");
        let mut styles = HashMap::new();
        styles.insert(
            3,
            vec![Declaration {
                property: "font-size".into(),
                value: "2em".into(),
                important: false,
            }],
        );
        let legacy = construct_layout_tree(&tree, &styles);
        let defaulted = construct_layout_tree_with(&tree, &styles, ConstructOptions::default());
        let mut a = String::new();
        let mut b = String::new();
        all_text(&legacy.root, &mut a);
        all_text(&defaulted.root, &mut b);
        assert_eq!(a, b);
        assert!(a.contains("EXAMPLE DOMAIN"));
        assert!(ConstructOptions::default().ascii_visuals);
        assert!(!ConstructOptions::pixel().ascii_visuals);
    }

    #[test]
    fn empty_strong_is_noop() {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let _ = t.insert(Some(root), elem("strong", ""));
        let layout = construct_layout_tree(&t, &HashMap::new());
        // No panic, no markers.
        assert_eq!(first_child_text(&layout), "");
    }

    #[test]
    fn wrap_first_last_spans_nested_leaves() {
        // <strong><a>link</a></strong>: first leaf gets prefix, same leaf
        // gets suffix (single-leaf case through a wrapper).
        let mut bx = LayoutBox::new(BoxType::Inline);
        let mut inner = LayoutBox::new(BoxType::Inline).with_text("link".into());
        inner.link = true;
        bx.children.push(inner);
        wrap_first_last_text(&mut bx, "**", "**");
        let leaf = find_first_text_leaf_mut(&mut bx).expect("leaf");
        assert_eq!(leaf.text.as_deref(), Some("**link**"));
    }

    // ---- M81: position:absolute/fixed detection ----

    fn decl(property: &str, value: &str) -> Declaration {
        Declaration {
            property: property.into(),
            value: value.into(),
            important: false,
        }
    }

    /// Document > body > div(text) with `position:absolute` style.
    fn absolute_div_tree() -> (Tree, NodeId) {
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let body = t.insert(Some(root), elem("body", ""));
        let div = t.insert(Some(body), elem("div", ""));
        let _ = t.insert(Some(div), text("BADGE"));
        (t, div)
    }

    #[test]
    fn position_absolute_marks_box_positioned() {
        let (tree, div) = absolute_div_tree();
        let mut styles = HashMap::new();
        styles.insert(div, vec![decl("position", "absolute")]);
        let layout = construct_layout_tree(&tree, &styles);
        // body > div(BADGE). The div box must carry positioned=true.
        let div_box = &layout.root.children[0].children[0];
        assert!(div_box.positioned, "position:absolute must set positioned");
    }

    #[test]
    fn position_fixed_marks_box_positioned() {
        let (tree, div) = absolute_div_tree();
        let mut styles = HashMap::new();
        styles.insert(div, vec![decl("position", "fixed")]);
        let layout = construct_layout_tree(&tree, &styles);
        let div_box = &layout.root.children[0].children[0];
        assert!(div_box.positioned, "position:fixed must set positioned");
    }

    #[test]
    fn position_static_or_relative_is_in_flow() {
        let (tree, div) = absolute_div_tree();
        let mut styles = HashMap::new();
        styles.insert(
            div,
            vec![decl("position", "relative"), decl("position", "static")],
        );
        let layout = construct_layout_tree(&tree, &styles);
        let div_box = &layout.root.children[0].children[0];
        assert!(!div_box.positioned, "static/relative stay in flow");
    }

    #[test]
    fn position_absolute_blockifies_inline_tag() {
        // CSS spec: absolutely positioned elements compute to
        // block-level, so `<span style="position:absolute">` becomes a
        // Block box (out-of-flow path) instead of an inline run member.
        let mut t = Tree::with_root(NodeData::Document);
        let root = t.root();
        let body = t.insert(Some(root), elem("body", ""));
        let span = t.insert(Some(body), elem("span", ""));
        let _ = t.insert(Some(span), text("BADGE"));
        let mut styles = HashMap::new();
        styles.insert(span, vec![decl("position", "absolute")]);
        let layout = construct_layout_tree(&t, &styles);
        // body is Block, span was inline → anonymous wrapper > span.
        let body_box = &layout.root.children[0];
        let wrap = &body_box.children[0];
        assert_eq!(wrap.box_type, BoxType::Anonymous);
        let span_box = &wrap.children[0];
        assert_eq!(span_box.box_type, BoxType::Block);
        assert!(span_box.positioned);
    }
}
