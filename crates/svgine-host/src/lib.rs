//! ADR-0019 阶段二（车道PF）：页面内 `<svg>` 交由 svgine 引擎真渲染。
//!
//! 数据流（浏览器宿主 std 消费面，`svgine-core` 默认全特性）：
//!
//! ```text
//! DOM Tree（JS 跑完后的最终树）
//!   ──extract_host_svgs──▶ 逐个 <svg> 子树重序列化（XML 转义 + xmlns 补齐）
//!   ──render_svg_rgba───▶ svgine parse_document → build_display_list
//!                          → raster::render_bg(BG_WHITE) → RGBA8
//!   ──composite_page_svgs──▶ 按布局盒锚点 alpha 复合进 pixel 截图画布
//! ```
//!
//! 契约边界（本阶段最小闭环）：
//! - 布局树占位符机制不动（construct 期 `[SVG w×h]` 文本占位保留）；
//!   布局集成（svg 占据真实布局空间）留给后续车道。
//! - 复合锚点 = 占位盒左上角（与 render::pixel::paint_box 同一
//!   cell→px 换算式），渲染尺寸 = svg width/height 属性（缺省
//!   viewBox，再缺省 100×100，与 construct 占位符缺省同源）。
//! - 白底复合（render_bg BG_WHITE）：先抹掉占位文本再落 SVG，
//!   保证「占位符文本消失」；透明 SVG 区域呈页面白底。
//! - 解析/渲染失败 = 保留占位符（stderr 计数，不 panic 不阻断截图）。

use std::collections::HashMap;

use browser_dom::{NodeData, NodeId, Tree};
use browser_layout::{LayoutBox, LayoutTree};

/// SVG 命名空间——HTML 解析不保留 xmlns，重序列化时补齐（svgine XML
/// 解析器需要名空间）。
const XMLNS: &str = "http://www.w3.org/2000/svg";

/// 单个 `<svg>` 的宿主渲染载荷。
pub struct HostSvg {
    /// DOM NodeId（与布局盒 `LayoutBox::element_id` 同一编号空间）。
    pub node_id: NodeId,
    /// 重序列化后的 SVG 标记（svgine `parse_document` 直读）。
    pub markup: String,
    /// 内在宽度（px）。width 属性 → viewBox → 100（construct 占位缺省）。
    pub width: f32,
    /// 内在高度（px）。height 属性 → viewBox → 100。
    pub height: f32,
}

/// 遍历 DOM 树，收集全部 `<svg>` 元素的渲染载荷（含 JS 注入的节点——
/// 调用方传入的是跑完 JS 的最终树）。文档序。
#[must_use]
pub fn extract_host_svgs(tree: &Tree) -> Vec<HostSvg> {
    let mut out = Vec::new();
    let mut stack: Vec<NodeId> = vec![tree.root()];
    while let Some(id) = stack.pop() {
        if let Some((tag, attrs)) = tree.data(id).as_element() {
            if tag.eq_ignore_ascii_case("svg") {
                out.push(host_svg_of(tree, id, attrs));
            }
        }
        // 文档序出栈：逆序压栈。
        for &child in tree.children_of(id).iter().rev() {
            stack.push(child);
        }
    }
    out
}

/// 单个 `<svg>` 节点 → 重序列化标记 + 内在尺寸。
fn host_svg_of(tree: &Tree, svg_id: NodeId, svg_attrs: &[(String, String)]) -> HostSvg {
    let viewbox = svg_attrs
        .iter()
        .find(|(k, _)| k == "viewBox")
        .and_then(|(_, v)| parse_viewbox(v));
    let attr_dim = |name: &str| -> Option<f32> {
        svg_attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .and_then(|(_, v)| v.trim().trim_end_matches("px").parse::<f32>().ok())
            .filter(|f| f.is_finite() && *f > 0.0)
    };
    // 尺寸缺省链：width/height 属性 → viewBox w/h → 100×100（与
    // construct.rs encode_svg_placeholder 的缺省同源）。
    let width = attr_dim("width")
        .or(viewbox.map(|(w, _)| w))
        .unwrap_or(100.0);
    let height = attr_dim("height")
        .or(viewbox.map(|(_, h)| h))
        .unwrap_or(100.0);

    let mut body = String::new();
    for child in tree.children_of(svg_id) {
        serialize_node(tree, *child, 1, &mut body);
    }
    let mut markup = String::with_capacity(body.len() + 128);
    markup.push_str("<svg xmlns=\"");
    markup.push_str(XMLNS);
    markup.push('"');
    for (k, v) in svg_attrs {
        markup.push_str(&format!(" {k}=\"{}\"", escape_attr(v)));
    }
    markup.push('>');
    markup.push_str(&body);
    markup.push_str("</svg>");

    HostSvg {
        node_id: svg_id,
        markup,
        width,
        height,
    }
}

/// 递归序列化 svg 子树。HTML 实体已在解析期解码，这里重新做 XML 转义。
fn serialize_node(tree: &Tree, id: NodeId, depth: usize, out: &mut String) {
    match tree.data(id) {
        NodeData::Element { tag, attrs } => {
            out.push_str(&"  ".repeat(depth));
            out.push('<');
            out.push_str(tag);
            for (k, v) in attrs {
                out.push_str(&format!(" {k}=\"{}\"", escape_attr(v)));
            }
            let children = tree.children_of(id);
            if children.is_empty() {
                out.push_str("/>\n");
                return;
            }
            out.push_str(">\n");
            for child in children {
                serialize_node(tree, *child, depth + 1, out);
            }
            out.push_str(&"  ".repeat(depth));
            out.push_str("</");
            out.push_str(tag);
            out.push_str(">\n");
        }
        NodeData::Text(s) => {
            let t = s.trim();
            if !t.is_empty() {
                out.push_str(&"  ".repeat(depth));
                out.push_str(&escape_text(t));
                out.push('\n');
            }
        }
        _ => {}
    }
}

/// `viewBox="min-x min-y w h"` → (w, h)。
fn parse_viewbox(v: &str) -> Option<(f32, f32)> {
    let nums: Vec<f32> = v
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    if nums.len() == 4 {
        Some((nums[2], nums[3]))
    } else {
        None
    }
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// svgine 渲染单枚 SVG → 白底不透明 RGBA8（长度 = w*h*4）。
///
/// `aa = 3`（svgine-cli 精档口径，与 OT 原型 B 路径同参）。
/// 解析失败返回 `None`（宿主侧保留占位符）。
#[must_use]
pub fn render_svg_rgba(markup: &str, w: u32, h: u32) -> Option<Vec<u8>> {
    if w == 0 || h == 0 {
        return None;
    }
    let doc = svgine_core::builder::parse_document(markup).ok()?;
    let dl = svgine_core::builder::build_display_list(&doc, w as f32, h as f32);
    // 空显示列表（解析容忍但无任何可绘制命令，如非 SVG 文本/纯 defs）
    // 返回 None：宿主保留占位符作为「未渲染」信号，不blank一块白矩形。
    if dl.cmds.is_empty() {
        return None;
    }
    let vp = svgine_core::raster::Viewport {
        width: w,
        height: h,
        format: svgine_core::raster::PixFmt::Rgba8,
        aa: 3,
    };
    let mut buf = vec![0u8; w as usize * h as usize * 4];
    // 白底不透明（ADR-0010 同源）：复合语义 = 白底上落 SVG，透明区域
    // 呈页面底色，且天然盖掉占位符文本。
    svgine_core::raster::render_bg(&dl, &vp, &mut buf, svgine_core::raster::BG_WHITE);
    Some(buf)
}

/// 单枚 SVG RGBA 复合进画布：先清白再 src-over（当前 render_svg_rgba
/// 恒不透明，blend 退化为行拷贝；保留通用式给未来透明底宿主）。
#[allow(clippy::too_many_arguments)]
fn blend_rect(
    canvas: &mut [u8],
    canvas_w: usize,
    canvas_h: usize,
    x0: i64,
    y0: i64,
    src: &[u8],
    src_w: usize,
    src_h: usize,
) {
    let x_start = x0.max(0);
    let y_start = y0.max(0);
    let x_end = (x0 + src_w as i64).min(canvas_w as i64);
    let y_end = (y0 + src_h as i64).min(canvas_h as i64);
    if x_end <= x_start || y_end <= y_start {
        return;
    }
    for cy in y_start..y_end {
        let sy = (cy - y0) as usize;
        let src_row = &src[sy * src_w * 4..(sy + 1) * src_w * 4];
        let dst_row_base = cy as usize * canvas_w * 4;
        for cx in x_start..x_end {
            let sx = (cx - x0) as usize;
            let s = &src_row[sx * 4..sx * 4 + 4];
            let d =
                &mut canvas[dst_row_base + (cx as usize) * 4..dst_row_base + (cx as usize) * 4 + 4];
            let a = u32::from(s[3]);
            if a == 255 {
                d[0] = s[0];
                d[1] = s[1];
                d[2] = s[2];
                d[3] = 255;
            } else if a > 0 {
                for c in 0..3 {
                    let sv = u32::from(s[c]);
                    let dv = u32::from(d[c]);
                    d[c] = ((sv * a + dv * (255 - a) + 127) / 255) as u8;
                }
                d[3] = 255;
            }
        }
    }
}

/// 把页面内全部 `<svg>` 真渲染复合进 pixel 截图画布（ADR-0019 阶段二
/// 最小闭环）。返回 `(成功复合枚数, 终态画布高 px)`。
///
/// 布局占位盒只有一行文字高，SVG 按 width/height 属性真渲染会纵向溢出
/// ——本函数把画布按需向下扩白（与页底同色），调用方以返回高度写 PNG。
///
/// 锚点换算与 `browser_render::pixel::paint_box` 同式：
/// `px = cell 坐标 × (cell_w, line_h)`（pixel 模式 scale=1.0）。
#[must_use]
pub fn composite_page_svgs(
    tree: &Tree,
    layout: &LayoutTree,
    canvas: &mut Vec<u8>,
    canvas_w: usize,
    canvas_h: usize,
    cell_w: f32,
    line_h: f32,
) -> (usize, usize) {
    if canvas.len() != canvas_w * canvas_h * 4 || canvas_w == 0 || canvas_h == 0 {
        return (0, canvas_h);
    }
    let hosts = extract_host_svgs(tree);
    if hosts.is_empty() {
        return (0, canvas_h);
    }
    let index: HashMap<NodeId, usize> = hosts
        .iter()
        .enumerate()
        .map(|(i, h)| (h.node_id, i))
        .collect();

    // 阶段一：布局盒 DFS 收集 (host 下标, 锚点, 渲染尺寸) 作业表。
    let mut jobs: Vec<(usize, i64, i64, u32, u32)> = Vec::new();
    collect_svg_jobs(
        &layout.root,
        tree,
        &hosts,
        &index,
        canvas_w,
        canvas_h,
        cell_w,
        line_h,
        &mut jobs,
    );
    if jobs.is_empty() {
        return (0, canvas_h);
    }

    // 画布扩高：占位盒只有一行高，真渲染纵向溢出时向下扩白
    // （与画布底色同色；上限防病态属性撑爆内存）。
    let needed = jobs
        .iter()
        .map(|(_, _, y, _, ih)| y + i64::from(*ih))
        .max()
        .unwrap_or(0)
        .min(MAX_GROW_PX);
    let mut height = canvas_h;
    if needed > height as i64 {
        height = needed as usize;
        canvas.resize(canvas_w * height * 4, 255);
    }

    // 阶段二：逐枚 svgine 渲染 + 锚点复合（失败保留占位符，跳过）。
    let mut painted = 0usize;
    for (hi, x0, y0, iw, ih) in &jobs {
        let Some(src) = render_svg_rgba(&hosts[*hi].markup, *iw, *ih) else {
            continue;
        };
        blend_rect(
            canvas,
            canvas_w,
            height,
            *x0,
            *y0,
            &src,
            *iw as usize,
            *ih as usize,
        );
        painted += 1;
    }
    (painted, height)
}

/// 扩高上限（px）——防 `height="100000"` 类病态属性。
const MAX_GROW_PX: i64 = 20_000;

/// 布局盒 DFS：element_id 命中 `<svg>` 节点即产出一份渲染作业
/// `(host 下标, 锚 x, 锚 y, 渲染 w, 渲染 h)`。
#[allow(clippy::too_many_arguments)]
fn collect_svg_jobs(
    bx: &LayoutBox,
    tree: &Tree,
    hosts: &[HostSvg],
    index: &HashMap<NodeId, usize>,
    canvas_w: usize,
    canvas_h: usize,
    cell_w: f32,
    line_h: f32,
    jobs: &mut Vec<(usize, i64, i64, u32, u32)>,
) {
    if let Some(id) = bx.element_id {
        if let Some(&hi) = index.get(&id) {
            // 双保险：确认该 DOM 节点确实是 svg（布局树与 DOM 树编号
            // 同源，此处只为防未来构造路径漂移）。
            let is_svg = tree
                .data(id)
                .as_element()
                .is_some_and(|(t, _)| t.eq_ignore_ascii_case("svg"));
            if is_svg {
                let (x0, y0, iw, ih) =
                    resolve_job(&hosts[hi], bx, canvas_w, canvas_h, cell_w, line_h);
                jobs.push((hi, x0, y0, iw, ih));
            }
        }
    }
    for child in &bx.children {
        collect_svg_jobs(
            child, tree, hosts, index, canvas_w, canvas_h, cell_w, line_h, jobs,
        );
    }
}

/// 单枚作业解析：内在尺寸 → 画布适配缩放 → 锚点（paint_box 同式换算）。
fn resolve_job(
    host: &HostSvg,
    bx: &LayoutBox,
    canvas_w: usize,
    canvas_h: usize,
    cell_w: f32,
    line_h: f32,
) -> (i64, i64, u32, u32) {
    // 画布适配：SVG 大于画布时等比缩小（viewBox 映射交还 svgine pAR，
    // 不在本层失真）。
    let mut w = host.width.max(1.0);
    let mut h = host.height.max(1.0);
    let fit = (canvas_w as f32 / w).min(canvas_h as f32 / h).min(1.0);
    w *= fit;
    h *= fit;
    let iw = (w.ceil() as u32).clamp(1, 8192);
    let ih = (h.ceil() as u32).clamp(1, 8192);
    // 锚点 = 占位盒左上角（paint_box 同式换算）。
    let d = &bx.dimensions;
    let x0 = (d.x * cell_w).round() as i64;
    let y0 = (d.y * line_h).round() as i64;
    (x0, y0, iw, ih)
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_layout::{BoxType, Dimensions, LayoutBox};

    /// 手工搭最小 DOM：document > body > svg(width=4 height=4) > rect。
    /// 本 crate 不依赖 html-parser，测试用构造 API 直建树。
    fn build_tree() -> (Tree, NodeId) {
        let mut tree = Tree::with_root(NodeData::Document);
        let body = tree.insert(
            Some(tree.root()),
            NodeData::Element {
                tag: "body".into(),
                attrs: vec![],
            },
        );
        let svg = tree.insert(
            Some(body),
            NodeData::Element {
                tag: "svg".into(),
                attrs: vec![
                    ("width".into(), "4".into()),
                    ("height".into(), "4".into()),
                    ("viewBox".into(), "0 0 4 4".into()),
                ],
            },
        );
        let _rect = tree.insert(
            Some(svg),
            NodeData::Element {
                tag: "rect".into(),
                attrs: vec![
                    ("width".into(), "4".into()),
                    ("height".into(), "4".into()),
                    ("fill".into(), "#ff0000".into()),
                ],
            },
        );
        (tree, svg)
    }

    fn svg_box(node_id: NodeId, x: f32, y: f32) -> LayoutTree {
        let mut bx = LayoutBox::new(BoxType::Inline);
        bx.element_id = Some(node_id);
        bx.dimensions = Dimensions::new(x, y, 13.0, 1.0);
        LayoutTree { root: bx }
    }

    #[test]
    fn escape_attr_covers_xml_metachars() {
        assert_eq!(escape_attr("a&b<c>d\"e"), "a&amp;b&lt;c&gt;d&quot;e");
    }

    #[test]
    fn parse_viewbox_wh() {
        assert_eq!(parse_viewbox("0 0 320 200"), Some((320.0, 200.0)));
        assert_eq!(parse_viewbox("0,0,10,20"), Some((10.0, 20.0)));
        assert_eq!(parse_viewbox("junk"), None);
    }

    #[test]
    fn render_rejects_zero_size_and_bad_markup() {
        assert!(render_svg_rgba("<svg/>", 0, 10).is_none());
        assert!(render_svg_rgba("not svg at all", 10, 10).is_none());
    }

    #[test]
    fn render_white_opaque_full_bleed() {
        // 注意 fill="#ff0000" 含 `"#` 序列——单 `#` raw string 会被提前
        // 终止（AGENTS.md 常踩坑清单的再实例），必须双 `##`。
        let buf = render_svg_rgba(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4" fill="#ff0000"/></svg>"##,
            4,
            4,
        )
        .expect("render ok");
        assert_eq!(buf.len(), 64);
        // 左上角像素 = 纯红、不透明（render_bg 白底 + 全覆盖矩形）。
        assert_eq!(&buf[0..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn extract_rewrites_xmlns_and_dims() {
        let (tree, svg) = build_tree();
        let hosts = extract_host_svgs(&tree);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].node_id, svg);
        assert_eq!((hosts[0].width, hosts[0].height), (4.0, 4.0));
        assert!(
            hosts[0]
                .markup
                .starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""),
            "xmlns must be injected: {}",
            hosts[0].markup
        );
        assert!(hosts[0].markup.contains("<rect"), "children serialized");
    }

    #[test]
    fn composite_lands_at_box_anchor_and_overwrites_placeholder() {
        let (tree, svg) = build_tree();
        // 锚 (2,1)，cell 尺寸 1×1 → px (2,1)。4×4 红 rect 盖 (2..6, 1..5)。
        let layout = svg_box(svg, 2.0, 1.0);
        let mut canvas = vec![255u8; 8 * 8 * 4];
        // 预埋「占位文本」黑像素在锚点处，验证被覆盖。
        let anchor = (8 + 2) * 4;
        canvas[anchor..anchor + 3].copy_from_slice(&[0, 0, 0]);
        let (painted, height) = composite_page_svgs(&tree, &layout, &mut canvas, 8, 8, 1.0, 1.0);
        assert_eq!(painted, 1);
        assert_eq!(height, 8, "4x4 svg at y=1 fits 8px canvas, no growth");
        assert_eq!(
            &canvas[anchor..anchor + 4],
            &[255, 0, 0, 255],
            "red over placeholder"
        );
        // 锚外邻域保持白底。
        let outside = (8 + 1) * 4;
        assert_eq!(&canvas[outside..outside + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn composite_grows_canvas_for_oversized_svg() {
        let (tree, svg) = build_tree();
        // 8px 画布装不下 y=6 起的 4px 高 svg → 扩高到 10。
        let layout = svg_box(svg, 0.0, 6.0);
        let mut canvas = vec![255u8; 8 * 8 * 4];
        let (painted, height) = composite_page_svgs(&tree, &layout, &mut canvas, 8, 8, 1.0, 1.0);
        assert_eq!(painted, 1);
        assert_eq!(height, 10);
        assert_eq!(canvas.len(), 8 * 10 * 4);
        // 扩白区末行保持白（rect 只盖 y=6..10 顶到 10 为止）。
        let last_row = 9 * 8 * 4;
        assert_eq!(
            &canvas[last_row..last_row + 4],
            &[255, 0, 0, 255],
            "svg bottom lands in grown area"
        );
    }

    #[test]
    fn composite_skips_non_svg_element_ids() {
        let (tree, _svg) = build_tree();
        let body_id = tree.children_of(tree.root())[0];
        let layout = svg_box(body_id, 0.0, 0.0);
        let mut canvas = vec![255u8; 8 * 8 * 4];
        let (painted, height) = composite_page_svgs(&tree, &layout, &mut canvas, 8, 8, 1.0, 1.0);
        assert_eq!(painted, 0, "body box must not composite");
        assert_eq!(height, 8);
        assert!(canvas.iter().all(|&v| v == 255), "canvas untouched");
    }
}
