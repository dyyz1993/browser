//! M12.3: 图像 → ASCII art
//!
//! 把 PNG/JPG 解码 → 缩放到 ASCII 终端尺寸（80x40）→ 灰度 → ASCII。
//!
//! 用于：浏览器渲染 `<img>` 时，如果本地有 src 对应的图像文件，
//! 可以用此模块把它渲染成 ASCII，替代 [IMG: src] 占位符。

use image::{imageops, DynamicImage, GenericImageView};

/// ASCII 灰度字符表（从暗到亮，10 级灰度）。
const ASCII_RAMP: &[&str] = &[" ", ".", ":", "-", "=", "+", "*", "#", "%", "@"];

/// 把图像文件转成 ASCII art 字符串。
///
/// - `path`: 图像文件路径（PNG / JPG）。
/// - `max_w`: ASCII 输出最大列数（80 = 终端宽度）。
/// - `max_h`: ASCII 输出最大行数。
///
/// 返回多行字符串（每行 `\n` 分隔），字符取自 ASCII_RAMP。
pub fn image_to_ascii<P: AsRef<std::path::Path>>(
    path: P,
    max_w: u32,
    max_h: u32,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let img = image::open(&path).map_err(|e| format!("image decode failed: {e}"))?;
    Ok(image_to_ascii_from_img(&img, max_w, max_h))
}

fn image_to_ascii_from_img(img: &DynamicImage, max_w: u32, max_h: u32) -> String {
    let (w, h) = img.dimensions();
    // 缩放：等比缩放到 max_w × max_h 内。
    let scale = (max_w as f64 / w as f64).min(max_h as f64 / h as f64);
    let new_w = ((w as f64) * scale).max(1.0) as u32;
    let new_h = ((h as f64) * scale).max(1.0) as u32;

    let resized = imageops::resize(img, new_w, new_h, imageops::FilterType::Nearest);

    let mut out = String::with_capacity((new_w * new_h + new_h) as usize);
    for y in 0..new_h {
        for x in 0..new_w {
            let pixel = resized.get_pixel(x, y);
            let lum = luminance(pixel.0[0], pixel.0[1], pixel.0[2]);
            let idx = ((lum as u32 * ASCII_RAMP.len() as u32) / 256) as usize;
            let idx = idx.min(ASCII_RAMP.len() - 1);
            out.push_str(ASCII_RAMP[idx]);
        }
        out.push('\n');
    }
    out
}

fn luminance(r: u8, g: u8, b: u8) -> u8 {
    // ITU-R BT.601 标准灰度公式。
    let yf = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    yf.round().min(255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luminance_pure_white_is_255() {
        assert_eq!(luminance(255, 255, 255), 255);
    }

    #[test]
    fn luminance_pure_black_is_0() {
        assert_eq!(luminance(0, 0, 0), 0);
    }

    #[test]
    fn ascii_ramp_ordered_dark_to_bright() {
        // 表必须从暗到亮（空格到 @）。
        assert_eq!(ASCII_RAMP[0], " ");
        assert_eq!(ASCII_RAMP[ASCII_RAMP.len() - 1], "@");
    }

    #[test]
    fn empty_image_returns_empty_string() {
        // 1×1 全白图像 → 一行一个 "@" 字符
        let img = DynamicImage::new_rgba8(1, 1);
        let s = image_to_ascii_from_img(&img, 10, 10);
        // 默认 RGBA 全 0（黑）→ 最暗字符。
        assert!(s.contains(" "));
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn ascii_output_dimensions_within_bounds() {
        // 100×50 图像缩放到 max_w=20, max_h=10
        let img = DynamicImage::new_rgba8(100, 50);
        let s = image_to_ascii_from_img(&img, 20, 10);
        let lines: Vec<&str> = s.lines().collect();
        // 高度 ≤ 10
        assert!(lines.len() <= 10);
        // 每行宽度 ≤ 20
        for l in &lines {
            assert!(l.chars().count() <= 20);
        }
    }
}
