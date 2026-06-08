//! M41: `Image` 构造器 shim（消除 baidu SPA `Image is not defined` 错误）。
//!
//! ## 爬虫场景设计决策
//!
//! 真实浏览器的 `new Image()` 会**异步 fetch** src 指向的 URL。
//! 对爬虫这是浪费 + 可靠性风险（慢/挂起服务器）。
//! 这里采用**爬虫友好**策略：设 `.src` 后**不 fetch**，
//! 通过 setTimeout(0) 假触发 `onload`（复用 M16 timer 基建）。
//! 让 SPA 的懒加载模式（`img.onload = function(){ render(img) }`）能工作，
//! 但不实际下载图像数据。
//!
//! ## 标准 API 子集
//!
//! ```js
//! var img = new Image();         // 或 new Image(w, h)
//! img.src = "logo.png";          // 设 src → setTimeout 后 fire onload
//! img.onload = function(){...};  // 设 onload（设 src 前或后都行）
//! img.onerror = function(){...};
//! img.complete                    // true 当 onload 已 fire
//! img.naturalWidth / naturalHeight
//! img.width / height             // 可由构造器参数设
//! img.alt
//! ```

use boa_engine::{Context, JsResult, Source};

/// 在 `ctx` 上注册 `Image` 构造器（全局 `new Image()`）。
///
/// 必须在 bridge + `setTimeout`（M16）安装**之后**调用。
pub fn install_image(ctx: &mut Context) -> JsResult<()> {
    let js = r#"(function() {
        function Image(width, height) {
            this._src = '';
            this.width = width || 0;
            this.height = height || 0;
            this.naturalWidth = width || 0;
            this.naturalHeight = height || 0;
            this.alt = '';
            this.complete = false;
            this.onload = null;
            this.onerror = null;
        }

        // src setter：设值后 setTimeout(0) 假触发 onload（爬虫不 fetch）。
        Object.defineProperty(Image.prototype, 'src', {
            get: function() { return this._src; },
            set: function(v) {
                this._src = String(v);
                var self = this;
                if (!this._src) return;
                setTimeout(function() {
                    self.complete = true;
                    if (typeof self.onload === 'function') {
                        try { self.onload(); } catch (e) {}
                    }
                }, 0);
            },
            enumerable: true, configurable: true,
        });

        globalThis.Image = Image;
    })();
    undefined;"#;
    ctx.eval(Source::from_bytes(js))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use boa_engine::JsValue;

    fn setup_ctx() -> Context {
        let mut ctx = Context::default();
        crate::bridge::install(&mut ctx);
        install_image(&mut ctx).expect("install image");
        ctx
    }

    fn str_result(v: JsValue) -> String {
        v.as_string()
            .map(|s| s.to_std_string_escaped())
            .unwrap_or_default()
    }

    #[test]
    fn image_is_a_constructor() {
        let mut ctx = setup_ctx();
        let ty = ctx.eval(Source::from_bytes("typeof Image")).unwrap();
        assert_eq!(str_result(ty), "function");
    }

    #[test]
    fn new_image_returns_object() {
        let mut ctx = setup_ctx();
        let ty = ctx.eval(Source::from_bytes("typeof new Image()")).unwrap();
        assert_eq!(str_result(ty), "object");
    }

    #[test]
    fn new_image_with_dimensions() {
        let mut ctx = setup_ctx();
        let w = ctx
            .eval(Source::from_bytes("new Image(100, 50).width"))
            .unwrap();
        assert_eq!(w.as_number().unwrap() as i64, 100);
        let h = ctx
            .eval(Source::from_bytes("new Image(100, 50).height"))
            .unwrap();
        assert_eq!(h.as_number().unwrap() as i64, 50);
    }

    #[test]
    fn new_image_default_dimensions_zero() {
        let mut ctx = setup_ctx();
        let w = ctx.eval(Source::from_bytes("new Image().width")).unwrap();
        assert_eq!(w.as_number().unwrap() as i64, 0);
    }

    #[test]
    fn set_src_stores_value() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(Source::from_bytes(
                "(function(){ var i = new Image(); i.src = 'x.png'; return i.src; })()",
            ))
            .unwrap();
        assert_eq!(str_result(r), "x.png");
    }

    #[test]
    fn complete_false_before_src_set() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(Source::from_bytes("new Image().complete"))
            .unwrap();
        assert!(!r.as_boolean().unwrap());
    }

    #[test]
    fn empty_src_stored_but_no_load() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(Source::from_bytes(
                "(function(){ var i = new Image(); i.src = ''; return i.src; })()",
            ))
            .unwrap();
        assert_eq!(str_result(r), "");
    }

    #[test]
    fn onload_and_onerror_assignable() {
        let mut ctx = setup_ctx();
        let ty = ctx
            .eval(Source::from_bytes(
                "(function(){ var i = new Image(); i.onload=function(){}; i.onerror=function(){}; return typeof i.onload; })()",
            ))
            .unwrap();
        assert_eq!(str_result(ty), "function");
    }
}
