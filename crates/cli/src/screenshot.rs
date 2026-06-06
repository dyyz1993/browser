//! M12.1: ASCII → PNG 截图
//!
//! 把 render 的 ASCII 输出转成 PNG 图像，用 fontdue 真实字体（嵌入的
//! DejaVuSans）栅格化。白底黑字、灰度抗锯齿。

use fontdue::Font;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

const FONT_BYTES: &[u8] = include_bytes!("../assets/font.ttf");
const FONT_SIZE: f32 = 16.0;
const LINE_HEIGHT: usize = 20; // 字号 16 + 4 行距
const COL_WIDTH: usize = 10; // 等宽近似（DejaVuSans 16pt 半角 ≈ 10px）

/// ASCII art → PNG 文件（白底黑字，灰度抗锯齿）。
pub fn render_text_to_png<P: AsRef<Path>>(text: &str, path: P) -> anyhow::Result<()> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        anyhow::bail!("empty text");
    }

    let font = Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default())
        .expect("embedded DejaVuSans.ttf must parse");

    let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let rows = lines.len();
    if cols == 0 {
        anyhow::bail!("all lines empty");
    }

    let img_w = (cols * COL_WIDTH) as u32;
    let img_h = (rows * LINE_HEIGHT) as u32;

    // RGBA buffer，白底。
    let mut buf = vec![255u8; (img_w * img_h * 4) as usize];

    // 字符 → (metrics, bitmap) 缓存（避免重复栅格化）。
    let mut cache: std::collections::HashMap<char, (usize, usize, Vec<u8>)> =
        std::collections::HashMap::new();

    for (row, line) in lines.iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            let (w, h, mask) = cache
                .entry(ch)
                .or_insert_with(|| {
                    let (m, b) = font.rasterize(ch, FONT_SIZE);
                    (m.width, m.height, b)
                })
                .clone();
            let x0 = col * COL_WIDTH;
            let y0 = row * LINE_HEIGHT;
            // 灰度 mask → RGBA 像素。
            for dy in 0..h {
                for dx in 0..w {
                    let alpha = mask[dy * w + dx] as u32;
                    if alpha == 0 {
                        continue;
                    }
                    let px = (x0 + dx) as u32;
                    let py = (y0 + dy) as u32;
                    if px >= img_w || py >= img_h {
                        continue;
                    }
                    let idx = (py * img_w + px) as usize * 4;
                    // alpha 0..=255 → 灰度：v = 255 - alpha（黑字白底）
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
    let mut encoder = png::Encoder::new(w, img_w, img_h);
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
        render_text_to_png("Hello\nWorld!", &tmp).expect("PNG write failed");
        let meta = std::fs::metadata(&tmp).expect("metadata");
        assert!(meta.len() > 100, "PNG file too small: {} bytes", meta.len());
        // 验证 PNG 文件签名。
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
        let r = render_text_to_png("", &tmp);
        assert!(r.is_err());
    }

    #[test]
    fn handles_chinese_chars() {
        let tmp = std::env::temp_dir().join("m12_chinese.png");
        let r = render_text_to_png("你好", &tmp);
        // DejaVuSans 不含 CJK 字形，fontdue 会绘制 .notdef 占位（不报错）。
        assert!(r.is_ok());
        std::fs::remove_file(&tmp).ok();
    }
}
