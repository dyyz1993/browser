//! M22.1: `<img>` 真实图像 → ASCII art 渲染。
//!
//! 解决 M9 的 `[IMG: src]` 纯文本占位符问题：爬虫/CLI 看不到图像内容。
//! 本模块把本地图像文件解码 → 等比缩放 → 灰度 → ASCII 字符，注入布局树
//! 替代占位符，让渲染输出包含可识别的图像内容。
//!
//! 复用 M12.3 的算法（ITU-R BT.601 灰度 + 10 级 ramp），但提升到 render crate
//! 让 CLI 渲染管线（render_html_to_string）能自动调用。
//!
//! 限制（MVP，爬虫够用）：
//! - 只支持本地文件 src（file: 或相对/绝对路径）；http(s) URL 不下载
//!   （避免在渲染管线引入网络依赖；爬虫可先下到本地再渲染）
//! - 只支持 PNG/JPEG（image crate 已启用 feature）
//! - 缩放到给定 max_w × max_h 内，等比

use image::{imageops, DynamicImage, GenericImageView};
use std::path::{Path, PathBuf};

/// ASCII 灰度字符表（从暗到亮，10 级灰度）。
const ASCII_RAMP: &[char] = &[' ', '.', ':', '-', '=', '+', '*', '#', '%', '@'];

/// 把图像文件转成 ASCII art 字符串（多行，`\n` 分隔）。
///
/// - `path`: 图像文件路径（PNG / JPEG）。
/// - `max_w`: ASCII 输出最大列数。
/// - `max_h`: ASCII 输出最大行数。
///
/// # Errors
/// Returns error string if image decode fails.
pub fn image_file_to_ascii(path: &Path, max_w: u32, max_h: u32) -> Result<String, String> {
    let img = image::open(path).map_err(|e| format!("image decode failed: {e}"))?;
    Ok(image_to_ascii_from_img(&img, max_w, max_h))
}

/// M70.2: 把图像文件转成**带颜色**的 ASCII art（每字符带 ANSI truecolor 前景）。
///
/// 与 `image_file_to_ascii` 相同的算法，但每个字符额外用对应像素的 RGB
/// 作为 ANSI 前景色 `\x1b[38;2;R;G;Bm`。相邻同色字符合并成一个 run，减少
/// escape 序列长度。下游 PNG 渲染器（screenshot.rs::parse_fg_color）已能
/// 解析 38;2;R;G;B，所以彩色 ASCII 能直接渲染进 PNG。
///
/// # Errors
/// Returns error string if image decode fails.
pub fn image_file_to_ascii_colored(path: &Path, max_w: u32, max_h: u32) -> Result<String, String> {
    let img = image::open(path).map_err(|e| format!("image decode failed: {e}"))?;
    Ok(image_to_ascii_from_img_colored(&img, max_w, max_h))
}

/// 把已解码的 `DynamicImage` 转 ASCII art（测试用 + 复用入口）。
#[must_use]
pub fn image_to_ascii_from_img(img: &DynamicImage, max_w: u32, max_h: u32) -> String {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return String::new();
    }
    // 等比缩放到 max_w × max_h 内。
    let scale = (max_w as f64 / w as f64).min(max_h as f64 / h as f64);
    let new_w = ((w as f64) * scale).max(1.0) as u32;
    let new_h = ((h as f64) * scale).max(1.0) as u32;

    let resized = imageops::resize(img, new_w, new_h, imageops::FilterType::Nearest);

    let mut out = String::with_capacity((new_w * new_h + new_h) as usize);
    for y in 0..new_h {
        for x in 0..new_w {
            let pixel = resized.get_pixel(x, y);
            let lum = luminance(pixel.0[0], pixel.0[1], pixel.0[2]);
            let idx = ((u32::from(lum) * ASCII_RAMP.len() as u32) / 256) as usize;
            let idx = idx.min(ASCII_RAMP.len() - 1);
            out.push(ASCII_RAMP[idx]);
        }
        out.push('\n');
    }
    out
}

/// ITU-R BT.601 标准灰度公式。
fn luminance(r: u8, g: u8, b: u8) -> u8 {
    let yf = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
    yf.round().min(255.0) as u8
}

/// M70.2: 把已解码的 `DynamicImage` 转**带颜色**的 ASCII art。
///
/// 每个字符 = 缩放图对应像素的灰度字符，前景色 = 该像素的原始 RGB。
/// 同一行里相邻且同色的字符合并进一个 ANSI run（`\x1b[38;2;R;G;Bm...\x1b[0m`），
/// 显著减少 escape 序列数量。
#[must_use]
pub fn image_to_ascii_from_img_colored(img: &DynamicImage, max_w: u32, max_h: u32) -> String {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return String::new();
    }
    let scale = (max_w as f64 / w as f64).min(max_h as f64 / h as f64);
    let new_w = ((w as f64) * scale).max(1.0) as u32;
    let new_h = ((h as f64) * scale).max(1.0) as u32;
    let resized = imageops::resize(img, new_w, new_h, imageops::FilterType::Nearest);

    let mut out = String::with_capacity((new_w * new_h * 8 + new_h) as usize);
    for y in 0..new_h {
        let mut run_color: Option<(u8, u8, u8)> = None;
        let mut run_chars = String::new();
        for x in 0..new_w {
            let pixel = resized.get_pixel(x, y);
            let (r, g, b) = (pixel.0[0], pixel.0[1], pixel.0[2]);
            let lum = luminance(r, g, b);
            let idx = ((u32::from(lum) * ASCII_RAMP.len() as u32) / 256) as usize;
            let idx = idx.min(ASCII_RAMP.len() - 1);
            let ch = ASCII_RAMP[idx];
            let color = (r, g, b);
            if run_color.is_some() && run_color != Some(color) {
                // flush previous run
                if let Some((rr, gg, bb)) = run_color.take() {
                    out.push_str(&format!("\x1b[38;2;{rr};{gg};{bb}m{run_chars}\x1b[0m"));
                }
                run_chars.clear();
            }
            run_color = Some(color);
            run_chars.push(ch);
        }
        // flush trailing run on this line
        if let Some((rr, gg, bb)) = run_color {
            out.push_str(&format!("\x1b[38;2;{rr};{gg};{bb}m{run_chars}\x1b[0m"));
        }
        out.push('\n');
    }
    out
}

/// M22.1: 尝试把 `<img src>` 解析为本地文件路径。
///
/// 返回 `Some(path)` 当 src 是：
/// - 绝对路径（`/tmp/x.png`）
/// - file: URL（`file:///tmp/x.png`）
/// - 相对路径且文件存在（`./x.png` 相对 `base_dir`）
///
/// 返回 `None` 当 src 是 http(s) URL 或文件不存在（保持占位符）。
#[must_use]
pub fn resolve_local_image_src(src: &str, base_dir: Option<&Path>) -> Option<PathBuf> {
    let trimmed = src.trim();
    if trimmed.is_empty() {
        return None;
    }
    // file: URL
    if let Some(rest) = trimmed.strip_prefix("file://") {
        let p = PathBuf::from(rest.trim_start_matches('/').insert_root());
        if p.is_file() {
            return Some(p);
        }
        return None;
    }
    // http(s) URL：不下载（渲染管线不引入网络）
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return None;
    }
    let candidate = PathBuf::from(trimmed);
    // 绝对路径
    if candidate.is_absolute() {
        return if candidate.is_file() {
            Some(candidate)
        } else {
            None
        };
    }
    // 相对路径：拼 base_dir
    if let Some(base) = base_dir {
        let joined = base.join(&candidate);
        if joined.is_file() {
            return Some(joined);
        }
    } else if candidate.is_file() {
        return Some(candidate);
    }
    None
}

/// 辅助：给无前导 / 的路径补 /（file:// 解析用）。
trait InsertRoot {
    fn insert_root(self) -> String;
}
impl InsertRoot for &str {
    fn insert_root(self) -> String {
        if self.starts_with('/') {
            self.to_string()
        } else {
            format!("/{self}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::DynamicImage;

    #[test]
    fn luminance_white_is_255() {
        assert_eq!(luminance(255, 255, 255), 255);
    }

    #[test]
    fn luminance_black_is_0() {
        assert_eq!(luminance(0, 0, 0), 0);
    }

    #[test]
    fn ascii_ramp_ordered_dark_to_bright() {
        assert_eq!(ASCII_RAMP[0], ' ');
        assert_eq!(ASCII_RAMP[ASCII_RAMP.len() - 1], '@');
    }

    #[test]
    fn empty_image_returns_empty() {
        let img = DynamicImage::new_rgba8(0, 0);
        let s = image_to_ascii_from_img(&img, 10, 10);
        assert!(s.is_empty());
    }

    #[test]
    fn one_by_one_white_image_is_all_at_signs() {
        // 1×1 全白缩放到 10×10 → 全部是最亮字符 '@'（nearest 放大不引入暗像素）
        let img = DynamicImage::ImageRgba8(
            image::RgbaImage::from_raw(1, 1, vec![255, 255, 255, 255]).unwrap(),
        );
        let s = image_to_ascii_from_img(&img, 10, 10);
        let trimmed = s.trim();
        assert!(!trimmed.is_empty(), "should have output");
        assert!(
            trimmed
                .chars()
                .filter(|c| !c.is_whitespace())
                .all(|c| c == '@'),
            "all non-space chars should be @. got={trimmed:?}"
        );
    }

    #[test]
    fn output_dimensions_within_bounds() {
        let img = DynamicImage::new_rgba8(100, 50);
        let s = image_to_ascii_from_img(&img, 20, 10);
        let lines: Vec<&str> = s.lines().collect();
        assert!(lines.len() <= 10);
        for l in &lines {
            assert!(l.chars().count() <= 20);
        }
    }

    #[test]
    fn resolve_https_url_returns_none() {
        assert!(resolve_local_image_src("https://x.com/a.png", None).is_none());
    }

    #[test]
    fn resolve_nonexistent_relative_returns_none() {
        assert!(resolve_local_image_src("nonexistent.png", None).is_none());
    }

    #[test]
    fn resolve_existing_relative_returns_some() {
        // 用本 crate 的 Cargo.toml 作为"存在的文件"
        let p = resolve_local_image_src("Cargo.toml", None);
        assert!(p.is_some());
    }

    #[test]
    fn resolve_file_url() {
        // 用绝对路径 file:// 指向 Cargo.toml
        let cwd = std::env::current_dir().unwrap();
        let target = cwd.join("Cargo.toml");
        let url = format!("file://{}", target.display());
        let p = resolve_local_image_src(&url, None);
        assert!(p.is_some(), "should resolve file:// URL: {url}");
    }

    // ---- M70.2: colored ASCII art ----

    #[test]
    fn colored_empty_image_returns_empty() {
        let img = DynamicImage::new_rgba8(0, 0);
        let s = image_to_ascii_from_img_colored(&img, 10, 10);
        assert!(s.is_empty());
    }

    #[test]
    fn colored_output_contains_ansi_truecolor() {
        // 纯红 1×1 图 → 缩放后每个像素 RGB=(255,0,0) → ANSI 38;2;255;0;0
        let img = DynamicImage::ImageRgba8(
            image::RgbaImage::from_raw(1, 1, vec![255, 0, 0, 255]).unwrap(),
        );
        let s = image_to_ascii_from_img_colored(&img, 5, 5);
        assert!(
            s.contains("38;2;255;0;0"),
            "red pixels should produce ANSI 38;2;255;0;0, got: {s:?}"
        );
    }

    #[test]
    fn colored_merges_adjacent_same_color() {
        // 纯色 1×1 图放大到 5×5 → 整行同色，应该合并成一个 ANSI run（一个 \x1b[ 开头）
        let img = DynamicImage::ImageRgba8(
            image::RgbaImage::from_raw(1, 1, vec![0, 200, 0, 255]).unwrap(),
        );
        let s = image_to_ascii_from_img_colored(&img, 5, 3);
        // 每行应只有 1 个 \x1b[38;2 开头（合并），不是 5 个
        for line in s.lines() {
            let count = line.matches("\x1b[38;2").count();
            assert_eq!(
                count, 1,
                "each line should merge to 1 ANSI run, got {count} in: {line:?}"
            );
        }
    }

    #[test]
    fn colored_uses_pixel_rgb_not_gray() {
        // 红 (255,0,0) 和 蓝 (0,0,255) 灰度相近但 RGB 不同，应产出不同 ANSI。
        let red = DynamicImage::ImageRgba8(
            image::RgbaImage::from_raw(1, 1, vec![255, 0, 0, 255]).unwrap(),
        );
        let blue = DynamicImage::ImageRgba8(
            image::RgbaImage::from_raw(1, 1, vec![0, 0, 255, 255]).unwrap(),
        );
        let red_s = image_to_ascii_from_img_colored(&red, 3, 3);
        let blue_s = image_to_ascii_from_img_colored(&blue, 3, 3);
        assert!(
            red_s.contains("38;2;255;0;0") && blue_s.contains("38;2;0;0;255"),
            "red→38;2;255;0;0, blue→38;2;0;0;255"
        );
        assert_ne!(red_s, blue_s, "different colors should differ");
    }
}
