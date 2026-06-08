//! M28.3: 注入 `document` 全局对象（对齐 W3C/Chrome 基础子集）。
//!
//! 百度等 SPA 实际用的 document API（实测频率降序）：
//! ```js
//! document.cookie              // 读 cookie 字符串（反爬/登录态检测）
//! document.body                // <body> 的 NodeId（window 默认）
//! document.head                // <head> 的 NodeId
//! document.title               // 读标题
//! document.getElementById(id)  // 包装 __getElById
//! document.querySelector(sel)  // 包装 __qs
//! document.createElement(tag)  // 包装 __createEl
//! document.getElementsByTagName(tag)
//! document.createTextNode(text)
//! document.location            // 指向 location 全局对象
//! document.documentElement     // <html> 的 NodeId
//! ```
//!
//! ## 设计决策：纯 JS 对象（非 NativeFunction）
//!
//! 遵循 M28.1/M28.2 + memory 教训（boa native fn 跨 eval 边界 this 不可靠），
//! 用纯 JS 对象字面量构造。方法内部直接 eval `__*` 桥（同 xhr/fetch/ws shim 模式）。
//! 数据属性（body/head/title/cookie）用 getter eval `__getBody` 等。
//!
//! 注意：DOM 桥返回 NodeId（数字），document.body/head 也返回数字 ID。
//! 这是 MVP（爬虫够用），非完整 DOM Element 对象。

use boa_engine::{Context, JsResult};

/// 在 `ctx` 上注册 `document` 全局对象。
///
/// 必须在 bridge `__*` 函数和 location 全局对象安装**之后**调用。
pub fn install_document(ctx: &mut Context) -> JsResult<()> {
    // 纯 JS 对象。方法包装现有 __* 桥；getter 读 body/head/title/cookie。
    // createTextNode 返回文本节点 ID（用 __createEl 近似，tag='__text__'）。
    let js = r#"(function() {
        var d = {
            // 数据属性 getter（eval __* 拿当前 DOM 状态）
            get body() { return __getBody(); },
            get head() { return __getTag('head'); },
            get documentElement() { return __getTag('html'); },
            get title() {
                var n = __getTag('title');
                // title 文本在 __getElById 无关，简化：返回空字符串（MVP）
                return typeof n === 'number' ? '' : '';
            },
            get cookie() {
                // 防御：__getCookie 桥可能未安装（cookie 是 M15 后端，
                // 独立于 document）。用 typeof 检查避免 ReferenceError。
                return (typeof __getCookie === 'function') ? __getCookie() : '';
            },
            // location 指向全局 location 对象（M14 已安装）
            get location() { return globalThis.location; },
            get referrer() { return ''; },
            get URL() { return globalThis.location ? globalThis.location.href() : ''; },
            get domain() {
                try { return globalThis.location.host(); } catch(e) { return ''; }
            },
            get readyState() { return 'complete'; },
            get visibilityState() { return 'visible'; },
            get hidden() { return false; },
            get contentType() { return 'text/html'; },
            get characterSet() { return 'UTF-8'; },
            // 方法（包装 __* 桥）
            // M37: 返回 Element 对象（包装 NodeId），而非裸数字
            getElementById: function(id) {
                return __makeElement(__getElById(id));
            },
            querySelector: function(sel) {
                return __makeElement(__qs(sel));
            },
            createElement: function(tag) {
                return __makeElement(__createEl(tag));
            },
            getElementsByTagName: function(tag) {
                // MVP: 返回数组，含第一个匹配的 Element 或空数组
                return __makeElement(__getTag(tag)) ? [__makeElement(__getTag(tag))] : [];
            },
            createTextNode: function(text) {
                // MVP: 近似为创建一个文本节点（用 __createEl 占位）
                return __makeElement(__createEl('__text__'));
            },
            addEventListener: function() { /* no-op */ },
            removeEventListener: function() { /* no-op */ },
            write: function(html) { /* no-op for crawler */ },
        };
        globalThis.document = d;
    })();
    undefined;"#;
    ctx.eval(boa_engine::Source::from_bytes(js))?;
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
        install_document(&mut ctx).expect("install document");
        ctx
    }

    fn str_result(v: JsValue) -> String {
        v.as_string()
            .map(|s| s.to_std_string_escaped())
            .unwrap_or_default()
    }

    #[test]
    fn document_is_object() {
        let mut ctx = setup_ctx();
        let ty = ctx
            .eval(boa_engine::Source::from_bytes("typeof document"))
            .unwrap();
        assert_eq!(str_result(ty), "object");
    }

    #[test]
    fn document_get_element_by_id_wraps_bridge() {
        // 用 install_current 装真实树
        use crate::bridge::install_current;
        use browser_html_parser::parse;
        let tree = parse("<html><body><div id=\"x\">hi</div></body></html>");
        let _g = install_current(tree);
        let mut ctx = Context::default();
        install(&mut ctx);
        install_document(&mut ctx).expect("install document");
        crate::element_shim::install_element(&mut ctx).expect("install element");
        // M37: document.getElementById 现在返回 Element 对象（不是裸数字 NodeId）
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "typeof document.getElementById('x')",
            ))
            .unwrap();
        assert_eq!(str_result(r), "object");
    }

    #[test]
    fn document_create_element_wraps_bridge() {
        // 必须装真实 tree（见 document_get_element_by_id_wraps_bridge 的成功模式）
        use crate::bridge::install_current;
        use browser_html_parser::parse;
        let tree = parse("<html><body><div>x</div></body></html>");
        let _g = install_current(tree);
        let mut ctx = Context::default();
        install(&mut ctx);
        install_document(&mut ctx).expect("install document");
        crate::element_shim::install_element(&mut ctx).expect("install element");
        // M37: document.createElement 现在返回 Element 对象
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "typeof document.createElement('div')",
            ))
            .unwrap();
        assert_eq!(str_result(r), "object");
    }

    #[test]
    fn document_query_selector_wraps_bridge() {
        use crate::bridge::install_current;
        use browser_html_parser::parse;
        let tree = parse("<html><body><div>x</div></body></html>");
        let _g = install_current(tree);
        let mut ctx = Context::default();
        install(&mut ctx);
        install_document(&mut ctx).expect("install document");
        crate::element_shim::install_element(&mut ctx).expect("install element");

        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "typeof document.querySelector('div')",
            ))
            .unwrap();
        // M37: querySelector 现在返回 Element 对象（非裸数字）
        assert_eq!(str_result(r), "object");
    }

    #[test]
    fn document_ready_state_complete() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("document.readyState"))
            .unwrap();
        assert_eq!(str_result(r), "complete");
    }

    #[test]
    fn document_visibility_state_visible() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("document.visibilityState"))
            .unwrap();
        assert_eq!(str_result(r), "visible");
    }

    #[test]
    fn document_hidden_is_false() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("document.hidden"))
            .unwrap();
        assert!(!r.as_boolean().unwrap());
    }

    #[test]
    fn document_content_type_text_html() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("document.contentType"))
            .unwrap();
        assert_eq!(str_result(r), "text/html");
    }

    #[test]
    fn document_character_set_utf8() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("document.characterSet"))
            .unwrap();
        assert_eq!(str_result(r), "UTF-8");
    }

    #[test]
    fn document_add_event_listener_is_noop() {
        let mut ctx = setup_ctx();
        // 不应抛异常
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "document.addEventListener('click', function(){}); 'ok'",
            ))
            .unwrap();
        assert_eq!(str_result(r), "ok");
    }
}
