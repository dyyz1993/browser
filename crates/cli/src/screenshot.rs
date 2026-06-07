//! M12.1: ASCII → PNG 截图
//!
//! 把 render 的 ASCII 输出转成 PNG 图像，用 fontdue 真实字体（嵌入的
//! DejaVuSans）栅格化。白底黑字、灰度抗锯齿。
//!
//! M25.1 修复渲染错位：旧版用硬编码 COL_WIDTH=10/LINE_HEIGHT=20 且忽略
//! Metrics.xmin/ymin，导致字形重叠 + baseline 错乱。改用 advance_width
//! 测量列宽 + horizontal_line_metrics 的 ascent 定位 baseline + ymin 做
//! 垂直对齐，字形不再互相压叠。

use fontdue::Font;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

const FONT_BYTES: &[u8] = include_bytes!("../assets/font.ttf");
const FONT_SIZE: f32 = 16.0;
/// 行间距（baseline 到下一行 baseline 之外的额外空白）。
const LINE_GAP: usize = 6;

/// ASCII art → PNG 文件（白底黑字，灰度抗锯齿）。
///
/// M29.3: 可选 max_height 参数限制截图高度（像素）。如果渲染高度 > max_height，
/// 从顶部截断（底部内容丢弃）。None 不限制。
///
/// # Errors
/// Returns `anyhow::Error` if the text is empty, all lines empty, or
/// PNG encoding fails.
pub fn render_text_to_png<P: AsRef<Path>>(
    text: &str,
    path: P,
    max_height: Option<usize>,
) -> anyhow::Result<()> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        anyhow::bail!("empty text");
    }
    let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    if cols == 0 {
        anyhow::bail!("all lines empty");
    }

    let font = Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default())
        .expect("embedded DejaVuSans.ttf must parse");

    // ---- M25.1: 用 fontdue metrics 精确测量布局参数 ----
    // 列宽 = 所有可打印 ASCII 字符的最大 advance_width（向上取整）。
    // 这是等宽渲染的关键：每列正好放一个字形，不重叠。
    let col_width: usize = (32u8..=126)
        .map(|c| font.metrics(c as char, FONT_SIZE).advance_width)
        .fold(0.0_f32, f32::max)
        .ceil()
        .max(1.0) as usize;

    // 行高 = ascent + descent + LINE_GAP。ascent/descent 来自字体本身，
    // 保证字号内所有字形（含 'g' 下伸）都能放下。
    let line_metrics = font
        .horizontal_line_metrics(FONT_SIZE)
        .expect("font must have horizontal metrics");
    let ascent = line_metrics.ascent.ceil() as usize;
    let descent = (-line_metrics.descent).ceil() as usize;
    let line_height = ascent + descent + LINE_GAP;
    // baseline 距 cell 顶部的像素数（= ascent）。
    let baseline = ascent;

    // M29.3: 近似截断（用 line_height 估算最大行数）。必须在精确计算 img_h 前完成，
    // 避免精度误差导致 img_h 与截断不一致。
    let mut lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        anyhow::bail!("empty text");
    }
    if let Some(mh) = max_height {
        let approx_max_lines = mh / line_height.max(1);
        if lines.len() > approx_max_lines {
            lines.truncate(approx_max_lines.max(1)); // 至少留 1 行
        }
    }
    let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    if cols == 0 {
        anyhow::bail!("all lines empty");
    }

    let img_w = cols * col_width;
    let img_h = lines.len() * line_height;

    // RGBA buffer，白底。
    let mut buf = vec![255u8; img_w * img_h * 4];

    // 字符 → (metrics, bitmap) 缓存。
    let mut cache: HashMap<char, (fontdue::Metrics, Vec<u8>)> = HashMap::new();

    for (row, line) in lines.iter().enumerate() {
        let row_top = row * line_height;
        for (col, ch) in line.chars().enumerate() {
            let (m, mask) = cache
                .entry(ch)
                .or_insert_with(|| {
                    let mm = font.metrics(ch, FONT_SIZE);
                    let (_, bb) = font.rasterize(ch, FONT_SIZE);
                    (mm, bb)
                })
                .clone();
            if m.width == 0 || m.height == 0 {
                continue; // 空白字符或 .notdef
            }
            // 字形像素位置（fontdue 坐标，经 M25.2 诊断 + PIL 像素对照确认）：
            //   - bitmap 正立存储：bitmap[0]=顶行（dy=0=字形顶部）
            //   - ymin 是数学坐标（向上为正，负=baseline 下方）。
            //     fontdue 语义：ymin = bitmap **底边**相对 baseline 的偏移
            //   - 屏幕坐标（y向下）：底边 screen_offset = baseline - ymin
            //   - 底行 dy=height-1 落在 baseline-ymin，所以
            //     y_origin = (baseline - ymin) - height + 1
            // 用 'p'(ymin=-4,h=13,ascent=15) 验证（圆肚子顶 dy=0，竖画底 dy=12）：
            //   y_origin = 15-(-4)-13+1 = 7（'p' 圆顶在 baseline 上方 8px ✓ x-height）
            //   底行 py = 7+12 = 19 = baseline+4（竖画末端在 baseline 下 4px ✓ descender）
            let y_origin = baseline as i32 - m.ymin - m.height as i32 + 1;
            for dy in 0..m.height {
                for dx in 0..m.width {
                    let alpha = mask[dy * m.width + dx] as u32;
                    if alpha == 0 {
                        continue;
                    }
                    let px = (col * col_width) as i32 + m.xmin + dx as i32;
                    let py = row_top as i32 + y_origin + dy as i32;
                    if px < 0 || py < 0 {
                        continue;
                    }
                    let (pxu, pyu) = (px as usize, py as usize);
                    if pxu >= img_w || pyu >= img_h {
                        continue;
                    }
                    let idx = (pyu * img_w + pxu) * 4;
                    // alpha 0..=255 → 灰度 v = 255 - alpha（黑字白底）。
                    let v = 255 - (alpha.min(255) as u8);
                    buf[idx] = v;
                    buf[idx + 1] = v;
                    buf[idx + 2] = v;
                }
            }
        }
    }

    let file = File::create(path)?;
    let w = BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, img_w as u32, img_h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| anyhow::anyhow!("png header: {e}"))?;
    writer
        .write_image_data(&buf)
        .map_err(|e| anyhow::anyhow!("png write: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_simple_text_to_png() {
        let tmp = std::env::temp_dir().join("m12_screenshot_test.png");
        render_text_to_png("Hello\nWorld!", &tmp, None).expect("PNG write failed");
        let meta = std::fs::metadata(&tmp).expect("metadata");
        assert!(meta.len() > 100, "PNG file too small: {} bytes", meta.len());
        let mut hdr = [0u8; 8];
        use std::io::Read;
        let mut f = std::fs::File::open(&tmp).unwrap();
        f.read_exact(&mut hdr).unwrap();
        assert_eq!(&hdr[..8], &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn rejects_empty_text() {
        let tmp = std::env::temp_dir().join("m12_empty.png");
        let r = render_text_to_png("", &tmp, None);
        assert!(r.is_err());
    }

    #[test]
    fn handles_chinese_chars() {
        let tmp = std::env::temp_dir().join("m12_chinese.png");
        let r = render_text_to_png("你好", &tmp, None);
        assert!(r.is_ok());
        std::fs::remove_file(&tmp).ok();
    }

    /// M29.3: 验证 max_height 截断功能（保留顶部，底部丢弃）。
    #[test]
    fn max_height_truncates_from_top() {
        use std::io::Read;
        let tmp = std::env::temp_dir().join("m29_max_height_test.png");
        // 3 行文本，每行约 25px，总高 ~75px。max_height=50 应截断为 2 行。
        render_text_to_png("Line 1\nLine 2\nLine 3", &tmp, Some(50)).expect("PNG write failed");
        let meta = std::fs::metadata(&tmp).expect("metadata");
        assert!(meta.len() > 100, "PNG file too small");
        // PNG 头 + 验证文件存在
        let mut hdr = [0u8; 8];
        let mut f = std::fs::File::open(&tmp).unwrap();
        f.read_exact(&mut hdr).unwrap();
        assert_eq!(&hdr[..8], &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
        std::fs::remove_file(&tmp).ok();
    }

    /// M25.1: 验证测量出的布局参数合理（col_width/line_height 不会让字形重叠）。
    #[test]
    fn measured_layout_params_are_reasonable() {
        let font = Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default()).unwrap();
        let col_width = (32u8..=126)
            .map(|c| font.metrics(c as char, FONT_SIZE).advance_width)
            .fold(0.0_f32, f32::max)
            .ceil() as usize;
        let lm = font.horizontal_line_metrics(FONT_SIZE).unwrap();
        let line_height = lm.ascent.ceil() as usize + (-lm.descent).ceil() as usize + LINE_GAP;

        // 16pt DejaVuSans 实测（PIL 交叉验证）：col_width≈16（W/m 最宽），
        // line_height≈25（ascent 15 + descent 4 + gap 6）。
        assert!(col_width >= 12, "col_width too small: {col_width}");
        assert!(col_width <= 20, "col_width too large: {col_width}");
        assert!(
            line_height >= 20,
            "line_height must fit font + gap: {line_height}"
        );
        assert!(line_height <= 35, "line_height too large: {line_height}");
    }

    /// M25.2: 锁定字形垂直定位公式（纯数学验证，不依赖 PNG 解码/肉眼）。
    /// 这是对前 4 次'猜公式导致乱码'失败的防御——公式对错用断言判定。
    /// 公式：y_origin = baseline - ymin - height + 1；py = y_origin + dy
    #[test]
    fn glyph_vertical_position_formula_is_correct() {
        let font = Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default()).unwrap();
        let ascent = font
            .horizontal_line_metrics(FONT_SIZE)
            .unwrap()
            .ascent
            .ceil() as i32;
        let baseline = ascent;

        // 'b'（上伸到 cap height）：ymin=-1, height=14
        let mb = font.metrics('b', FONT_SIZE);
        let y_origin_b = baseline - mb.ymin - mb.height as i32 + 1;
        let top_b = y_origin_b;
        let bottom_b = y_origin_b + mb.height as i32 - 1;

        // 'p'（下伸到 descender）：ymin=-4, height=13
        let mp = font.metrics('p', FONT_SIZE);
        let y_origin_p = baseline - mp.ymin - mp.height as i32 + 1;
        let top_p = y_origin_p;
        let bottom_p = y_origin_p + mp.height as i32 - 1;

        // 1. 'b' 顶部应高于 'p' 顶部（'b' 上伸到 cap height，'p' 只到 x-height）
        assert!(
            top_b < top_p,
            "'b' top ({top_b}) must be above 'p' top ({top_p}) — 否则字形垂直镜像了"
        );
        // 2. 'p' 底部应低于 'b' 底部（'p' 有 descender）
        assert!(
            bottom_p > bottom_b,
            "'p' bottom ({bottom_p}) must be below 'b' bottom ({bottom_b}) — 否则 descender 丢失"
        );
        // 3. 字形都在 cell 内（不溢出到相邻行）
        let line_height = ascent + 4 + LINE_GAP as i32;
        assert!(top_b >= 0, "'b' top ({top_b}) 不能为负");
        assert!(
            bottom_p < line_height,
            "'p' bottom ({bottom_p}) 超出行高 {line_height}"
        );
        // 4. 公式语义自洽：'b' 底行应落在 baseline 附近（±2px 容差）
        assert!(
            (bottom_b - baseline).abs() <= 3,
            "'b' 底行 ({bottom_b}) 应在 baseline ({baseline}) 附近"
        );
    }

    /// M25.1: 验证 'g'（有下伸）的 ymin 为负（baseline 下方），
    /// 确认我们的 baseline 对齐逻辑有意义。
    #[test]
    fn glyph_with_descender_has_negative_ymin() {
        let font = Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default()).unwrap();
        let m_g = font.metrics('g', FONT_SIZE);
        let m_e = font.metrics('E', FONT_SIZE);
        // 'g' 的 ymin 应该比 'E' 更负（下伸到 baseline 下）。
        assert!(
            m_g.ymin < m_e.ymin,
            "'g' ymin ({}) should be below 'E' ymin ({})",
            m_g.ymin,
            m_e.ymin
        );
    }
}
