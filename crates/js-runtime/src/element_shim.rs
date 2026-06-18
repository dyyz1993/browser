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
        function __attrGet(nodeId, key) {
            return (typeof __getAttr === 'function') ? __getAttr(nodeId, key) : null;
        }
        function __attrSet(nodeId, key, value) {
            if (typeof __setAttr === 'function') {
                __setAttr(nodeId, key, String(value));
            }
        }
        function __attrRemove(nodeId, key) {
            if (typeof __removeAttr === 'function') {
                __removeAttr(nodeId, key);
            }
        }
        function __dataAttrName(key) {
            return 'data-' + String(key).replace(/[A-Z]/g, function(ch) {
                return '-' + ch.toLowerCase();
            });
        }
        function __readStyleProp(nodeId, prop) {
            var style = __attrGet(nodeId, 'style');
            if (typeof style !== 'string' || !style) return '';
            var parts = style.split(';');
            for (var i = 0; i < parts.length; i++) {
                var pair = parts[i].split(':');
                if (pair.length < 2) continue;
                var key = pair[0].trim();
                if (key === prop) {
                    return pair.slice(1).join(':').trim();
                }
            }
            return '';
        }
        function __writeStyleProp(nodeId, prop, value) {
            var style = __attrGet(nodeId, 'style');
            var text = (typeof style === 'string' && style) ? style : '';
            var parts = text ? text.split(';') : [];
            var out = [];
            var found = false;
            for (var i = 0; i < parts.length; i++) {
                var raw = parts[i].trim();
                if (!raw) continue;
                var pair = raw.split(':');
                if (pair.length < 2) continue;
                var key = pair[0].trim();
                var val = pair.slice(1).join(':').trim();
                if (key === prop) {
                    found = true;
                    if (value !== '') out.push(prop + ':' + value);
                } else {
                    out.push(key + ':' + val);
                }
            }
            if (!found && value !== '') {
                out.push(prop + ':' + value);
            }
            __attrSet(nodeId, 'style', out.join(';'));
        }
        function __childListToElements(rawIds) {
            if (!rawIds) return [];
            var parts = String(rawIds).split(',');
            var out = [];
            for (var i = 0; i < parts.length; i++) {
                var n = Number(parts[i]);
                if (isFinite(n) && n >= 0) {
                    out.push(__makeElement(n));
                }
            }
            return out;
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
                return __attrGet(this.__nodeId, 'id') || '';
            },
            set: function(v) { __attrSet(this.__nodeId, 'id', v); },
            enumerable: true, configurable: true,
        });

        // ── tagName: getter（只读，大写）──
        // M62: nodeType（React/Vue DOM 元素验证核心：el.nodeType === 1）。
        Object.defineProperty(Element.prototype, 'nodeType', {
            get: function() { return 1; }
        });

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
        Object.defineProperty(Element.prototype, 'parentNode', {
            get: function() {
                var parentId = (typeof __getParent === 'function') ? __getParent(this.__nodeId) : undefined;
                return __makeElement(parentId);
            },
            enumerable: true, configurable: true,
        });
        Object.defineProperty(Element.prototype, 'parentElement', {
            get: function() {
                var parentId = (typeof __getParent === 'function') ? __getParent(this.__nodeId) : undefined;
                return __makeElement(parentId);
            },
            enumerable: true, configurable: true,
        });
        Object.defineProperty(Element.prototype, 'children', {
            get: function() {
                var raw = (typeof __children === 'function') ? __children(this.__nodeId) : '';
                return __childListToElements(raw);
            },
            enumerable: true, configurable: true,
        });
        Object.defineProperty(Element.prototype, 'childNodes', {
            get: function() {
                return this.children;
            },
            enumerable: true, configurable: true,
        });
        Object.defineProperty(Element.prototype, 'firstChild', {
            get: function() {
                return this.children.length > 0 ? this.children[0] : null;
            },
            enumerable: true, configurable: true,
        });
        Object.defineProperty(Element.prototype, 'lastChild', {
            get: function() {
                return this.children.length > 0 ? this.children[this.children.length - 1] : null;
            },
            enumerable: true, configurable: true,
        });
        // M62: classList 真实现（之前 no-op）。框架 class 切换需要。
        Object.defineProperty(Element.prototype, 'classList', {
            get: function() {
                var nodeId = this.__nodeId;
                return {
                    add: function() {
                        var cls = __attrGet(nodeId, 'class') || '';
                        var parts = cls ? cls.split(/\s+/) : [];
                        for (var i = 0; i < arguments.length; i++) {
                            var c = String(arguments[i]);
                            if (parts.indexOf(c) < 0) parts.push(c);
                        }
                        __attrSet(nodeId, 'class', parts.join(' '));
                    },
                    remove: function() {
                        var cls = __attrGet(nodeId, 'class') || '';
                        var parts = cls ? cls.split(/\s+/) : [];
                        for (var i = 0; i < arguments.length; i++) {
                            var idx = parts.indexOf(String(arguments[i]));
                            if (idx >= 0) parts.splice(idx, 1);
                        }
                        __attrSet(nodeId, 'class', parts.join(' '));
                    },
                    contains: function(c) {
                        var cls = __attrGet(nodeId, 'class') || '';
                        return cls ? cls.split(/\s+/).indexOf(String(c)) >= 0 : false;
                    },
                    toggle: function(c, force) {
                        var has = this.contains(c);
                        if (has && force !== true) { this.remove(c); return false; }
                        if (!has && force !== false) { this.add(c); return true; }
                        return has;
                    }
                };
            },
            enumerable: true, configurable: true,
        });
        // M62: dataset 动态遍历常见 data-* key（之前只硬编码 2 个）。
        Object.defineProperty(Element.prototype, 'dataset', {
            get: function() {
                var nodeId = this.__nodeId;
                var ds = {};
                // M62: 驼峰 → kebab-case（dplId → data-dpl-id，dataset 标准）。
                function dataAttrName(name) {
                    return 'data-' + name.replace(/([A-Z])/g, '-$1').toLowerCase();
                }
                function defineDatasetProp(name) {
                    Object.defineProperty(ds, name, {
                        get: function() {
                            var v = __attrGet(nodeId, dataAttrName(name));
                            return (v === null || v === undefined) ? undefined : String(v);
                        },
                        set: function(v) { __attrSet(nodeId, dataAttrName(name), v); },
                        enumerable: true, configurable: true
                    });
                }
                var commonKeys = ['id', 'index', 'url', 'src', 'type', 'name', 'value', 'target',
                    'action', 'method', 'controller', 'dplId', 'scrollBehavior', 'reactRoot'];
                for (var i = 0; i < commonKeys.length; i++) defineDatasetProp(commonKeys[i]);
                return ds;
            },
            enumerable: true, configurable: true,
        });
        Object.defineProperty(Element.prototype, 'style', {
            get: function() {
                var nodeId = this.__nodeId;
                var style = {
                    get cssText() {
                        var v = __attrGet(nodeId, 'style');
                        return (typeof v === 'string') ? v : '';
                    },
                    set cssText(v) {
                        __attrSet(nodeId, 'style', v || '');
                    },
                    getPropertyValue: function(name) {
                        return __readStyleProp(nodeId, String(name));
                    },
                    setProperty: function(name, value) {
                        __writeStyleProp(nodeId, String(name), String(value));
                    },
                    removeProperty: function(name) {
                        var old = __readStyleProp(nodeId, String(name));
                        __writeStyleProp(nodeId, String(name), '');
                        return old;
                    }
                };
                Object.defineProperty(style, 'scrollBehavior', {
                    get: function() { return __readStyleProp(nodeId, 'scroll-behavior'); },
                    set: function(v) { __writeStyleProp(nodeId, 'scroll-behavior', String(v)); },
                    enumerable: true,
                    configurable: true
                });
                return style;
            },
            enumerable: true, configurable: true,
        });
        Object.defineProperty(Element.prototype, 'nonce', {
            get: function() {
                var v = __attrGet(this.__nodeId, 'nonce');
                return (v === null || v === undefined) ? '' : String(v);
            },
            set: function(v) { __attrSet(this.__nodeId, 'nonce', v); },
            enumerable: true, configurable: true,
        });
        Object.defineProperty(Element.prototype, 'nodeName', {
            get: function() { return this.tagName; },
            enumerable: true, configurable: true,
        });
        Object.defineProperty(Element.prototype, 'ownerDocument', {
            get: function() { return globalThis.document || null; },
            enumerable: true, configurable: true,
        });

        // ── 方法 ──
        Element.prototype.appendChild = function(child) {
            if (child && typeof child.__nodeId === 'number') {
                __appendChild(this.__nodeId, child.__nodeId);
            }
            return child;
        };
        Element.prototype.insertBefore = function(child, reference) {
            if (child && typeof child.__nodeId === 'number') {
                if (typeof __insertBefore === 'function') {
                    var refId = (reference && typeof reference.__nodeId === 'number')
                        ? reference.__nodeId : undefined;
                    __insertBefore(this.__nodeId, child.__nodeId, refId);
                } else {
                    __appendChild(this.__nodeId, child.__nodeId);
                }
            }
            return child;
        };
        Element.prototype.setAttribute = function(key, value) {
            __attrSet(this.__nodeId, String(key), value);
        };
        Element.prototype.getAttribute = function(key) {
            var v = __attrGet(this.__nodeId, String(key));
            return (v === null || v === undefined) ? null : String(v);
        };
        Element.prototype.removeAttribute = function(key) {
            __attrRemove(this.__nodeId, String(key));
        };
        Element.prototype.getElementById = function(id) {
            var nid = (typeof __findChild === 'function')
                ? __findChild(this.__nodeId, id) : undefined;
            return (typeof nid === 'number') ? new Element(nid) : null;
        };
        // M62: Element 级 querySelectorAll（之前写死返回 []）。
        // 注：Element 级搜索需要从该元素子树开始，简化版仍从 document 根搜索。
        Element.prototype.querySelectorAll = function(sel) {
            if (!sel) return [];
            var ids = (typeof __qsAll === 'function') ? __qsAll(sel) : [];
            var out = [];
            for (var i = 0; i < ids.length; i++) {
                if (typeof ids[i] === 'number' && ids[i] >= 0) {
                    out.push(__makeElement(ids[i]));
                }
            }
            return out;
        };
        Element.prototype.addEventListener = function(type, cb) {
            if (!this.__listeners) this.__listeners = {};
            if (!this.__listeners[type]) this.__listeners[type] = [];
            this.__listeners[type].push(cb);
        };
        Element.prototype.removeEventListener = function(type, cb) {
            if (!this.__listeners || !this.__listeners[type]) return;
            this.__listeners[type] = this.__listeners[type].filter(function(f) { return f !== cb; });
        };
        Element.prototype.dispatchEvent = function(ev) {
            if (!this.__listeners || !ev || !this.__listeners[ev.type]) return true;
            var cbs = this.__listeners[ev.type];
            ev.target = this; ev.currentTarget = this;
            for (var i = 0; i < cbs.length; i++) { try { cbs[i](ev); } catch(e) {} }
            return true;
        };
        Element.prototype.scrollIntoView = function() {};
        Element.prototype.getClientRects = function() { return []; };
        Element.prototype.cloneNode = function() {
            var copy = __makeElement(__createEl(String(this.tagName || '').toLowerCase()));
            if (!copy) return null;
            if (this.id) copy.id = this.id;
            if (this.nonce) copy.nonce = this.nonce;
            var styleText = this.getAttribute('style');
            if (styleText) copy.setAttribute('style', styleText);
            var className = this.getAttribute('class');
            if (className) copy.setAttribute('class', className);
            var text = this.textContent;
            if (text) copy.textContent = text;
            return copy;
        };
        Element.prototype.isEqualNode = function(other) {
            if (!other || typeof other.__nodeId !== 'number') return false;
            return this.tagName === other.tagName
                && this.textContent === other.textContent
                && this.getAttribute('nonce') === other.getAttribute('nonce')
                && this.getAttribute('style') === other.getAttribute('style')
                && this.getAttribute('class') === other.getAttribute('class');
        };
        Element.prototype.remove = function() {
            var parent = __getParent(this.__nodeId);
            if (typeof parent === 'number' && typeof __removeChild === 'function') {
                __removeChild(parent, this.__nodeId);
            }
        };
        Element.prototype.removeChild = function(child) {
            if (child && typeof child.__nodeId === 'number' && typeof __removeChild === 'function') {
                __removeChild(this.__nodeId, child.__nodeId);
                return child;
            }
            return null;
        };

        // __makeElement 工厂：包装 bridge 返回的 NodeId。
        globalThis.__makeElement = function(nodeId) {
            if (typeof nodeId === 'number' && nodeId >= 0) {
                return new Element(nodeId);
            }
            return null;
        };

        globalThis.Element = Element;
        globalThis.HTMLElement = Element;
        globalThis.Node = Element;
        // M62: Node 常量（React/Vue 用 nodeType === Node.ELEMENT_NODE 验证 DOM 元素）。
        globalThis.Node.ELEMENT_NODE = 1;
        globalThis.Node.TEXT_NODE = 3;
        globalThis.Node.DOCUMENT_NODE = 9;
        globalThis.Node.DOCUMENT_FRAGMENT_NODE = 11;
        globalThis.Node.COMMENT_NODE = 8;
        // M62: 补 HTML 具体元素子类（框架 instanceof 检查需要）。
        // 我们的 DOM 模型不区分子类型，全部等价于 Element。
        globalThis.HTMLScriptElement = Element;
        globalThis.HTMLDivElement = Element;
        globalThis.HTMLAnchorElement = Element;
        globalThis.HTMLImageElement = Element;
        globalThis.HTMLInputElement = Element;
        globalThis.HTMLFormElement = Element;
        globalThis.HTMLBodyElement = Element;
        globalThis.HTMLHeadElement = Element;
        globalThis.HTMLLinkElement = Element;
        globalThis.HTMLMetaElement = Element;
        globalThis.HTMLSpanElement = Element;
        globalThis.HTMLParagraphElement = Element;
        globalThis.HTMLUListElement = Element;
        globalThis.HTMLLIElement = Element;
        globalThis.HTMLTableElement = Element;
        globalThis.HTMLTableRowElement = Element;
        globalThis.HTMLTableCellElement = Element;
        globalThis.HTMLButtonElement = Element;
        globalThis.HTMLCanvasElement = Element;
        globalThis.HTMLSelectElement = Element;
        globalThis.HTMLTextAreaElement = Element;
        globalThis.HTMLHeadingElement = Element;
        globalThis.HTMLLabelElement = Element;
        globalThis.HTMLOptionElement = Element;
        globalThis.HTMLFieldSetElement = Element;
        globalThis.HTMLLegendElement = Element;
        globalThis.HTMLDataListElement = Element;
        globalThis.HTMLOutputElement = Element;
        globalThis.HTMLProgressElement = Element;
        globalThis.HTMLMeterElement = Element;
        globalThis.HTMLDetailsElement = Element;
        globalThis.HTMLSummaryElement = Element;
        globalThis.HTMLDialogElement = Element;
        globalThis.HTMLPictureElement = Element;
        globalThis.HTMLSourceElement = Element;
        globalThis.HTMLTrackElement = Element;
        globalThis.HTMLMediaElement = Element;
        globalThis.HTMLVideoElement = Element;
        globalThis.HTMLAudioElement = Element;
        globalThis.HTMLIFrameElement = Element;
        globalThis.HTMLEmbedElement = Element;
        globalThis.HTMLObjectElement = Element;
        globalThis.HTMLParamElement = Element;
        globalThis.HTMLMapElement = Element;
        globalThis.HTMLAreaElement = Element;
        globalThis.HTMLStyleElement = Element;
        globalThis.HTMLBaseElement = Element;
        globalThis.HTMLTitleElement = Element;
        globalThis.HTMLTemplateElement = Element;
        globalThis.HTMLSlotElement = Element;
        globalThis.HTMLOListElement = Element;
        globalThis.HTMLDirectoryElement = Element;
        globalThis.HTMLFontElement = Element;
        globalThis.HTMLFrameSetElement = Element;
        globalThis.HTMLFrameElement = Element;
        globalThis.HTMLMarqueeElement = Element;
        globalThis.HTMLBRElement = Element;
        globalThis.HTMLHRElement = Element;
        globalThis.HTMLUnknownElement = Element;
        globalThis.HTMLDataElement = Element;
        globalThis.HTMLTimeElement = Element;
        globalThis.HTMLWBRElement = Element;
        // SVG 元素（框架可能引用）
        globalThis.SVGElement = Element;
        globalThis.SVGSVGElement = Element;
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

    #[test]
    fn dataset_reads_data_attributes() {
        let (mut ctx, _g) =
            setup("<html data-dpl-id=\"abc\"><body><div id=\"t1\">X</div></body></html>");
        let r = ctx
            .eval(Source::from_bytes("document.documentElement.dataset.dplId"))
            .unwrap();
        assert_eq!(str_result(r), "abc");
    }

    #[test]
    fn style_scroll_behavior_round_trips() {
        let (mut ctx, _g) = setup("<html><body><div id=\"t1\"></div></body></html>");
        ctx.eval(Source::from_bytes(
            "document.getElementById('t1').style.scrollBehavior = 'smooth';",
        ))
        .unwrap();
        let r = ctx
            .eval(Source::from_bytes(
                "document.getElementById('t1').style.scrollBehavior",
            ))
            .unwrap();
        assert_eq!(str_result(r), "smooth");
    }
}
