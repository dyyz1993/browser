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

/// 在 `ctx` 上注册 `window` 全局对象（指向 globalThis + 视口数据属性 +
/// EventTarget 三件套：addEventListener/removeEventListener/dispatchEvent）。
///
/// 必须在 navigator/location/history/storage/fetch/XMLHttpRequest/WebSocket 等
/// 全局对象安装**之后**调用，否则 `window.xxx` 会 undefined（虽然之后这些全局
/// 再安装时仍会经 globalThis 可见，但顺序明确更安全）。
pub fn install_window(ctx: &mut Context) -> JsResult<()> {
    // 让 window/self/top/parent/frames 都指向 globalThis（W3C：window===self===globalThis）。
    // 这样 window.navigator 等"属性"经 globalThis 自动可见，无需复制状态。
    // 视口数据属性直接挂在 globalThis 上（= window 的属性）。
    //
    // M62: window 同时是 EventTarget——docsify/React/Vue 等框架会在 window 上
    // 注册 click/hashchange/popstate/DOMContentLoaded/load 等事件。缺 addEventListener
    // 会导致框架初始化崩溃（bark.day.app docsify initRouter 即此 bug）。
    // 监听器存 globalThis.__winListeners（与 document 分开，避免混淆）。
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
            // M62: window 作为 EventTarget，支持事件监听。
            // 监听器存独立对象（避免与 globalThis 属性名冲突）。
            if (!g.__winListeners) g.__winListeners = {{}};
            g.addEventListener = function(type, cb) {{
                // M64: Vue/React 用 addEventListener('test', null, opts)
                // 检测 passive 支持。null listener 应静默忽略。
                if (cb === null || cb === undefined) return;
                if (!g.__winListeners) g.__winListeners = {{}};
                if (!g.__winListeners[type]) g.__winListeners[type] = [];
                var self = g;
                // 包装回调——确保事件对象 target/currentTarget 指向 window。
                var wrapped = function(ev) {{
                    if (!ev) ev = {{}};
                    if (ev.target === undefined) ev.target = self;
                    if (ev.currentTarget === undefined) ev.currentTarget = self;
                    return cb.call(self, ev);
                }};
                cb.__winWrapped = wrapped;  // 便于 removeEventListener 精确移除
                g.__winListeners[type].push(wrapped);
            }};
            g.removeEventListener = function(type, cb) {{
                if (!g.__winListeners || !g.__winListeners[type]) return;
                var wrapped = cb && cb.__winWrapped;
                g.__winListeners[type] = g.__winListeners[type].filter(function(f) {{
                    return wrapped ? (f !== wrapped) : (f !== cb);
                }});
            }};
            g.dispatchEvent = function(ev) {{
                if (!g.__winListeners || !ev || !g.__winListeners[ev.type]) return true;
                var cbs = g.__winListeners[ev.type];
                ev.target = g; ev.currentTarget = g;
                for (var i = 0; i < cbs.length; i++) {{
                    try {{ cbs[i](ev); }} catch(e) {{
                        if (typeof __log === 'function') __log('[event] window listener threw: ' + e.message);
                    }}
                }}
                return true;
            }};
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

    #[test]
    fn window_has_event_target_methods() {
        // M62: window 必须支持 addEventListener/removeEventListener/dispatchEvent
        // docsify initRouter 调 window.addEventListener('hashchange', ...) 注册路由回调。
        let mut ctx = setup_ctx();
        for method in ["addEventListener", "removeEventListener", "dispatchEvent"] {
            let ty = ctx
                .eval(boa_engine::Source::from_bytes(&format!(
                    "typeof window.{method}"
                )))
                .unwrap();
            assert_eq!(
                str_result(ty),
                "function",
                "window.{method} should be a function"
            );
        }
    }

    #[test]
    fn window_dispatch_event_invokes_listener() {
        let mut ctx = setup_ctx();
        let js = r#"
            var got = '';
            window.addEventListener('test-event', function(ev) {
                got = ev.type + ':' + (ev.target === window);
            });
            window.dispatchEvent({ type: 'test-event' });
            got;
        "#;
        let r = ctx.eval(boa_engine::Source::from_bytes(js)).unwrap();
        assert_eq!(str_result(r), "test-event:true");
    }

    #[test]
    fn window_remove_event_listener() {
        let mut ctx = setup_ctx();
        let js = r#"
            var count = 0;
            var cb = function() { count++; };
            window.addEventListener('tick', cb);
            window.dispatchEvent({ type: 'tick' });
            window.removeEventListener('tick', cb);
            window.dispatchEvent({ type: 'tick' });
            count;
        "#;
        let r = ctx.eval(boa_engine::Source::from_bytes(js)).unwrap();
        assert_eq!(num_result(r), 1.0); // 只触发一次（removeEventListener 后不再触发）
    }

    #[test]
    fn window_listener_event_target_defaults_to_window() {
        // 包装回调确保 ev.target 默认指向 window
        let mut ctx = setup_ctx();
        let js = r#"
            var targetIsWindow = false;
            window.addEventListener('load', function(ev) {
                targetIsWindow = (ev.target === window);
            });
            window.dispatchEvent({ type: 'load' });
            targetIsWindow;
        "#;
        let r = ctx.eval(boa_engine::Source::from_bytes(js)).unwrap();
        assert!(r.as_boolean().unwrap());
    }
}
