//! Tier C 真字体解析集成测试（ADR-0008）：真实 TTF fixture
//!（Arial 副本，tests/fixture-noto-arial.ttf）端到端断言。
//! 覆盖：sfnt 目录 / cmap4 查找 / hmtx / loca+glyf simple 轮廓 /
//! 复合字形一级组件展开 / advance。

use svgine_core::font::Font;

const ARIAL: &[u8] = include_bytes!("fixture-noto-arial.ttf");

#[test]
fn parse_ok_and_metrics() {
    let f = Font::parse(ARIAL).expect("Arial 解析应成功");
    // Arial units_per_em = 2048
    assert_eq!(f.units_per_em(), 2048.0);
}

#[test]
fn cmap_finds_ascii_glyphs() {
    let f = Font::parse(ARIAL).expect("parse");
    // 已知 Arial 映射：'A'=0x41、'a'=0x61、'0'=0x30、'~'=0x7E 都有字形
    for cp in [0x41u32, 0x61, 0x30, 0x7E] {
        let gid = f.glyph_id(cp).expect("ASCII 字形应存在");
        assert!(gid > 0, "cp={cp:#x} 不应映射到 .notdef");
    }
    // 未映射码点 → None 或 0（.notdef）
    assert!(f.glyph_id(0x4E00).is_none() || f.glyph_id(0x4E00) == Some(0));
}

#[test]
fn advance_matches_em() {
    let f = Font::parse(ARIAL).expect("parse");
    let gid_a = f.glyph_id(0x41).expect("A gid");
    let adv = f.advance_em(gid_a).expect("A advance");
    // Arial 'A' advance = 1479/2048 ≈ 0.722em；宽容区间验证归一化正确
    assert!(adv > 0.5 && adv < 1.0, "A advance_em={adv}");
}

#[test]
fn outline_caps_and_lower_have_contours() {
    let f = Font::parse(ARIAL).expect("parse");
    for cp in [0x41u32, 0x61, 0x67] {
        // A a g（g 含双轮廓 + 下伸）
        let gid = f.glyph_id(cp).expect("gid");
        let o = f.outline(gid).expect("outline");
        assert!(!o.subs.is_empty(), "cp={cp:#x} 轮廓不应为空");
        for sp in &o.subs {
            assert!(sp.closed, "轮廓应闭合");
            assert!(sp.pts.len() >= 3, "折线至少 3 点");
        }
    }
}

#[test]
fn composite_glyph_expands() {
    let f = Font::parse(ARIAL).expect("parse");
    // 'é'（U+00E9）在 Arial 是复合字形（accent 组件）
    let gid = f.glyph_id(0xE9);
    if let Some(gid) = gid {
        let o = f.outline(gid).expect("é outline");
        assert!(!o.subs.is_empty(), "é 复合字形应展开出轮廓");
    }
    // 空格（U+0020）：无轮廓但 advance > 0
    let sp = f.glyph_id(0x20).expect("space gid");
    let o = f.outline(sp).expect("space outline");
    assert!(o.subs.is_empty(), "空格不应有轮廓");
    assert!(o.advance_em > 0.0, "空格应有 advance");
}

#[test]
fn outline_extents_within_reasonable_em() {
    let f = Font::parse(ARIAL).expect("parse");
    let gid = f.glyph_id(0x41).expect("A gid");
    let o = f.outline(gid).expect("A outline");
    for sp in &o.subs {
        for &(x, y) in &sp.pts {
            assert!(x.is_finite() && y.is_finite());
            // em 归一化后 |coord| 不应超 ~2（宽容上界；含下伸/上标极端）
            assert!(x.abs() < 2.0 && y.abs() < 2.0, "坐标超界 ({x},{y})");
        }
    }
}
