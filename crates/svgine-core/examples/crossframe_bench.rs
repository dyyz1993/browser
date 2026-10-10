//! 车道M3 验收工具（host 侧 example，仅开发用——不进任何构建产物）：
//! ①`parity` 语料级字节等值红线 sweep（CrossFrameCache vs 逐帧 render）
//! ②`bench <svg>` 连续帧性能对照（svgine_render 等价调用序列：
//!   每帧 build_display_list_at + render，带缓存 vs 无缓存）。
//!
//! 用法：
//!   cargo run --release -p svgine-core --example crossframe_bench -- parity [corpus_dir]
//!   cargo run --release -p svgine-core --example crossframe_bench -- bench <file.svg> [frames] [w] [h]
// host 工具：std 可用（svgine-core 本体保持 no_std 不受影响）。
#![allow(clippy::print_literal)]
use std::fs;
use std::time::Instant;

use svgine_core::builder::{build_display_list_at, parse_document};
use svgine_core::dl::DisplayList;
use svgine_core::raster::{CrossFrameCache, PixFmt, Viewport};

fn vp(w: u32, h: u32) -> Viewport {
    Viewport {
        width: w,
        height: h,
        format: PixFmt::Rgba8,
        aa: 2,
    }
}

/// 动画文档采 5 帧、静态文档采 1 帧（t=0）。
fn sample_ts(has_anim: bool) -> Vec<f64> {
    if has_anim {
        vec![0.0, 250.0, 500.0, 750.0, 999.0]
    } else {
        vec![0.0]
    }
}

fn parse_or_panic(text: &str, path: &str) -> svgine_core::builder::Document {
    match parse_document(text) {
        Ok(d) => d,
        Err(e) => panic!("{path}: parse 失败 {e:?}"),
    }
}

fn parity_dir(dir: &std::path::Path) -> (usize, usize) {
    let mut files: Vec<_> = walk_svgs(dir);
    files.sort();
    let (mut pass, mut fail) = (0usize, 0usize);
    for f in &files {
        let text = fs::read_to_string(f).unwrap_or_else(|e| panic!("读 {f:?}: {e}"));
        let has_anim = text.contains("<animate") || text.contains("animation");
        let (w, h) = (240u32, 180u32);
        let doc = parse_or_panic(&text, &f.to_string_lossy());
        let mut cf = CrossFrameCache::new();
        let mut plain = vec![0u8; (w * h * 4) as usize];
        let mut cfb = vec![0u8; (w * h * 4) as usize];
        let mut ok = true;
        for t in sample_ts(has_anim) {
            let dl: DisplayList = build_display_list_at(&doc, w as f32, h as f32, t);
            svgine_core::raster::render(&dl, &vp(w, h), &mut plain);
            cf.render(&dl, &vp(w, h), &mut cfb);
            if plain != cfb {
                println!("FAIL {} t={t}", f.display());
                ok = false;
                break;
            }
        }
        if ok {
            pass += 1;
            let st = cf.stats();
            println!(
                "pass {} (hits={} misses={})",
                f.display(),
                st.hits,
                st.misses
            );
        } else {
            fail += 1;
        }
    }
    (pass, fail)
}

/// 递归收集目录下全部 .svg（语料按 s1-s13 分阶段子目录组织）。
fn walk_svgs(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("读 {dir:?}: {e}"))
        .flatten()
    {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk_svgs(&p));
        } else if p.extension().is_some_and(|x| x == "svg") {
            out.push(p);
        }
    }
    out
}

fn bench(file: &str, frames: usize, w: u32, h: u32) {
    let text = fs::read_to_string(file).unwrap_or_else(|e| panic!("读 {file}: {e}"));
    let doc = parse_or_panic(&text, file);
    let buf_len = (w as usize) * (h as usize) * 4;
    let has_anim = text.contains("<animate") || text.contains("animation");
    let dur_ms: f64 = 5000.0;

    // 交错 A/B（同帧交替测量，消顺序/热漂移偏差——perf_interleave 同哲学）
    let mut plain = vec![0u8; buf_len];
    let mut cfb = vec![0u8; buf_len];
    let mut cf = CrossFrameCache::new();
    let mut times_a: Vec<f64> = Vec::with_capacity(frames);
    let mut times_b: Vec<f64> = Vec::with_capacity(frames);
    // 预热（首帧建池 + 缓存/分配器热身，不计入）
    {
        let dl = build_display_list_at(&doc, w as f32, h as f32, 0.0);
        svgine_core::raster::render(&dl, &vp(w, h), &mut plain);
        cf.render(&dl, &vp(w, h), &mut cfb);
    }
    for i in 0..frames {
        let t = if has_anim {
            (i as f64 / frames as f64) * dur_ms
        } else {
            0.0
        };
        // 每帧两段独立计时，各含自己的 DL 构建（== svgine_render 序列
        // 「建 DL + 渲染」）；起始方逐帧交替，消同帧内先后热偏差。
        let (ta, tb) = if i % 2 == 0 {
            let t0 = Instant::now();
            let dl = build_display_list_at(&doc, w as f32, h as f32, t);
            svgine_core::raster::render(&dl, &vp(w, h), &mut plain);
            let a = t0.elapsed().as_secs_f64() * 1e3;
            let t1 = Instant::now();
            let dl = build_display_list_at(&doc, w as f32, h as f32, t);
            cf.render(&dl, &vp(w, h), &mut cfb);
            (a, t1.elapsed().as_secs_f64() * 1e3)
        } else {
            let t1 = Instant::now();
            let dl = build_display_list_at(&doc, w as f32, h as f32, t);
            cf.render(&dl, &vp(w, h), &mut cfb);
            let b = t1.elapsed().as_secs_f64() * 1e3;
            let t0 = Instant::now();
            let dl = build_display_list_at(&doc, w as f32, h as f32, t);
            svgine_core::raster::render(&dl, &vp(w, h), &mut plain);
            (t0.elapsed().as_secs_f64() * 1e3, b)
        };
        times_a.push(ta);
        times_b.push(tb);
    }
    assert_eq!(plain, cfb, "bench 双路径输出必须逐字节一致");

    let stats = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        let mean: f64 = v.iter().sum::<f64>() / v.len() as f64;
        let p50 = v[v.len() / 2];
        let p95 = v[(v.len() as f32 * 0.95) as usize % v.len()];
        (mean, p50, p95)
    };
    let (am, a50, a95) = stats(times_a);
    let (bm, b50, b95) = stats(times_b);
    println!(
        "{{\n  \"file\": \"{file}\",\n  \"frames\": {frames},\n  \"vp\": \"{w}x{h}\",\n  \"plain_mean_ms\": {am:.3},\n  \"plain_p50_ms\": {a50:.3},\n  \"plain_p95_ms\": {a95:.3},\n  \"crossframe_mean_ms\": {bm:.3},\n  \"crossframe_p50_ms\": {b50:.3},\n  \"crossframe_p95_ms\": {b95:.3},\n  \"speedup_mean\": {:.2},\n  \"stats\": {:?}\n}}",
        am / bm,
        cf.stats()
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("parity") => {
            let dir = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| "tests/corpus".to_string());
            let (pass, fail) = parity_dir(std::path::Path::new(&dir));
            println!("parity: {pass} pass, {fail} fail");
            if fail > 0 {
                std::process::exit(1);
            }
        }
        Some("bench") => {
            let file = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| "tests/corpus/s11-scene/travel-frog.svg".to_string());
            let frames: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(100);
            let w: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(960);
            let h: u32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(640);
            bench(&file, frames, w, h);
        }
        _ => {
            eprintln!(
                "用法: crossframe_bench parity [corpus_dir] | bench <file.svg> [frames] [w] [h]"
            );
            std::process::exit(2);
        }
    }
}
