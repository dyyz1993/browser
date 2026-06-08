//! M37: 完整 JS Element 对象（包装 NodeId + setter/getter 反射到 bridge）。
//!
//! ## 背景
//!
//! M28.3 的 document_shim 让 `getElementById()` 返回裸数字 NodeId。
//! 导致 `el.textContent = 'X'` 对数字设属性（no-op），`el.appendChild()`
//! 报错——SPA 的 DOM 写操作完全断裂。
//!
//! ## 设计
//!
//! 纯 JS `Element` constructor（遵循 memory 教训：boa native fn 跨 eval
//! 边界 this 不可靠，纯 JS 原型更稳）。`document.getElementById` 等方法
//! 返回 `new Element(nodeId)` 而非裸数字。
//!
//! 属性 setter/getter 通过 `Object.defineProperty` 定义（JS 级别 accessor，
//! `this` 绑定可靠），反射到 bridge 的 `__setText`/`__getText`/`__setAttr` 等。

use boa_engine::{Context, JsResult, Source};

/// 在 `ctx` 上注册 `Element` constructor + `__makeElement` 工厂。
///
/// 必须在 bridge `__*` 函数安装**之后**调用。
pub fn install_element(ctx: &mut Context) -> JsResult<()> {
    let js = r#"(function() {
        // Element constructor: 包装 NodeId 为完整 DOM Element 对象。
        function Element(nodeId) {
            this.__nodeId = nodeId;
        }

        // ── textContent: getter/setter 反射到 __getText / __setText ──
        Object.defineProperty(Element.prototype, 'textContent', {
            get: function() { return __getText(this.__nodeId); },
            set: function(v) { __setText(this.__nodeId, String(v)); },
            enumerable: true, configurable: true,
        });

        // ── id: getter/setter 反射到 __getAttr / __setAttr ──
        Object.defineProperty(Element.prototype, 'id', {
            get: function() {
                return (typeof __getAttr === 'function')
                    ? __getAttr(this.__nodeId, 'id') : '';
            },
            set: function(v) { __setAttr(this.__nodeId, 'id', String(v)); },
            enumerable: true, configurable: true,
        });

        // ── tagName: getter（只读，大写）──
        Object.defineProperty(Element.prototype, 'tagName', {
            get: function() {
                return (typeof __getTagName === 'function')
                    ? String(__getTagName(this.__nodeId)).toUpperCase() : '';
            },
            enumerable: true, configurable: true,
        });

        // ── innerHTML: MVP 简化为 textContent（爬虫够用）──
        Object.defineProperty(Element.prototype, 'innerHTML', {
            get: function() { return __getText(this.__nodeId); },
            set: function(v) { __setText(this.__nodeId, String(v)); },
            enumerable: true, configurable: true,
        });

        // ── 方法 ──
        Element.prototype.appendChild = function(child) {
            if (child && typeof child.__nodeId === 'number') {
                __appendChild(this.__nodeId, child.__nodeId);
            }
            return child;
        };
        Element.prototype.setAttribute = function(key, value) {
            __setAttr(this.__nodeId, String(key), String(value));
        };
        Element.prototype.getElementById = function(id) {
            var nid = (typeof __findChild === 'function')
                ? __findChild(this.__nodeId, id) : undefined;
            return (typeof nid === 'number') ? new Element(nid) : null;
        };
        Element.prototype.addEventListener = function() {};
        Element.prototype.removeEventListener = function() {};

        // __makeElement 工厂：包装 bridge 返回的 NodeId。
        globalThis.__makeElement = function(nodeId) {
            if (typeof nodeId === 'number' && nodeId >= 0) {
                return new Element(nodeId);
            }
            return null;
        };

        globalThis.Element = Element;
    })();
    undefined;"#;
    ctx.eval(Source::from_bytes(js))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{install, install_current, TreeGuard};
    use boa_engine::JsValue;
    use browser_html_parser::parse;

    fn str_result(v: JsValue) -> String {
        v.as_string()
            .map(|s| s.to_std_string_escaped())
            .unwrap_or_default()
    }

    /// 装真实树 + bridge + document + Element。
    /// 返回 `(Context, TreeGuard)` —— guard 必须保持存活到 eval 结束，
    /// 否则 Drop 会清空 thread-local tree（导致 `with_tree` panic）。
    fn setup(html: &str) -> (Context, TreeGuard) {
        let tree = parse(html);
        let (_shared, guard) = install_current(tree);
        let mut ctx = Context::default();
        install(&mut ctx);
        crate::document_shim::install_document(&mut ctx).expect("install document");
        install_element(&mut ctx).expect("install element");
        (ctx, guard)
    }

    #[test]
    fn element_is_constructor() {
        let (mut ctx, _g) = setup("<html><body><div>x</div></body></html>");
        let ty = ctx.eval(Source::from_bytes("typeof Element")).unwrap();
        assert_eq!(str_result(ty), "function");
    }

    #[test]
    fn get_element_by_id_returns_element_not_number() {
        let (mut ctx, _g) = setup("<html><body><p id=\"t1\">ORIGINAL</p></body></html>");
        let ty = ctx
            .eval(Source::from_bytes("typeof document.getElementById('t1')"))
            .unwrap();
        assert_eq!(str_result(ty), "object");
    }

    #[test]
    fn text_content_getter_reads_text() {
        let (mut ctx, _g) = setup("<html><body><p id=\"t1\">HELLO</p></body></html>");
        let r = ctx
            .eval(Source::from_bytes(
                "document.getElementById('t1').textContent",
            ))
            .unwrap();
        assert_eq!(str_result(r), "HELLO");
    }

    #[test]
    fn text_content_setter_writes_text() {
        let (mut ctx, _g) = setup("<html><body><p id=\"t1\">OLD</p></body></html>");
        ctx.eval(Source::from_bytes(
            "document.getElementById('t1').textContent = 'NEW';",
        ))
        .unwrap();
        let r = ctx
            .eval(Source::from_bytes(
                "document.getElementById('t1').textContent",
            ))
            .unwrap();
        assert_eq!(str_result(r), "NEW");
    }

    #[test]
    fn create_element_returns_element() {
        let (mut ctx, _g) = setup("<html><body></body></html>");
        ctx.eval(Source::from_bytes(
            "var el = document.createElement('div'); el.textContent = 'CREATED';",
        ))
        .unwrap();
        let ty = ctx.eval(Source::from_bytes("typeof el")).unwrap();
        assert_eq!(str_result(ty), "object");
        // 验证文本确实写入了 DOM tree
        let r = ctx.eval(Source::from_bytes("el.textContent")).unwrap();
        assert_eq!(str_result(r), "CREATED");
    }

    #[test]
    fn append_child_attaches_element() {
        let (mut ctx, _g) = setup("<html><body><div id=\"parent\"></div></body></html>");
        ctx.eval(Source::from_bytes(
            "var child = document.createElement('p');\
             child.textContent = 'CHILD';\
             document.getElementById('parent').appendChild(child);",
        ))
        .unwrap();
        let r = ctx
            .eval(Source::from_bytes(
                "document.getElementById('parent').textContent",
            ))
            .unwrap();
        assert_eq!(str_result(r), "CHILD");
    }

    #[test]
    fn set_attribute_works() {
        let (mut ctx, _g) = setup("<html><body><p id=\"t1\">x</p></body></html>");
        ctx.eval(Source::from_bytes(
            "document.getElementById('t1').setAttribute('class', 'highlight');",
        ))
        .unwrap();
        let r = ctx
            .eval(Source::from_bytes(
                "__getAttr(document.getElementById('t1').__nodeId, 'class')",
            ))
            .unwrap();
        assert_eq!(str_result(r), "highlight");
    }

    #[test]
    fn get_element_by_id_missing_returns_null() {
        let (mut ctx, _g) = setup("<html><body></body></html>");
        let r = ctx
            .eval(Source::from_bytes("document.getElementById('nonexistent')"))
            .unwrap();
        assert!(r.is_null() || r.is_undefined());
    }

    #[test]
    fn tag_name_getter_works() {
        let (mut ctx, _g) = setup("<html><body><p id=\"t1\">x</p></body></html>");
        let r = ctx
            .eval(Source::from_bytes("document.getElementById('t1').tagName"))
            .unwrap();
        assert_eq!(str_result(r), "P");
    }
}
