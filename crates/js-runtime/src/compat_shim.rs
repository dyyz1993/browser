//! 基础兼容 shim（爬虫场景优先）
//!
//! 目标：在不增加完整 JS 引擎语义的前提下，降低常见网站脚本在
//! `globalThis` 上查不到入口对象导致的基础报错。

use boa_engine::{Context, JsResult, Source};

/// 在 `ctx` 上补齐基础浏览器兼容入口，覆盖常见前端库假设（最小集合）。
///
/// - `require`：常见打包产物/UMD 入口兜底。
/// - `define`：AMD 兼容最小实现（不执行 loader 语义，只避免 ReferenceError）。
/// - `$`/`jQuery`：常见模板/组件库依赖。
/// - `window.console`：若基础运行时未自带控制台，提供最小 no-op 实现。
pub fn install_compat_shims(ctx: &mut Context) -> JsResult<()> {
    let js = r#"(function() {
        function __isElement(v) {
            return v && typeof v === 'object' && typeof v.__nodeId === 'number';
        }

        function __coerceNodeList(v) {
            var out = [];
            if (!v) {
                return out;
            }
            if (Array.isArray(v)) {
                for (var i = 0; i < v.length; i++) {
                    if (__isElement(v[i])) {
                        out.push(v[i]);
                    }
                }
                return out;
            }
            if (__isElement(v)) {
                return [v];
            }
            return out;
        }

        function __readClassList(node) {
            if (!__isElement(node)) {
                return [];
            }
            if (typeof globalThis.__getAttr === 'function') {
                var c = __getAttr(node.__nodeId, 'class');
                if (typeof c === 'string') {
                    return c.trim().split(/\\s+/).filter(function(s) { return s.length > 0; });
                }
            }
            return [];
        }

        function __setClassList(node, cls) {
            if (!__isElement(node) || !Array.isArray(cls)) {
                return;
            }
            if (typeof globalThis.__setAttr === 'function') {
                __setAttr(node.__nodeId, 'class', cls.join(' '));
            }
        }

        function __serializeStyle(raw) {
            if (!raw || typeof raw !== 'object') {
                return String(raw == null ? '' : raw);
            }
            if (typeof raw === 'string') {
                return raw;
            }
            var parts = [];
            for (var k in raw) {
                if (Object.prototype.hasOwnProperty.call(raw, k)) {
                    var v = raw[k];
                    if (v === undefined || v === null || v === false) {
                        continue;
                    }
                    parts.push(k + ':' + v);
                }
            }
            return parts.join(';');
        }

        function __makeNodesWrapper(nodes) {
            var list = __coerceNodeList(nodes);
            var wrapper = {
                __nodes: list,
                length: list.length,
                get: function(i) {
                    return list[i];
                },
                each: function(cb) {
                    if (typeof cb === 'function') {
                        for (var i = 0; i < list.length; i++) {
                            cb.call(list[i], i, list[i]);
                        }
                    }
                    return wrapper;
                },
                ready: function(cb) {
                    if (typeof cb === 'function') {
                        cb();
                    }
                    return wrapper;
                },
                eq: function(i) {
                    if (i < 0) {
                        i = list.length + i;
                    }
                    if (i < 0 || i >= list.length) {
                        return __makeNodesWrapper([]);
                    }
                    return __makeNodesWrapper([list[i]]);
                },
                first: function() {
                    return wrapper.eq(0);
                },
                last: function() {
                    return wrapper.eq(list.length - 1);
                },
                find: function() {
                    if (!globalThis.document || typeof globalThis.document.querySelector !== 'function') {
                        return __makeNodesWrapper([]);
                    }
                    var selector = arguments[0];
                    if (!selector || list.length === 0) {
                        return __makeNodesWrapper([]);
                    }
                    try {
                        if (typeof selector === 'string') {
                            return __makeNodesWrapper(globalThis.document.querySelector(selector));
                        }
                    } catch (e) {}
                    return __makeNodesWrapper([]);
                },
                html: function(v) {
                    if (arguments.length === 0) {
                        return (list.length > 0 && __isElement(list[0]) && typeof globalThis.__getText === 'function')
                            ? __getText(list[0].__nodeId)
                            : '';
                    }
                    for (var i = 0; i < list.length; i++) {
                        if (__isElement(list[i]) && typeof globalThis.__setText === 'function') {
                            __setText(list[i].__nodeId, String(v));
                        }
                    }
                    return wrapper;
                },
                text: function(v) {
                    return wrapper.html(v);
                },
                val: function(v) {
                    if (arguments.length === 0) {
                        return (list.length > 0 && __isElement(list[0]) && typeof globalThis.__getAttr === 'function')
                            ? __getAttr(list[0].__nodeId, 'value')
                            : '';
                    }
                    for (var i = 0; i < list.length; i++) {
                        if (__isElement(list[i]) && typeof globalThis.__setAttr === 'function') {
                            __setAttr(list[i].__nodeId, 'value', String(v));
                        }
                    }
                    return wrapper;
                },
                attr: function(name, value) {
                    if (list.length === 0 || !name) {
                        if (arguments.length > 1) {
                            return wrapper;
                        }
                        return '';
                    }
                    if (typeof name === 'object' && name && arguments.length === 1) {
                        for (var key in name) {
                            if (Object.prototype.hasOwnProperty.call(name, key)) {
                                wrapper.attr(key, name[key]);
                            }
                        }
                        return wrapper;
                    }
                    if (arguments.length === 1) {
                        if (typeof globalThis.__getAttr !== 'function') {
                            return '';
                        }
                        var node = list[0];
                        return __isElement(node) ? __getAttr(node.__nodeId, String(name)) : '';
                    }
                    for (var i = 0; i < list.length; i++) {
                        if (__isElement(list[i])) {
                            __setAttr(list[i].__nodeId, String(name), String(value));
                        }
                    }
                    return wrapper;
                },
                css: function(name, value) {
                    if (list.length === 0 || typeof globalThis.__setAttr !== 'function' || typeof globalThis.__getAttr !== 'function') {
                        return wrapper;
                    }
                    if (typeof name === 'object') {
                        var styleText = __serializeStyle(name);
                        for (var i = 0; i < list.length; i++) {
                            if (__isElement(list[i])) {
                                __setAttr(list[i].__nodeId, 'style', styleText);
                            }
                        }
                        return wrapper;
                    }
                    if (arguments.length === 1) {
                        var n0 = list[0];
                        if (!__isElement(n0)) return '';
                        var style = __getAttr(n0.__nodeId, 'style');
                        if (!style) return '';
                        if (typeof style !== 'string') return '';
                        var i = style.indexOf(name + ':');
                        if (i < 0) return '';
                        var part = style.slice(i + name.length + 1).split(';')[0];
                        return part.trim();
                    }
                    for (var j = 0; j < list.length; j++) {
                        if (__isElement(list[j])) {
                            var current = __getAttr(list[j].__nodeId, 'style');
                            var currentStr = (typeof current === 'string') ? current : '';
                            if (currentStr && currentStr.length > 0 && currentStr.slice(-1) !== ';') {
                                currentStr += ';';
                            }
                            __setAttr(list[j].__nodeId, 'style', currentStr + String(name) + ':' + String(value));
                        }
                    }
                    return wrapper;
                },
                addClass: function(name) {
                    if (!name || list.length === 0) return wrapper;
                    for (var i = 0; i < list.length; i++) {
                        var set = __readClassList(list[i]);
                        if (set.indexOf(name) === -1) set.push(name);
                        __setClassList(list[i], set);
                    }
                    return wrapper;
                },
                removeClass: function(name) {
                    if (list.length === 0) return wrapper;
                    for (var i = 0; i < list.length; i++) {
                        if (!name) {
                            __setClassList(list[i], []);
                            continue;
                        }
                        var set = __readClassList(list[i]);
                        var out = [];
                        for (var j = 0; j < set.length; j++) {
                            if (set[j] !== name) out.push(set[j]);
                        }
                        __setClassList(list[i], out);
                    }
                    return wrapper;
                },
                hasClass: function(name) {
                    if (!name || list.length === 0) return false;
                    if (list.length === 0) return false;
                    var set = __readClassList(list[0]);
                    for (var i = 0; i < set.length; i++) {
                        if (set[i] === name) return true;
                    }
                    return false;
                },
                toggleClass: function(name) {
                    if (!name || list.length === 0) return wrapper;
                    for (var i = 0; i < list.length; i++) {
                        var set = __readClassList(list[i]);
                        var found = false;
                        var out = [];
                        for (var j = 0; j < set.length; j++) {
                            if (set[j] === name) {
                                found = true;
                                continue;
                            }
                            out.push(set[j]);
                        }
                        if (!found) {
                            out.push(name);
                        }
                        __setClassList(list[i], out);
                    }
                    return wrapper;
                },
                append: function(v) {
                    if (list.length === 0) return wrapper;
                    if (typeof v === 'string') {
                        for (var i = 0; i < list.length; i++) {
                            if (__isElement(list[i]) && typeof globalThis.__setText === 'function') {
                                __setText(list[i].__nodeId, String(v));
                            }
                        }
                        return wrapper;
                    }
                    var childList = __coerceNodeList(v);
                    if (childList.length === 0) {
                        return wrapper;
                    }
                    if (typeof globalThis.__appendChild === 'function') {
                        for (var a = 0; a < list.length; a++) {
                            if (!__isElement(list[a])) {
                                continue;
                            }
                            for (var b = 0; b < childList.length; b++) {
                                __appendChild(list[a].__nodeId, childList[b].__nodeId);
                            }
                        }
                    }
                    return wrapper;
                },
                appendTo: function(target) {
                    var targetList = __coerceNodeList(target);
                    if (targetList.length === 0) return wrapper;
                    for (var i = 0; i < targetList.length; i++) {
                        var host = targetList[i];
                        if (!__isElement(host) || typeof host.__nodeId !== 'number' || typeof globalThis.__appendChild !== 'function') {
                            continue;
                        }
                        for (var j = 0; j < list.length; j++) {
                            if (__isElement(list[j])) {
                                __appendChild(host.__nodeId, list[j].__nodeId);
                            }
                        }
                    }
                    return wrapper;
                },
                before: function() { return wrapper; },
                after: function() { return wrapper; },
                prepend: function() { return wrapper.append.apply(wrapper, arguments); },
                prependTo: function() { return wrapper.appendTo.apply(wrapper, arguments); },
                remove: function() {
                    if (typeof globalThis.__setAttr === 'function') {
                        for (var i = 0; i < list.length; i++) {
                            if (__isElement(list[i])) {
                                __setAttr(list[i].__nodeId, 'data-removed', '1');
                            }
                        }
                    }
                    return wrapper;
                },
                removeAttr: function(name) {
                    if (typeof globalThis.__setAttr !== 'function') return wrapper;
                    for (var i = 0; i < list.length; i++) {
                        if (__isElement(list[i])) {
                            __setAttr(list[i].__nodeId, String(name), '');
                        }
                    }
                    return wrapper;
                },
                on: function() {
                    return wrapper;
                },
                off: function() {
                    return wrapper;
                },
                click: function(handler) {
                    if (typeof globalThis.__click === 'function') {
                        for (var i = 0; i < list.length; i++) {
                            if (__isElement(list[i]) && list[i].__nodeId >= 0) {
                                __click(list[i].__nodeId);
                            }
                        }
                    }
                    return wrapper;
                },
                trigger: function() { return wrapper; },
                map: function(cb) {
                    var out = [];
                    wrapper.each(function(i, el) {
                        if (typeof cb === 'function') out.push(cb.call(el, i, el));
                    });
                    return out;
                },
                toArray: function() {
                    return list.slice(0);
                },
                prop: function(name, value) {
                    return wrapper.attr(name, value);
                }
            };
            for (var i = 0; i < list.length; i++) {
                wrapper[i] = list[i];
            }
            return wrapper;
        }

        function __jqFactory(selector) {
            if (typeof selector === 'function') {
                if (typeof selector === 'function') selector();
                return __makeNodesWrapper([]);
            }
            if (typeof selector === 'string') {
                if (!globalThis.document || typeof globalThis.document.querySelector !== 'function') {
                    return __makeNodesWrapper([]);
                }
                return __makeNodesWrapper([globalThis.document.querySelector(selector)]);
            }
            if (__isElement(selector)) {
                return __makeNodesWrapper([selector]);
            }
            if (Array.isArray(selector)) {
                return __makeNodesWrapper(selector);
            }
            return __makeNodesWrapper([]);
        }

        if (typeof globalThis.$ === 'function') {
            var existing = globalThis.$;
            __jqFactory = existing;
        }

        if (typeof globalThis.$ !== 'function') {
            globalThis.$ = __jqFactory;
        }

        if (typeof globalThis.jQuery === 'undefined') {
            globalThis.jQuery = globalThis.$;
        }

        var $api = globalThis.$;
        if (typeof $api.extend !== 'function') {
            $api.extend = function(target) {
                var out = target || {};
                for (var i = 1; i < arguments.length; i++) {
                    var src = arguments[i];
                    if (!src) continue;
                    for (var k in src) {
                        if (Object.prototype.hasOwnProperty.call(src, k)) {
                            out[k] = src[k];
                        }
                    }
                }
                return out;
            };
        }
        if (typeof $api.each !== 'function') {
            $api.each = function(obj, cb) {
                if (!obj) return obj;
                var list = obj;
                if (typeof obj.length === 'number' && !isNaN(obj.length)) {
                    for (var i = 0; i < obj.length; i++) {
                        if (typeof cb === 'function') cb.call(obj[i], i, obj[i]);
                    }
                    return obj;
                }
                if (typeof cb === 'function') {
                    cb.call(obj, 0, obj);
                }
                return obj;
            };
        }
        if (typeof $api.map !== 'function') {
            $api.map = function(list, cb) {
                var out = [];
                if (!list) return out;
                for (var i = 0; i < list.length; i++) {
                    out.push(cb(list[i], i));
                }
                return out;
            };
        }
        if (typeof $api.ajax !== 'function') {
            $api.ajax = function() {
                return {
                    done: function() { return this; },
                    fail: function() { return this; },
                    always: function() { return this; },
                    then: function() { return this; },
                    catch: function() { return this; },
                };
            };
        }
        if (typeof $api.get !== 'function') {
            $api.get = function() { return null; };
        }
        if (typeof $api.post !== 'function') {
            $api.post = function() { return null; };
        }
        if (typeof $api.isFunction !== 'function') {
            $api.isFunction = function(x) {
                return typeof x === 'function';
            };
        }
        if (typeof $api.inArray !== 'function') {
            $api.inArray = function(item, arr) {
                if (!arr || !arr.length) return -1;
                for (var i = 0; i < arr.length; i++) {
                    if (arr[i] === item) return i;
                }
                return -1;
            };
        }
        if (typeof $api.parseJSON !== 'function') {
            $api.parseJSON = function(s) {
                try { return JSON.parse(s); } catch (e) { return null; }
            };
        }
        if (typeof $api.makeArray !== 'function') {
            $api.makeArray = function(arr) {
                return Array.prototype.slice.call(arr || []);
            };
        }

        if (typeof globalThis.require !== 'function') {
            globalThis.require = function() {
                return {};
            };
        }
        if (typeof globalThis.define !== 'function') {
            globalThis.define = function() {
                var args = Array.prototype.slice.call(arguments);
                if (args.length > 0 && typeof args[args.length - 1] === 'function') {
                    try { args[args.length - 1](); } catch (e) {}
                }
                return {};
            };
        }

        if (typeof globalThis.console !== 'object' || !globalThis.console) {
            globalThis.console = {};
        }
        function __compatConsoleArg(arg) {
            if (typeof arg === 'string') return arg;
            if (arg instanceof Error) return arg.stack || arg.message || String(arg);
            try {
                if (typeof arg === 'object') return JSON.stringify(arg);
            } catch (e) {}
            return String(arg);
        }
        function __isNil(v) {
            return v === null || v === undefined;
        }
        function __logCompatError(message) {
            if (typeof __log === 'function') {
                __log('[compat] ' + message);
            }
        }
        function __installWebpackChunkHook(name, queue) {
            if (!queue || typeof queue.push !== 'function' || queue.__compatWebpackPatched) {
                return;
            }
            var __origPush = queue.push;

            function __wrapWebpackChunkModule(moduleId, fn) {
                if (typeof fn !== 'function' || fn.__compatWrapped) {
                    return fn;
                }
                var wrapped = function(module, exports, __require) {
                    try {
                        return fn(module, exports, __require);
                    } catch (__err) {
                        if (typeof __log === 'function') {
                            __log('[webpack] module ' + String(moduleId) + ' error: ' + ((__err && __err.message) ? __err.message : String(__err)));
                        }
                        throw __err;
                    }
                };
                wrapped.__compatWrapped = true;
                wrapped.__compatOriginal = fn;
                return wrapped;
            }

            function __patchWebpackChunk(chunk) {
                if (!chunk || !Array.isArray(chunk) || chunk.length < 2) {
                    return;
                }
                var modules = chunk[1];
                if (!modules || typeof modules !== 'object') {
                    return;
                }
                for (var moduleId in modules) {
                    if (Object.prototype.hasOwnProperty.call(modules, moduleId)) {
                        modules[moduleId] = __wrapWebpackChunkModule(moduleId, modules[moduleId]);
                    }
                }
            }

            for (var i = 0; i < queue.length; i++) {
                __patchWebpackChunk(queue[i]);
            }

            queue.push = function() {
                for (var j = 0; j < arguments.length; j++) {
                    if (typeof __log === 'function') {
                        var __arg = arguments[j];
                        var __ids = Array.prototype.toString.call((__arg && __arg[0]) ? __arg[0] : []);
                        var __modCount = (__arg && __arg[1] && typeof __arg[1] === 'object') ? Object.keys(__arg[1]).length : 0;
                        __log('[webpack] push ' + name + ' chunk=' + __ids + ' modules=' + __modCount);
                    }
                    __patchWebpackChunk(arguments[j]);
                }
                return __origPush.apply(queue, arguments);
            };
            queue.__compatWebpackPatched = true;
            if (typeof __log === 'function') {
                __log('[webpack] patch installed on ' + name);
            }
        }
        function __installWebpackChunkAccessor(name, target) {
            if (typeof name !== 'string' || !target || typeof target !== 'object') {
                return;
            }
            var installedKey = '__compatWebpackObserved_' + name;
            if (target[installedKey]) {
                return;
            }
            var current = target[name];
            try {
                var descriptor = Object.getOwnPropertyDescriptor(target, name);
                if (!descriptor || descriptor.configurable) {
                    Object.defineProperty(target, name, {
                        configurable: true,
                        enumerable: true,
                        get: function() {
                            return current;
                        },
                        set: function(value) {
                            current = value;
                            if (typeof current === 'object' && current !== null && typeof current.push === 'function') {
                                __installWebpackChunkHook(name, current);
                            }
                        }
                    });
                }
            } catch (e) {}
            if (typeof current === 'object' && current !== null && typeof current.push === 'function') {
                __installWebpackChunkHook(name, current);
            }
            target[installedKey] = true;
        }
        function __collectWebpackQueues() {
            for (var key in globalThis) {
                if (!Object.prototype.hasOwnProperty.call(globalThis, key)) {
                    continue;
                }
                if (/^webpackChunk/.test(key) && Array.isArray(globalThis[key])) {
                    __installWebpackChunkHook(key, globalThis[key]);
                }
                if (/^webpackChunk/.test(key)) {
                    __installWebpackChunkAccessor(key, globalThis);
                }
            }
            __installWebpackChunkAccessor('webpackChunk_N_E', globalThis);
            if (typeof globalThis.self === 'object' && globalThis.self !== null) {
                __installWebpackChunkAccessor('webpackChunk_N_E', globalThis.self);
            }
        }
        __collectWebpackQueues();
        function __compatConsoleMethod(level) {
            return function() {
                var parts = [];
                for (var i = 0; i < arguments.length; i++) {
                    parts.push(__compatConsoleArg(arguments[i]));
                }
                if (typeof globalThis.__log === 'function') {
                    globalThis.__log('[console.' + level + '] ' + parts.join(' '));
                }
            };
        }
        if (typeof globalThis.console.log !== 'function') {
            globalThis.console.log = __compatConsoleMethod('log');
        }
        if (typeof globalThis.console.error !== 'function') {
            globalThis.console.error = __compatConsoleMethod('error');
        }
        if (typeof globalThis.console.warn !== 'function') {
            globalThis.console.warn = __compatConsoleMethod('warn');
        }
        if (typeof globalThis.console.info !== 'function') {
            globalThis.console.info = __compatConsoleMethod('info');
        }
        if (typeof globalThis.console.debug !== 'function') {
            globalThis.console.debug = __compatConsoleMethod('debug');
        }

        // 抗空值对象工具：Next/老站常会拿 null/undefined 调 Object/Array API；
        // 保底返回空集合，避免 "Cannot convert undefined or null to object" 直接中断渲染。
        if (typeof Object.keys === 'function') {
            var __origKeys = Object.keys;
            Object.keys = function(obj) {
                if (__isNil(obj)) {
                    __logCompatError('Object.keys(null_or_undefined)');
                    return [];
                }
                try {
                    return __origKeys(obj);
                } catch (e) {
                    __logCompatError('Object.keys threw, ignored');
                    return [];
                }
            };
        }
        if (typeof Object.values === 'function') {
            var __origValues = Object.values;
            Object.values = function(obj) {
                if (__isNil(obj)) {
                    __logCompatError('Object.values(null_or_undefined)');
                    return [];
                }
                try {
                    return __origValues(obj);
                } catch (e) {
                    __logCompatError('Object.values threw, ignored');
                    return [];
                }
            };
        }
        if (typeof Object.entries === 'function') {
            var __origEntries = Object.entries;
            Object.entries = function(obj) {
                if (__isNil(obj)) {
                    __logCompatError('Object.entries(null_or_undefined)');
                    return [];
                }
                try {
                    return __origEntries(obj);
                } catch (e) {
                    __logCompatError('Object.entries threw, ignored');
                    return [];
                }
            };
        }
        // Always install a crawler-tolerant Object.hasOwn. Some Next.js bundles
        // include a throwing polyfill when the engine lacks Object.hasOwn; if the
        // page installs that version first, nullish probes abort hydration.
        var __origHasOwn = typeof Object.hasOwn === 'function' ? Object.hasOwn : null;
        Object.hasOwn = function(obj, prop) {
            if (__isNil(obj)) {
                __logCompatError('Object.hasOwn(null_or_undefined)');
                return false;
            }
            try {
                if (__origHasOwn) {
                    return __origHasOwn(obj, prop);
                }
                return Object.prototype.hasOwnProperty.call(Object(obj), prop);
            } catch (e) {
                __logCompatError('Object.hasOwn threw, ignored');
                return false;
            }
        };
        if (typeof Object.assign === 'function') {
            Object.assign = function(target) {
                var out = target;
                if (__isNil(out)) {
                    out = {};
                } else if (typeof out !== 'object') {
                    out = Object(out);
                }
                for (var i = 1; i < arguments.length; i++) {
                    var source = arguments[i];
                    if (__isNil(source)) {
                        continue;
                    }
                    try {
                        for (var k in source) {
                            if (Object.prototype.hasOwnProperty.call(source, k)) {
                                out[k] = source[k];
                            }
                        }
                    } catch (e) {
                        __logCompatError('Object.assign copy threw, ignored');
                        return out;
                    }
                }
                return out;
            };
        }
        if (typeof Object.fromEntries === 'function') {
            var __origFromEntries = Object.fromEntries;
            Object.fromEntries = function(iterable) {
                if (__isNil(iterable)) {
                    if (typeof __log === 'function') {
                        __log('[compat] Object.fromEntries(null_or_undefined)');
                    }
                    return {};
                }
                try {
                    return __origFromEntries(iterable);
                } catch (e) {
                    if (typeof __log === 'function') {
                        __log('[compat] Object.fromEntries threw, ignored');
                    }
                    return {};
                }
            };
        }
        if (typeof Object.getOwnPropertyNames === 'function') {
            var __origGetOwnPropertyNames = Object.getOwnPropertyNames;
            Object.getOwnPropertyNames = function(obj) {
                if (__isNil(obj)) {
                    __logCompatError('Object.getOwnPropertyNames(null_or_undefined)');
                    return [];
                }
                try {
                    return __origGetOwnPropertyNames(obj);
                } catch (e) {
                    __logCompatError('Object.getOwnPropertyNames threw, ignored');
                    return [];
                }
            };
        }
        if (typeof Object.getOwnPropertySymbols === 'function') {
            var __origGetOwnPropertySymbols = Object.getOwnPropertySymbols;
            Object.getOwnPropertySymbols = function(obj) {
                if (__isNil(obj)) {
                    __logCompatError('Object.getOwnPropertySymbols(null_or_undefined)');
                    return [];
                }
                try {
                    return __origGetOwnPropertySymbols(obj);
                } catch (e) {
                    __logCompatError('Object.getOwnPropertySymbols threw, ignored');
                    return [];
                }
            };
        }
        if (typeof Object.getOwnPropertyDescriptor === 'function') {
            var __origGetOwnPropertyDescriptor = Object.getOwnPropertyDescriptor;
            Object.getOwnPropertyDescriptor = function(obj, prop) {
                if (__isNil(obj)) {
                    __logCompatError('Object.getOwnPropertyDescriptor(null_or_undefined)');
                    return undefined;
                }
                try {
                    return __origGetOwnPropertyDescriptor(obj, prop);
                } catch (e) {
                    __logCompatError('Object.getOwnPropertyDescriptor threw, ignored');
                    return undefined;
                }
            };
        }
        if (typeof Object.getOwnPropertyDescriptors === 'function') {
            var __origGetOwnPropertyDescriptors = Object.getOwnPropertyDescriptors;
            Object.getOwnPropertyDescriptors = function(obj) {
                if (__isNil(obj)) {
                    __logCompatError('Object.getOwnPropertyDescriptors(null_or_undefined)');
                    return {};
                }
                try {
                    return __origGetOwnPropertyDescriptors(obj);
                } catch (e) {
                    __logCompatError('Object.getOwnPropertyDescriptors threw, ignored');
                    return {};
                }
            };
        }
        if (typeof Object.defineProperty === 'function') {
            var __origDefineProperty = Object.defineProperty;
            Object.defineProperty = function(obj, prop, descriptor) {
                if (__isNil(obj) || __isNil(descriptor)) {
                    __logCompatError('Object.defineProperty(null_or_undefined_or_descriptor)');
                    return __isNil(obj) ? {} : obj;
                }
                try {
                    return __origDefineProperty(obj, prop, descriptor);
                } catch (e) {
                    __logCompatError('Object.defineProperty threw, ignored');
                    return obj;
                }
            };
        }
        if (typeof Object.defineProperties === 'function') {
            var __origDefineProperties = Object.defineProperties;
            Object.defineProperties = function(obj, props) {
                if (__isNil(obj) || __isNil(props)) {
                    __logCompatError('Object.defineProperties(null_or_undefined)');
                    return __isNil(obj) ? {} : obj;
                }
                try {
                    return __origDefineProperties(obj, props);
                } catch (e) {
                    __logCompatError('Object.defineProperties threw, ignored');
                    return obj;
                }
            };
        }
        if (typeof Object.create === 'function') {
            var __origCreate = Object.create;
            Object.create = function(proto, propertiesObject) {
                if (typeof proto === 'undefined') {
                    __logCompatError('Object.create(undefined)');
                    proto = Object.prototype;
                }
                try {
                    return __origCreate(proto, propertiesObject);
                } catch (e) {
                    __logCompatError('Object.create threw, ignored');
                    return {};
                }
            };
        }
        if (typeof Object.setPrototypeOf === 'function') {
            var __origSetPrototypeOf = Object.setPrototypeOf;
            Object.setPrototypeOf = function(obj, proto) {
                if (__isNil(obj)) {
                    __logCompatError('Object.setPrototypeOf(null_or_undefined)');
                    return {};
                }
                try {
                    return __origSetPrototypeOf(obj, proto);
                } catch (e) {
                    __logCompatError('Object.setPrototypeOf threw, ignored');
                    return obj;
                }
            };
        }
        if (typeof Object.getPrototypeOf === 'function') {
            var __origGetPrototypeOf = Object.getPrototypeOf;
            Object.getPrototypeOf = function(obj) {
                if (__isNil(obj)) {
                    __logCompatError('Object.getPrototypeOf(null_or_undefined)');
                    return null;
                }
                try {
                    return __origGetPrototypeOf(obj);
                } catch (e) {
                    __logCompatError('Object.getPrototypeOf threw, ignored');
                    return null;
                }
            };
        }
        if (typeof Object.freeze === 'function') {
            var __origFreeze = Object.freeze;
            Object.freeze = function(obj) {
                if (__isNil(obj)) {
                    __logCompatError('Object.freeze(null_or_undefined)');
                    return {};
                }
                try {
                    return __origFreeze(obj);
                } catch (e) {
                    __logCompatError('Object.freeze threw, ignored');
                    return obj;
                }
            };
        }
        if (typeof Object.seal === 'function') {
            var __origSeal = Object.seal;
            Object.seal = function(obj) {
                if (__isNil(obj)) {
                    __logCompatError('Object.seal(null_or_undefined)');
                    return {};
                }
                try {
                    return __origSeal(obj);
                } catch (e) {
                    __logCompatError('Object.seal threw, ignored');
                    return obj;
                }
            };
        }
        if (typeof Object.preventExtensions === 'function') {
            var __origPreventExtensions = Object.preventExtensions;
            Object.preventExtensions = function(obj) {
                if (__isNil(obj)) {
                    __logCompatError('Object.preventExtensions(null_or_undefined)');
                    return {};
                }
                try {
                    return __origPreventExtensions(obj);
                } catch (e) {
                    __logCompatError('Object.preventExtensions threw, ignored');
                    return obj;
                }
            };
        }
        if (typeof Object.isExtensible === 'function') {
            var __origIsExtensible = Object.isExtensible;
            Object.isExtensible = function(obj) {
                if (__isNil(obj)) {
                    __logCompatError('Object.isExtensible(null_or_undefined)');
                    return false;
                }
                try {
                    return __origIsExtensible(obj);
                } catch (e) {
                    __logCompatError('Object.isExtensible threw, ignored');
                    return false;
                }
            };
        }
        if (typeof Object.isSealed === 'function') {
            var __origIsSealed = Object.isSealed;
            Object.isSealed = function(obj) {
                if (__isNil(obj)) {
                    __logCompatError('Object.isSealed(null_or_undefined)');
                    return true;
                }
                try {
                    return __origIsSealed(obj);
                } catch (e) {
                    __logCompatError('Object.isSealed threw, ignored');
                    return true;
                }
            };
        }
        if (typeof Object.isFrozen === 'function') {
            var __origIsFrozen = Object.isFrozen;
            Object.isFrozen = function(obj) {
                if (__isNil(obj)) {
                    __logCompatError('Object.isFrozen(null_or_undefined)');
                    return true;
                }
                try {
                    return __origIsFrozen(obj);
                } catch (e) {
                    __logCompatError('Object.isFrozen threw, ignored');
                    return true;
                }
            };
        }
        if (typeof Object.prototype.hasOwnProperty === 'function') {
            var __origHasOwnProperty = Object.prototype.hasOwnProperty;
            Object.prototype.hasOwnProperty = function(prop) {
                if (this === null || this === undefined) {
                    __logCompatError('Object.prototype.hasOwnProperty(null_or_undefined)');
                    return false;
                }
                try {
                    return __origHasOwnProperty.call(this, prop);
                } catch (e) {
                    __logCompatError('Object.prototype.hasOwnProperty threw, ignored');
                    return false;
                }
            };
        }
        if (typeof Reflect !== 'undefined' && typeof Reflect.ownKeys === 'function') {
            var __origOwnKeys = Reflect.ownKeys;
            Reflect.ownKeys = function(obj) {
                if (__isNil(obj)) {
                    __logCompatError('Reflect.ownKeys(null_or_undefined)');
                    return [];
                }
                try {
                    return __origOwnKeys(obj);
                } catch (e) {
                    __logCompatError('Reflect.ownKeys threw, ignored');
                    return [];
                }
            };
        }
        if (typeof Reflect !== 'undefined' && typeof Reflect.get === 'function') {
            var __origReflectGet = Reflect.get;
            Reflect.get = function(target, propertyKey, receiver) {
                if (__isNil(target)) {
                    __logCompatError('Reflect.get(null_or_undefined)');
                    return undefined;
                }
                try {
                    return __origReflectGet(target, propertyKey, receiver);
                } catch (e) {
                    __logCompatError('Reflect.get threw, ignored');
                    return undefined;
                }
            };
        }
        if (typeof Reflect !== 'undefined' && typeof Reflect.set === 'function') {
            var __origReflectSet = Reflect.set;
            Reflect.set = function(target, propertyKey, value, receiver) {
                if (__isNil(target)) {
                    __logCompatError('Reflect.set(null_or_undefined)');
                    return false;
                }
                try {
                    return __origReflectSet(target, propertyKey, value, receiver);
                } catch (e) {
                    __logCompatError('Reflect.set threw, ignored');
                    return false;
                }
            };
        }
        if (typeof Reflect !== 'undefined' && typeof Reflect.has === 'function') {
            var __origReflectHas = Reflect.has;
            Reflect.has = function(target, propertyKey) {
                if (__isNil(target)) {
                    __logCompatError('Reflect.has(null_or_undefined)');
                    return false;
                }
                try {
                    return __origReflectHas(target, propertyKey);
                } catch (e) {
                    __logCompatError('Reflect.has threw, ignored');
                    return false;
                }
            };
        }
        if (typeof Reflect !== 'undefined' && typeof Reflect.defineProperty === 'function') {
            var __origReflectDefineProperty = Reflect.defineProperty;
            Reflect.defineProperty = function(target, propertyKey, attributes) {
                if (__isNil(target) || __isNil(attributes)) {
                    if (typeof __log === 'function') {
                        __log('[compat] Reflect.defineProperty(null_or_undefined)');
                    }
                    return false;
                }
                try {
                    return __origReflectDefineProperty(target, propertyKey, attributes);
                } catch (e) {
                    if (typeof __log === 'function') {
                        __log('[compat] Reflect.defineProperty threw, ignored');
                    }
                    return false;
                }
            };
        }
        if (typeof Reflect !== 'undefined' && typeof Reflect.deleteProperty === 'function') {
            var __origReflectDeleteProperty = Reflect.deleteProperty;
            Reflect.deleteProperty = function(target, propertyKey) {
                if (__isNil(target)) {
                    if (typeof __log === 'function') {
                        __log('[compat] Reflect.deleteProperty(null_or_undefined)');
                    }
                    return false;
                }
                try {
                    return __origReflectDeleteProperty(target, propertyKey);
                } catch (e) {
                    if (typeof __log === 'function') {
                        __log('[compat] Reflect.deleteProperty threw, ignored');
                    }
                    return false;
                }
            };
        }
        if (typeof Reflect !== 'undefined' && typeof Reflect.getOwnPropertyDescriptor === 'function') {
            var __origReflectGetOwnPropertyDescriptor = Reflect.getOwnPropertyDescriptor;
            Reflect.getOwnPropertyDescriptor = function(target, propertyKey) {
                if (__isNil(target)) {
                    __logCompatError('Reflect.getOwnPropertyDescriptor(null_or_undefined)');
                    return undefined;
                }
                try {
                    return __origReflectGetOwnPropertyDescriptor(target, propertyKey);
                } catch (e) {
                    __logCompatError('Reflect.getOwnPropertyDescriptor threw, ignored');
                    return undefined;
                }
            };
        }
        if (typeof Reflect !== 'undefined' && typeof Reflect.apply === 'function') {
            var __origReflectApply = Reflect.apply;
            Reflect.apply = function(target, thisArgument, argumentsList) {
                if (__isNil(target) || __isNil(argumentsList)) {
                    if (typeof __log === 'function') {
                        __log('[compat] Reflect.apply(null_or_undefined)');
                    }
                    return undefined;
                }
                try {
                    return __origReflectApply(target, thisArgument, argumentsList);
                } catch (e) {
                    if (typeof __log === 'function') {
                        __log('[compat] Reflect.apply threw, ignored');
                    }
                    return undefined;
                }
            };
        }
        if (false) { // M62: boa 0.21 原生支持 Reflect.construct
            var __origReflectConstruct = Reflect.construct;
            Reflect.construct = function(target, args, newTarget) {
                if (__isNil(target) || __isNil(args)) {
                    if (typeof __log === 'function') {
                        __log('[compat] Reflect.construct(null_or_undefined)');
                    }
                    return {};
                }
                try {
                    return __origReflectConstruct(target, args, newTarget);
                } catch (e) {
                    if (typeof __log === 'function') {
                        __log('[compat] Reflect.construct threw, ignored');
                    }
                    return {};
                }
            };
        }
        if (false) { // M62: boa 0.21 原生支持 Array.from
            var __origArrayFrom = Array.from;
            Array.from = function() {
                if (__isNil(arguments[0])) {
                    __logCompatError('Array.from(null_or_undefined)');
                    return [];
                }
                try {
                    return __origArrayFrom.apply(Array, arguments);
                } catch (e) {
                    __logCompatError('Array.from threw, ignored');
                    return [];
                }
            };
        }
        function __wrapArrayMethod(name, fallback) {
            var __orig = Array.prototype[name];
            if (typeof __orig !== 'function') return;
            Array.prototype[name] = function() {
                if (this === null || this === undefined) {
                    __logCompatError('Array.prototype.' + name + ' called on null_or_undefined');
                    if (typeof fallback === 'function') {
                        return fallback.apply(this, arguments);
                    }
                    return fallback;
                }
                try {
                    return __orig.apply(this, arguments);
                } catch (e) {
                    __logCompatError('Array.prototype.' + name + ' threw, ignored');
                    if (typeof fallback === 'function') {
                        return fallback.apply(this, arguments);
                    }
                    return fallback;
                }
            };
        }
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('forEach');
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('map', function() { return []; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('filter', function() { return []; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('reduce', function() { return this; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('reduceRight', function() { return this; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('every', function() { return true; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('some', function() { return false; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('find', function() { return undefined; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('findIndex', function() { return -1; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('concat', function() { return []; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('copyWithin', function() { return []; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('entries', function() { return []; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('every', function() { return true; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('fill', function() { return []; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('includes', function() { return false; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('indexOf', function() { return -1; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('join', function() { return ''; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('keys', function() { return []; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('lastIndexOf', function() { return -1; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('map', function() { return []; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('pop', function() { return undefined; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('push', function() { return 0; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('reduce', function() { return this; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('reverse', function() { return []; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('shift', function() { return undefined; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('sort', function() { return []; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('splice', function() { return []; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('unshift', function() { return 0; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('values', function() { return []; });

        // 缺失的现代 Web API 兼容兜底：在 boa 中大量网站会直接依赖这些 API，
        // 但未实现时会在页面脚本第一层直接抛错，导致 SPA 无法完成 hydration。
        if (typeof globalThis.URL === 'undefined') {
            globalThis.URL = function(url, base) {
                var raw = (url === null || typeof url === 'undefined') ? '' : String(url);
                var baseStr = (typeof base === 'string' || base instanceof String) ? String(base) : (base && base.href ? String(base.href) : '');
                if (!baseStr) baseStr = 'http://localhost';
                if (!baseStr.match(/^[a-zA-Z][a-zA-Z\d+\-.]*:\/\//)) {
                    baseStr = 'http://localhost' + (baseStr.charAt(0) === '/' ? '' : '/') + baseStr;
                }
                var baseProto = baseStr;
                var baseMatch = /^(.*?:\/\/[^\/\?#]+)(.*)$/.exec(baseProto);
                var protocolAndHost = baseMatch ? baseMatch[1] : 'http://localhost';
                var basePath = baseMatch ? baseMatch[2] : '/';
                var hashIdx = basePath.indexOf('#');
                if (hashIdx >= 0) basePath = basePath.slice(0, hashIdx);
                var qIdx = basePath.indexOf('?');
                if (qIdx >= 0) basePath = basePath.slice(0, qIdx);
                if (basePath === '') basePath = '/';

                var full = raw;
                if (!/^[a-zA-Z][a-zA-Z\d+\-.]*:\/\//.test(full)) {
                    if (full.charAt(0) === '/') {
                        full = protocolAndHost + full;
                    } else {
                        var seg = basePath;
                        if (seg.indexOf('/') === -1) seg = '/';
                        else if (!seg.endsWith('/')) seg = seg.slice(0, seg.lastIndexOf('/') + 1);
                        full = protocolAndHost + seg + full;
                    }
                }
                var hashPos = full.indexOf('#');
                var hash = hashPos >= 0 ? full.slice(hashPos + 1) : '';
                var searchPos = full.indexOf('?');
                var pathStart = full.indexOf('/', full.indexOf('://') + 3);
                var withoutSearchHash = full;
                var search = '';
                if (searchPos >= 0) {
                    search = full.slice(searchPos + 1);
                    withoutSearchHash = full.slice(0, searchPos);
                }
                if (hashPos >= 0) {
                    withoutSearchHash = withoutSearchHash.slice(0, hashPos);
                }
                if (searchPos >= 0 && hashPos >= 0 && hashPos > searchPos) {
                    search = full.slice(searchPos + 1, hashPos);
                }
                if (pathStart < 0) {
                    pathStart = full.indexOf('://') >= 0 ? full.indexOf('://') + 3 : 0;
                }
                var hostStart = full.indexOf('://') + 3;
                var hostEnd = full.slice(hostStart).indexOf('/');
                if (hostEnd < 0) hostEnd = full.length - hostStart;
                var host = full.slice(hostStart, hostStart + hostEnd);
                var pathname = (full.slice(pathStart).split(/[?#]/)[0]) || '/';
                this.protocol = protocolAndHost.split(':')[0] + ':';
                this.hostname = host.indexOf(':') >= 0 ? host.split(':')[0] : host;
                this.host = host;
                this.port = '';
                this.origin = protocolAndHost;
                this.pathname = pathname;
                this.search = search ? ('?' + search.split('#')[0].replace(/^[?]?/,'')) : '';
                this.hash = hash ? ('#' + hash) : '';
                this.href = protocolAndHost + pathname + this.search + this.hash;
                this.searchParams = new globalThis.URLSearchParams(this.search);
            };
            globalThis.URL.prototype = {
                toString: function() {
                    // M63: 注意 href 在构造器里已设为普通 own property（非 getter），
                    // 不能在这里读 this.href 否则若被覆盖会递归。直接拼已有字段。
                    return String(this.href || '');
                },
                toJSON: function() {
                    return String(this.href || '');
                }
            };
        }

        if (typeof globalThis.URLSearchParams === 'undefined') {
            globalThis.URLSearchParams = function(init) {
                var pairs = [];
                this._pairs = pairs;
                if (typeof init === 'string' || init instanceof String) {
                    var s = String(init);
                    if (s.charAt(0) === '?') {
                        s = s.slice(1);
                    }
                    if (s.length) {
                        var items = s.split('&');
                        for (var i = 0; i < items.length; i++) {
                            if (!items[i]) continue;
                            var kv = items[i].split('=');
                            var k = decodeURIComponent(kv[0] || '');
                            var v = decodeURIComponent((kv.slice(1).join('=')).replace(/\+/g, ' ') || '');
                            pairs.push([k, v]);
                        }
                    }
                } else if (init && typeof init === 'object' && typeof init.forEach === 'function') {
                    init.forEach(function(v, k) {
                        pairs.push([String(k), String(v)]);
                    });
                }
            };
            globalThis.URLSearchParams.prototype = {
                append: function(name, value) {
                    this._pairs.push([String(name), String(value)]);
                },
                delete: function(name) {
                    var out = [];
                    for (var i = 0; i < this._pairs.length; i++) {
                        if (this._pairs[i][0] !== String(name)) out.push(this._pairs[i]);
                    }
                    this._pairs = out;
                },
                get: function(name) {
                    var key = String(name);
                    for (var i = 0; i < this._pairs.length; i++) {
                        if (this._pairs[i][0] === key) return this._pairs[i][1];
                    }
                    return null;
                },
                getAll: function(name) {
                    var key = String(name);
                    var out = [];
                    for (var i = 0; i < this._pairs.length; i++) {
                        if (this._pairs[i][0] === key) out.push(this._pairs[i][1]);
                    }
                    return out;
                },
                has: function(name) {
                    var key = String(name);
                    for (var i = 0; i < this._pairs.length; i++) {
                        if (this._pairs[i][0] === key) return true;
                    }
                    return false;
                },
                set: function(name, value) {
                    var key = String(name);
                    var has = false;
                    for (var i = 0; i < this._pairs.length; i++) {
                        if (this._pairs[i][0] === key) {
                            if (!has) {
                                this._pairs[i][1] = String(value);
                                has = true;
                            } else {
                                this._pairs[i][1] = String(value);
                            }
                        }
                    }
                    if (!has) this._pairs.push([key, String(value)]);
                },
                forEach: function(cb, thisArg) {
                    for (var i = 0; i < this._pairs.length; i++) {
                        cb.call(thisArg, this._pairs[i][1], this._pairs[i][0], this);
                    }
                },
                entries: function() {
                    return this._pairs.slice();
                },
                keys: function() {
                    var out = [];
                    for (var i = 0; i < this._pairs.length; i++) out.push(this._pairs[i][0]);
                    return out;
                },
                values: function() {
                    var out = [];
                    for (var i = 0; i < this._pairs.length; i++) out.push(this._pairs[i][1]);
                    return out;
                },
                toString: function() {
                    return this._pairs.map(function(p) { return encodeURIComponent(p[0]) + '=' + encodeURIComponent(p[1]); }).join('&');
                },
                toJSON: function() {
                    return this.toString();
                }
            };
        }

        // M62: 事件系统。爬虫场景不需要真交互，但框架初始化（DOMContentLoaded、
        // 自定义事件）必须不报错。Event/CustomEvent/EventTarget 让框架不挂。
        if (typeof globalThis.Event !== 'function') {
            globalThis.Event = function Event(type, options) {
                this.type = type;
                this.bubbles = (options && options.bubbles) || false;
                this.cancelable = (options && options.cancelable) || false;
                this.detail = undefined;
                this.target = null;
                this.currentTarget = null;
                this.preventDefault = function() {};
                this.stopPropagation = function() {};
                this.stopImmediatePropagation = function() {};
            };
        }
        if (typeof globalThis.CustomEvent !== 'function') {
            globalThis.CustomEvent = function CustomEvent(type, options) {
                this.type = type;
                this.bubbles = (options && options.bubbles) || false;
                this.cancelable = (options && options.cancelable) || false;
                this.detail = (options && options.detail) || undefined;
                this.target = null;
                this.currentTarget = null;
                this.preventDefault = function() {};
                this.stopPropagation = function() {};
            };
            globalThis.CustomEvent.prototype = Object.create(globalThis.Event.prototype);
            globalThis.CustomEvent.prototype.constructor = globalThis.CustomEvent;
        }
        if (typeof globalThis.EventTarget !== 'function') {
            globalThis.EventTarget = function EventTarget() {
                this.__listeners = {};
            };
            globalThis.EventTarget.prototype.addEventListener = function(type, cb) {
                if (!this.__listeners) this.__listeners = {};
                if (!this.__listeners[type]) this.__listeners[type] = [];
                this.__listeners[type].push(cb);
            };
            globalThis.EventTarget.prototype.removeEventListener = function(type, cb) {
                if (!this.__listeners || !this.__listeners[type]) return;
                this.__listeners[type] = this.__listeners[type].filter(function(f) {
                    return f !== cb;
                });
            };
            globalThis.EventTarget.prototype.dispatchEvent = function(ev) {
                if (!this.__listeners || !ev || !this.__listeners[ev.type]) return true;
                var cbs = this.__listeners[ev.type];
                ev.target = this;
                ev.currentTarget = this;
                for (var i = 0; i < cbs.length; i++) {
                    try { cbs[i](ev); } catch (e) {
                        if (typeof __log === 'function') __log('[event] listener threw: ' + e.message);
                    }
                }
                return true;
            };
        }
        // M62: MutationObserver（Vue 3 响应式 / 框架 hydration 需要）。
        // 爬虫场景：存回调但不真 observe（DOM 变化由 JS 执行驱动，不需监听）。
        if (typeof globalThis.MutationObserver !== 'function') {
            globalThis.MutationObserver = function MutationObserver(cb) {
                this.__cb = cb;
                this.__observing = false;
            };
            globalThis.MutationObserver.prototype.observe = function(target, opts) {
                this.__observing = true;
                this.__target = target;
                this.__opts = opts;
            };
            globalThis.MutationObserver.prototype.disconnect = function() {
                this.__observing = false;
            };
            globalThis.MutationObserver.prototype.takeRecords = function() {
                return [];
            };
        }
        // M62: window.matchMedia（框架响应式布局检测，base.js/todomvc 报错根源）。
        if (typeof globalThis.matchMedia !== 'function') {
            globalThis.matchMedia = function(query) {
                return {
                    matches: false,
                    media: query || '',
                    onchange: null,
                    addListener: function() {},
                    removeListener: function() {},
                    addEventListener: function() {},
                    removeEventListener: function() {},
                    dispatchEvent: function() { return true; }
                };
            };
        }

        if (typeof globalThis.MessageEvent !== 'function') {
            globalThis.MessageEvent = function MessageEvent(type, options) {
                this.type = type;
                this.data = (options && options.data) || null;
                this.origin = (options && options.origin) || '';
                this.target = null;
            };
        }

        // M62: TextEncoder/TextDecoder（UTF-8，fetch/stream 配套）。
        if (typeof globalThis.TextEncoder !== 'function') {
            globalThis.TextEncoder = function TextEncoder() {
                this.encoding = 'utf-8';
            };
            globalThis.TextEncoder.prototype.encode = function(str) {
                str = str || '';
                var arr = [];
                for (var i = 0; i < str.length; i++) {
                    var c = str.charCodeAt(i);
                    if (c < 0x80) arr.push(c);
                    else if (c < 0x800) {
                        arr.push(0xc0 | (c >> 6));
                        arr.push(0x80 | (c & 0x3f));
                    } else {
                        arr.push(0xe0 | (c >> 12));
                        arr.push(0x80 | ((c >> 6) & 0x3f));
                        arr.push(0x80 | (c & 0x3f));
                    }
                }
                return new Uint8Array(arr);
            };
        }
        if (typeof globalThis.TextDecoder !== 'function') {
            globalThis.TextDecoder = function TextDecoder(label) {
                this.encoding = (label || 'utf-8').toLowerCase();
            };
            globalThis.TextDecoder.prototype.decode = function(buf) {
                if (!buf) return '';
                var arr = buf.buffer ? new Uint8Array(buf.buffer) : new Uint8Array(buf);
                var out = '', i = 0;
                while (i < arr.length) {
                    var b = arr[i++];
                    if (b < 0x80) { out += String.fromCharCode(b); }
                    else if (b < 0xe0) {
                        var b2 = arr[i++];
                        out += String.fromCharCode(((b & 0x1f) << 6) | (b2 & 0x3f));
                    } else {
                        var b2 = arr[i++], b3 = arr[i++];
                        out += String.fromCharCode(((b & 0xf) << 12) | ((b2 & 0x3f) << 6) | (b3 & 0x3f));
                    }
                }
                return out;
            };
        }
        // M62: escape/unescape（deprecated 全局函数，但部分旧库/混淆代码仍用）。
        // builder.io persist-attribution 等第三方依赖 escape()。
        if (typeof globalThis.escape !== 'function') {
            globalThis.escape = function escape(str) {
                str = String(str);
                var out = '';
                for (var i = 0; i < str.length; i++) {
                    var c = str.charAt(i);
                    var cc = str.charCodeAt(i);
                    if ((cc >= 0x30 && cc <= 0x39) ||  // 0-9
                        (cc >= 0x41 && cc <= 0x5a) ||  // A-Z
                        (cc >= 0x61 && cc <= 0x7a) ||  // a-z
                        c === '@' || c === '*' || c === '_' || c === '+' ||
                        c === '-' || c === '.' || c === '/') {
                        out += c;
                    } else if (cc < 256) {
                        out += '%' + (cc < 16 ? '0' : '') + cc.toString(16).toUpperCase();
                    } else {
                        out += '%u' + cc.toString(16).toUpperCase().padStart(4, '0');
                    }
                }
                return out;
            };
        }
        if (typeof globalThis.unescape !== 'function') {
            globalThis.unescape = function unescape(str) {
                str = String(str);
                return decodeURIComponent(str.replace(/%u([0-9a-fA-F]{4})/g, function(_, hex) {
                    return '%u' + hex;
                }).replace(/%([0-9a-fA-F]{2})/g, '%$1'));
            };
        }
        // M62: TextEncoderStream/TextDecoderStream（Stream API，builder.io 等）。
        // 爬虫场景：构造器存在即可，不需要真流式编码。
        if (typeof globalThis.TextEncoderStream !== 'function') {
            globalThis.TextEncoderStream = function TextEncoderStream() {
                this.encoding = 'utf-8';
                this.readable = { locked: false, getReader: function() { return { read: function() { return Promise.resolve({ done: true }); } }; } };
                this.writable = { locked: false, getWriter: function() { return { write: function() {}, close: function() { return Promise.resolve(); } }; } };
            };
        }
        if (typeof globalThis.TextDecoderStream !== 'function') {
            globalThis.TextDecoderStream = function TextDecoderStream(label) {
                this.encoding = (label || 'utf-8').toLowerCase();
                this.readable = { locked: false, getReader: function() { return { read: function() { return Promise.resolve({ done: true }); } }; } };
                this.writable = { locked: false, getWriter: function() { return { write: function() {}, close: function() { return Promise.resolve(); } }; } };
            };
        }
        // M62: Headers（fetch 配套，键值对存储）。
        if (typeof globalThis.Headers !== 'function') {
            globalThis.Headers = function Headers(init) {
                this.__h = {};
                if (init) {
                    if (typeof init.forEach === 'function') {
                        init.forEach(function(v, k) { this[k.toLowerCase()] = v; }, this.__h);
                    } else {
                        for (var k in init) { this.__h[k.toLowerCase()] = init[k]; }
                    }
                }
            };
            globalThis.Headers.prototype.get = function(k) { return this.__h[k.toLowerCase()] || null; };
            globalThis.Headers.prototype.set = function(k, v) { this.__h[k.toLowerCase()] = v; };
            globalThis.Headers.prototype.append = function(k, v) {
                var lk = k.toLowerCase();
                if (this.__h[lk]) this.__h[lk] += ', ' + v;
                else this.__h[lk] = v;
            };
            globalThis.Headers.prototype.has = function(k) { return k.toLowerCase() in this.__h; };
            globalThis.Headers.prototype.delete = function(k) { delete this.__h[k.toLowerCase()]; };
        }
        // M62: FormData（表单数据，键值对）。
        if (typeof globalThis.FormData !== 'function') {
            globalThis.FormData = function FormData() { this.__d = {}; };
            globalThis.FormData.prototype.append = function(k, v) {
                if (!this.__d[k]) this.__d[k] = [];
                this.__d[k].push(v);
            };
            globalThis.FormData.prototype.get = function(k) { return this.__d[k] ? this.__d[k][0] : null; };
            globalThis.FormData.prototype.has = function(k) { return k in this.__d; };
        }
        // M62: Blob（二进制数据，简化版存字符串）。
        if (typeof globalThis.Blob !== 'function') {
            globalThis.Blob = function Blob(parts, opts) {
                this.size = 0;
                this.type = (opts && opts.type) || '';
                this.__text = '';
                if (parts) {
                    for (var i = 0; i < parts.length; i++) {
                        var s = String(parts[i]);
                        this.__text += s;
                        this.size += s.length;
                    }
                }
            };
            globalThis.Blob.prototype.text = function() {
                var self = this;
                return Promise.resolve(self.__text);
            };
        }

        // M62: marked（markdown 解析器桩）。docsify/vuepress 等文档框架依赖
        // 全局 marked。爬虫场景给最小实现（标题/段落/链接/列表/代码），让框架不崩。
        if (typeof globalThis.marked !== 'function') {
            globalThis.marked = function(src) {
                if (src == null) return '';
                src = String(src);
                var html = src
                    .replace(/^### (.+)$/gm, '<h3>$1</h3>')
                    .replace(/^## (.+)$/gm, '<h2>$1</h2>')
                    .replace(/^# (.+)$/gm, '<h1>$1</h1>')
                    .replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
                    .replace(/\[(.+?)\]\(([^)]+)\)/g, '<a href="$2">$1</a>')
                    .replace(/`(.+?)`/g, '<code>$1</code>')
                    .replace(/^\* (.+)$/gm, '<li>$1</li>')
                    .replace(/^\d+\. (.+)$/gm, '<li>$1</li>')
                    .replace(/\n\n/g, '</p><p>');
                return '<p>' + html + '</p>';
            };
            globalThis.marked.parse = globalThis.marked;
            globalThis.marked.parseInline = function(src) {
                if (src == null) return '';
                return String(src).replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
                    .replace(/\[(.+?)\]\(([^)]+)\)/g, '<a href="$2">$1</a>');
            };
            globalThis.marked.setOptions = function() { return globalThis.marked; };
        }
        // M62: Prism（代码高亮桩，no-op）。爬虫不需要语法高亮。
        if (typeof globalThis.Prism !== 'object') {
            globalThis.Prism = { highlight: function(code) { return code; }, languages: {}, tokenize: function(t) { return t; } };
        }

        // M62: Prism.languages.DFS null guard（docsify/bark 崩溃根因）。
        // Prism 的 DFS 遍历语言定义时对 null 调 objId 崩。
        // 延迟 patch（Prism 在 docsify 加载后才存在）。
        var __origDFS = null;
        function __patchPrismDFS() {
            if (typeof Prism === 'object' && Prism.languages && typeof Prism.languages.DFS === 'function' && !Prism.languages.__dfsPatched) {
                __origDFS = Prism.languages.DFS;
                Prism.languages.DFS = function(o, callback, parent) {
                    if (o === null || o === undefined) return;
                    return __origDFS.call(this, o, callback, parent);
                };
                Prism.languages.__dfsPatched = true;
            }
        }
        // 多次尝试（Prism 可能在不同时机加载）
        if (typeof setTimeout === 'function') { setTimeout(__patchPrismDFS, 0); setTimeout(__patchPrismDFS, 50); }

        // M62: ga（Google Analytics no-op）。base.js 等库直接调 ga()，
        // 若 hostname 检查失败（boa getter 是方法形式）则 ga 未初始化 → not a callable。
        if (typeof globalThis.ga !== 'function') {
            globalThis.ga = function() {};
        }

        // M62: queueMicrotask —— 用 Promise 微任务队列实现（boa 0.21 支持）。
        if (typeof globalThis.queueMicrotask !== 'function') {
            globalThis.queueMicrotask = function(cb) {
                Promise.resolve().then(cb);
            };
        }

        if (typeof globalThis.structuredClone !== 'function') {
            globalThis.structuredClone = function(value) {
                try {
                    if (value === undefined || value === null) return value;
                    if (value instanceof Date) return new Date(value.getTime());
                    return JSON.parse(JSON.stringify(value));
                } catch (e) {
                    return value;
                }
            };
        }

        if (typeof globalThis.performance === 'undefined') {
            globalThis.performance = {
                __start: Date.now(),
                now: function() {
                    return Date.now() - this.__start;
                }
            };
        } else if (typeof globalThis.performance.now !== 'function') {
            globalThis.performance.now = function() {
                return Date.now();
            };
        }

        if (typeof globalThis.requestAnimationFrame !== 'function') {
            globalThis.requestAnimationFrame = function(cb) {
                return setTimeout(function() {
                    cb(typeof performance === 'object' ? performance.now() : Date.now());
                }, 0);
            };
        }
        if (typeof globalThis.cancelAnimationFrame !== 'function') {
            globalThis.cancelAnimationFrame = function(id) {
                return clearTimeout(id);
            };
        }
        if (typeof globalThis.requestIdleCallback !== 'function') {
            globalThis.requestIdleCallback = function(cb) {
                return setTimeout(function() {
                    cb({
                        didTimeout: false,
                        timeRemaining: function() { return 0; }
                    });
                }, 0);
            };
        }
        if (typeof globalThis.IntersectionObserver === 'undefined') {
            globalThis.IntersectionObserver = function() {};
            globalThis.IntersectionObserver.prototype.observe = function() {};
            globalThis.IntersectionObserver.prototype.unobserve = function() {};
            globalThis.IntersectionObserver.prototype.disconnect = function() {};
        }
        if (typeof globalThis.AbortController === 'undefined') {
            globalThis.AbortController = function() {
                var signal = {
                    aborted: false,
                    reason: null,
                    onabort: null,
                    addEventListener: function() {},
                    removeEventListener: function() {}
                };
                this.signal = signal;
                this.abort = function(reason) {
                    signal.aborted = true;
                    signal.reason = reason || null;
                    if (typeof signal.onabort === 'function') {
                        signal.onabort({ type: 'abort' });
                    }
                };
            };
        }
        if (typeof globalThis.ReadableStream === 'undefined') {
            globalThis.ReadableStream = function() {
                this.getReader = function() { return {}; };
            };
        }
        // M62: 真 Base64 编解码（之前实现是错的透传，破坏 JWT 场景）。
        var __B64_CHARS = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
        if (typeof globalThis.btoa !== 'function') {
            globalThis.btoa = function(str) {
                if (typeof str !== 'string') return '';
                var out = '', i = 0;
                while (i < str.length) {
                    var b1 = str.charCodeAt(i++) & 0xff;
                    var b2 = i < str.length ? (str.charCodeAt(i++) & 0xff) : -1;
                    var b3 = i < str.length ? (str.charCodeAt(i++) & 0xff) : -1;
                    out += __B64_CHARS[b1 >> 2];
                    out += __B64_CHARS[((b1 & 0x3) << 4) | ((b2 > -1 ? b2 : 0) >> 4)];
                    out += b2 > -1 ? __B64_CHARS[((b2 & 0xf) << 2) | ((b3 > -1 ? b3 : 0) >> 6)] : '=';
                    out += b3 > -1 ? __B64_CHARS[b3 & 0x3f] : '=';
                }
                return out;
            };
        }
        if (typeof globalThis.atob !== 'function') {
            globalThis.atob = function(str) {
                if (typeof str !== 'string') return '';
                str = str.replace(/[^A-Za-z0-9+/=]/g, '');
                var out = '', i = 0;
                while (i < str.length) {
                    var c1 = __B64_CHARS.indexOf(str.charAt(i++));
                    var c2 = __B64_CHARS.indexOf(str.charAt(i++));
                    var c3 = __B64_CHARS.indexOf(str.charAt(i++));
                    var c4 = __B64_CHARS.indexOf(str.charAt(i++));
                    out += String.fromCharCode((c1 << 2) | (c2 >> 4));
                    if (c3 >= 0 && str.charAt(i - 2) !== '=') {
                        out += String.fromCharCode(((c2 & 0xf) << 4) | (c3 >> 2));
                    }
                    if (c4 >= 0 && str.charAt(i - 1) !== '=') {
                        out += String.fromCharCode(((c3 & 0x3) << 6) | c4);
                    }
                }
                return out;
            };
        }
        if (typeof globalThis.crypto === 'undefined') {
            globalThis.crypto = {
                getRandomValues: function(arr) {
                    for (var i = 0; i < arr.length; i++) {
                        arr[i] = Math.floor(Math.random() * 256);
                    }
                    return arr;
                },
                randomUUID: function() {
                    return '00000000-0000-4000-8000-000000000000'.replace(/[0]/g, function() {
                        return Math.floor(Math.random() * 16).toString(16);
                    });
                }
            };
        }

        if (typeof Object.prototype.toString === 'function') {
            var __origObjToString = Object.prototype.toString;
            Object.prototype.toString = function() {
                if (this === null || this === undefined) {
                    __logCompatError('Object.prototype.toString(null_or_undefined)');
                    return '[object Undefined]';
                }
                try {
                    return __origObjToString.call(this);
                } catch (e) {
                    __logCompatError('Object.prototype.toString threw, ignored');
                    return '[object Object]';
                }
            };
        }
        if (typeof Object.prototype.isPrototypeOf === 'function') {
            var __origIsPrototypeOf = Object.prototype.isPrototypeOf;
            Object.prototype.isPrototypeOf = function(obj) {
                if (this === null || this === undefined) {
                    __logCompatError('Object.prototype.isPrototypeOf(null_or_undefined)');
                    return false;
                }
                try {
                    return __origIsPrototypeOf.call(this, obj);
                } catch (e) {
                    __logCompatError('Object.prototype.isPrototypeOf threw, ignored');
                    return false;
                }
            };
        }
        if (typeof Object.prototype.propertyIsEnumerable === 'function') {
            var __origPropertyIsEnumerable = Object.prototype.propertyIsEnumerable;
            Object.prototype.propertyIsEnumerable = function(prop) {
                if (this === null || this === undefined) {
                    __logCompatError('Object.prototype.propertyIsEnumerable(null_or_undefined)');
                    return false;
                }
                try {
                    return __origPropertyIsEnumerable.call(this, prop);
                } catch (e) {
                    __logCompatError('Object.prototype.propertyIsEnumerable threw, ignored');
                    return false;
                }
            };
        }
        if (typeof Object.prototype.__defineGetter__ === 'function') {
            var __origDefineGetter = Object.prototype.__defineGetter__;
            Object.prototype.__defineGetter__ = function(prop, getter) {
                if (this === null || this === undefined) {
                    __logCompatError('Object.prototype.__defineGetter__(null_or_undefined)');
                    return undefined;
                }
                try {
                    return __origDefineGetter.call(this, prop, getter);
                } catch (e) {
                    __logCompatError('Object.prototype.__defineGetter__ threw, ignored');
                    return undefined;
                }
            };
        }
        if (typeof Object.prototype.__defineSetter__ === 'function') {
            var __origDefineSetter = Object.prototype.__defineSetter__;
            Object.prototype.__defineSetter__ = function(prop, setter) {
                if (this === null || this === undefined) {
                    __logCompatError('Object.prototype.__defineSetter__(null_or_undefined)');
                    return undefined;
                }
                try {
                    return __origDefineSetter.call(this, prop, setter);
                } catch (e) {
                    __logCompatError('Object.prototype.__defineSetter__ threw, ignored');
                    return undefined;
                }
            };
        }
        if (typeof Object.prototype.__lookupGetter__ === 'function') {
            var __origLookupGetter = Object.prototype.__lookupGetter__;
            Object.prototype.__lookupGetter__ = function(prop) {
                if (this === null || this === undefined) {
                    __logCompatError('Object.prototype.__lookupGetter__(null_or_undefined)');
                    return undefined;
                }
                try {
                    return __origLookupGetter.call(this, prop);
                } catch (e) {
                    __logCompatError('Object.prototype.__lookupGetter__ threw, ignored');
                    return undefined;
                }
            };
        }
        if (typeof Object.prototype.__lookupSetter__ === 'function') {
            var __origLookupSetter = Object.prototype.__lookupSetter__;
            Object.prototype.__lookupSetter__ = function(prop) {
                if (this === null || this === undefined) {
                    __logCompatError('Object.prototype.__lookupSetter__(null_or_undefined)');
                    return undefined;
                }
                try {
                    return __origLookupSetter.call(this, prop);
                } catch (e) {
                    __logCompatError('Object.prototype.__lookupSetter__ threw, ignored');
                    return undefined;
                }
            };
        }
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('slice', function() { return []; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapArrayMethod('concat', function() { return []; });

        // 兼容字符串/数字对象在空值下调用原型方法（如 startsWith / includes / split）。
        // Next/React 常见运行时会在尚未归一化参数时做链式字符串处理，若 this 为 undefined
        // 会抛 TypeError: cannot convert 'null' or 'undefined' to object；对于爬虫场景改为降级。
        function __wrapStringMethod(name, fallback) {
            var __orig = String.prototype[name];
            if (typeof __orig !== 'function') return;
            String.prototype[name] = function() {
                if (this === null || this === undefined) {
                    __logCompatError('String.prototype.' + name + ' called on null_or_undefined');
                    if (typeof fallback === 'function') {
                        return fallback.apply(this, arguments);
                    }
                    return fallback;
                }
                try {
                    return __orig.apply(this, arguments);
                } catch (e) {
                    __logCompatError('String.prototype.' + name + ' threw, ignored');
                    if (typeof fallback === 'function') {
                        return fallback.apply(this, arguments);
                    }
                    return fallback;
                }
            };
        }
        function __wrapStringMethodsWithReturn(nameList, fallbackForString) {
            for (var i = 0; i < nameList.length; i++) {
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod(nameList[i], fallbackForString);
            }
        }
        function __strFallbackToSelf(args) {
            if (!args || args.length < 1) {
                return '';
            }
            return String(this);
        }
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('startsWith', function() { return false; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('endsWith', function() { return false; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('includes', function() { return false; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('indexOf', function() { return -1; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('lastIndexOf', function() { return -1; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('match', function() { return null; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('matchAll', function() { return []; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('split', function() { return []; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('replace', __strFallbackToSelf);
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('replaceAll', __strFallbackToSelf);
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('slice', function() { return ''; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('substring', function() { return ''; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('substr', function() { return ''; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('trim', __strFallbackToSelf);
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('trimStart', __strFallbackToSelf);
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('trimEnd', __strFallbackToSelf);
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('toLowerCase', __strFallbackToSelf);
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('toUpperCase', __strFallbackToSelf);
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('toString', __strFallbackToSelf);
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('valueOf', __strFallbackToSelf);
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('charAt', function() { return ''; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('charCodeAt', function() { return NaN; });
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('concat', __strFallbackToSelf);
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('padStart', __strFallbackToSelf);
        // M62: boa 0.21 原生支持，移除包装
        //__wrapStringMethod('padEnd', __strFallbackToSelf);

        function __wrapNumberMethod(name, fallback) {
            var __orig = Number.prototype[name];
            if (typeof __orig !== 'function') return;
            Number.prototype[name] = function() {
                if (this === null || this === undefined) {
                    __logCompatError('Number.prototype.' + name + ' called on null_or_undefined');
                    if (typeof fallback === 'function') {
                        return fallback.apply(this, arguments);
                    }
                    return fallback;
                }
                try {
                    return __orig.apply(this, arguments);
                } catch (e) {
                    __logCompatError('Number.prototype.' + name + ' threw, ignored');
                    if (typeof fallback === 'function') {
                        return fallback.apply(this, arguments);
                    }
                    return fallback;
                }
            };
        }
        __wrapNumberMethod('toFixed', function() { return '0'; });
        __wrapNumberMethod('toString', function() { return '0'; });
        __wrapNumberMethod('toExponential', function() { return '0'; });
        __wrapNumberMethod('toPrecision', function() { return '0'; });

        // 兼容常见 `location.href.replace(...)` 写法（不少站点在 location 作为字符串对象上调用）。
        if (globalThis.location && typeof globalThis.location === 'object' && !globalThis.location.__compatPatched) {
            var locationObj = globalThis.location;
            var locationHref = typeof locationObj.href === 'function'
                ? locationObj.href
                : (typeof globalThis.__locationHref === 'function' ? globalThis.__locationHref : null);
            var locationReplace = typeof locationObj.replace === 'function'
                ? locationObj.replace
                : (typeof globalThis.__locationReplace === 'function' ? globalThis.__locationReplace : null);
            var locationAssign = typeof locationObj.assign === 'function'
                ? locationObj.assign
                : (typeof globalThis.__locationAssign === 'function' ? globalThis.__locationAssign : null);
            var locationParts = typeof globalThis.__locationParts === 'function'
                ? globalThis.__locationParts
                : null;
            try {
                Object.defineProperty(locationObj, 'href', {
                    get: function() {
                        if (typeof locationHref === 'function') {
                            try {
                                return String(locationHref.call(locationObj));
                            } catch (e) {}
                        }
                        return '';
                    },
                    set: function(v) {
                        if (typeof locationAssign === 'function') {
                            locationAssign.call(locationObj, String(v));
                        } else if (typeof locationReplace === 'function') {
                            locationReplace.call(locationObj, String(v));
                        } else if (typeof locationHref === 'function') {
                            // 最后降级：不直接可赋值时尝试触发原始实现。
                            try { locationHref.call(locationObj, String(v)); } catch (e) {}
                        }
                    },
                    configurable: true
                });
                Object.defineProperty(locationObj, 'search', {
                    get: function() {
                        if (!locationParts) return '';
                        try {
                            var p = locationParts();
                            return p && p.search ? p.search : '';
                        } catch (e) { return ''; }
                    },
                    configurable: true
                });
                Object.defineProperty(locationObj, 'pathname', {
                    get: function() {
                        if (!locationParts) return '';
                        try {
                            var p = locationParts();
                            return p && p.pathname ? p.pathname : '';
                        } catch (e) { return ''; }
                    },
                    configurable: true
                });
                Object.defineProperty(locationObj, 'host', {
                    get: function() {
                        if (!locationParts) return '';
                        try {
                            var p = locationParts();
                            return p && p.host ? p.host : '';
                        } catch (e) { return ''; }
                    },
                    configurable: true
                });
                Object.defineProperty(locationObj, 'protocol', {
                    get: function() {
                        if (!locationParts) return '';
                        try {
                            var p = locationParts();
                            return p && p.protocol ? p.protocol : '';
                        } catch (e) { return ''; }
                    },
                    configurable: true
                });
                Object.defineProperty(locationObj, 'hash', {
                    get: function() {
                        if (!locationParts) return '';
                        try {
                            var p = locationParts();
                            return p && p.hash ? p.hash : '';
                        } catch (e) { return ''; }
                    },
                    configurable: true
                });
                locationObj.__compatPatched = true;
            } catch (e) {}
        }
    })();
    undefined;"#;
    ctx.eval(Source::from_bytes(js))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_ctx() -> Context {
        let mut ctx = Context::default();
        super::install_compat_shims(&mut ctx).expect("install compat shim");
        ctx
    }

    #[test]
    fn install_default_compat_shapes() {
        let mut ctx = setup_ctx();
        let has_require = ctx
            .eval(boa_engine::Source::from_bytes("typeof require"))
            .unwrap();
        assert_eq!(
            has_require.as_string().unwrap().to_std_string_escaped(),
            "function"
        );

        let has_dollar = ctx
            .eval(boa_engine::Source::from_bytes("typeof $"))
            .unwrap();
        assert_eq!(
            has_dollar.as_string().unwrap().to_std_string_escaped(),
            "function"
        );

        let has_jq = ctx
            .eval(boa_engine::Source::from_bytes("jQuery === $"))
            .unwrap();
        assert!(has_jq.as_boolean().unwrap());
    }

    #[test]
    fn define_is_noop_callable() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                "define(function(){return 1;}); typeof define",
            ))
            .unwrap();
        assert_eq!(r.as_string().unwrap().to_std_string_escaped(), "function");
    }

    #[test]
    fn jquery_chain_apis_are_tolerant() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                r#"
                var a = $('<div>');
                a.addClass('x').css('color', 'red').removeClass('x').attr('id', 'box').hasClass('x');
                "#,
            ))
            .unwrap();
        // 有些链式 API 可能返回空包装，不抛异常是核心行为。
        assert!(!r.as_boolean().unwrap());
    }

    #[test]
    fn jquery_each_runs_callback() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes(
                r#"
                var n = 0;
                $.each([1,2,3], function() { n++; });
                n;
                "#,
            ))
            .unwrap();
        assert_eq!(r.as_number().unwrap(), 3.0);
    }
}
