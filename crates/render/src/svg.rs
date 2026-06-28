//! M70.3: `<svg>` 基础图形 → ASCII art 渲染。
//!
//! 支持最小子集：`<circle>` `<rect>` `<line>` `<polygon>` 的 fill 属性。
//! 用 ASCII ramp 表示填充/描边，fill 颜色 → ANSI 38;2;R;G;B 前景色。
//!
//! 不支持 path（贝塞尔）、gradient、filter、mask（GOALS 非目标）。
//! 设计：仿 `<img>` 范式——construct 阶段把 svg 子元素编码成占位符字符串，
//! CLI 渲染后用本模块替换占位符为 ASCII art。
//
// clippy: 光栅化函数用显式索引访问 2D 网格（比迭代器更清晰），grid 的
// 复杂元组类型是局部实现细节。
#![allow(clippy::needless_range_loop, clippy::type_complexity)]

use browser_css_engine::parse_color;

/// 单个网格格子的状态：(是否填充, 颜色)。
type Cell = (bool, Option<(u8, u8, u8)>);

/// SVG 形状（从 DOM attrs 解析）。
#[derive(Debug, Clone)]
pub enum SvgShape {
    /// circle: cx, cy, r, fill
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
        fill: Option<String>,
    },
    /// rect: x, y, w, h, fill
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        fill: Option<String>,
    },
    /// line: x1, y1, x2, y2, stroke
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        stroke: Option<String>,
    },
    /// polygon: points [(x,y),...], fill
    Polygon {
        points: Vec<(f32, f32)>,
        fill: Option<String>,
    },
}

/// ASCII 灰度字符表（从暗到亮），复用 image.rs 的 ramp。
const ASCII_RAMP: &[char] = &[' ', '.', ':', '-', '=', '+', '*', '#', '%', '@'];

/// 把一批 SVG 形状光栅化成带颜色的 ASCII art 字符串。
///
/// - `shapes`: svg 内的图形列表
/// - `vb_w`, `vb_h`: viewBox 宽高（决定字符网格的纵横比映射）
/// - `max_w`, `max_h`: ASCII 输出的最大列数/行数
#[must_use]
pub fn svg_to_ascii(shapes: &[SvgShape], vb_w: f32, vb_h: f32, max_w: u32, max_h: u32) -> String {
    if vb_w <= 0.0 || vb_h <= 0.0 || shapes.is_empty() {
        return String::new();
    }
    let new_w = max_w.max(1) as usize;
    let new_h = max_h.max(1) as usize;
    // viewBox 坐标 → 字符网格坐标的缩放
    let sx = new_w as f32 / vb_w;
    let sy = new_h as f32 / vb_h;

    // 像素缓冲：每个格子 (filled: bool, rgb: Option<(u8,u8,u8)>)
    // 默认未填充（背景白）。
    let mut grid = vec![vec![(false, None); new_w]; new_h];

    for shape in shapes {
        match shape {
            SvgShape::Circle { cx, cy, r, fill } => {
                let color = fill.as_deref().and_then(parse_color);
                draw_circle(&mut grid, cx * sx, cy * sy, r * ((sx + sy) / 2.0), color);
            }
            SvgShape::Rect { x, y, w, h, fill } => {
                let color = fill.as_deref().and_then(parse_color);
                draw_rect(&mut grid, x * sx, y * sy, w * sx, h * sy, color);
            }
            SvgShape::Line {
                x1,
                y1,
                x2,
                y2,
                stroke,
            } => {
                let color = stroke.as_deref().and_then(parse_color);
                draw_line(&mut grid, x1 * sx, y1 * sy, x2 * sx, y2 * sy, color);
            }
            SvgShape::Polygon { points, fill } => {
                let color = fill.as_deref().and_then(parse_color);
                let pts: Vec<(f32, f32)> = points.iter().map(|(x, y)| (x * sx, y * sy)).collect();
                draw_polygon(&mut grid, &pts, color);
            }
        }
    }

    // 渲染成带 ANSI 颜色的 ASCII（复用 image.rs 的 run-merge 思路）
    let mut out = String::with_capacity(new_w * new_h * 8 + new_h);
    for y in 0..new_h {
        let mut run_color: Option<(u8, u8, u8)> = None;
        let mut run_chars = String::new();
        for x in 0..new_w {
            let (filled, color) = grid[y][x];
            let ch = if filled {
                // 填充用接近满的字符（密度高）
                ASCII_RAMP[ASCII_RAMP.len() - 2]
            } else {
                ' '
            };
            let cell_color = if filled {
                color.unwrap_or((60, 60, 60))
            } else {
                (255, 255, 255)
            };
            if run_color.is_some() && run_color != Some(cell_color) {
                if let Some((rr, gg, bb)) = run_color.take() {
                    out.push_str(&format!("\x1b[38;2;{rr};{gg};{bb}m{run_chars}\x1b[0m"));
                }
                run_chars.clear();
            }
            run_color = Some(cell_color);
            run_chars.push(ch);
        }
        if let Some((rr, gg, bb)) = run_color {
            out.push_str(&format!("\x1b[38;2;{rr};{gg};{bb}m{run_chars}\x1b[0m"));
        }
        out.push('\n');
    }
    out
}

/// 中点画圆算法（填充版）：画一个实心圆。
fn draw_circle(grid: &mut [Vec<Cell>], cx: f32, cy: f32, r: f32, color: Option<(u8, u8, u8)>) {
    if r <= 0.0 {
        return;
    }
    let h = grid.len();
    let w = if h > 0 { grid[0].len() } else { 0 };
    let r2 = r * r;
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            if dx * dx + dy * dy <= r2 {
                grid[y][x] = (true, color);
            }
        }
    }
}

/// 实心矩形。
fn draw_rect(grid: &mut [Vec<Cell>], x: f32, y: f32, w: f32, h: f32, color: Option<(u8, u8, u8)>) {
    let gh = grid.len();
    let gw = if gh > 0 { grid[0].len() } else { 0 };
    let x0 = x.round().max(0.0) as usize;
    let y0 = y.round().max(0.0) as usize;
    let x1 = ((x + w).round() as usize).min(gw);
    let y1 = ((y + h).round() as usize).min(gh);
    for yy in y0..y1 {
        for xx in x0..x1 {
            grid[yy][xx] = (true, color);
        }
    }
}

/// Bresenham 直线算法。
fn draw_line(
    grid: &mut [Vec<Cell>],
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    color: Option<(u8, u8, u8)>,
) {
    let h = grid.len();
    let w = if h > 0 { grid[0].len() } else { 0 };
    let mut x0 = x1.round() as isize;
    let mut y0 = y1.round() as isize;
    let xend = x2.round() as isize;
    let yend = y2.round() as isize;
    let dx = (xend - x0).abs();
    let dy = -(yend - y0).abs();
    let sx = if x0 < xend { 1 } else { -1 };
    let sy = if y0 < yend { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        if y0 >= 0 && (y0 as usize) < h && x0 >= 0 && (x0 as usize) < w {
            grid[y0 as usize][x0 as usize] = (true, color);
        }
        if x0 == xend && y0 == yend {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

/// 多边形：先画边框（连线），再做扫描线填充（凸多边形够用）。
fn draw_polygon(grid: &mut [Vec<Cell>], points: &[(f32, f32)], color: Option<(u8, u8, u8)>) {
    if points.len() < 2 {
        return;
    }
    // 画边框
    for i in 0..points.len() {
        let (x1, y1) = points[i];
        let (x2, y2) = points[(i + 1) % points.len()];
        draw_line(grid, x1, y1, x2, y2, color);
    }
    // 扫描线填充（凸多边形）
    let h = grid.len();
    let w = if h > 0 { grid[0].len() } else { 0 };
    for y in 0..h {
        let yc = y as f32 + 0.5;
        // 求所有边与扫描线的交点 x 坐标
        let mut xs = Vec::new();
        for i in 0..points.len() {
            let (x1, y1) = points[i];
            let (x2, y2) = points[(i + 1) % points.len()];
            if (y1 <= yc && y2 > yc) || (y2 <= yc && y1 > yc) {
                let t = (yc - y1) / (y2 - y1);
                xs.push(x1 + t * (x2 - x1));
            }
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut i = 0;
        while i + 1 < xs.len() {
            let xa = xs[i].round().max(0.0) as usize;
            let xb = (xs[i + 1].round() as usize).min(w);
            for x in xa..xb {
                grid[y][x] = (true, color);
            }
            i += 2;
        }
    }
}

/// M70.3: 解析 SVG 数值属性（容错：无效返回 0.0）。
fn parse_svg_float(attrs: &[(String, String)], key: &str) -> f32 {
    attrs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .and_then(|(_, v)| v.trim().parse::<f32>().ok())
        .unwrap_or(0.0)
}

/// M70.3: 解析 SVG polygon points 字符串 "x1,y1 x2,y2 ..." → 坐标列表。
fn parse_points(s: &str) -> Vec<(f32, f32)> {
    s.split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .chunks(2)
        .filter_map(|chunk| {
            if chunk.len() == 2 {
                let x = chunk[0].parse::<f32>().ok()?;
                let y = chunk[1].parse::<f32>().ok()?;
                Some((x, y))
            } else {
                None
            }
        })
        .collect()
}

/// M70.3: 从 svg 的子元素 attrs 列表解析出 SvgShape 列表。
///
/// `child_elements`: [(tag, attrs)] —— svg 的每个子元素。
/// attrs 是 Vec<(String, String)>。
#[must_use]
pub fn parse_svg_shapes(child_elements: &[(&str, &[(String, String)])]) -> Vec<SvgShape> {
    let mut shapes = Vec::new();
    for (tag, attrs) in child_elements {
        let lower = tag.to_ascii_lowercase();
        match lower.as_str() {
            "circle" => {
                let cx = parse_svg_float(attrs, "cx");
                let cy = parse_svg_float(attrs, "cy");
                let r = parse_svg_float(attrs, "r");
                let fill = attrs
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("fill"))
                    .map(|(_, v)| v.clone());
                shapes.push(SvgShape::Circle { cx, cy, r, fill });
            }
            "rect" => {
                let x = parse_svg_float(attrs, "x");
                let y = parse_svg_float(attrs, "y");
                let w = parse_svg_float(attrs, "width");
                let h = parse_svg_float(attrs, "height");
                let fill = attrs
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("fill"))
                    .map(|(_, v)| v.clone());
                shapes.push(SvgShape::Rect { x, y, w, h, fill });
            }
            "line" => {
                let x1 = parse_svg_float(attrs, "x1");
                let y1 = parse_svg_float(attrs, "y1");
                let x2 = parse_svg_float(attrs, "x2");
                let y2 = parse_svg_float(attrs, "y2");
                let stroke = attrs
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("stroke"))
                    .map(|(_, v)| v.clone());
                shapes.push(SvgShape::Line {
                    x1,
                    y1,
                    x2,
                    y2,
                    stroke,
                });
            }
            "polygon" | "polyline" => {
                let pts_str = attrs
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("points"))
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("");
                let points = parse_points(pts_str);
                let fill = attrs
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("fill"))
                    .map(|(_, v)| v.clone());
                shapes.push(SvgShape::Polygon { points, fill });
            }
            _ => {}
        }
    }
    shapes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circle_renders_filled_disk() {
        let shapes = vec![SvgShape::Circle {
            cx: 50.0,
            cy: 50.0,
            r: 40.0,
            fill: Some("red".into()),
        }];
        let s = svg_to_ascii(&shapes, 100.0, 100.0, 20, 20);
        // 中心行应该有填充字符
        let middle_line = s.lines().nth(10).unwrap_or("");
        let filled_count = middle_line.chars().filter(|c| !c.is_whitespace()).count();
        assert!(
            filled_count > 5,
            "circle middle row should have many filled chars, got {filled_count}"
        );
        // 含红色 ANSI
        assert!(s.contains("38;2;255;0;0"), "red fill → 38;2;255;0;0");
    }

    #[test]
    fn rect_renders_filled_box() {
        let shapes = vec![SvgShape::Rect {
            x: 10.0,
            y: 10.0,
            w: 80.0,
            h: 80.0,
            fill: Some("blue".into()),
        }];
        let s = svg_to_ascii(&shapes, 100.0, 100.0, 10, 10);
        // rect 几乎填满，蓝色
        assert!(s.contains("38;2;0;0;255"), "blue fill → 38;2;0;0;255");
    }

    #[test]
    fn line_renders_diagonal() {
        let shapes = vec![SvgShape::Line {
            x1: 0.0,
            y1: 0.0,
            x2: 100.0,
            y2: 100.0,
            stroke: Some("black".into()),
        }];
        let s = svg_to_ascii(&shapes, 100.0, 100.0, 10, 10);
        // 对角线至少有几个填充点
        let total_filled = s
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '\n')
            .count();
        assert!(total_filled > 0, "diagonal line should have points");
    }

    #[test]
    fn empty_shapes_returns_empty() {
        let s = svg_to_ascii(&[], 100.0, 100.0, 10, 10);
        assert!(s.is_empty());
    }

    #[test]
    fn parse_points_basic() {
        let pts = parse_points("10,20 30,40 50,60");
        assert_eq!(pts, vec![(10.0, 20.0), (30.0, 40.0), (50.0, 60.0)]);
    }

    #[test]
    fn parse_svg_float_default_zero() {
        let attrs = vec![("r".into(), "42".into())];
        assert_eq!(parse_svg_float(&attrs, "r"), 42.0);
        assert_eq!(parse_svg_float(&attrs, "missing"), 0.0);
    }

    #[test]
    fn parse_svg_shapes_from_elements() {
        let circle_attrs = vec![
            ("cx".into(), "50".into()),
            ("cy".into(), "50".into()),
            ("r".into(), "40".into()),
            ("fill".into(), "red".into()),
        ];
        let elements: Vec<(&str, &[(String, String)])> = vec![("circle", &circle_attrs)];
        let shapes = parse_svg_shapes(&elements);
        assert_eq!(shapes.len(), 1);
        match &shapes[0] {
            SvgShape::Circle { cx, cy, r, fill } => {
                assert_eq!((*cx, *cy, *r), (50.0, 50.0, 40.0));
                assert_eq!(fill.as_deref(), Some("red"));
            }
            _ => panic!("expected Circle"),
        }
    }
}
