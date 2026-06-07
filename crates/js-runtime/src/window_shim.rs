//! M28.2: 注入 `window` 全局对象（对齐 W3C/Chrome 基础子集）。
//!
//! 浏览器核心语义：`window === self === globalThis`。让 SPA 的 JS 用：
//! ```js
//! window.navigator.userAgent    // 经 globalThis 自动可见
//! window.location.href()
//! window.innerWidth             // 1280（特性检测常用）
//! window.innerHeight            // 720
//! window.self === window        // true
//! window.window === window      // true（自引用）
//! ```
//!
//! ## 设计决策：window = globalThis 自引用
//!
//! 不复制 navigator/location/history 到 window（会割裂状态）。浏览器里 window
//! **就是**全局对象，所有全局变量自动是 window 的属性。所以最符合 W3C 的做法
//! 是 `globalThis.window = globalThis`，window.navigator 等经 globalThis 自动可见。
//!
//! 额外的视口数据属性（innerWidth/innerHeight/...）SPA 特性检测常用（响应式布局、
//! 懒加载阈值）。这些是静态值（无显示器环境，给一个合理的桌面默认值 1280×720）。

use boa_engine::{Context, JsResult};

/// 默认视口宽度（桌面环境合理值，爬虫场景常用）。
const DEFAULT_INNER_WIDTH: i64 = 1280;
/// 默认视口高度。
const DEFAULT_INNER_HEIGHT: i64 = 720;
/// 默认设备像素比（普通屏幕）。
const DEFAULT_DEVICE_PIXEL_RATIO: f64 = 1.0;

/// 在 `ctx` 上注册 `window` 全局对象（指向 globalThis + 视口数据属性）。
///
/// 必须在 navigator/location/history/storage/fetch/XMLHttpRequest/WebSocket 等
/// 全局对象安装**之后**调用，否则 `window.xxx` 会 undefined（虽然之后这些全局
/// 再安装时仍会经 globalThis 可见，但顺序明确更安全）。
pub fn install_window(ctx: &mut Context) -> JsResult<()> {
    // 让 window/self/top/parent/frames 都指向 globalThis（W3C：window===self===globalThis）。
    // 这样 window.navigator 等"属性"经 globalThis 自动可见，无需复制状态。
    // 视口数据属性直接挂在 globalThis 上（= window 的属性）。
    let js = format!(
        r#"(function() {{
            var g = globalThis;
            g.window = g;
            g.self = g;
            g.top = g;
            g.parent = g;
            g.frames = g;
            g.innerWidth = {width};
            g.innerHeight = {height};
            g.outerWidth = {width};
            g.outerHeight = {height};
            g.devicePixelRatio = {dpr};
            g.pageXOffset = 0;
            g.pageYOffset = 0;
            g.scrollX = 0;
            g.scrollY = 0;
        }})();
        undefined;"#,
        width = DEFAULT_INNER_WIDTH,
        height = DEFAULT_INNER_HEIGHT,
        dpr = DEFAULT_DEVICE_PIXEL_RATIO,
    );
    ctx.eval(boa_engine::Source::from_bytes(&js))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::install;
    use crate::navigator_shim::install_navigator;
    use boa_engine::JsValue;

    fn setup_ctx() -> Context {
        let mut ctx = Context::default();
        install(&mut ctx);
        // 先装 navigator，再装 window（模拟 run_scripts 顺序）
        install_navigator(&mut ctx).expect("install navigator");
        install_window(&mut ctx).expect("install window");
        ctx
    }

    fn str_result(v: JsValue) -> String {
        v.as_string()
            .map(|s| s.to_std_string_escaped())
            .unwrap_or_default()
    }

    fn num_result(v: JsValue) -> f64 {
        v.as_number().unwrap_or(f64::NAN)
    }

    #[test]
    fn window_equals_globalthis() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("window === globalThis"))
            .unwrap();
        assert!(r.as_boolean().unwrap());
    }

    #[test]
    fn self_and_window_aliases() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "window.self === window && window.window === window && window.top === window",
            ))
            .unwrap();
        assert!(r.as_boolean().unwrap());
    }

    #[test]
    fn window_navigator_visible() {
        // 关键：window.navigator 经 globalThis 自动可见
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("window.navigator.userAgent"))
            .unwrap();
        let ua = str_result(r);
        assert!(ua.contains("Chrome/"), "ua={ua}");
    }

    #[test]
    fn inner_width_and_height() {
        let mut ctx = setup_ctx();
        let w = ctx
            .eval(boa_engine::Source::from_bytes("window.innerWidth"))
            .unwrap();
        assert_eq!(num_result(w), 1280.0);
        let h = ctx
            .eval(boa_engine::Source::from_bytes("window.innerHeight"))
            .unwrap();
        assert_eq!(num_result(h), 720.0);
    }

    #[test]
    fn device_pixel_ratio() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("window.devicePixelRatio"))
            .unwrap();
        assert_eq!(num_result(r), 1.0);
    }

    #[test]
    fn scroll_offsets() {
        let mut ctx = setup_ctx();
        let x = ctx
            .eval(boa_engine::Source::from_bytes("window.pageXOffset"))
            .unwrap();
        assert_eq!(num_result(x), 0.0);
    }

    #[test]
    fn window_not_a_function() {
        // window 本身是对象不是函数
        let mut ctx = setup_ctx();
        let ty = ctx
            .eval(boa_engine::Source::from_bytes("typeof window"))
            .unwrap();
        assert_eq!(str_result(ty), "object");
    }
}
