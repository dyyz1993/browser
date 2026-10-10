//! 车道hwB2：band 索引跳过率观测（诊断面，非等值断言）。
//!
//! 对 bench 基准四枚 fixture（pelican-bike / travel-frog / circuit /
//! refraction）打印 `--bands {2,4,8}` 均匀划分下每带
//! `(窗内需处理命令数, 索引跳过命令数)` 与全帧跳过率。fixture 缺失
//! 时跳过（不红）——本文件只服务报告数字，不承担回归钉扎职责
//! （钉扎在 hwb_band_equality 的 band_idx_* 测试）。
//!
//! 跑法：`cargo test -p svgine-core --test hwb_band_idx_corpus_stats -- --nocapture`

use svgine_core::builder::{build_display_list_at_with_font, parse_document};
use svgine_core::raster::{band_index_stats, PixFmt, Viewport};

fn fixture(rel: &[&str]) -> Option<std::path::PathBuf> {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/svgine-core → crates
    p.pop(); // crates → 仓库根
    for seg in rel {
        p.push(seg);
    }
    p.is_file().then_some(p)
}

fn stats_for(path: &std::path::Path, w: u32, h: u32, aa: u8) {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            println!("  读取失败 {}：{e}", path.display());
            return;
        }
    };
    let doc = match parse_document(&src) {
        Ok(d) => d,
        Err(e) => {
            println!("  解析失败 {}：{e:?}", path.display());
            return;
        }
    };
    let dl = build_display_list_at_with_font(&doc, w as f32, h as f32, 0.0, None);
    let vp = Viewport {
        width: w,
        height: h,
        format: PixFmt::Rgba8,
        aa,
    };
    println!(
        "  cmds={} pts={} masks={} patterns={} filters={} images={} clips={}（视口 {w}×{h} aa={aa}）",
        dl.cmds.len(),
        dl.points.len() / 2,
        dl.masks.len(),
        dl.patterns.len(),
        dl.filters.len(),
        dl.images.len(),
        dl.clips.len(),
    );
    // precompute 串行段成本（含索引构建）——Amdahl 墙的定量
    {
        let t0 = std::time::Instant::now();
        let mut us = Vec::new();
        for _ in 0..9 {
            let t1 = std::time::Instant::now();
            let shared = svgine_core::raster::precompute_frame(&dl, &vp);
            let dt = t1.elapsed().as_secs_f64() * 1e6;
            us.push(dt);
            let _ = &shared;
        }
        us.sort_by(|a, b| a.partial_cmp(b).expect("finite timings"));
        let _ = t0;
        println!(
            "  precompute_frame 中位数 {:.0}µs（min {:.0} / max {:.0}）",
            us[4], us[0], us[8]
        );
    }
    for bands in [2usize, 4, 8] {
        let cuts: Vec<usize> = (1..bands).map(|i| i * h as usize / bands).collect();
        let stats = band_index_stats(&dl, &vp, &cuts);
        // 逐带串行计时（band-local cache 各自独立）＝各带工作量的
        // 直接观测（并行 wall ≈ 最重带 + 串行段 + spawn）。
        {
            let shared = svgine_core::raster::precompute_frame(&dl, &vp);
            let row_bytes = w as usize * 4;
            let mut bands_us = Vec::new();
            let mut y = 0usize;
            for &cut in cuts.iter().chain(std::iter::once(&(h as usize))) {
                let rows = cut - y;
                let mut band = vec![0u8; rows * row_bytes];
                let mut cache = svgine_core::raster::RenderCache::default();
                let t1 = std::time::Instant::now();
                svgine_core::raster::render_band(
                    &dl,
                    &vp,
                    &mut band,
                    y,
                    cut,
                    svgine_core::dl::Rgba8 {
                        r: 255,
                        g: 255,
                        b: 255,
                        a: 255,
                    },
                    &shared,
                    &mut cache,
                );
                bands_us.push(t1.elapsed().as_secs_f64() * 1e6);
                y = cut;
            }
            let s: Vec<String> = bands_us.iter().map(|v| format!("{:.0}", v)).collect();
            println!("    逐带串行 µs = [{}]", s.join(" "));
        }
        let total: usize = stats.iter().map(|(k, _)| *k).sum();
        let skipped: usize = stats.iter().map(|(_, s)| *s).sum();
        let per = stats
            .iter()
            .map(|(k, s)| format!("({k},{s})"))
            .collect::<Vec<_>>()
            .join(" ");
        let rate = 100.0 * skipped as f64 / (total + skipped) as f64;
        println!(
            "  --bands {bands}: 每带(处理,跳过) = {per} | 处理指令带宽均值 {:.1} / 全帧跳过率 {rate:.1}%",
            total as f64 / bands as f64
        );
    }
}

#[test]
fn print_band_idx_skip_rates() {
    // 视口 = 各 fixture 的 SVG width/height（与 CLI bench 同口径）
    let fixtures: [(&str, &[&str], u32, u32); 4] = [
        (
            "pelican-bike",
            &["tests", "oracle", "repros", "pelican-bike.svg"],
            960,
            640,
        ),
        (
            "travel-frog",
            &["tests", "corpus", "s11-scene", "travel-frog.svg"],
            900,
            600,
        ),
        (
            "circuit",
            &["tests", "oracle", "std_sets", "physics_demo", "circuit.svg"],
            480,
            360,
        ),
        (
            "refraction",
            &[
                "tests",
                "oracle",
                "std_sets",
                "physics_demo",
                "refraction.svg",
            ],
            480,
            360,
        ),
    ];
    for (name, rel, w, h) in fixtures {
        match fixture(rel) {
            Some(p) => {
                println!("{name}:");
                stats_for(&p, w, h, 2);
            }
            None => println!("{name}: fixture 不存在，跳过"),
        }
    }
}
