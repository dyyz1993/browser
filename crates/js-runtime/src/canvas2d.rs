//! M94: canvas 2D 真像素实现（阶段 1）。
//!
//! 目标不是与 Chrome 字节级一致（M94 评估已判不可行），而是让
//! CanvasRenderingContext2D 从 no-op stub 变成**真实光栅化**：
//! 文本（fontdue）、路径填充（rect/arc，4x4 超采样 AA，evenodd/nonzero）、
//! 半透明/`multiply` 合成、toDataURL（png + base64）、getImageData。
//! canvasFingerprint 因此从常量变成真实稳定值（同 Firefox 的合法形态）。
//!
//! 架构：thread_local 槽位存 `HashMap<usize, Canvas2D>`（与 CURRENT_TREE
//! 等 slot 模式一致——`SharedTree !Send`，只能线程内共享）。JS 侧
//! CanvasRenderingContext2D 持有数字 id，方法经 `__cv*` 桥调本模块。
//! 限制（爬虫够用）：无描边（stroke*）、无 gradient/pattern 真实现、
//! emoji（非 BMP 字形）跳过。

use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    static CANVASES: RefCell<HashMap<usize, Canvas2D>> = RefCell::new(HashMap::new());
    static CANVAS_SEQ: RefCell<usize> = const { RefCell::new(0) };
    // M94: 当前绘制状态（桥参数上限拆分——JS 每次 draw 前先 setStyle）
    static CV_STATE: RefCell<(String, f64, bool)> =
        RefCell::new(("#000000".to_string(), 1.0, false));
}

/// 当前绘制样式（桥 state-first 模式的快照）。
#[derive(Clone, Copy)]
struct CvStyle {
    color: (u8, u8, u8, u8),
    alpha: f64,
    multiply: bool,
}

fn cv_style_now() -> CvStyle {
    let (style, alpha, multiply) = CV_STATE.with(|s| s.borrow().clone());
    CvStyle {
        color: parse_color(&style),
        alpha,
        multiply,
    }
}

#[derive(Clone)]
enum PathCmd {
    Rect { x: f32, y: f32, w: f32, h: f32 },
    Poly(Vec<(f32, f32)>),
}

pub struct Canvas2D {
    w: usize,
    h: usize,
    buf: Vec<u8>, // RGBA 行主序
    path: Vec<PathCmd>,
}

/// CSS 颜色解析（#rgb/#rrggbb/rgb()/rgba()/命名子集）→ 非透明 (r,g,b,a)。
fn parse_color(s: &str) -> (u8, u8, u8, u8) {
    let t = s.trim();
    let named = |n: &str| -> Option<(u8, u8, u8)> {
        Some(match n {
            "black" => (0, 0, 0),
            "white" => (255, 255, 255),
            "red" => (255, 0, 0),
            "green" => (0, 128, 0),
            "blue" => (0, 0, 255),
            "transparent" => return Some((0, 0, 0)),
            _ => return None,
        })
    };
    if let Some(hex) = t.strip_prefix('#') {
        let (r, g, b) = match hex.len() {
            3 => (
                u8::from_str_radix(&hex[0..1].repeat(2), 16).unwrap_or(0),
                u8::from_str_radix(&hex[1..2].repeat(2), 16).unwrap_or(0),
                u8::from_str_radix(&hex[2..3].repeat(2), 16).unwrap_or(0),
            ),
            6 => (
                u8::from_str_radix(&hex[0..2], 16).unwrap_or(0),
                u8::from_str_radix(&hex[2..4], 16).unwrap_or(0),
                u8::from_str_radix(&hex[4..6], 16).unwrap_or(0),
            ),
            8 => (
                u8::from_str_radix(&hex[0..2], 16).unwrap_or(0),
                u8::from_str_radix(&hex[2..4], 16).unwrap_or(0),
                u8::from_str_radix(&hex[4..6], 16).unwrap_or(0),
            ),
            _ => (0, 0, 0),
        };
        let a = if hex.len() == 8 {
            u8::from_str_radix(&hex[6..8], 16).unwrap_or(255)
        } else {
            255
        };
        return (r, g, b, a);
    }
    if let Some(rest) = t.strip_prefix("rgba(").and_then(|r| r.strip_suffix(')')) {
        let parts: Vec<&str> = rest.split(',').map(str::trim).collect();
        if parts.len() == 4 {
            let f = |i: usize, d: f64| -> f64 {
                parts.get(i).and_then(|s| s.parse().ok()).unwrap_or(d)
            };
            let a = f(3, 1.0);
            return (
                (f(0, 0.0) * 255.0).round().clamp(0.0, 255.0) as u8,
                (f(1, 0.0) * 255.0).round().clamp(0.0, 255.0) as u8,
                (f(2, 0.0) * 255.0).round().clamp(0.0, 255.0) as u8,
                (a.clamp(0.0, 1.0) * 255.0).round() as u8,
            );
        }
    }
    if let Some(rest) = t.strip_prefix("rgb(").and_then(|r| r.strip_suffix(')')) {
        let parts: Vec<f64> = rest
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        if parts.len() == 3 {
            return (parts[0] as u8, parts[1] as u8, parts[2] as u8, 255);
        }
    }
    if let Some(c) = named(t.to_lowercase().as_str()) {
        let (r, g, b) = c;
        return (r, g, b, if t == "transparent" { 0 } else { 255 });
    }
    (0, 0, 0, 255)
}

impl Canvas2D {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w: w.max(1),
            h: h.max(1),
            buf: vec![0; w.max(1) * h.max(1) * 4],
            path: Vec::new(),
        }
    }

    fn resize(&mut self, w: usize, h: usize) {
        let (w, h) = (w.max(1), h.max(1));
        if w != self.w || h != self.h {
            self.buf = vec![0; w * h * 4];
            self.w = w;
            self.h = h;
        }
    }

    /// 源-目标合成（multiply 按 canvas 规约每通道 s*d/255；其余按 source-over）。
    fn blend(
        &mut self,
        x: i64,
        y: i64,
        cov: u8,
        color: (u8, u8, u8, u8),
        alpha: f64,
        multiply: bool,
    ) {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 || cov == 0 {
            return;
        }
        let sa = (color.3 as f64 / 255.0) * (cov as f64 / 255.0) * alpha;
        if sa <= 0.0 {
            return;
        }
        let idx = ((y as usize) * self.w + x as usize) * 4;
        let (dr, dg, db, da) = (
            self.buf[idx] as f64,
            self.buf[idx + 1] as f64,
            self.buf[idx + 2] as f64,
            self.buf[idx + 3] as f64 / 255.0,
        );
        if multiply {
            // sRGB 域逐通道相乘（与 Skia canvas 的 multiply 近似；色彩管理差异
            // 见 M94 评估层 4——阶段 1 不做 linearized）。
            let sr = color.0 as f64 * dr / 255.0;
            let sg = color.1 as f64 * dg / 255.0;
            let sb = color.2 as f64 * db / 255.0;
            let out_a = sa + da * (1.0 - sa);
            self.buf[idx] = ((sr * sa + dr * (1.0 - sa)) / out_a.max(1e-6) * 255.0) as u8;
            self.buf[idx + 1] = ((sg * sa + dg * (1.0 - sa)) / out_a.max(1e-6) * 255.0) as u8;
            self.buf[idx + 2] = ((sb * sa + db * (1.0 - sa)) / out_a.max(1e-6) * 255.0) as u8;
            self.buf[idx + 3] = (out_a * 255.0) as u8;
        } else {
            // source-over（canvas 默认）：预乘合成
            let out_a = sa + da * (1.0 - sa);
            self.buf[idx] =
                ((color.0 as f64 * sa + dr * (1.0 - sa)) / out_a.max(1e-6) * out_a) as u8;
            self.buf[idx + 1] =
                ((color.1 as f64 * sa + dg * (1.0 - sa)) / out_a.max(1e-6) * out_a) as u8;
            self.buf[idx + 2] =
                ((color.2 as f64 * sa + db * (1.0 - sa)) / out_a.max(1e-6) * out_a) as u8;
            self.buf[idx + 3] = (out_a * 255.0) as u8;
        }
    }

    /// 点在多边形集内的 winding（nonzero）或 crossings（evenodd）判定。
    fn hit(&self, px: f64, py: f64, evenodd: bool) -> bool {
        let mut winding: i32 = 0;
        for cmd in &self.path {
            match cmd {
                PathCmd::Rect { x, y, w, h } => {
                    let (x0, y0, x1, y1) = (
                        *x as f64,
                        *y as f64,
                        *x as f64 + *w as f64,
                        *y as f64 + *h as f64,
                    );
                    // 矩形按 4 边折线处理（与 arc 采样统一）
                    let pts = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
                    wind_poly(px, py, &pts, evenodd, &mut winding);
                }
                PathCmd::Poly(p) => {
                    let pts: Vec<(f64, f64)> =
                        p.iter().map(|(x, y)| (*x as f64, *y as f64)).collect();
                    wind_poly(px, py, &pts, evenodd, &mut winding);
                }
            }
        }
        if evenodd {
            winding % 2 != 0
        } else {
            winding != 0
        }
    }

    /// 当前路径填充：4x4 超采样覆盖 → blend。
    fn fill(&mut self, color: (u8, u8, u8, u8), alpha: f64, rule: &str, multiply: bool) {
        let evenodd = rule.eq_ignore_ascii_case("evenodd");
        // 路径包围盒（限界扫描）
        let (mut minx, mut miny, mut maxx, mut maxy) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for cmd in &self.path {
            match cmd {
                PathCmd::Rect { x, y, w, h } => {
                    minx = minx.min(*x as f64);
                    miny = miny.min(*y as f64);
                    maxx = maxx.max(*x as f64 + *w as f64);
                    maxy = maxy.max(*y as f64 + *h as f64);
                }
                PathCmd::Poly(p) => {
                    for (x, y) in p {
                        minx = minx.min(*x as f64);
                        miny = miny.min(*y as f64);
                        maxx = maxx.max(*x as f64);
                        maxy = maxy.max(*y as f64);
                    }
                }
            }
        }
        if minx > maxx {
            return;
        }
        let x0 = minx.floor().max(0.0) as i64;
        let y0 = miny.floor().max(0.0) as i64;
        let x1 = maxx.ceil().min(self.w as f64) as i64;
        let y1 = maxy.ceil().min(self.h as f64) as i64;
        const SUB: f64 = 4.0;
        for py in y0..y1 {
            for px in x0..x1 {
                let mut hits = 0u32;
                for sy in 0..SUB as u32 {
                    for sx in 0..SUB as u32 {
                        let fx = px as f64 + (sx as f64 + 0.5) / SUB;
                        let fy = py as f64 + (sy as f64 + 0.5) / SUB;
                        if self.hit(fx, fy, evenodd) {
                            hits += 1;
                        }
                    }
                }
                let cov = ((hits as f64 / (SUB * SUB)) * 255.0).round() as u8;
                self.blend(px, py, cov, color, alpha, multiply);
            }
        }
    }

    /// 文本：fontdue 光栅化（BMP 字形；emoji 等非 BMP 跳过——M94 评估层 3）。
    fn fill_text(&mut self, text: &str, x: f64, y: f64, px: f64, family: &str, st: CvStyle) {
        let font = match font_for(family) {
            Some(f) => f,
            None => return, // 系统字体不可用：文本 no-op（几何路径不受影响）
        };
        let mut pen = x as f32;
        for ch in text.chars() {
            if (ch as u32) >= 0x1_0000 {
                pen += px as f32 * 0.6; // emoji 占位推进（不渲染）
                continue;
            }
            let (m, bmp) = font.rasterize(ch, px as f32);
            let y0 = y as i32 - m.ymin - m.height as i32 + 1;
            for dy in 0..m.height as i32 {
                for dx in 0..m.width as i32 {
                    let a = bmp[(dy * m.width as i32 + dx) as usize];
                    if a == 0 {
                        continue;
                    }
                    let gx = pen as i32 + m.xmin + dx;
                    let gy = y0 + dy;
                    // 字形 coverage 直接作 alpha（文本与几何统一走 blend）
                    self.blend(gx as i64, gy as i64, a, st.color, st.alpha, st.multiply);
                }
            }
            pen += m.advance_width;
        }
    }
}

fn wind_poly(px: f64, py: f64, pts: &[(f64, f64)], evenodd: bool, winding: &mut i32) {
    let n = pts.len();
    if n < 3 {
        return;
    }
    let mut cross: i32 = 0;
    for i in 0..n {
        let (ax, ay) = (pts[i].0, pts[i].1);
        let (bx, by) = (pts[(i + 1) % n].0, pts[(i + 1) % n].1);
        if (ay <= py) != (by <= py) {
            let t = (py - ay) / (by - ay);
            let xint = ax + t * (bx - ax);
            if xint > px {
                cross += if by > ay { 1 } else { -1 };
            }
        }
    }
    if evenodd {
        *winding += cross.abs();
    } else {
        *winding += cross;
    }
}

// ---- 字体（一次性加载 + Box::leak 取 'static；与原型同法） ----

fn font_for(family: &str) -> Option<&'static fontdue::Font> {
    use std::sync::OnceLock;
    static HELVETICA: OnceLock<Option<&'static fontdue::Font>> = OnceLock::new();
    static ARIAL: OnceLock<Option<&'static fontdue::Font>> = OnceLock::new();
    let fam = family.to_lowercase();
    if fam.contains("arial") {
        *ARIAL.get_or_init(|| load_font("/System/Library/Fonts/Supplemental/Arial.ttf"))
    } else {
        *HELVETICA.get_or_init(|| load_font("/System/Library/Fonts/Helvetica.ttc"))
    }
}

fn load_font(path: &str) -> Option<&'static fontdue::Font> {
    let bytes = std::fs::read(path).ok()?;
    let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    let font = fontdue::Font::from_bytes(leaked, fontdue::FontSettings::default()).ok()?;
    Some(Box::leak(Box::new(font)))
}

// ---- base64（无第三方依赖的 20 行实现） ----

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// ---- 桥入口（JS `__cv*` 调用；全部纯数据进出） ----

fn with_canvas<T>(id: f64, f: impl FnOnce(&mut Canvas2D) -> T) -> Option<T> {
    let id = id as usize;
    CANVASES.with(|c| c.borrow_mut().get_mut(&id).map(f))
}

pub fn cv_new(w: f64, h: f64) -> f64 {
    CANVAS_SEQ.with(|s| {
        let mut s = s.borrow_mut();
        *s += 1;
        let id = *s;
        CANVASES.with(|c| {
            c.borrow_mut()
                .insert(id, Canvas2D::new(w.max(0.0) as usize, h.max(0.0) as usize));
        });
        id as f64
    })
}

pub fn cv_resize(id: f64, w: f64, h: f64) {
    let _ = with_canvas(id, |c| c.resize(w.max(0.0) as usize, h.max(0.0) as usize));
}

pub fn cv_begin_path(id: f64) {
    let _ = with_canvas(id, |c| c.path.clear());
}

pub fn cv_rect(id: f64, x: f64, y: f64, w: f64, h: f64) {
    let _ = with_canvas(id, |c| {
        c.path.push(PathCmd::Rect {
            x: x as f32,
            y: y as f32,
            w: w as f32,
            h: h as f32,
        })
    });
}

pub fn cv_arc(id: f64, x: f64, y: f64, r: f64, a0: f64, a1: f64, ccw: bool) {
    // arc → 64 段折线（closePath 语义由 fill 的 Poly 闭合承担）
    let _ = with_canvas(id, |c| {
        // Chrome 实测归一化（CDP 铁证）：差值取模后按方向行走；
        // 差值恰为整圈（如 arc(0, TAU, ccw)）画满圆，a1==a0 才是空弧。
        let t = std::f64::consts::TAU;
        let raw = a1 - a0;
        let mut d = raw % t;
        if ccw {
            if d > 0.0 {
                d -= t;
            }
            if d == 0.0 && raw.abs() >= t {
                d = -t;
            }
        } else {
            if d < 0.0 {
                d += t;
            }
            if d == 0.0 && raw.abs() >= t {
                d = t;
            }
        }
        let (a0, a1) = (a0, a0 + d);
        let steps = ((a1 - a0).abs() / t * 64.0).ceil().max(3.0) as usize;
        let mut pts = Vec::with_capacity(steps + 1);
        for i in 0..=steps {
            let a = a0 + (a1 - a0) * i as f64 / steps as f64;
            pts.push(((x + r * a.cos()) as f32, (y + r * a.sin()) as f32));
        }
        c.path.push(PathCmd::Poly(pts));
    });
}

pub fn cv_set_style(style: &str, alpha: f64, multiply: bool) {
    CV_STATE.with(|s| *s.borrow_mut() = (style.to_string(), alpha, multiply));
}

pub fn cv_fill(id: f64, rule: &str) {
    let (style, alpha, multiply) = CV_STATE.with(|s| s.borrow().clone());
    let color = parse_color(&style);
    let _ = with_canvas(id, |c| c.fill(color, alpha, rule, multiply));
}

pub fn cv_fill_rect(id: f64, x: f64, y: f64, w: f64, h: f64) {
    let (style, alpha, multiply) = CV_STATE.with(|s| s.borrow().clone());
    let color = parse_color(&style);
    let _ = with_canvas(id, |c| {
        c.path.clear();
        c.path.push(PathCmd::Rect {
            x: x as f32,
            y: y as f32,
            w: w as f32,
            h: h as f32,
        });
        c.fill(color, alpha, "nonzero", multiply);
        c.path.clear();
    });
}

pub fn cv_fill_text(id: f64, text: &str, x: f64, y: f64, px: f64, family: &str) {
    let st = cv_style_now();
    let _ = with_canvas(id, |c| c.fill_text(text, x, y, px, family, st));
}

pub fn cv_to_data_url(id: f64) -> String {
    with_canvas(id, |c| {
        let mut png_out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_out, c.w as u32, c.h as u32);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            if let Ok(mut w) = enc.write_header() {
                let _ = w.write_image_data(&c.buf);
            }
        }
        format!("data:image/png;base64,{}", b64(&png_out))
    })
    .unwrap_or_else(|| "data:image/png;base64,".to_string())
}

pub fn cv_get_image_data(id: f64, x: f64, y: f64, w: f64, h: f64) -> String {
    with_canvas(id, |c| {
        let (x, y, w, h) = (
            x.max(0.0) as usize,
            y.max(0.0) as usize,
            w.max(0.0) as usize,
            h.max(0.0) as usize,
        );
        let mut out = Vec::with_capacity(w * h * 4);
        for row in y..y + h {
            let mut rowbuf = vec![0u8; w * 4];
            if row < c.h && x < c.w {
                let start = (row * c.w + x) * 4;
                let avail = w.min(c.w - x);
                rowbuf[..avail * 4].copy_from_slice(&c.buf[start..start + avail * 4]);
            }
            out.extend_from_slice(&rowbuf);
        }
        b64(&out)
    })
    .unwrap_or_default()
}

pub fn cv_put_image_data(id: f64, b64data: &str, dx: f64, dy: f64, w: f64) {
    // 逐像素 source-over 铺回（宽度由 JS 侧 ImageData 提供）
    let _ = with_canvas(id, |c| {
        let data = unb64(b64data);
        let (dx, dy, w) = (dx as i64, dy as i64, w.max(1.0) as i64);
        for (i, chunk) in data.chunks_exact(4).enumerate() {
            let px = dx + (i as i64) % w;
            let py = dy + (i as i64) / w;
            if chunk[3] > 0 {
                c.blend(
                    px,
                    py,
                    chunk[3],
                    (chunk[0], chunk[1], chunk[2], 255),
                    1.0,
                    false,
                );
            }
        }
    });
}

fn unb64(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0;
    for ch in s.bytes() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_rect_and_readback() {
        let id = cv_new(10.0, 10.0);
        cv_set_style("#f60", 1.0, false);
        cv_fill_rect(id, 2.0, 2.0, 4.0, 4.0);
        let data = cv_get_image_data(id, 0.0, 0.0, 10.0, 10.0);
        assert_eq!(data.len() % 4, 0);
        let raw = unb64(&data);
        // 中心像素 (4,4) 应为橙色实心
        let idx = (4 * 10 + 4) * 4;
        assert_eq!(&raw[idx..idx + 3], &[255, 102, 0]);
    }

    #[test]
    fn text_rasterizes_nonzero() {
        let id = cv_new(400.0, 200.0);
        cv_set_style("#f60", 1.0, false);
        cv_fill_text(id, "Cwm fjordbank", 2.0, 15.0, 14.6667, "no-real-font-123");
        let data = unb64(&cv_get_image_data(id, 0.0, 0.0, 400.0, 200.0));
        let nonzero = data.iter().filter(|&&b| b != 0).count();
        assert!(
            nonzero > 500,
            "text should rasterize ink, got {nonzero} nonzero bytes"
        );
    }

    #[test]
    fn to_data_url_is_png() {
        let id = cv_new(20.0, 10.0);
        cv_set_style("rgb(255,0,255)", 1.0, false);
        cv_fill_rect(id, 0.0, 0.0, 10.0, 10.0);
        let url = cv_to_data_url(id);
        assert!(url.starts_with("data:image/png;base64,"));
        assert!(url.len() > 100);
        // 同状态两次编码稳定（canvasFingerprint 自一致性前提）
        assert_eq!(url, cv_to_data_url(id));
    }

    #[test]
    fn evenodd_donut() {
        let id = cv_new(200.0, 200.0);
        cv_begin_path(id);
        cv_arc(id, 75.0, 75.0, 75.0, 0.0, std::f64::consts::TAU, true);
        cv_arc(id, 75.0, 75.0, 25.0, 0.0, std::f64::consts::TAU, true);
        cv_set_style("rgb(255,255,0)", 1.0, false);
        cv_fill(id, "evenodd");
        let data = unb64(&cv_get_image_data(id, 0.0, 0.0, 200.0, 200.0));
        // 圆心 (75,75) 在内圆内——evenodd 应为空（alpha 0）
        let center = (75 * 200 + 75) * 4 + 3;
        assert_eq!(data[center], 0, "donut hole must be empty");
        // 外环 (75,10) 应有墨
        let ring = (10 * 200 + 75) * 4 + 3;
        assert!(data[ring] > 200, "outer ring must be filled");
    }
}
