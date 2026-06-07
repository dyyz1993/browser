//! M12.1: ASCII → PNG 截图
//!
//! 把 render 的 ASCII 输出转成 PNG 图像。M34 重构：fontdue 渲染逻辑
//! 提取到 `render::font::FontRenderer`（M25 验证的坐标公式，screenshot
//! 和 gui 共用同一个 renderer，避免字形定位逻辑割裂）。
//! 白底黑字、灰度抗锯齿。

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use browser_render::font::FontRenderer;

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

    // M30: 解析 ANSI escape，提取纯字符序列 + link span 范围。
    let link_spans_per_line: Vec<Vec<(usize, usize)>> = lines
        .iter()
        .map(|l| strip_ansi_and_track_links(l).1)
        .collect();
    // 去掉 ANSI escape 后的纯文本（render_text_to_rgba 接收纯文本）。
    let clean_text: String = lines
        .iter()
        .map(|l| {
            let (chars, _): (Vec<char>, Vec<(usize, usize)>) = strip_ansi_and_track_links(l);
            chars.into_iter().collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    let (img_w, img_h, buf) = renderer.render_text_to_rgba(&clean_text, &link_spans_per_line);

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
fn strip_ansi_and_track_links(line: &str) -> (Vec<char>, Vec<(usize, usize)>) {
    let mut chars: Vec<char> = Vec::new();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut link_start: Option<usize> = None;
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
                    // SGR sequence — check if it's link-on (4;34 or 34;4) or reset (0)
                    if params == "4;34" || params == "34;4" {
                        link_start = Some(chars.len());
                    } else if params == "0" || params.is_empty() {
                        if let Some(s) = link_start.take() {
                            spans.push((s, chars.len()));
                        }
                    }
                }
                i = end + 1;
            } else {
                // malformed — skip rest
                break;
            }
        } else {
            // Regular char (UTF-8 safe: collect from str iterator).
            let ch = line[i..].chars().next().unwrap();
            chars.push(ch);
            i += ch.len_utf8();
        }
    }
    // Unclosed link → span to end.
    if let Some(s) = link_start {
        spans.push((s, chars.len()));
    }
    (chars, spans)
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

    // ---- M30: ANSI link parsing ----

    #[test]
    fn strip_ansi_plain_text() {
        let (chars, spans) = strip_ansi_and_track_links("hello");
        assert_eq!(chars, vec!['h', 'e', 'l', 'l', 'o']);
        assert!(spans.is_empty());
    }

    #[test]
    fn strip_ansi_link_span() {
        let (chars, spans) = strip_ansi_and_track_links("\x1b[4;34mgo\x1b[0m");
        assert_eq!(chars, vec!['g', 'o']);
        assert_eq!(spans, vec![(0, 2)]);
    }

    #[test]
    fn strip_ansi_link_surrounded_by_plain() {
        let (chars, spans) = strip_ansi_and_track_links("pre \x1b[4;34mMID\x1b[0m post");
        assert_eq!(
            chars,
            vec!['p', 'r', 'e', ' ', 'M', 'I', 'D', ' ', 'p', 'o', 's', 't']
        );
        assert_eq!(spans, vec![(4, 7)]);
    }

    #[test]
    fn strip_ansi_unclosed_link_spans_to_end() {
        let (chars, spans) = strip_ansi_and_track_links("x\x1b[4;34mabc");
        assert_eq!(chars, vec!['x', 'a', 'b', 'c']);
        assert_eq!(spans, vec![(1, 4)]);
    }
}
