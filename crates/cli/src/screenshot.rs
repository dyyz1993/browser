//! M12.1: ASCII → PNG 截图
//!
//! 把 render 的 ASCII 输出转成 PNG 图像。M34 重构：fontdue 渲染逻辑
//! 提取到 `render::font::FontRenderer`（M25 验证的坐标公式，screenshot
//! 和 gui 共用同一个 renderer，避免字形定位逻辑割裂）。
//! 白底黑字、灰度抗锯齿。

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use browser_render::font::{BgSpans, FgSpans, FontRenderer};

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
    let mut renderer = FontRenderer::new();
    let line_height = renderer.metrics().line_height;

    let mut lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        anyhow::bail!("empty text");
    }
    if let Some(mh) = max_height {
        let approx_max_lines = mh / line_height.max(1);
        if lines.len() > approx_max_lines {
            lines.truncate(approx_max_lines.max(1));
        }
    }
    if lines.iter().all(|l| l.chars().count() == 0) {
        anyhow::bail!("all lines empty");
    }

    // M30/M39/M70: 解析 ANSI escape，提取纯字符序列 + link span + bg span + fg span。
    let mut link_spans_per_line: Vec<Vec<(usize, usize)>> = Vec::new();
    let mut bg_spans_per_line: Vec<BgSpans> = Vec::new();
    let mut fg_spans_per_line: Vec<FgSpans> = Vec::new();
    let clean_text: String = lines
        .iter()
        .map(|l| {
            let (chars, links, bgs, fgs) = strip_ansi_and_track_styles(l);
            link_spans_per_line.push(links);
            bg_spans_per_line.push(bgs);
            fg_spans_per_line.push(fgs);
            chars.into_iter().collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    let (img_w, img_h, buf) = renderer.render_text_to_rgba(
        &clean_text,
        &link_spans_per_line,
        &bg_spans_per_line,
        &fg_spans_per_line,
    );

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

/// M80: RGBA 位图 → PNG 文件（pixel 渲染模式专用；ASCII 路径不受影响）。
///
/// `max_height`：像素高度上限，超出时从底部截断（保留顶部）。
///
/// # Errors
/// PNG 编码失败时返回错误。
pub fn render_rgba_to_png<P: AsRef<Path>>(
    rgba: &[u8],
    width: usize,
    height: usize,
    path: P,
    max_height: Option<usize>,
) -> anyhow::Result<()> {
    if width == 0 || height == 0 || rgba.len() != width * height * 4 {
        anyhow::bail!(
            "invalid pixel buffer: {}x{}, {} bytes",
            width,
            height,
            rgba.len()
        );
    }
    let h = max_height.map(|mh| mh.min(height)).unwrap_or(height);
    let file = File::create(path)?;
    let w = BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width as u32, h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| anyhow::anyhow!("png header: {e}"))?;
    writer
        .write_image_data(&rgba[..h * width * 4])
        .map_err(|e| anyhow::anyhow!("png write: {e}"))?;
    Ok(())
}

/// M30: 解析 ANSI escape，提取纯字符序列 + link span 范围。
///
/// 识别 `\x1b[4;34m`（link 开始）和 `\x1b[0m`（link 结束）。
/// 其他 ANSI escape 直接 strip。
///
/// 返回 `(clean_chars, link_spans)`：
/// - `clean_chars`: strip ANSI 后的纯字符序列
/// - `link_spans`: link 范围列表，每个元素 `(start_col, end_col)` 半开区间
///
/// # Examples
/// ```ignore
/// let (chars, spans) = strip_ansi_and_track_links("\x1b[4;34mgo\x1b[0m");
/// assert_eq!(chars, vec!['g', 'o']);
/// assert_eq!(spans, vec![(0, 2)]);
/// ```
/// M30/M39/M70: 解析 ANSI escape，提取纯字符序列 + link span + background span + foreground span。
///
/// 识别的 SGR 序列（参数以 `;` 分隔）：
/// - `4`：link 开始（下划线）。M70 fix：仅下划线(4)判为 link。
/// - `38;2;R;G;B`：foreground（CSS color）truecolor。**M70：不再被误判为 link**
///   （旧逻辑把任意 `38;2` 都当 link，导致 CSS 文字颜色丢失）。
/// - `4;38;2;0;0;238`：link 开始（下划线 + W3C 蓝，ascii.rs::build_sgr_prefix 输出）。
///   这里 `4` 才是 link 标记，`38;2;0;0;238` 是 link 的固定蓝前景。
/// - `48;2;R;G;B`：背景色 truecolor
/// - 任意组合：`4;38;2;0;0;238;48;2;R;G;B`
/// - `0`：reset（结束当前 link + bg + fg 范围）
///
/// 返回 `(clean_chars, link_spans, bg_spans, fg_spans)`：
/// - `clean_chars`: strip ANSI 后的纯字符序列
/// - `link_spans`: link 范围列表，每个 `(start_col, end_col)` 半开区间
/// - `bg_spans`: 背景范围列表，每个 `(start_col, end_col, (r,g,b))` 半开区间
/// - `fg_spans`: 前景范围列表，每个 `(start_col, end_col, (r,g,b))` 半开区间（M70 新增）
#[allow(clippy::type_complexity)] // 4-tuple return: chars + 3 span kinds
fn strip_ansi_and_track_styles(
    line: &str,
) -> (
    Vec<char>,
    Vec<(usize, usize)>,
    Vec<browser_render::font::BgSpan>,
    Vec<browser_render::font::FgSpan>,
) {
    let mut chars: Vec<char> = Vec::new();
    let mut link_spans: Vec<(usize, usize)> = Vec::new();
    let mut bg_spans: Vec<browser_render::font::BgSpan> = Vec::new();
    let mut fg_spans: Vec<browser_render::font::FgSpan> = Vec::new();
    let mut link_start: Option<usize> = None;
    let mut bg_start: Option<(usize, (u8, u8, u8))> = None;
    let mut fg_start: Option<(usize, (u8, u8, u8))> = None;
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // ESC = 0x1B
        if bytes[i] == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // CSI sequence: ESC [ ... letter
            let j = i + 2;
            let mut end = j;
            while end < bytes.len() && !bytes[end].is_ascii_alphabetic() {
                end += 1;
            }
            if end < bytes.len() {
                let params = &line[j..end];
                let final_byte = bytes[end] as char;
                if final_byte == 'm' {
                    // 解析 SGR 参数。
                    // reset = 单独 "0"（CSI 0 m）或 params 完全为空（CSI m）。
                    // 不能 contains("0")，因为 RGB 分量里也有 0（4;38;2;0;0;238）。
                    let params_trimmed = params.trim();
                    let is_reset = params_trimmed.is_empty() || params_trimmed == "0";
                    let tokens: Vec<&str> = params.split(';').collect();
                    // M70: link 仅看下划线(4)。旧的 `38;2` 误判逻辑已移除
                    // （现在 38;2 表示 CSS 前景色，不再是 link）。
                    let has_underline = tokens.iter().any(|t| t.trim() == "4");
                    // 检测背景色：48;2;R;G;B
                    let bg_color = parse_bg_color(&tokens);
                    // M70: 检测前景色 38;2;R;G;B。link 的固定蓝 38;2;0;0;238
                    // 也会被解析成 fg color，但因为该 span 同时 has_underline，
                    // 会被记进 link_spans —— font.rs 光栅化时 link 优先级高于 fg，
                    // 所以 fg span 里的蓝值不会影响渲染（link 覆盖之）。
                    let fg_color = parse_fg_color(&tokens);
                    if has_underline && !is_reset {
                        link_start = Some(chars.len());
                    }
                    if let Some(color) = bg_color {
                        if !is_reset {
                            bg_start = Some((chars.len(), color));
                        }
                    }
                    if let Some(color) = fg_color {
                        if !is_reset {
                            fg_start = Some((chars.len(), color));
                        }
                    }
                    if is_reset {
                        if let Some(s) = link_start.take() {
                            link_spans.push((s, chars.len()));
                        }
                        if let Some((s, color)) = bg_start.take() {
                            bg_spans.push((s, chars.len(), color));
                        }
                        if let Some((s, color)) = fg_start.take() {
                            fg_spans.push((s, chars.len(), color));
                        }
                    }
                }
                i = end + 1;
            } else {
                break;
            }
        } else {
            let ch = line[i..].chars().next().unwrap();
            chars.push(ch);
            i += ch.len_utf8();
        }
    }
    // Unclosed → span to end.
    if let Some(s) = link_start {
        link_spans.push((s, chars.len()));
    }
    if let Some((s, color)) = bg_start {
        bg_spans.push((s, chars.len(), color));
    }
    if let Some((s, color)) = fg_start {
        fg_spans.push((s, chars.len(), color));
    }
    (chars, link_spans, bg_spans, fg_spans)
}

/// 从 SGR tokens 中解析背景色 `48;2;R;G;B`。
fn parse_bg_color(tokens: &[&str]) -> Option<(u8, u8, u8)> {
    for idx in 0..tokens.len() {
        if tokens[idx].trim() == "48"
            && idx + 4 < tokens.len() + 1
            && tokens.get(idx + 1).map(|t| t.trim()) == Some("2")
        {
            let r = tokens.get(idx + 2)?.trim().parse::<u8>().ok()?;
            let g = tokens.get(idx + 3)?.trim().parse::<u8>().ok()?;
            let b = tokens.get(idx + 4)?.trim().parse::<u8>().ok()?;
            return Some((r, g, b));
        }
    }
    None
}

/// M70: 从 SGR tokens 中解析前景色 `38;2;R;G;B`（对称 parse_bg_color 的 48;2）。
fn parse_fg_color(tokens: &[&str]) -> Option<(u8, u8, u8)> {
    for idx in 0..tokens.len() {
        if tokens[idx].trim() == "38"
            && idx + 4 < tokens.len() + 1
            && tokens.get(idx + 1).map(|t| t.trim()) == Some("2")
        {
            let r = tokens.get(idx + 2)?.trim().parse::<u8>().ok()?;
            let g = tokens.get(idx + 3)?.trim().parse::<u8>().ok()?;
            let b = tokens.get(idx + 4)?.trim().parse::<u8>().ok()?;
            return Some((r, g, b));
        }
    }
    None
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

    // M25 公式锁定测试已迁移到 render::font::tests（M34 重构去重）。
    // 以下 3 个测试在 screenshot.rs 删除（避免重复）:
    //   - measured_layout_params_are_reasonable → render::font::metrics_are_reasonable
    //   - glyph_vertical_position_formula_is_correct → render::font（同名）
    //   - glyph_with_descender_has_negative_ymin → render::font::descender_glyph_has_negative_ymin

    // ---- M30/M39/M70: ANSI link + background + foreground parsing ----

    #[test]
    fn strip_ansi_plain_text() {
        let (chars, spans, bgs, fgs) = strip_ansi_and_track_styles("hello");
        assert_eq!(chars, vec!['h', 'e', 'l', 'l', 'o']);
        assert!(spans.is_empty());
        assert!(bgs.is_empty());
        assert!(fgs.is_empty());
    }

    #[test]
    fn strip_ansi_link_span() {
        // M39/M70: link color is truecolor (4;38;2;0;0;238). `4` marks the
        // link; the 38;2 is the link's blue fg. Both link_spans and fg_spans
        // get populated (fg is overridden by link at raster time).
        let (chars, spans, bgs, fgs) = strip_ansi_and_track_styles("\x1b[4;38;2;0;0;238mgo\x1b[0m");
        assert_eq!(chars, vec!['g', 'o']);
        assert_eq!(spans, vec![(0, 2)]);
        assert!(bgs.is_empty());
        // link's blue fg is captured as fg span too (link priority wins at raster)
        assert_eq!(fgs, vec![(0, 2, (0, 0, 238))]);
    }

    #[test]
    fn strip_ansi_link_surrounded_by_plain() {
        let (chars, spans, bgs, _fgs) =
            strip_ansi_and_track_styles("pre \x1b[4;38;2;0;0;238mMID\x1b[0m post");
        assert_eq!(
            chars,
            vec!['p', 'r', 'e', ' ', 'M', 'I', 'D', ' ', 'p', 'o', 's', 't']
        );
        assert_eq!(spans, vec![(4, 7)]);
        assert!(bgs.is_empty());
    }

    #[test]
    fn strip_ansi_unclosed_link_spans_to_end() {
        let (chars, spans, _, _) = strip_ansi_and_track_styles("x\x1b[4;38;2;0;0;238mabc");
        assert_eq!(chars, vec!['x', 'a', 'b', 'c']);
        assert_eq!(spans, vec![(1, 4)]);
    }

    #[test]
    fn strip_ansi_background_color() {
        // M39: background-color truecolor (48;2;R;G;B)
        let (chars, _, bgs, _) = strip_ansi_and_track_styles("\x1b[48;2;255;0;0mRED\x1b[0m");
        assert_eq!(chars, vec!['R', 'E', 'D']);
        assert_eq!(bgs, vec![(0, 3, (255, 0, 0))]);
    }

    #[test]
    fn strip_ansi_link_plus_background_combined() {
        // M39: link + bg in single SGR sequence
        let (chars, spans, bgs, _fgs) =
            strip_ansi_and_track_styles("\x1b[4;38;2;0;0;238;48;2;255;255;0mL\x1b[0m");
        assert_eq!(chars, vec!['L']);
        assert_eq!(spans, vec![(0, 1)]);
        assert_eq!(bgs, vec![(0, 1, (255, 255, 0))]);
    }

    // ---- M70: foreground color (CSS `color`) parsing ----

    #[test]
    fn strip_ansi_foreground_color() {
        // M70: CSS color → 38;2;R;G;B (NO underline `4`) → fg span, NOT a link.
        let (chars, links, _bgs, fgs) =
            strip_ansi_and_track_styles("\x1b[38;2;33;150;243mBLUE\x1b[0m");
        assert_eq!(chars, vec!['B', 'L', 'U', 'E']);
        // 关键：38;2 不带 4 不应被判为 link（M70 修复的 bug）
        assert!(links.is_empty(), "38;2 without `4` must NOT be a link");
        assert_eq!(fgs, vec![(0, 4, (33, 150, 243))]);
    }

    #[test]
    fn strip_ansi_foreground_plus_background_combined() {
        // M70: CSS color + background-color in one SGR sequence (绿底白字按钮)
        let (chars, links, bgs, fgs) =
            strip_ansi_and_track_styles("\x1b[38;2;255;255;255;48;2;76;175;80mBTN\x1b[0m");
        assert_eq!(chars, vec!['B', 'T', 'N']);
        assert!(links.is_empty());
        assert_eq!(bgs, vec![(0, 3, (76, 175, 80))]);
        assert_eq!(fgs, vec![(0, 3, (255, 255, 255))]);
    }

    #[test]
    fn parse_fg_color_basic() {
        let toks: Vec<&str> = "38;2;255;0;0".split(';').collect();
        assert_eq!(parse_fg_color(&toks), Some((255, 0, 0)));
    }

    #[test]
    fn parse_fg_color_not_confused_with_bg() {
        // 48 is background, parse_fg_color must ignore it
        let toks: Vec<&str> = "48;2;10;20;30".split(';').collect();
        assert_eq!(parse_fg_color(&toks), None);
    }
}
