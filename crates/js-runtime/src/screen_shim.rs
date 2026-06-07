//! M28.4: 注入 `screen` 全局对象（对齐 W3C/Chrome 基础子集）。
//!
//! SPA 特性检测/响应式布局常用（如 `if (screen.width < 768)`）。全部静态值
//!（无显示器环境，给桌面默认值 1280×720）。
//! ```js
//! screen.width          // 1280
//! screen.height         // 720
//! screen.availWidth     // 1280（无任务栏）
//! screen.availHeight    // 720
//! screen.colorDepth     // 24（真彩色）
//! screen.pixelDepth     // 24
//! screen.orientation   // { type: 'landscape-primary' }
//! ```
//!
//! ## 设计决策：纯 JS 对象字面量（与 navigator 一致）
//!
//! 所有值静态，不需要 NativeFunction getter。直接 JS 对象字面量注入
//!（同 navigator）。`orientation` 对象也用字面量。

use boa_engine::{Context, JsResult};

/// 默认屏幕宽度（桌面环境合理值）。
const DEFAULT_WIDTH: i64 = 1280;
/// 默认屏幕高度。
const DEFAULT_HEIGHT: i64 = 720;
/// 默认颜色深度（24 位真彩色）。
const DEFAULT_COLOR_DEPTH: i64 = 24;

/// 在 `ctx` 上注册 `screen` 全局对象。
pub fn install_screen(ctx: &mut Context) -> JsResult<()> {
    let js = format!(
        r#"(function() {{
            globalThis.screen = {{
                width: {w},
                height: {h},
                availWidth: {w},
                availHeight: {h},
                colorDepth: {cd},
                pixelDepth: {cd},
                orientation: {{ type: 'landscape-primary', angle: 0 }},
                left: 0,
                top: 0
            }};
        }})();
        undefined;"#,
        w = DEFAULT_WIDTH,
        h = DEFAULT_HEIGHT,
        cd = DEFAULT_COLOR_DEPTH,
    );
    ctx.eval(boa_engine::Source::from_bytes(&js))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::install;
    use boa_engine::JsValue;

    fn setup_ctx() -> Context {
        let mut ctx = Context::default();
        install(&mut ctx);
        install_screen(&mut ctx).expect("install screen");
        ctx
    }

    fn num_result(v: JsValue) -> f64 {
        v.as_number().unwrap_or(f64::NAN)
    }

    #[test]
    fn screen_width_and_height() {
        let mut ctx = setup_ctx();
        let w = ctx
            .eval(boa_engine::Source::from_bytes("screen.width"))
            .unwrap();
        assert_eq!(num_result(w), 1280.0);
        let h = ctx
            .eval(boa_engine::Source::from_bytes("screen.height"))
            .unwrap();
        assert_eq!(num_result(h), 720.0);
    }

    #[test]
    fn screen_avail_same_as_width_height() {
        let mut ctx = setup_ctx();
        let aw = ctx
            .eval(boa_engine::Source::from_bytes("screen.availWidth"))
            .unwrap();
        let ah = ctx
            .eval(boa_engine::Source::from_bytes("screen.availHeight"))
            .unwrap();
        assert_eq!(num_result(aw), 1280.0);
        assert_eq!(num_result(ah), 720.0);
    }

    #[test]
    fn screen_color_depth_24() {
        let mut ctx = setup_ctx();
        let cd = ctx
            .eval(boa_engine::Source::from_bytes("screen.colorDepth"))
            .unwrap();
        assert_eq!(num_result(cd), 24.0);
        let pd = ctx
            .eval(boa_engine::Source::from_bytes("screen.pixelDepth"))
            .unwrap();
        assert_eq!(num_result(pd), 24.0);
    }

    #[test]
    fn screen_orientation() {
        let mut ctx = setup_ctx();
        let ty = ctx
            .eval(boa_engine::Source::from_bytes("screen.orientation.type"))
            .unwrap();
        assert_eq!(
            ty.as_string()
                .map(|s| s.to_std_string_escaped())
                .unwrap_or_default(),
            "landscape-primary"
        );
    }

    #[test]
    fn screen_left_and_top() {
        let mut ctx = setup_ctx();
        let l = ctx
            .eval(boa_engine::Source::from_bytes("screen.left"))
            .unwrap();
        let t = ctx
            .eval(boa_engine::Source::from_bytes("screen.top"))
            .unwrap();
        assert_eq!(num_result(l), 0.0);
        assert_eq!(num_result(t), 0.0);
    }
}
