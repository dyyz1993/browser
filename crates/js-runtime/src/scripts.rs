//! Extract and execute `<script>` bodies from a DOM tree.
//!
//! M3.3 scope: walk a parsed tree, pull the text content of every
//! `<script>` element, then run them in order through a boa `Context`
//! (with the bridge installed). The DOM mutations made by JS become
//! visible to the subsequent layout + render passes.

use browser_dom::{NodeData, NodeId, Tree};

// M71.1: boa 类型仅在 --features boa 时可用。
#[cfg(feature = "boa")]
use boa_engine::{Context, JsValue, Module, Source};

// M71.1: bridge::install 是 boa 专属（注册所有 NativeFn bridge 函数）。
#[cfg(feature = "boa")]
use crate::bridge::install;

/// M-cls.2: 收紧 JS 运行时限制（纵深防御第二层）。
///
/// 历史 `main.js`(Next.js bundle) 在 boa 0.20 下 eval 时把循环迭代吃到
/// 250_000 上限仍未抛错，但期间分配了数 GB 内存触发 OOM。把上限降到 40_000
/// 既足够跑常见 SPA 的内联脚本（秒级几百次迭代的渲染逻辑），又能在 runaway
/// 循环早期抛 `loop iteration limit reached`，配合子进程内存护栏（M-cls.1）
/// 双保险。stack/recursion 也从 boa 默认(10240/512)收紧到 4096/256。
#[cfg(feature = "boa")]
const JS_LOOP_ITERATION_LIMIT: u64 = 40_000;
#[cfg(feature = "boa")]
const JS_STACK_SIZE_LIMIT: usize = 4096;
#[cfg(feature = "boa")]
const JS_RECURSION_LIMIT: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScriptEntry {
    Inline(String),
    External(String),
    /// M64: ESM module（`<script type="module">`）。URL 是绝对/可解析的 script src。
    ExternalModule(String),
    /// M64: 内联 module（`<script type="module">code</script>`）。
    InlineModule(String),
}

/// M79: Vite `import.meta.env` 生产构建缺省语义（`__vite_env__` 初始值）。
///
/// 背景：QuickJS 的 `import.meta` 不可赋（M76 发现），无法直接挂 `env` 属性，
/// 采用源码替换 `import.meta.env` → `__vite_env__` 模块级变量。Vite 生产构建的
/// `import.meta.env.VITE_*` 已被替换为静态值；运行时读**未定义键返回 undefined
/// 不抛错**（普通对象语义），`import.meta.env.MODE` 等取这里的缺省值。
///
/// 三处共用（单一事实来源）：
/// - `engine_quickjs::HttpLoader`（依赖模块，loader 路径）
/// - scripts.rs ExternalModule preamble（入口模块）
/// - `try_strip_esm_for_eval`（strip 退化路径）
pub(crate) const VITE_ENV_DEFAULT_JS: &str =
    "{MODE:'production',DEV:false,PROD:true,BASE_URL:'/',SSR:false}";

/// PERF-M80: 动态外链 script 的延迟加载标记（QuickJS shim ↔ pump 的私有约定）。
///
/// `Element.prototype.appendChild` 遇到带 src 的 `<script>` 时**不再同步
/// fetch**（webpack 一 tick 连挂多个 chunk 时会串行阻塞，react.dev 实测
/// 3 chunk 串行 ~6.6s），而是把 `DYN_URL_PREFIX + src` 经既有的
/// `__enqueueDynamicScript` 桥入队。pump（`drain_and_eval_dynamic_scripts`）
/// 展开标记：并行 fetch（去重 + MIME 强制）→ 按入队顺序 eval → eval 后
/// 触发该元素的 onload/onerror（经 `__dynPending` 注册表，保住
/// eval-before-onload 语义）。
///
/// U+0001 控制字符不可能出现在正常 JS 源码开头，不会与用户代码冲突。
/// 两侧字符串必须逐字符一致（shim 是 JS 字面量 `"\u0001DYNURL\u0001"`）。
pub(crate) const DYN_URL_PREFIX: &str = "\u{1}DYNURL\u{1}";

/// Collect the text content of every `<script>` element in `tree`,
/// in document order. Empty scripts are filtered out.
#[must_use]
pub fn extract_scripts(tree: &Tree) -> Vec<String> {
    extract_script_entries(tree)
        .into_iter()
        .filter_map(|entry| match entry {
            ScriptEntry::Inline(script) | ScriptEntry::InlineModule(script) => Some(script),
            ScriptEntry::External(_) | ScriptEntry::ExternalModule(_) => None,
        })
        .collect()
}

fn extract_script_entries(tree: &Tree) -> Vec<ScriptEntry> {
    let mut scripts = Vec::new();
    let mut stack: Vec<NodeId> = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let node = tree.get(id);
        if let NodeData::Element { tag, attrs } = &node.data {
            if tag.eq_ignore_ascii_case("script") {
                if !script_tag_has_executable_type(attrs) {
                    continue;
                }
                let is_module = script_is_module(attrs);
                if let Some(src) = script_src(attrs) {
                    if is_module {
                        scripts.push(ScriptEntry::ExternalModule(src));
                    } else {
                        scripts.push(ScriptEntry::External(src));
                    }
                    continue;
                }
                let mut text = String::new();
                for &child_id in &node.children {
                    if let NodeData::Text(s) = tree.data(child_id) {
                        text.push_str(s);
                    }
                }
                if !text.trim().is_empty() {
                    if is_module {
                        scripts.push(ScriptEntry::InlineModule(text));
                    } else {
                        scripts.push(ScriptEntry::Inline(text));
                    }
                }
                // Don't recurse into scripts (no nested scripts allowed by HTML5).
                continue;
            }
        }
        // Push children in reverse so left-most is popped first
        // (preserves document order).
        for &child in node.children.iter().rev() {
            stack.push(child);
        }
    }
    scripts
}

/// M64: 检测 `<script type="module">`。
fn script_is_module(attrs: &[(String, String)]) -> bool {
    for (name, value) in attrs {
        if name.eq_ignore_ascii_case("type") {
            let mut token = value.trim();
            if let Some((head, _)) = token.split_once(';') {
                token = head;
            }
            return token.trim().eq_ignore_ascii_case("module");
        }
    }
    false
}

/// M64: 检测源码是否有静态 ESM 语法（import/export，非动态 import()）。
/// 用于决定走 Module::parse 还是退化 ctx.eval。无静态 import/export 的 module
/// 文件（如 Nuxt 的 1.3MB 入口 bundle）退化 eval 可省 ~100MB 内存。
///
/// 注意：`import.meta` 虽然需要 Module 模式，但我们可以源码补丁替换成字符串，
/// 然后退化 eval（见 `strip_esm_syntax`）。
fn has_static_esm_syntax(code: &str) -> bool {
    regex_static_import(code) || regex_static_export(code)
}

/// M64: 对无静态 import/export 但有 import.meta 的 module 做源码补丁：
/// 把 `import.meta.url` 替换为页面 URL 字符串，使其能在 Script eval 模式运行。
/// 返回补丁后的源码。如果源码有静态 import/export（无法补丁），返回 None。
fn try_strip_esm_for_eval(code: &str, base_url: &str) -> Option<String> {
    // 有静态 import/export → 必须走 Module，无法退化
    if has_static_esm_syntax(code) {
        return None;
    }
    let mut patched = code.to_string();
    // 替换 import.meta.url → "页面URL"
    if patched.contains("import.meta.url") {
        let url = if base_url.is_empty() {
            "about:blank".to_string()
        } else {
            base_url.to_string()
        };
        patched = patched.replace("import.meta.url", &format!("\"{url}\""));
    }
    // 替换 import.meta.env → Vite 环境变量桩（M79: 统一用 VITE_ENV_DEFAULT_JS）
    if patched.contains("import.meta.env") {
        patched = patched.replace("import.meta.env", &format!("({VITE_ENV_DEFAULT_JS})"));
    }
    // 其他裸 import.meta → 替换为带 url 属性的对象（防 import.meta["url"] 等）
    if patched.contains("import.meta") {
        let url = if base_url.is_empty() {
            "about:blank".to_string()
        } else {
            base_url.to_string()
        };
        patched = patched.replace(
            "import.meta",
            &format!("({{url:\"{url}\",env:{VITE_ENV_DEFAULT_JS}}})"),
        );
    }
    Some(patched)
}

/// 检测静态 import 语句（排除动态 import()）。
/// 模式：`import{` 或 `import ` 或 `;import{` 等，后面不紧跟 `(`。
fn regex_static_import(code: &str) -> bool {
    let bytes = code.as_bytes();
    let mut i = 0;
    while i + 6 < bytes.len() {
        // 找 "import" 关键字
        if &bytes[i..i + 6] == b"import" {
            // 检查前一个字符（不是字母/数字/_，否则是 importXxx 属性名）
            let prev = if i > 0 { bytes[i - 1] } else { b' ' };
            if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'$' {
                i += 6;
                continue;
            }
            // 看后面：跳过空格
            let mut j = i + 6;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n') {
                j += 1;
            }
            if j >= bytes.len() {
                i += 6;
                continue;
            }
            // 静态 import: 后面是 {、引号（import "..."）、星号（import *）
            // 注意：后面是字母不一定是 import（可能是 importXxx 词如 imported）。
            // 但 `import x from "..."` 中 x 是合法标识符——只接受特定后续模式。
            // 动态 import: 后面是 (
            // 排除：import.meta（. 后面是 m）和 import/ （除法，/ 后面不是 ( { " ' *）
            if bytes[j] == b'(' || bytes[j] == b'/' {
                // 动态 import() 或除法操作 import/x → 跳过
            } else if bytes[j] == b'{' || bytes[j] == b'"' || bytes[j] == b'\'' || bytes[j] == b'*'
            {
                return true;
            } else if bytes[j] == b'.' {
                // import.meta —— 不视为静态 import（退化 eval + try_strip 处理）
                // import.meta —— 虽然是合法 ESM，但不需要 Module loader 解析依赖图。
                // 不视为静态 import（退化 eval 即可，eval 能跑 import.meta）。
                // 注意：eval 在 Script 模式不支持 import.meta 语法，所以仍需 Module。
                // 但 nuxt 的 import.meta 在 IIFE 内部，eval 也能跑（取决于上下文）。
                // 为安全起见，有 import.meta 也走 Module path。
                // → 不在此 return，让 import.meta 由调用方决定
            }
            // 不 return，继续搜（import x from 也算，但 minified 很少用）
        }
        i += 1;
    }
    false
}

/// 检测静态 export 语句。
fn regex_static_export(code: &str) -> bool {
    let bytes = code.as_bytes();
    let mut i = 0;
    while i + 6 < bytes.len() {
        if &bytes[i..i + 6] == b"export" {
            let prev = if i > 0 { bytes[i - 1] } else { b' ' };
            if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'$' {
                i += 6;
                continue;
            }
            let mut j = i + 6;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n') {
                j += 1;
            }
            if j >= bytes.len() {
                i += 6;
                continue;
            }
            // export { / export * — 只接受这些（export default/const/function 带空格，
            // 但 minified 代码几乎不用裸 export 声明）。
            // 注意：exported/exports 等词的 export 后面跟字母，不是 export 关键字。
            if bytes[j] == b'{' || bytes[j] == b'*' {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// M65: 从 Vite bundle 源码提取 __vite__mapDeps 的 chunk 列表，并行预取到缓存。
/// Vite 用 __vite__mapDeps 注册所有动态 import() 的 chunk 路径，eval 时 JS fetch()
/// 会串行请求它们。预取后 __fetchSync 命中缓存（0ms），避免 24 × 1s = 24s 串行。
#[allow(dead_code)]
fn prefetch_vite_chunks(code: &str, entry_url: &str) {
    // 找 __vite__mapDeps=(...m.f||(m.f=["./xxx.js","./yyy.js",...])
    let marker = ".f=[";
    let Some(start) = code.find(marker) else {
        return;
    };
    let arr_start = start + marker.len() - 1; // 指向 '['
                                              // 找匹配的 ']'
    let bytes = code.as_bytes();
    let mut depth = 0;
    let mut end = arr_start;
    for (i, &b) in bytes.iter().enumerate().skip(arr_start) {
        match b {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    if end <= arr_start {
        return;
    }
    let arr_str = &code[arr_start + 1..end];
    // 提取所有 "./xxx.js" 字符串
    let mut chunks: Vec<String> = Vec::new();
    let mut in_str = false;
    let mut cur = String::new();
    for ch in arr_str.chars() {
        if ch == '"' {
            if in_str && !cur.is_empty() {
                chunks.push(cur.clone());
                cur.clear();
            }
            in_str = !in_str;
        } else if in_str {
            cur.push(ch);
        }
    }
    if chunks.is_empty() {
        return;
    }
    // 把相对路径解析成绝对 URL
    let base_dir = entry_url
        .rfind('/')
        .map(|i| &entry_url[..i])
        .unwrap_or(entry_url);
    let urls: Vec<String> = chunks
        .iter()
        .filter_map(|c| c.strip_prefix("./").map(|s| format!("{base_dir}/{s}")))
        .collect();
    if !urls.is_empty() {
        crate::bridge::prefetch_to_cache(&urls);
    }
}

/// M64: 包装脚本为 IIFE + try/catch（复用现有 wrap 逻辑）。
#[cfg(feature = "boa")]
fn wrap_script(code: &str, label: &str) -> String {
    let mut wrapped = String::new();
    wrapped.push_str("(function(){\ntry{\n");
    wrapped.push_str(code);
    let catch_prefix = "\n}catch(__err){\nif(typeof __log === 'function'){\nvar __parts = [];\nvar __errObj = (__err !== null && __err !== undefined) ? __err : {};\nif (typeof __errObj.message === 'string') {\n    __parts.push('message=' + __errObj.message);\n} else if (typeof __errObj.toString === 'function') {\n    __parts.push('message=' + String(__errObj.toString()));\n} else {\n    __parts.push('message=' + String(__errObj));\n}\nif (typeof __errObj.stack !== 'undefined') __parts.push('stack=' + String(__errObj.stack));\n__log('[";
    wrapped.push_str(catch_prefix);
    wrapped.push_str(label);
    wrapped.push_str("] ' + __parts.join(' | '));\n}\n}\n})();\n");
    wrapped
}

/// M64: 从 URL 提取 origin（`scheme://host[:port]`）。
/// `https://vite.dev/assets/app.js` → `https://vite.dev`
fn url_origin(url: &str) -> String {
    // 简单解析：找 scheme:// 然后到下一个 /
    if let Some(scheme_end) = url.find("://") {
        let after_scheme = &url[scheme_end + 3..];
        let host_end = after_scheme.find('/').unwrap_or(after_scheme.len());
        return format!("{}://{}", &url[..scheme_end], &after_scheme[..host_end]);
    }
    url.to_string()
}

fn script_src(attrs: &[(String, String)]) -> Option<String> {
    attrs
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("src"))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn script_tag_has_executable_type(attrs: &[(String, String)]) -> bool {
    if attrs
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("nomodule"))
    {
        return false;
    }
    let mut script_type: Option<&str> = None;
    let mut language: Option<&str> = None;
    for (name, value) in attrs {
        if name.eq_ignore_ascii_case("type") {
            script_type = Some(value);
        } else if language.is_none() && name.eq_ignore_ascii_case("language") {
            language = Some(value);
        }
    }
    if let Some(ty) = script_type {
        is_js_script_type(ty)
    } else if let Some(lang) = language {
        is_js_script_type(lang)
    } else {
        true
    }
}

fn is_js_script_type(raw: &str) -> bool {
    let mut token = raw.trim();
    if token.is_empty() {
        return true;
    }
    if let Some((head, _)) = token.split_once(';') {
        token = head;
    }
    let token = token.trim();
    if token.eq_ignore_ascii_case("module") {
        return true;
    }
    if token.eq_ignore_ascii_case("text/javascript")
        || token.eq_ignore_ascii_case("application/javascript")
        || token.eq_ignore_ascii_case("text/ecmascript")
        || token.eq_ignore_ascii_case("application/ecmascript")
        || token.eq_ignore_ascii_case("application/x-javascript")
        || token.eq_ignore_ascii_case("text/jscript")
    {
        return true;
    }
    let token = token.to_ascii_lowercase();
    token.contains("javascript") || token.contains("ecmascript")
}

/// Run all `<script>` bodies in `tree` against `ctx`, with the bridge
/// installed so JS can mutate the DOM. Returns the count of scripts
/// that executed without throwing.
///
/// # Errors
/// Individual script errors are logged to stderr and don't abort the
/// run; the count returned reflects only successful executions.
#[cfg(feature = "boa")]
pub fn execute_scripts(tree_shared: &crate::bridge::SharedTree, ctx: &mut Context) -> usize {
    execute_scripts_with_base(tree_shared, ctx, None)
}

/// Same as [`execute_scripts`], but also installs a base URL used to
/// resolve relative URLs in `__fetchSetBody` / `__fetchAppendBody`.
#[cfg(feature = "boa")]
pub fn execute_scripts_with_base(
    tree_shared: &crate::bridge::SharedTree,
    ctx: &mut Context,
    base_url: Option<String>,
) -> usize {
    let limits = ctx.runtime_limits_mut();
    limits.set_loop_iteration_limit(JS_LOOP_ITERATION_LIMIT);
    limits.set_stack_size_limit(JS_STACK_SIZE_LIMIT);
    limits.set_recursion_limit(JS_RECURSION_LIMIT);
    let scripts: Vec<ScriptEntry> = {
        let borrowed = tree_shared.borrow();
        extract_script_entries(&borrowed)
    };
    let script_base_url = base_url.clone();
    let _guard = crate::bridge::install_shared_with_base(tree_shared.clone(), base_url);
    let mut executed = 0;
    let trace_scripts = std::env::var("BROWSER_TRACE_SCRIPTS").is_ok();
    let raw_script_mode = std::env::var("BROWSER_RAW_SCRIPT_ERRORS").is_ok();
    for (idx, script) in scripts.iter().enumerate() {
        let script_code = match script {
            ScriptEntry::Inline(code) => {
                if trace_scripts {
                    eprintln!("[js-runtime] script[{idx}] inline len={}", code.len());
                }
                Some((code.clone(), format!("inline[{idx}]")))
            }
            ScriptEntry::External(src) => match resolve_script_url(src, script_base_url.as_deref())
            {
                Some(url) => match fetch_external_script(&url) {
                    Ok(code) => {
                        // M62: docsify/Prism 兼容——source-level patch DFS 加 null guard。
                        // Prism.languages.DFS 遍历语言定义时对 null 属性值调 objId 崩。
                        // 在 r=n[a] 后加 if(null===r)continue; 跳过 null 属性。
                        let final_code = if url.contains("docsify") {
                            // M62: Prism DFS 对 null 属性值调 objId 崩。
                            code
                                .replace("t[u(r)]", "t[r?u(r):0]")
                                .replace("(c=R.util.type(r))", "(c=R.util.type(r||0))")
                                // M62: docsify 事件注册 on(e,n,i) 对 null 元素调 addEventListener 崩。
                                // 在 ternary 前加 null guard。
                                .replace(
                                    "o(n)?window.addEventListener(e,n):e.addEventListener(n,i)",
                                    "null===e||void 0===e||o(n)?window.addEventListener(e,n):e.addEventListener(n,i)",
                                )
                        } else {
                            code
                        };
                        if trace_scripts {
                            eprintln!(
                                "[js-runtime] script[{idx}] external {url} len={}",
                                final_code.len()
                            );
                        }
                        Some((final_code, format!("external[{idx}] {url}")))
                    }
                    Err(e) => {
                        eprintln!("[js-runtime] external script fetch failed: {url}: {e}");
                        None
                    }
                },
                None => {
                    eprintln!("[js-runtime] external script skipped: {src}");
                    None
                }
            },
            // M64: ESM module。先 fetch 源码检测有无静态 import/export。
            // 有 → 走 Module::parse（boa 自动链接依赖图）。
            // 无 → 退化用 ctx.eval（省去 Module 系统的 ~100MB 内存开销，对超大 bundle 关键）。
            ScriptEntry::ExternalModule(src) => {
                match resolve_script_url(src, script_base_url.as_deref()) {
                    Some(url) => {
                        if trace_scripts {
                            eprintln!("[js-runtime] script[{idx}] ESM module {url}");
                        }
                        match fetch_external_script(&url) {
                            Ok(code) => {
                                // M64 优化：无静态 import/export 的 module 尝试源码补丁退化 eval。
                                // 对超大 bundle（如 Nuxt 1.3MB）可省 ~100MB Module parser 内存。
                                let base = script_base_url.as_deref().unwrap_or("");
                                if let Some(patched) = try_strip_esm_for_eval(&code, base) {
                                    // M65: Vite chunk 预取——提取 __vite__mapDeps 的 chunk 列表，
                                    // 并行 fetch 到 bridge 缓存，让后续 __fetchSync 命中缓存。
                                    // 这些 chunk 是 Vite 动态 import() 的懒加载组件，
                                    // eval 时 JS fetch() 会串行请求它们（~1s/个）。
                                    // 预取后 __fetchSync 直接返回缓存（0ms）。
                                    prefetch_vite_chunks(&patched, &url);
                                    // 退化 eval 成功
                                    if trace_scripts {
                                        eprintln!(
                                            "[js-runtime] script[{idx}] ESM → eval fallback (import.meta patched) {url} len={}",
                                            code.len()
                                        );
                                    }
                                    let wrapped =
                                        wrap_script(&patched, &format!("module[{idx}] {url}"));
                                    match ctx.eval(Source::from_bytes(wrapped.as_bytes())) {
                                        Ok(_) => executed += 1,
                                        Err(e) => {
                                            eprintln!(
                                                "[js-runtime] script[{idx}] module eval {url} error: {e}"
                                            );
                                        }
                                    }
                                } else {
                                    // 有静态 import/export → 走 Module API
                                    if let Some(loader) = ctx
                                        .downcast_module_loader::<crate::esm_loader::HttpModuleLoader>()
                                    {
                                        match loader.load_and_eval(&url, ctx) {
                                            Ok(_) => {
                                                executed += 1;
                                                if trace_scripts {
                                                    eprintln!("[js-runtime] script[{idx}] ESM module OK (Module path)");
                                                }
                                            }
                                            Err(e) => {
                                                eprintln!(
                                                    "[js-runtime] script[{idx}] ESM module {url} error: {e:?}"
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("[js-runtime] ESM module fetch failed: {url}: {e}");
                            }
                        }
                        continue;
                    }
                    None => {
                        eprintln!("[js-runtime] ESM module skipped: {src}");
                        continue;
                    }
                }
            }
            // M64: 内联 module（`<script type="module">code</script>`）。
            ScriptEntry::InlineModule(code) => {
                if trace_scripts {
                    eprintln!(
                        "[js-runtime] script[{idx}] inline module len={}",
                        code.len()
                    );
                }
                // 写临时文件，用 Module::parse（Module 模式接受 import/export/import.meta）
                let temp_file = std::env::temp_dir().join(format!(
                    "browser_inline_module_{idx}_{}.mjs",
                    std::process::id()
                ));
                if std::fs::write(&temp_file, code).is_ok() {
                    let source = Source::from_filepath(&temp_file);
                    if let Ok(source) = source {
                        if let Ok(module) = Module::parse(source, None, ctx) {
                            let promise = module.load_link_evaluate(ctx);
                            let _ = ctx.run_jobs();
                            match promise.state() {
                                boa_engine::builtins::promise::PromiseState::Fulfilled(_) => {
                                    executed += 1;
                                }
                                boa_engine::builtins::promise::PromiseState::Rejected(err) => {
                                    let msg = err
                                        .to_string(ctx)
                                        .map(|s| s.to_std_string_escaped())
                                        .unwrap_or_default();
                                    eprintln!(
                                        "[js-runtime] script[{idx}] inline module error: {msg}"
                                    );
                                }
                                _ => {}
                            }
                            let _ = std::fs::remove_file(&temp_file);
                            continue;
                        }
                    }
                    let _ = std::fs::remove_file(&temp_file);
                }
                continue;
            }
        };
        let Some((script_code, script_label)) = script_code else {
            continue;
        };
        if raw_script_mode {
            if trace_scripts {
                eprintln!("[js-runtime] script[{idx}] {script_label} raw eval start");
            }
            match ctx.eval(Source::from_bytes(script_code.as_bytes())) {
                Ok(_) => {
                    executed += 1;
                    if trace_scripts {
                        eprintln!("[js-runtime] script[{idx}] {script_label} raw eval end");
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[js-runtime] script[{idx}] {script_label} raw eval error: {:?}",
                        e
                    );
                }
            }
            if trace_scripts {
                eprintln!("[js-runtime] script[{idx}] eval end");
            }
            continue;
        }
        let mut wrapped_code = String::new();
        wrapped_code.push_str("(function(){\ntry{\n");
        wrapped_code.push_str(&script_code);
        let catch_prefix = "\n}catch(__err){\nif(typeof __log === 'function'){\nvar __parts = [];\nvar __errObj = (__err !== null && __err !== undefined) ? __err : {};\nif (typeof __errObj.message === 'string') {\n    __parts.push('message=' + __errObj.message);\n} else if (typeof __errObj.toString === 'function') {\n    __parts.push('message=' + String(__errObj.toString()));\n} else {\n    __parts.push('message=' + String(__errObj));\n}\nif (typeof __errObj.name !== 'undefined') __parts.push('name=' + String(__errObj.name));\nif (typeof __errObj.fileName !== 'undefined') __parts.push('fileName=' + String(__errObj.fileName));\nif (typeof __errObj.lineNumber !== 'undefined') __parts.push('lineNumber=' + String(__errObj.lineNumber));\nif (typeof __errObj.columnNumber !== 'undefined') __parts.push('columnNumber=' + String(__errObj.columnNumber));\nif (typeof __errObj.stack !== 'undefined') __parts.push('stack=' + String(__errObj.stack));\nif (typeof __errObj.constructor === 'function' && __errObj.constructor.name) __parts.push('constructor=' + String(__errObj.constructor.name));\ntry { __parts.push('type=' + (typeof __errObj)); } catch(e) {}\ntry { if (typeof __errObj === 'object' && __errObj !== null) { __parts.push('errKeys=' + String(Object.keys(__errObj))); __parts.push('errToJSON=' + String(JSON.stringify(__errObj))); } } catch(e) {}\ntry { if (typeof __errObj.toString === 'function') __parts.push('errToString=' + String(__errObj.toString())); } catch(e) {}\n__log('[";
        wrapped_code.push_str(catch_prefix);
        wrapped_code.push_str(&script_label);
        wrapped_code.push_str("] ' + __parts.join(' | '));\n}\n}\n})();\n//# sourceURL=");
        wrapped_code.push_str(&script_label);
        wrapped_code.push('\n');
        if trace_scripts {
            eprintln!("[js-runtime] script[{idx}] eval start");
        }
        match ctx.eval(Source::from_bytes(wrapped_code.as_bytes())) {
            Ok(_) => executed += 1,
            Err(e) => {
                eprintln!("[js-runtime] script[{idx}] {script_label} eval error: {e}");
            }
        }
        if trace_scripts {
            eprintln!("[js-runtime] script[{idx}] eval end");
        }
    }
    // M16.3: pump the event loop. 执行完所有 script 后，drain 到期 timer
    // 回调，回调可能 schedule 新 timer（或本身 schedule），重复直到 idle。
    // 防死循环：最多迭代 MAX_TICKS 次（防止 setTimeout 无限递归卡死爬虫）。
    let t0 = std::time::Instant::now();
    executed += pump_event_loop(ctx);
    if std::env::var("BROWSER_TRACE_SCRIPTS").is_ok() {
        eprintln!(
            "[profile] pump_event_loop #1 took {:.2}s (invoked={executed})",
            t0.elapsed().as_secs_f64()
        );
    }
    // M62: dispatch DOMContentLoaded + load 事件。SPA 框架（React/Vue/jQuery）
    // 常在 document.addEventListener('DOMContentLoaded', init) 里初始化，
    // 爬虫场景脚本执行完即可视为 DOM 就绪。best-effort，失败不阻断。
    let _ = ctx.eval(boa_engine::Source::from_bytes(
        r#"
        try {
            if (typeof document !== 'undefined' && typeof document.dispatchEvent === 'function') {
                var ev1 = (typeof Event === 'function') ? new Event('DOMContentLoaded') : { type: 'DOMContentLoaded' };
                document.dispatchEvent(ev1);
                var ev2 = (typeof Event === 'function') ? new Event('load') : { type: 'load' };
                document.dispatchEvent(ev2);
                if (typeof window !== 'undefined' && typeof window.dispatchEvent === 'function') {
                    window.dispatchEvent(ev2);
                }
            }
        } catch(e) {}
        "#,
    ));
    // dispatch 后可能 schedule 了新 timer（框架初始化逻辑），再 pump 一次。
    let t1 = std::time::Instant::now();
    executed += pump_event_loop(ctx);
    if std::env::var("BROWSER_TRACE_SCRIPTS").is_ok() {
        eprintln!(
            "[profile] pump_event_loop #2 took {:.2}s",
            t1.elapsed().as_secs_f64()
        );
    }
    let _ = t1;
    // M65: JS eval + event loop 完成后，手动触发 GC 释放 AST/字节码/临时对象。
    // 插桩数据显示 100% 的内存增长在 JS eval 阶段（boa JS heap）。
    // force_collect 释放 eval 后不再引用的编译产物。
    boa_engine::gc::force_collect();
    executed
}

fn resolve_script_url(src: &str, base_url: Option<&str>) -> Option<String> {
    if let Ok(url) = url::Url::parse(src) {
        return Some(url.to_string());
    }
    if let Some(base) = base_url {
        if let Ok(base) = url::Url::parse(base) {
            if let Ok(url) = base.join(src) {
                return Some(url.to_string());
            }
        }
    }
    None
}

/// PERF-M80: 从模块源码扫描静态 import 说明符（宽松扫描）。
///
/// 命中形态：`from"x"` / `from 'x'` / `import"x"` / `import 'x'` /
/// `import("x")` / `import ('x')`（覆盖 Vite/Nuxt 压缩产物）。
///
/// 宽松的代价是可能扫出假说明符（字符串字面量里的 `from"` 等）——
/// 后果只是一个 404 的投机请求（被忽略），无语义影响；漏扫的模块走
/// 原有的 loader 串行 fetch，也无回归。保序去重交由调用方。
#[cfg(feature = "quickjs")]
fn scan_import_specifiers(code: &str) -> Vec<String> {
    let mut specs: Vec<String> = Vec::new();
    let mut push = |spec: &str| {
        let s = spec.trim();
        // 过滤明显不是模块路径的（空串 / 含空白 / 含 <> 的注入形态）。
        if !s.is_empty() && !s.chars().any(char::is_whitespace) && !s.contains('<') {
            specs.push(s.to_string());
        }
    };
    let bytes = code.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';

    // `from"..."`：要求 from 前是非标识符字符（排除 removeFrom" 这类）。
    let mut i = 0usize;
    while i + 4 <= bytes.len() {
        if &bytes[i..i + 4] == b"from" && (i == 0 || !is_word(bytes[i - 1])) {
            let mut j = i + 4;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
                let q = bytes[j];
                if let Some(end) = code[j + 1..].find(q as char) {
                    push(&code[j + 1..j + 1 + end]);
                    i = j + 1 + end;
                    continue;
                }
            }
        }
        i += 1;
    }

    // `import"..."` / `import("...")`：要求 import 前是非标识符字符。
    let mut i = 0usize;
    while i + 6 <= bytes.len() {
        if &bytes[i..i + 6] == b"import" && (i == 0 || !is_word(bytes[i - 1])) {
            let mut j = i + 6;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
                let q = bytes[j];
                if let Some(end) = code[j + 1..].find(q as char) {
                    push(&code[j + 1..j + 1 + end]);
                    i = j + 1 + end;
                    continue;
                }
            } else if j < bytes.len() && bytes[j] == b'(' {
                let mut k = j + 1;
                while k < bytes.len() && (bytes[k] as char).is_whitespace() {
                    k += 1;
                }
                if k < bytes.len() && (bytes[k] == b'"' || bytes[k] == b'\'') {
                    let q = bytes[k];
                    if let Some(end) = code[k + 1..].find(q as char) {
                        push(&code[k + 1..k + 1 + end]);
                        i = k + 1 + end;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    specs
}

/// PERF-M80: 解析模块说明符为绝对 URL（镜像 engine_quickjs HttpResolver 的
/// 规则；bare specifier 打包器已内联，返回 None 跳过）。
#[cfg(feature = "quickjs")]
fn resolve_module_specifier(base_url: &str, name: &str) -> Option<String> {
    if name.starts_with("http://") || name.starts_with("https://") {
        return Some(name.to_string());
    }
    if let Ok(base) = url::Url::parse(base_url) {
        if let Ok(full) = base.join(name) {
            return Some(full.to_string());
        }
    }
    None
}

/// PERF-M80: 模块依赖图 BFS 并行预取。
///
/// 背景：nuxt.com 的 ESM 依赖图有 **119 个模块**，loader
/// （engine_quickjs HttpLoader::load → bridge::fetch_sync）按 import 发现
/// 顺序**串行**拉取，每个一次网络往返，合计 ~75s。
///
/// 本函数从入口模块出发，扫描源码静态 import（`scan_import_specifiers`）、
/// 解析为绝对 URL、逐层并行 fetch 进 SCRIPT_CACHE。loader 的 fetch_sync
/// （M80 起会查 SCRIPT_CACHE）随后全部命中缓存 → 串行链变成 0ms。
///
/// 预算：MAX_MODULES / MAX_DEPTH / 单波 FETCH_BATCH=48（66 模块一波拉完，
/// 避免小批串行把波次拉长）。运行时（数据驱动）才会发现的动态 import 无法
/// 静态预取——那些仍由 loader 串行拉，属可接受残余。
///
/// 语义安全：fetch 路径与结果内容完全不变（loader 仍走 fetch_sync，缓存
/// 命中返回同一份字节）；投机请求最坏情况是几个 404 被忽略。
#[cfg(feature = "quickjs")]
fn prefetch_module_graph(entry_urls: &[String], trace: bool, t0: std::time::Instant) {
    const MAX_MODULES: usize = 256;
    const MAX_DEPTH: usize = 6;
    const FETCH_BATCH: usize = 48;
    let _ = t0;
    let mut seen: std::collections::HashSet<String> = entry_urls.iter().cloned().collect();
    let mut frontier: Vec<String> = Vec::new();
    // 入口模块体已在 prefetch_urls 阶段拉好（在 SCRIPT_CACHE）。
    for url in entry_urls {
        if script_cache()
            .lock()
            .ok()
            .map(|c| c.contains_key(url.as_str()))
            .unwrap_or(false)
        {
            frontier.push(url.clone());
        }
    }
    // 并行 fetch 一组 URL（fetch_external_script 自带缓存/MIME/写回）。
    fn fetch_batch(urls: &[String]) {
        for batch in urls.chunks(FETCH_BATCH) {
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            let handles: Vec<_> = batch
                .iter()
                .map(|url| {
                    let url = url.clone();
                    let tx = tx.clone();
                    std::thread::spawn(move || {
                        let _ = fetch_external_script(&url);
                        let _ = tx.send(());
                    })
                })
                .collect();
            drop(tx);
            for _ in rx.iter() {}
            for h in handles {
                let _ = h.join();
            }
        }
    }

    for _depth in 0..MAX_DEPTH {
        if frontier.is_empty() || seen.len() >= MAX_MODULES {
            break;
        }
        // M82: 全局 deadline 到 → 停止 BFS 预取。
        if crate::bridge::js_deadline_exceeded() {
            if trace {
                eprintln!("[js-runtime] global JS deadline exceeded — stop module graph prefetch");
            }
            break;
        }
        // 扫描当前层的 import 说明符，解析入 next。
        // 注意 base 用**被扫模块自身的 URL**（HttpResolver 语义：相对说明符
        // 相对导入模块所在目录解析，不是页面 URL）。
        let mut next: Vec<String> = Vec::new();
        for url in &frontier {
            let code = script_cache().lock().ok().and_then(|c| c.get(url).cloned());
            let Some(code) = code else { continue };
            for spec in scan_import_specifiers(&code) {
                if let Some(abs) = resolve_module_specifier(url, &spec) {
                    if !should_skip_script(&abs)
                        && !seen.contains(&abs)
                        && seen.insert(abs.clone())
                        && seen.len() <= MAX_MODULES
                    {
                        next.push(abs);
                    }
                }
            }
        }
        if next.is_empty() {
            break;
        }
        if trace {
            eprintln!(
                "[js-runtime] M80 module graph depth {}: prefetching {} modules",
                _depth + 1,
                next.len()
            );
        }
        fetch_batch(&next);
        frontier = next;
    }
}

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
/// M70.13: 脚本 fetch 用全局复用 HttpClient（避免每脚本新建 TLS 连接）。
static SCRIPT_FETCH_CLIENT: OnceLock<browser_net::HttpClient> = OnceLock::new();

/// M70.13: 外链脚本源码缓存（URL → JS 源码）。
/// 同一个 URL（如 cdn.jsdelivr.net/npm/docsify@4）只需 fetch 一次，
/// 后续请求直接从内存读。避免重复网络请求 + TLS 握手。
static SCRIPT_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn script_cache() -> &'static Mutex<HashMap<String, String>> {
    SCRIPT_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// M70.14: 公开 script_cache 给 serve 主进程用于预取。
pub fn script_cache_public() -> &'static Mutex<HashMap<String, String>> {
    script_cache()
}

/// 预先初始化全局 HttpClient。在事件循环开始前调用，避免首脚本延迟。
pub(crate) fn ensure_script_client() {
    SCRIPT_FETCH_CLIENT.get_or_init(browser_net::HttpClient::new);
    script_cache();
}

/// M4: 收集所有 <script> 并逐一 eval（同步，按文档顺序）。
fn fetch_external_script(url: &str) -> Result<String, String> {
    // M70.13: 先查内存缓存——同一个 CDN URL 只 fetch 一次。
    if let Ok(cache) = script_cache().lock() {
        if let Some(code) = cache.get(url) {
            return Ok(code.clone());
        }
    }
    // M82: 全局 deadline 已到 → 跳过剩余脚本网络请求（错误只打日志，不致命）。
    if crate::bridge::js_deadline_exceeded() {
        return Err("global JS deadline exceeded (script fetch skipped)".to_string());
    }

    // M65: 外部脚本用独立 spawn + 新 HttpClient（而非 net worker）。
    // M70.13: 改用全局 OnceLock+HttpClient，TLS 连接池跨脚本复用。
    // M70.13: 给 fetch 加 8s 硬超时——CDN 偶尔慢不能让整个进程卡死。
    let url_owned = url.to_string();
    let handle = std::thread::spawn(move || {
        let client = SCRIPT_FETCH_CLIENT.get_or_init(browser_net::HttpClient::new);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("tokio runtime build failed: {e}"))?;
        // 用 tokio::time::timeout 给 fetch 加上限（GitHub rspack chunk 最大 258KB）。
        // M82: 上限收紧到 min(30s, 距全局 deadline 剩余)。
        let per_fetch = crate::bridge::js_deadline_remaining()
            .unwrap_or(std::time::Duration::from_secs(30))
            .min(std::time::Duration::from_secs(30));
        let result = rt.block_on(async {
            tokio::time::timeout(per_fetch, client.get_with_headers(&url_owned, None)).await
        });
        match result {
            Ok(Ok((bytes, headers))) => {
                // M78: 脚本 MIME 强制（对齐浏览器）：非 JS MIME 的 classic script 不执行。
                let mime = headers
                    .iter()
                    .find(|(k, _)| k.as_str().eq_ignore_ascii_case("content-type"))
                    .and_then(|(_, v)| v.to_str().ok())
                    .map(str::to_string);
                if !script_mime_executable(mime.as_deref()) {
                    return Err(format!("blocked script MIME: {}", mime.unwrap_or_default()));
                }
                String::from_utf8(bytes).map_err(|e| format!("non-utf8 response: {e}"))
            }
            Ok(Err(e)) => Err(format!("{e:?}")),
            Err(_) => Err(format!("fetch timeout (30s): {url_owned}")),
        }
    });
    let code = handle
        .join()
        .map_err(|_| "external script fetch thread panicked".to_string())??;

    // M70.13: 写入缓存，后续相同 URL 直接命中。
    if let Ok(mut cache) = script_cache().lock() {
        cache.insert(url.to_string(), code.clone());
    }
    Ok(code)
}

/// M78: classic script 可执行的 MIME 集。JS 系列 + 缺失头可执行；
/// text/html / text/plain 历史兼容可加载（WPT block-mime-as-script 断言）。
/// 其余（text/csv、audio/*、video/*、image/* 等）阻止执行。
fn script_mime_executable(mime: Option<&str>) -> bool {
    let Some(m) = mime else {
        return true;
    };
    let base = m.split(';').next().unwrap_or("").trim().to_lowercase();
    matches!(
        base.as_str(),
        "application/javascript"
            | "text/javascript"
            | "application/x-javascript"
            | "application/ecmascript"
            | "text/ecmascript"
            | "text/html"
            | "text/plain"
    )
}

/// M78: 动态 script（appendChild）加载前的 MIME 检查。
/// 放行时把 body 写入 script cache（后续 __fetchSync 命中，只发一次请求）。
///
/// PERF-M80: 原实现自带一份 fetch（缓存 key 用**原始 URL**，而 __fetchSync 用
/// **绝对 URL** 查 FETCH_CACHE——key 不匹配导致同一 chunk 被完整下载两次，
/// react.dev 实测 6 次 fetch_sync 共 17.6s）。现统一委托 `fetch_external_script`
///（MIME 强制 + 绝对 URL key + 写 SCRIPT_CACHE），配合 `fetch_sync` 的
/// SCRIPT_CACHE 只读查询，同一 URL 全程只发一次请求。
#[cfg(feature = "quickjs")]
pub fn fetch_script_mime_ok(url: String) -> bool {
    let resolved = crate::bridge::resolve_url(&url);
    fetch_external_script(&resolved).is_ok()
}

/// M16.3: Drain due timer callbacks until the wheel is idle or the
/// M66: QuickJS TypeScript 检测——QuickJS 不支持 TS 语法。
/// M69: 移出 quickjs feature 门控——boa pump 的动态 script drain 也用它跳过 TS chunk。
/// M78: 探测前先剥离注释——WPT testharness.js 的文档注释含 `interface TestEnvironment`
/// 被子串匹配误杀（整个 script 静默跳过）。注释里提到 TS 关键字的普通 JS 必须照常执行。
fn has_ts_syntax(code: &str) -> bool {
    let stripped = strip_js_comments(code);
    // M78.140: `: void` 收紧到返回位置（`): void`）——对象字面量的
    // `{ toString: void 0 }` 是合法 JS 值语义，裸子串曾整脚本误杀
    // （test262 String/prototype/search S15.5.4.12_A1_T9）。
    if regex_is_match(&stripped, r"\)\s*:\s*void\b") {
        return true;
    }
    stripped.contains(": string")
        || stripped.contains(": number")
        || stripped.contains(": boolean")
        || stripped.contains(": any")
        || stripped.contains(" as const")
        || stripped.contains(": ReturnType<")
        || (stripped.contains(": \"") && stripped.contains(" | "))
        || stripped.contains("interface ")
}

/// M78.140: has_ts_syntax 用的轻量正则（避免为启发式引入 regex crate——
/// 只支持 \s \b 和字面量，够用）。
fn regex_is_match(text: &str, _pattern: &str) -> bool {
    // 手写：找 ")" 后跳过空白，期望 ": void"。
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b')' {
            let mut j = i + 1;
            while j < b.len() && (b[j] == b' ' || b[j] == b'\t' || b[j] == b'\n' || b[j] == b'\r') {
                j += 1;
            }
            if text[j..].starts_with(": void") {
                let after = j + ": void".len();
                let next_ok = after >= text.len()
                    || !(text.as_bytes()[after].is_ascii_alphanumeric()
                        || text.as_bytes()[after] == b'_');
                if next_ok {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

/// M78: 剥离 JS 源码的行/块注释（保守状态机）。仅用于 TS 启发式探测：
/// 不识别正则字面量（`/a\/\/b/` 尾部 `//` 可能被当注释起点），误隐藏
/// 少量内容的代价远小于现状的误杀正常 JS。
fn strip_js_comments(code: &str) -> String {
    let b = code.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    // 0 普通 | 1 单引号串 | 2 双引号串 | 3 模板串 | 4 行注释 | 5 块注释
    // | 6 正则字面量（M78.140）
    let mut state = 0u8;
    while i < b.len() {
        let c = b[i];
        match state {
            1..=3 => {
                // M78.9: 字符串内容与注释同等置空——TS 探测只看代码结构。
                // 字符串里的 "interface " / ": string" 字样（WPT Event-constants
                // 的 "Event interface object" 描述串）曾让整脚本被误杀。
                if c == b'\\' && i + 1 < b.len() {
                    i += 2;
                    continue;
                }
                let quote = match state {
                    1 => b'\'',
                    2 => b'"',
                    _ => b'`',
                };
                if c == quote {
                    out.push(b' '); // 保占位（字符串边界仍可辨）
                    state = 0;
                }
                i += 1;
            }
            4 => {
                if c == b'\n' {
                    out.push(c);
                    state = 0;
                }
                i += 1;
            }
            5 => {
                if c == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
                    out.push(b' ');
                    i += 2;
                    state = 0;
                } else {
                    i += 1;
                }
            }
            6 => {
                // 正则字面量内部：`\/` 转义、`[...]` 字符类里 `/` 不闭合、
                // 裸换行 = 非法正则（回退普通态——按行内字面量约定）。
                // 内容置空（同字符串），防止 `/a\/\/b/` 尾部 `//` 被当注释。
                if c == b'\\' && i + 1 < b.len() {
                    i += 2;
                    continue;
                }
                if c == b'[' {
                    i += 1;
                    while i < b.len() && b[i] != b']' {
                        if b[i] == b'\\' && i + 1 < b.len() {
                            i += 1;
                        }
                        i += 1;
                    }
                    i += 1;
                    continue;
                }
                if c == b'/' {
                    out.push(b' ');
                    state = 0;
                    i += 1;
                    continue;
                }
                if c == b'\n' {
                    out.push(c);
                    state = 0;
                    i += 1;
                    continue;
                }
                i += 1;
            }
            _ => {
                if c == b'\'' || c == b'"' || c == b'`' {
                    state = if c == b'\'' {
                        1
                    } else if c == b'"' {
                        2
                    } else {
                        3
                    };
                    out.push(c);
                    i += 1;
                } else if c == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
                    state = 4;
                    i += 2;
                } else if c == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
                    state = 5;
                    i += 2;
                } else if c == b'/' && regex_can_start(&out) {
                    // M78.140: 前一 token 启发式——`(,=:[!&|?{};+-*%~^<>` 后或
                    // return/typeof 等关键词后的 `/` 是正则起点（否则是除法）。
                    state = 6;
                    i += 1;
                } else {
                    out.push(c);
                    i += 1;
                }
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// M78.140: `/` 是否可能是正则起点——看已输出部分的最后一个非空白字符。
fn regex_can_start(out: &[u8]) -> bool {
    let mut j = out.len();
    while j > 0 {
        let c = out[j - 1];
        if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
            j -= 1;
            continue;
        }
        return matches!(
            c,
            b'(' | b','
                | b'='
                | b':'
                | b'['
                | b'!'
                | b'&'
                | b'|'
                | b'?'
                | b'{'
                | b'}'
                | b';'
                | b'+'
                | b'-'
                | b'*'
                | b'%'
                | b'~'
                | b'^'
                | b'<'
                | b'>'
        );
    }
    true // 输入开头
}

/// M66: 跳过分析/追踪脚本 + TypeScript 文件
#[cfg(feature = "quickjs")]
fn should_skip_script(url: &str) -> bool {
    url.contains("usefathom.com")
        || url.contains("cloudflareinsights.com")
        || url.contains("google-analytics")
        || url.contains("googletagmanager")
        || url.contains("doubleclick.net")
        || url.contains("facebook.net")
        || url.contains("sentry.io")
        || url.contains("hotjar")
        || url.contains("fullstory")
        || url.ends_with(".ts")
        || url.contains(".ts?")
}

/// M69: 取出动态 script 队列里的所有代码，逐个 eval。
///
/// 由 `run_scripts_quickjs` 的 event loop pump 每轮调用。appendChild(script)
/// 的 JS shim 检测到 script 标签后，把代码（inline textContent）入队；这里取出
/// 用 `eval_user_script` 执行（GC 安全，CaughtError 在 ctx.with 闭包内 drop）。
///
/// eval 出的代码可能又 appendChild 新 script 入队，下一轮 pump 处理（多层链式加载）。
///
/// 返回本轮执行的 script 数（用于 pump 判断是否还有进展）。
///
/// PERF-M80: 外链 script 由 shim 入队 `DYN_URL_PREFIX + src` 标记（不再在
/// appendChild 内同步 fetch）。本函数展开标记：
/// 1. 收集全部标记 URL（按解析后的绝对 URL 去重）；
/// 2. 并行 fetch（M75 模式：一线程一 URL，共享连接池；`fetch_external_script`
///    自带缓存 + MIME 强制）；
/// 3. 按入队顺序 eval（fetch 并行、执行串行，保住 chunk 注册顺序）；
/// 4. 每条 URL 处理完立刻触发该元素的 onload/onerror——JS shim 在
///    `window.__dynPending[rawSrc]` 挂了回调，成败状态在
///    `window.__dynStatus[rawSrc]`（'loaded'/'failed'）。触发时机从
///    "pump eval 之后" 保证 eval-before-onload（链式加载时 shim 侧
///    setTimeout(0) 会早于下一轮 eval，故改由 pump 侧触发）。
#[cfg(feature = "quickjs")]
fn drain_and_eval_dynamic_scripts(engine: &mut crate::engine_quickjs::QuickJsEngine) -> usize {
    let codes = crate::bridge::drain_dynamic_scripts();
    let n = codes.len();
    if n == 0 {
        return 0;
    }

    // 1. 收集标记（rawSrc → 绝对 URL 映射；同一绝对 URL 只 fetch 一次）。
    let mut marks: Vec<(usize, String, String)> = Vec::new(); // (idx, raw_src, resolved)
    for (i, code) in codes.iter().enumerate() {
        if let Some(raw) = code.strip_prefix(DYN_URL_PREFIX) {
            let resolved = crate::bridge::resolve_url(raw);
            marks.push((i, raw.to_string(), resolved));
        }
    }

    // 2. 并行 fetch 未命中的 URL（上限 16 并发，分批；已缓存的命中零成本）。
    let mut results: std::collections::HashMap<String, Result<String, String>> =
        std::collections::HashMap::new();
    {
        let mut unique: Vec<String> = marks
            .iter()
            .map(|(_, _, resolved)| resolved.clone())
            .collect();
        unique.sort();
        unique.dedup();
        // fetch_external_script 自带缓存查询（命中零网络）+ MIME 强制 + 写缓存。
        const FETCH_BATCH: usize = 16;
        for batch in unique.chunks(FETCH_BATCH) {
            let __t_wave = std::time::Instant::now();
            let (tx, rx) = std::sync::mpsc::channel::<(String, Result<String, String>)>();
            let handles: Vec<_> = batch
                .iter()
                .map(|url| {
                    let url = url.clone();
                    let tx = tx.clone();
                    std::thread::spawn(move || {
                        let r = fetch_external_script(&url);
                        let _ = tx.send((url, r));
                    })
                })
                .collect();
            drop(tx);
            for (url, r) in rx.iter() {
                results.insert(url, r);
            }
            for h in handles {
                let _ = h.join();
            }
            if std::env::var("BROWSER_TRACE_SCRIPTS").is_ok() {
                eprintln!(
                    "[js-runtime] pump fetch wave: {} urls in {}ms",
                    batch.len(),
                    __t_wave.elapsed().as_millis()
                );
            }
        }
    }

    // 3 + 4. 按入队顺序处理：标记 URL → eval + 触发 pending；普通代码 → eval。
    for (i, code) in codes.into_iter().enumerate() {
        let mark = marks.iter().find(|(mi, _, _)| *mi == i);
        if let Some((_, raw, resolved)) = mark {
            let raw = raw.clone();
            let resolved = resolved.clone();
            match results.get(&resolved) {
                Some(Ok(body)) => {
                    // 与原 shim 语义对齐：
                    // - 空响应体（404 空页等）→ 原 `if (!code)` → onerror；
                    // - 含 < 和 > 的"chunk"（HTML 404 页等）不是 ES 规范产物
                    //   （原 JSX 检查）→ onerror 而非 eval。
                    if body.is_empty() || (body.contains('<') && body.contains('>')) {
                        fire_dyn_pending(engine, &raw, false);
                        continue;
                    }
                    match engine.eval_user_script(body) {
                        Ok(()) => {}
                        Err(e) => {
                            let err_str = e.to_string();
                            if !err_str.contains("token in expression: '<'")
                                && !err_str.contains("unexpected token")
                            {
                                eprintln!("[js] [quickjs dynamic] {e}");
                            }
                        }
                    }
                    fire_dyn_pending(engine, &raw, true);
                }
                _ => {
                    // fetch 失败（404/网络错误/MIME 被禁）→ onerror。
                    fire_dyn_pending(engine, &raw, false);
                }
            }
        } else {
            // M71.4: 用户 script 用 sloppy mode（eval_user_script），
            // 兼容 SvelteKit 等框架的裸全局赋值（`__sveltekit_x = {}`）。
            match engine.eval_user_script(&code) {
                Ok(()) => {}
                Err(e) => {
                    // M75: JSX 语法（<tag>）不是 ES 规范产物，Chrome/V8 也不解析。
                    // QuickJS 解析含 < 的表达式时会报 unexpected token '<'。
                    // 这是构建时转换（Babel/SWC）的产物，不是运行时缺口。
                    let err_str = e.to_string();
                    if !err_str.contains("token in expression: '<'")
                        && !err_str.contains("unexpected token")
                    {
                        eprintln!("[js] [quickjs dynamic] {e}");
                    }
                }
            }
        }
    }
    n
}

/// PERF-M80: 在 JS 侧记录 URL 成败并触发该元素的 onload/onerror 回调
///（shim 在 `__dynPending[rawSrc]` 挂的闭包）。rawSrc 里的 `\` 和 `'`
/// 转义后嵌入 JS 字符串字面量（同 ws 事件派发的转义方式）。
#[cfg(feature = "quickjs")]
fn fire_dyn_pending(engine: &mut crate::engine_quickjs::QuickJsEngine, raw_src: &str, ok: bool) {
    let esc = raw_src.replace('\\', "\\\\").replace('\'', "\\'");
    let status = if ok { "loaded" } else { "failed" };
    let js = format!(
        "try{{window.__dynStatus=window.__dynStatus||{{}};window.__dynStatus['{esc}']='{status}';}}catch(e){{}}\
         try{{var __f=(window.__dynPending||{{}})['{esc}'];if(typeof __f==='function'){{try{{__f()}}catch(e2){{}}}}}}catch(e1){{}}\
         try{{delete window.__dynPending['{esc}'];}}catch(e3){{}}"
    );
    let _ = engine.eval_safe(&js);
}

/// M93: QuickJS 主入口 = **文档导航循环**。
///
/// 每跳流程：创建独立引擎（每页全新 JS 全局空间——导航即销毁旧文档 JS
/// 环境，`window.__anubisBooted` 之类的全局不得泄漏到下一页）→ 单页执行
/// （[`run_page_quickjs`]）→ 检查文档导航。若 JS 触发了 `location.href = X`
/// / `assign` / `replace`（非 hash、非 pushState），单页执行已经用
/// [`bridge::fetch_navigation_document`] 把目标文档取回（cookie jar 逐跳
/// 传递），这里换 DOM 树、更新 base_url，进入下一跳。上限 [`MAX_NAV_HOPS`]
/// 跳防导航环。
///
/// M93.4: storage 与 JS 全局空间不同——同源跳复用同一 StorageHandle
/// （localStorage/sessionStorage 跨页保留，对齐真实浏览器语义），跨源跳
/// 新建。首跳总是新建。
///
/// 真实场景：Anubis PoW 挑战页解完 `location.replace(pass-challenge?...)`
/// → Set-Cookie + 302 回原页 → 真身文档（Nitter SSR 时间线）。
#[cfg(feature = "quickjs")]
fn run_scripts_quickjs(
    shared: crate::bridge::SharedTree,
    mut base_url: Option<String>,
    engine_kind: &crate::engine::EngineKind,
    post_exprs: &[String],
) -> (crate::bridge::SharedTree, usize) {
    let mut executed_total = 0usize;
    // M93.4: storage 句柄跨跳管理。真实浏览器语义：同源导航后 localStorage /
    // sessionStorage 都保留（session 作用域是标签页不是文档）；跨源导航才是
    // 全新 storage。每跳开始前比较下一跳 origin 与当前页 origin，同源则复用
    // 上一跳的 Rc 句柄（TreeGuard::drop 只清 thread_local slot，不清句柄
    // 本身），跨源/首跳才 `new_storage()`。
    let mut prev_storage: Option<browser_storage::StorageHandle> = None;
    let mut prev_origin: Option<String> = None;
    for hop in 0..=MAX_NAV_HOPS {
        // M64/M93: 每页预扫描 ESM module 脚本（决定引擎是否带 HttpModuleLoader）。
        let has_module = {
            let borrowed = shared.borrow();
            extract_script_entries(&borrowed).iter().any(|e| {
                matches!(
                    e,
                    ScriptEntry::ExternalModule(_) | ScriptEntry::InlineModule(_)
                )
            })
        };
        let origin = base_url
            .as_deref()
            .map(url_origin)
            .unwrap_or_else(|| "about:blank".to_string());
        let esm_origin = if has_module {
            Some(origin.as_str())
        } else {
            None
        };
        let engine = engine_kind.create(esm_origin);
        let storage = match (&prev_storage, &prev_origin) {
            (Some(handle), Some(po)) if *po == origin => handle.clone(),
            _ => browser_storage::new_storage(),
        };
        prev_storage = Some(storage.clone());
        prev_origin = Some(origin);
        let (executed, next) = run_page_quickjs(
            shared.clone(),
            base_url.clone(),
            engine,
            storage,
            post_exprs,
        );
        executed_total += executed;
        match next {
            Some((url, html)) if hop < MAX_NAV_HOPS => {
                eprintln!(
                    "[nav] M93 document navigation hop {}/{}: {url}",
                    hop + 1,
                    MAX_NAV_HOPS
                );
                *shared.borrow_mut() = browser_html_parser::parse(&html);
                base_url = Some(url);
            }
            Some((url, _)) => {
                eprintln!(
                    "[nav] M93 max navigation hops ({MAX_NAV_HOPS}) reached, staying on {url}"
                );
                break;
            }
            None => break,
        }
    }
    (shared, executed_total)
}

/// M93: 文档导航循环跳数上限。5 跳足够覆盖 挑战页→pass-challenge→真身
/// 以及一层短跳转链；超出按导航环处理（保留当前 DOM 返回）。
#[cfg(feature = "quickjs")]
const MAX_NAV_HOPS: usize = 5;

/// M93: 单页 QuickJS 执行（[`run_scripts_quickjs`] 导航循环的一跳）。
/// 安装 bridge（已在 engine 内部完成）+ JS shim + eval 脚本 + event loop。
/// M81: `post_exprs` —— 页面脚本 + 事件循环跑完后在同一会话内按序 eval 的
/// 表达式（`--click` 合成点击用；addEventListener 监听器注册在会话内
/// `__elCache` 缓存的元素包装上，引擎 drop 即失效，点击必须留在本会话）。
/// M93.4: `storage` 由导航循环层传入（同源跳复用句柄，跨源跳新建）——
/// 本函数只负责 install；TreeGuard::drop 清 slot 后下一跳重新 install。
/// 返回 (executed 数, 待跟进的文档导航 (url, html))——None = 本页无导航。
#[cfg(feature = "quickjs")]
fn run_page_quickjs(
    shared: crate::bridge::SharedTree,
    base_url: Option<String>,
    mut engine_box: Box<dyn crate::engine::JsEngine>,
    storage: browser_storage::StorageHandle,
    post_exprs: &[String],
) -> (usize, Option<(String, String)>) {
    // M93: 每页开始清空导航队列（防上一页/上一次 run 的残留串页）。
    crate::bridge::reset_pending_navigation();
    // downcast 到 QuickJsEngineWrapper（需要 &mut）
    let wrapper: &mut crate::engine_quickjs::QuickJsEngineWrapper = (*engine_box)
        .as_any_mut()
        .downcast_mut::<crate::engine_quickjs::QuickJsEngineWrapper>()
        .expect("engine_name was quickjs but type mismatch");
    let engine = wrapper.engine();

    // 安装 thread_local DOM 后端
    let _guard = crate::bridge::install_shared_with_base(shared.clone(), base_url.clone());
    crate::bridge::install_storage(storage);
    let initial_url = base_url
        .clone()
        .unwrap_or_else(|| "about:blank".to_string());
    let nav = browser_navigation::new_navigation(&initial_url);
    crate::bridge::install_navigation(nav);
    crate::bridge::ensure_cookie_jar();

    // M70.13: 预初始化脚本 fetch 的 HttpClient（避免首脚本冷启动延迟）。
    ensure_script_client();

    let mut executed = 0;

    // 安装 JS shim——所有 shim 拼接成一个大字符串一次 eval。
    // QuickJS 的 ctx.eval 每次是独立 scope，var 声明不跨 eval 泄漏。
    // 必须拼接后一次执行，让 window/document 等 var 在后续 eval 中可见。
    let shims = get_all_shim_js(&base_url);
    let combined_shim: String = shims
        .iter()
        .map(|(_, js)| js.as_str())
        .collect::<Vec<_>>()
        .join("\n;\n");
    // M78.128-debug: eval_safe 打印异常 message（eval 的 Debug 格式只有 Exception）。
    if let Err(e) = engine.eval_safe(&combined_shim) {
        eprintln!("[js-runtime] QuickJS combined shim install failed: {e}");
        // M78.36-debug: 逐段定位 + 段内二分找首个失败行。
        for (name, js) in &shims {
            if let Err(se) = engine.eval_safe(js) {
                eprintln!("[js-runtime] shim 段 [{name}] 失败: {se}");
                let lines: Vec<&str> = js.split('\n').collect();
                let (mut lo, mut hi) = (0usize, lines.len() - 1);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    if engine.eval(&lines[..=mid].join("\n")).is_ok() {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                eprintln!(
                    "[js-runtime] [{name}] 首个失败行 ≈ {}: {}",
                    hi + 1,
                    lines[hi].trim()
                );
            }
        }
    }
    // M66: 设置裸全局变量——QuickJS 的 globalThis.xxx 不会被解析为裸变量 xxx。
    // 用 eval 设置 var 让后续 eval 能用裸 document/window/navigator 等。
    let _ = engine.eval(
        r#"var document = globalThis.document;
        var navigator = globalThis.navigator;
        var location = globalThis.location;
        var history = globalThis.history;
        var localStorage = globalThis.localStorage;
        var sessionStorage = globalThis.sessionStorage;
        var Event = globalThis.Event;
        var CustomEvent = globalThis.CustomEvent;
        var URL = globalThis.URL;
        var URLSearchParams = globalThis.URLSearchParams;
        var fetch = globalThis.fetch;
        var setTimeout = globalThis.setTimeout;
        var clearTimeout = globalThis.clearTimeout;
        var setInterval = globalThis.setInterval;
        var clearInterval = globalThis.clearInterval;
        var requestAnimationFrame = globalThis.requestAnimationFrame;
        var queueMicrotask = globalThis.queueMicrotask;
        var atob = globalThis.atob;
        var btoa = globalThis.btoa;
        var crypto = globalThis.crypto;
        var self = globalThis;
        var Worker = globalThis.Worker;
        "#,
    );

    // M66: QuickJS —— 逐个 eval script（shim 已经通过初始 eval 设置了 globalThis 属性）
    let scripts = {
        let borrowed = shared.borrow();
        extract_script_entries(&borrowed)
    };
    let __t_scripts_start = std::time::Instant::now();

    // M75: 并行预取所有 external module 到 cache，避免串行 timeout。
    // GitHub 152 个 rspack chunk 通过 globalThis 共享 registry，
    // 并行 fetch 后 cache 命中 = 0ms，eval 时不会缺模块。
    //
    // PERF-M80: 预取范围扩大到 pass-2 的 `<script src>` 外链。原实现在 pass-2
    // 循环里逐个 `fetch_external_script`（串行，每条完整 TLS 往返）——react.dev
    // 9 条外链脚本串行 56s，是「脚本阶段 61.7s」的主体。并行预取后 pass-2
    // 全部 cache 命中，墙钟时间 = max(单请求) 而非 sum(所有请求)。
    {
        let mut prefetch_urls: Vec<String> = Vec::new();
        let mut module_entry_urls: Vec<String> = Vec::new();
        for s in &scripts {
            match s {
                ScriptEntry::ExternalModule(src) => {
                    if let Some(url) = resolve_script_url(src, base_url.as_deref()) {
                        prefetch_urls.push(url.clone());
                        module_entry_urls.push(url);
                    }
                }
                ScriptEntry::External(src) => {
                    if let Some(url) = resolve_script_url(src, base_url.as_deref()) {
                        // 与 pass-2 一致：跳过 analytics 类（今天也不会 fetch，
                        // 预取它们等于新增请求）。
                        if !should_skip_script(&url) {
                            prefetch_urls.push(url);
                        }
                    }
                }
                _ => {}
            }
        }
        prefetch_urls.sort();
        prefetch_urls.dedup();
        if !prefetch_urls.is_empty() {
            let module_count = prefetch_urls.len();
            if std::env::var("BROWSER_TRACE_SCRIPTS").is_ok() {
                eprintln!("[js-runtime] M75+M80 pre-fetching {module_count} external scripts");
            }
            // M82: 预取墙钟预算 = min(180s, 全局 JS deadline 剩余)。
            // 原来固定 180s 是 juejin 挂 4 分钟的主体之一。
            let prefetch_budget = crate::bridge::js_deadline_remaining()
                .unwrap_or(std::time::Duration::from_secs(180))
                .min(std::time::Duration::from_secs(180));
            let deadline = std::time::Instant::now() + prefetch_budget;
            let handles: Vec<_> = prefetch_urls
                .into_iter()
                .filter_map(|url| {
                    if std::time::Instant::now() >= deadline {
                        return None;
                    }
                    Some(std::thread::spawn(move || {
                        let _ = fetch_external_script(&url);
                    }))
                })
                .collect();
            for h in handles {
                let _ = h.join();
            }
            if std::env::var("BROWSER_TRACE_SCRIPTS").is_ok() {
                eprintln!(
                    "[js-runtime] M75+M80 pre-fetch done ({:?})",
                    __t_scripts_start.elapsed()
                );
            }
            // PERF-M80: 模块依赖图 BFS 预取——nuxt.com 119 个依赖模块原本由
            // loader 串行 fetch（~75s）；预取后 loader 的 fetch_sync 全部命中
            // SCRIPT_CACHE（见 bridge::fetch_sync）。
            if !module_entry_urls.is_empty() {
                let trace = std::env::var("BROWSER_TRACE_SCRIPTS").is_ok();
                prefetch_module_graph(&module_entry_urls, trace, __t_scripts_start);
                if trace {
                    eprintln!(
                        "[js-runtime] M80 module graph prefetch done ({:?})",
                        __t_scripts_start.elapsed()
                    );
                }
            }
        }
    }

    // M75: 两遍执行——先 module（注册 registry / rspack/Webpack），后 non-module。
    // GitHub 的 inline auto-executing script 需要 rspack registry 先就绪。
    // Chrome 语义：<script type="module"> 是 defer，在所有 non-module 脚本后执行，
    // 但它们按 HTML 顺序在 DOMContentLoaded 前完成。inline script 依赖 module registry。
    // Pass 1: module scripts（先注册所有 rspack/Webpack chunk registry）
    for script in &scripts {
        // M82: 全局 deadline 到 → 放弃剩余脚本（返回当前已渲染内容）。
        if crate::bridge::js_deadline_exceeded() {
            eprintln!(
                "[js-runtime] global JS deadline exceeded — skipping remaining module scripts"
            );
            break;
        }
        let code = match script {
            ScriptEntry::ExternalModule(src) => {
                match resolve_script_url(src, base_url.as_deref()) {
                    Some(url) => {
                        if should_skip_script(&url) {
                            continue;
                        }
                        match fetch_external_script(&url) {
                            Ok(code) => {
                                // M76: 备份原 code（eval_module_with_imports 可能包装 code）
                                let raw_code = code;
                                // M76: 用 has_static_esm_syntax（稳健正则）替代脆字符串检测。
                                // Vite 用绝对路径 from "/@react-refresh"——from"./" 检测漏了。
                                let has_static = has_static_esm_syntax(&raw_code);
                                if has_static {
                                    // M77: 先尝试 Module::eval（触发 Loader → 模块作用域执行）
                                    // Vite 的 ESM module 在模块作用域内正确执行所有依赖链。
                                    // 修补 import.meta.env（QuickJS 不可赋，Loader 只处理依赖模块，
                                    // 主模块 raw_code 是 scripts.rs 直接 fetch 的，不走 Loader patch）
                                    // 原始 raw_code 保留给 strip 路径用
                                    // 修补 import.meta.env / import.meta.hot
                                    //（QuickJS 模块作用域不可写 meta 属性）
                                    // __vite_hot_stub__ + __vite_env__ 必须提前定义
                                    let preamble = format!(
                                        "if(typeof __vite_hot_stub__==='undefined')var __vite_hot_stub__={{accept:function(){{}},dispose:function(){{}},on:function(){{}},decline:function(){{}},invalidate:function(){{}},data:{{}}}};\nif(typeof __vite_env__==='undefined')var __vite_env__={VITE_ENV_DEFAULT_JS};\n"
                                    );
                                    let patched = preamble.to_string()
                                        + &raw_code
                                            .replace("import.meta.env", "__vite_env__")
                                            .replace("import.meta.hot", "__vite_hot_stub__");
                                    if engine.eval_module_with_imports(&url, &patched).is_ok() {
                                        // ESM module 已成功——模块自身代码（含 React createRoot 等）
                                        // 已在模块作用域执行并修改 DOM，跳过 strip 路径。
                                        // strip 路径会在全局作用域重跑代码，导致依赖引用碎裂。
                                        executed += 1;
                                        continue;
                                    } else {
                                        eprintln!(
                                            "[js-runtime] module eval (strip fallback): {url}"
                                        );
                                    }
                                    // M76ter→M79: 仅当模块 eval 失败时，strip import/export 后 eval 为普通 script
                                    // preamble 用 VITE_ENV_DEFAULT_JS（完整 Vite 生产语义）
                                    let mut stripped = String::new();
                                    for line in raw_code.lines() {
                                        let t = line.trim_start();
                                        if t.starts_with("import ")
                                            || t.starts_with("import{")
                                            || t.starts_with("import*")
                                            || t.starts_with("export ")
                                            || t.starts_with("export{")
                                            || t.starts_with("export*")
                                        {
                                            // M76fin: __vite__cjsImport → registry lookup
                                            if t.contains("__vite__cjsImport") {
                                                if let Some(var_name) = t.split_whitespace().nth(1)
                                                {
                                                    // Extract URL from from "..."
                                                    let val = if let Some(q) = t.find("from \"") {
                                                        let u = &t[q + 6..];
                                                        let e = u.find('\"').unwrap_or(0);
                                                        let url = &u[..e];
                                                        format!(
                                                            "window.__vite_ns_registry__?.[\"http://localhost:5173{url}\"]||__react_stub()"
                                                        )
                                                    } else {
                                                        "__react_stub()".to_string()
                                                    };
                                                    stripped.push_str(&format!(
                                                        "var {var_name}={val};\n"
                                                    ));
                                                }
                                            }
                                            continue;
                                        }
                                        let mut l = if t.contains("import.meta.url") {
                                            line.replace(
                                                "import.meta.url",
                                                "\"http://localhost:5173/\"",
                                            )
                                        } else if t.contains("import.meta.env") {
                                            line.replace("import.meta.env", "__vite_env__")
                                        } else if t.contains("import.meta.hot") {
                                            line.replace("import.meta.hot", "__vite_hot_stub__")
                                        } else {
                                            line.to_string()
                                        };
                                        // M76fin: inline import → registry lookup
                                        if l.contains("import __vite__cjsImport") {
                                            if let Some(idx) = l.find("import __vite__cjsImport") {
                                                let rest = &l[idx..];
                                                if let Some(var_start) =
                                                    rest.split_whitespace().nth(1)
                                                {
                                                    let var_name =
                                                        var_start.trim_end_matches(|c: char| {
                                                            !c.is_alphanumeric() && c != '_'
                                                        });
                                                    let before = &l[..idx];
                                                    let val = if let Some(q) = rest.find("from \"")
                                                    {
                                                        let u = &rest[q + 6..];
                                                        let e = u.find('\"').unwrap_or(0);
                                                        let url = &u[..e];
                                                        format!(
                                                            "window.__vite_ns_registry__?.[\"http://localhost:5173{url}\"]||__react_stub()"
                                                        )
                                                    } else {
                                                        "__react_stub()".to_string()
                                                    };
                                                    l = format!("{before}var {var_name}={val};");
                                                }
                                            }
                                        }
                                        stripped.push_str(&l);
                                        stripped.push('\n');
                                    }
                                    // M76fin4: 移除 export{...}（solidjs 行尾 ESM export）
                                    let stripped = {
                                        let mut s = String::with_capacity(stripped.len());
                                        let b = stripped.as_bytes();
                                        let mut i = 0;
                                        while i < b.len() {
                                            if i + 7 <= b.len() && &b[i..i + 7] == b"export{"
                                                || (i + 9 <= b.len() && &b[i..i + 9] == b"}export{")
                                            {
                                                let start = if &b[i..i + 7] == b"export{" {
                                                    i + 7
                                                } else {
                                                    i + 9
                                                };
                                                // skip everything between export{ and matching }
                                                let mut depth = 1;
                                                let mut j = start;
                                                while j < b.len() && depth > 0 {
                                                    if b[j] == b'{' {
                                                        depth += 1;
                                                    } else if b[j] == b'}' {
                                                        depth -= 1;
                                                    }
                                                    j += 1;
                                                }
                                                // skip trailing ;
                                                while j < b.len() && b[j] == b';' {
                                                    j += 1;
                                                }
                                                // If the export{ was at start of segment (after }), only skip the export{ part
                                                if start == i + 9 {
                                                    s.push('}');
                                                }
                                                i = j;
                                            } else {
                                                s.push(b[i] as char);
                                                i += 1;
                                            }
                                        }
                                        s
                                    };
                                    let eval_code = format!(
                                        "if(typeof __react_stub==='undefined')function __react_stub(){{var r={{}};['jsxDEV','jsxs','Fragment','StrictMode','createElement','createRoot','useState','useEffect','useRef','useMemo','useCallback','useContext','useReducer','forwardRef','lazy','memo','createRef','hydrateRoot','render','createPortal','unmountComponentAtNode'].forEach(function(k){{r[k]=function(){{return null}}}});r['createRoot']=function(root){{return{{render:function(e){{}}}}}};return r;}}
                                         window.__vite_plugin_react_preamble_installed__=true;
\
                                         if(typeof __vite_env__===\'undefined\')var __vite_env__={{}};
\
                                         if(typeof __vite_hot_stub__===\'undefined\')var __vite_hot_stub__={{}};
\
                                         if(typeof createRoot===\'undefined\')var createRoot=function(r){{return{{render:function(e){{r.textContent=\'[React stub]\'}}}}}};
\
                                         {stripped}"
                                    );
                                    match engine.eval_user_script(&eval_code) {
                                        Ok(_) => executed += 1,
                                        Err(e2) => eprintln!("[js-runtime] module eval (strip) failed: {url}: {e2:#}"),
                                    }
                                    continue;
                                }
                                let base = base_url.as_deref().unwrap_or("");
                                match try_strip_esm_for_eval(&raw_code, base) {
                                    Some(p) => Some(p),
                                    None => Some(raw_code),
                                }
                            }
                            Err(e) => {
                                eprintln!("[js-runtime] QuickJS module fetch failed: {url}: {e}");
                                continue;
                            }
                        }
                    }
                    None => continue,
                }
            }
            ScriptEntry::InlineModule(code) => {
                // M77: inline module 有 import → 走 Module::declare + eval（正确解析命名导出）。
                // 老路径 strip import 后当普通 script eval，`import { hello }` 的 hello 未定义。
                if code.contains("import ") || code.contains("import{") {
                    let module_url = base_url.as_deref().unwrap_or("about:blank").to_string()
                        + "?inline="
                        + &executed.to_string();
                    if engine.eval_module_with_imports(&module_url, code).is_ok() {
                        executed += 1;
                        continue;
                    }
                    // 失败时回退到 strip 路径
                }
                // M75+M76: 检测静态 import { / import{ / import *，Vite minified 无空格。
                let has_static_import = code.contains("import {")
                    || code.contains("import *")
                    || code.contains("import{");
                if has_static_import {
                    // M76bis: 不跳过——strip import 行 + injectIntoGlobalHook 后 eval（$RefreshReg$ 设置）
                    let cleaned: Vec<&str> = code
                        .lines()
                        .filter(|l| {
                            let t = l.trim_start();
                            !t.starts_with("import {")
                                && !t.starts_with("import{")
                                && !t.contains("injectIntoGlobalHook")
                        })
                        .collect();
                    if cleaned.is_empty() {
                        continue;
                    }
                    let refined = cleaned.join("\n");
                    Some(refined)
                } else {
                    let base = base_url.as_deref().unwrap_or("");
                    match try_strip_esm_for_eval(code, base) {
                        Some(p) => Some(p),
                        None => Some(code.clone()),
                    }
                }
            }
            _ => continue,
        };
        if let Some(code) = code {
            match engine.eval_user_script(&code) {
                Ok(_) => executed += 1,
                Err(e) => {
                    let err_str = e.to_string();
                    // M75: JSX 级联错误（not a function / property of undefined）
                    // 非 ES 规范缺口——根因是 JSX chunk 解析失败，Chrome/V8 也不解析。
                    if !err_str.contains("not a function")
                        && !err_str.contains("cannot read property")
                        && !err_str.contains("token in expression: '<'")
                        && !err_str.contains("unexpected token")
                    {
                        eprintln!("[js] [quickjs] {e}");
                    }
                }
            }
        }
    }
    // Pass 2: non-module scripts（inline + external，在 module registry 就绪后执行）
    for script in &scripts {
        // M82: 全局 deadline 到 → 放弃剩余脚本（返回当前已渲染内容）。
        if crate::bridge::js_deadline_exceeded() {
            eprintln!("[js-runtime] global JS deadline exceeded — skipping remaining scripts");
            break;
        }
        let code = match script {
            ScriptEntry::Inline(code) => {
                if has_ts_syntax(code) {
                    continue;
                }
                Some(code.clone())
            }
            ScriptEntry::External(src) => match resolve_script_url(src, base_url.as_deref()) {
                Some(url) => {
                    if should_skip_script(&url) {
                        continue;
                    }
                    match fetch_external_script(&url) {
                        Ok(code) => {
                            if has_ts_syntax(&code) {
                                continue;
                            }
                            Some(code)
                        }
                        Err(e) => {
                            eprintln!("[js-runtime] QuickJS fetch failed: {url}: {e}");
                            continue;
                        }
                    }
                }
                None => continue,
            },
            _ => continue,
        };
        if let Some(code) = code {
            // M83-debug: 逐脚本 trace（BROWSER_TRACE_SCRIPTS=1 时打印 eval 起点，
            // 定位同步 spin 的脚本——错误栈的 eval_script 名不含来源）。
            let trace_scripts = std::env::var("BROWSER_TRACE_SCRIPTS").is_ok();
            let label = match script {
                ScriptEntry::Inline(_) => "inline".to_string(),
                ScriptEntry::External(src) | ScriptEntry::ExternalModule(src) => src.clone(),
                _ => "?".to_string(),
            };
            if trace_scripts {
                eprintln!("[trace] eval start: {label}");
            }
            let eval_start = std::time::Instant::now();
            match engine.eval_user_script(&code) {
                Ok(_) => executed += 1,
                Err(e) => {
                    eprintln!("[js-runtime] QuickJS eval failed ({label}): {e}");
                }
            }
            if trace_scripts {
                let ms = eval_start.elapsed().as_millis();
                if ms > 500 {
                    eprintln!("[trace] eval SLOW: {label} took {ms}ms");
                }
            }
        }
    }

    // M92: DOMContentLoaded/load 派发前移一轮 drain——脚本期（onload 前）排队
    // 的跨条目遍历（history.go(-1)）触发的 popstate 必须先于 load 事件
    //（WPT 007.html "popstate event should fire before onload fires"；
    // 旧序 load 同步派发在 drain 之前，popstate 永远晚到）。
    // M66-fix: 必须先 drain Promise microtask（.then 回调），再 drain timer（setTimeout）。
    // 标准 JS 语义：同一 tick 内 microtask 优先级高于 macrotask。
    // 否则 setTimeout(0) 回调跑得比 Promise.then 早，拿不到 then 准备的数据。
    // M69: 首轮 drain 动态 script——初始 script 执行时（如 webpack runtime）可能
    // 已经 appendChild(script) 入队了 chunk。必须在 timer drain 之前 eval 它们：
    // appendChild 时同时入队了 script 代码和 onload 的 setTimeout(0)，
    // 必须先 eval script（设置 window.__xxx 等），onload 回调读这些状态才正确。
    let mut dyn_executed = drain_and_eval_dynamic_scripts(engine);
    executed += dyn_executed;

    engine.run_jobs();
    let _ = engine.eval_i32("__drainDueTimers()");

    // dispatch DOMContentLoaded/load
    let __t_dcl_start = std::time::Instant::now();
    let _ = engine.eval(
        r#"try {
            // M93.8: DCL 前 readyState → 'interactive'，load 派发后 → 'complete'。
            if (typeof globalThis.__docReadyState !== 'undefined') { globalThis.__docReadyState = 'interactive'; }
            if (typeof document !== 'undefined' && typeof document.dispatchEvent === 'function') {
                var ev1 = new Event('DOMContentLoaded');
                document.dispatchEvent(ev1);
                if (typeof globalThis.__docReadyState !== 'undefined') { globalThis.__docReadyState = 'complete'; }
                var ev2 = new Event('load');
                document.dispatchEvent(ev2);
                if (typeof window !== 'undefined' && typeof window.dispatchEvent === 'function') {
                    window.dispatchEvent(ev1);
                    window.dispatchEvent(ev2);
                }
                // M78.14: 静态 iframe 的 load 派发——WPT iframe 页在
                // iframe.onload 里跑断言（子文档 script 执行超目标，属性近似）。
                // M78.133: 同源 iframe 子页脚本 same-realm 执行——WPT storage
                // 事件系列（~14 个）依赖子页 setItem 在父窗口触发 StorageEvent。
                // 跨 realm 基建被拒（非目标），same-realm 近似：取 src（或
                // srcdoc）里的内联 <script> 在当前 realm eval，执行期间打
                // __inIframeScript 标记——__fireStorage 只在标记时派发（规范：
                // storage 事件不回发起变更的同一窗口；此前父页自己的
                // localStorage.clear() 抢发 key=null 干扰断言顺序）。
                if (typeof __qsAll === 'function') {
                    var _ifrIds = __qsAll('iframe');
                    (_ifrIds || '').split(',').forEach(function(_sid) {
                        if (!_sid) return;
                        var _nid = parseInt(_sid, 10);
                        setTimeout(function() {
                            try {
                                var _el = __makeElement(_nid);
                                try {
                                    var _childHtml = null;
                                    // M92: 子文档 URL（src 解析为绝对地址；srcdoc
                                    // 继承父页 URL）——__fireStorage 的 event.url 源。
                                    var _frameUrl = location.href;
                                    var _src = __getAttr(_nid, 'src');
                                    if (_src && typeof __fetchSync === 'function') {
                                        var _abs = _src;
                                        try { _abs = new URL(_src, location.href).href; } catch (pe) {}
                                        _frameUrl = _abs;
                                        var _okOrigin = false;
                                        try {
                                            var _u = new URL(_abs), _p = new URL(location.href);
                                            _okOrigin = (_u.protocol + '//' + _u.host) === (_p.protocol + '//' + _p.host);
                                        } catch (pe2) {}
                                        if (_okOrigin && _abs.indexOf('http') === 0) {
                                            try { _childHtml = __fetchSync(_abs); } catch (fe) {}
                                        }
                                    } else {
                                        var _sd = __getAttr(_nid, 'srcdoc');
                                        if (_sd) _childHtml = _sd;
                                    }
                                    if (_childHtml) {
                                        window.__iframeSrcUrl = _frameUrl;
                                        var _re = /<script[^>]*>([\s\S]*?)<\/script>/gi;
                                        var _m;
                                        while ((_m = _re.exec(_childHtml)) !== null) {
                                            if (_m[1] && _m[1].trim()) {
                                                window.__inIframeScript = true;
                                                try { (0, eval)(_m[1]); } catch (ce) {} finally {
                                                    window.__inIframeScript = false;
                                                }
                                            }
                                        }
                                        window.__iframeSrcUrl = null;
                                    }
                                } catch (ie) {}
                                var _on = _el.onload;
                                if (typeof _on === 'function') _on.call(_el, { type: 'load', target: _el });
                                if (_el.__listeners && _el.__listeners['load']) {
                                    for (var _i = 0; _i < _el.__listeners['load'].length; _i++) {
                                        try { _el.__listeners['load'][_i].call(_el, { type: 'load', target: _el }); } catch (e) {}
                                    }
                                }
                            } catch (e) {}
                        }, 0);
                    });
                }
            }
        } catch(e) {}"#,
    );
    eprintln!(
        "[serve] scripts: {}ms, dcl: {}ms",
        __t_scripts_start.elapsed().as_millis(),
        __t_dcl_start.elapsed().as_millis()
    );

    // 所有脚本执行完后，循环触发 setTimeout/setInterval 回调，
    // 直到 pending timer 清空或动态 script 队列清空，或超时。
    // M70.13: 事件循环参数——缩短超时 + 更快 idle 退出。
    // M78.8: BROWSER_EL_MAX_MS 环境变量可配（默认 2s）。评分 harness 设
    // 12s——testharness 页内 10s harness timeout 才能真正触发，死测试
    // 产出规范的 TIMEOUT 状态而非 no-results（语义对齐 WPT）。
    const EL_TICK_MS: u64 = 5;
    const EL_IDLE_ROUNDS: u32 = 3;
    const EL_IDLE_GRACE: std::time::Duration = std::time::Duration::from_millis(300);
    let el_max_total = std::env::var("BROWSER_EL_MAX_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or_else(|| std::time::Duration::from_secs(2));
    let el_start = std::time::Instant::now();
    let mut idle_rounds: u32 = 0;
    let mut idle_start: Option<std::time::Duration> = None;
    loop {
        // M82: 全局 deadline 到 → 事件循环立即退出（返回当前已渲染内容）。
        if crate::bridge::js_deadline_exceeded() {
            eprintln!("[js-runtime] global JS deadline exceeded — stop event loop");
            break;
        }
        // M69: 每轮先 drain 动态 script（上一轮 timer 回调/onload 可能 appendChild
        // 新 chunk 入队）。eval 出的代码可能又入队，下一轮处理（支持多层链式加载）。
        dyn_executed = drain_and_eval_dynamic_scripts(engine);
        executed += dyn_executed;
        let fired = engine.eval_i32("__drainDueTimers()").unwrap_or(0);
        // M70.13: drain CSS transition 队列——触发到期的 transitionend/animationend。
        // 在 timer drain 之后、microtask drain 之前执行（transition 回调可能 schedule 新 timer）。
        let trans_fired = engine.eval_i32("__drainDueTransitions()").unwrap_or(0);
        // M72.4: drain WebSocket 事件（Open/Message/Close/Error）。
        // 后台线程 WsManager 产生的事件，通过 __wsDispatchEvent 分派到 JS 回调。
        let ws_events = crate::bridge::drain_ws_events();
        let mut ws_fired = 0;
        for (id, etype, data) in &ws_events {
            let escaped = data.replace('\\', "\\\\").replace('\'', "\\'");
            let js = format!("__wsDispatchEvent({id}, '{etype}', '{escaped}')");
            let _ = engine.eval_safe(&js);
            ws_fired += 1;
        }
        // M66-fix: 每轮 timer 回调触发后，drain 其 schedule 的新 Promise microtask。
        if fired > 0 || dyn_executed > 0 || trans_fired > 0 || ws_fired > 0 {
            engine.run_jobs();
            idle_start = None;
            idle_rounds = 0;
        } else {
            // 无活动 → idle 检测 + DOM 稳定检测
            // M72.4: 有活动 WebSocket 连接时不 idle 退出（等 onopen/onmessage）
            let has_ws = crate::bridge::ws_connection_count() > 0;
            // M78.8: 500ms 地平线内有待到期 timer → 视为活动，不推进 idle。
            // 修复双峰：旧逻辑 grace 后 3 个 idle tick（~15-20ms kill 窗口）无视
            // pending 未到期 timer，100ms 链式 timer 被拦腰杀；窗口宽度随 OS
            // 调度档位漂移，导致整族 WPT 页面完成与否同翻（storage 32↔105）。
            // analytics 的 60s 长 timer 不在地平线内，不阻塞退出（保 M70.13 意图）。
            let pending_soon = engine
                .eval_js_bool("__nextTimerDueInMs()<=500")
                .unwrap_or(false);
            if has_ws || pending_soon {
                idle_start = None;
                idle_rounds = 0;
            } else if idle_start.is_none() {
                idle_start = Some(el_start.elapsed());
            } else {
                // M78.8: 修正 grace 语义——"连续 idle ≥300ms"。
                // 旧实现比较 idle 起点的绝对时刻：最后活动停在 <300ms 的页面
                // idle_start 永不达标，白烧到 EL_MAX_TOTAL（每页浪费 ~1.7s）。
                let idle_for = el_start.elapsed() - idle_start.unwrap();
                if idle_for >= EL_IDLE_GRACE {
                    idle_rounds += 1;
                    if idle_rounds >= EL_IDLE_ROUNDS {
                        break;
                    }
                    // M77: DOM 稳定检测——body 内有**可见文本**（非空 div 占位）
                    // 则内容已就绪。只在 idle grace 后检测（给 React 至少 300ms）。
                    let dom_ready = engine
                        // M78.12: 排除测试框架环境——WPT 测试页天然有大量静态
                        // 文本（>80 字符），dom_ready 会把"测试还没跑"误判为
                        // "渲染完成"而 ~301ms 早退，杀掉 testharness 完成链
                        // （reflection-* 8 页 no-results 的根因）。test/setup 是
                        // testharness.js 装的全局，CSR 页面不会有。
                        .eval_js_bool(
                            "__findTag('body')>0&&__visibleBodyTextLen()>80\
                             &&typeof test!=='function'&&typeof setup!=='function'",
                        )
                        .unwrap_or(false);
                    if dom_ready {
                        break;
                    }
                } else {
                    idle_rounds = 0;
                }
            }
        }
        // M77: 移除了 `__hasPendingTimers()` 立即 break 逻辑——
        // CSR 框架（React/Vue）的异步渲染不依赖我们的 setTimeout shim，
        // 无 pending timer 不代表渲染完成。让 idle 检测链
        // （grace → rounds → dom_ready → max_total）决定退出时机。
        if el_start.elapsed() >= el_max_total {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(EL_TICK_MS));
    }
    // M81: --click 后处理——同一会话内按序合成点击，每次后泵一轮事件循环
    // （点击回调常排 setTimeout(0)/fetch，需 drain 才能反映到 DOM）。
    if !post_exprs.is_empty() {
        run_post_exprs_quickjs(engine, post_exprs);
    }
    // M66-fix: 最后再 drain 一轮（最后一波 timer 回调可能 schedule 了 microtask）。
    engine.run_jobs();
    engine.gc();
    eprintln!("[serve] event_loop: {}ms", el_start.elapsed().as_millis());
    // M93: JS 阶段结束——在 TreeGuard 存活期间（cookie slot 可写）取出文档
    // 导航并 fetch 新文档（逐跳跟 3xx，每跳 Set-Cookie 先进 jar 再请求下一跳）。
    // 返回 (url, html) 给上层导航循环换树重跑。
    let next = crate::bridge::take_pending_navigation().and_then(|url| {
        match crate::bridge::fetch_navigation_document(&url) {
            Ok(html) => Some((url, html)),
            Err(e) => {
                eprintln!("[nav] fetch navigation target failed: {e} — keeping current DOM");
                None
            }
        }
    });
    (executed, next)
}

/// M81: 单次合成点击后的事件循环泵上限（ms）。点击回调排 setTimeout/fetch
/// 需要时间完成；上限防挂死（对齐主循环 EL_TICK/IDLE 语义，窗口更短）。
const CLICK_PUMP_MAX_MS: u64 = 500;

/// M81: 在同一 QuickJS 会话内按序 eval 点击/悬停表达式，每次后泵一轮事件
/// 循环。表达式约定返回数字：`-1` = 未命中（选择器没匹配到元素）；否则
/// 目标 nodeId。
#[cfg(feature = "quickjs")]
fn run_post_exprs_quickjs(
    engine: &mut crate::engine_quickjs::QuickJsEngine,
    post_exprs: &[String],
) {
    for (idx, expr) in post_exprs.iter().enumerate() {
        match engine.eval_i32(expr) {
            Some(-1) => eprintln!("[synthetic] #{idx} no element matched"),
            Some(id) => eprintln!("[synthetic] #{idx} dispatched on node {id}"),
            None => eprintln!("[synthetic] #{idx} eval failed (see [js] errors above)"),
        }
        pump_after_click_quickjs(engine, CLICK_PUMP_MAX_MS);
    }
}

/// M81: 点击后事件循环泵——drain 动态 script / timer / microtask / WS 事件，
/// 连续 idle 三轮或超时退出。镜像 `run_scripts_quickjs` 主循环的 drain 顺序
/// （动态 script → timer → transition → WS → microtask）。
#[cfg(feature = "quickjs")]
fn pump_after_click_quickjs(engine: &mut crate::engine_quickjs::QuickJsEngine, max_ms: u64) {
    let start = std::time::Instant::now();
    let deadline = std::time::Duration::from_millis(max_ms);
    let mut idle_rounds = 0u32;
    while idle_rounds < 3 && start.elapsed() < deadline {
        let dyn_executed = drain_and_eval_dynamic_scripts(engine);
        let fired = engine.eval_i32("__drainDueTimers()").unwrap_or(0);
        let _ = engine.eval_i32("__drainDueTransitions()").unwrap_or(0);
        let ws_events = crate::bridge::drain_ws_events();
        let mut ws_fired = 0usize;
        for (id, etype, data) in &ws_events {
            let escaped = data.replace('\\', "\\\\").replace('\'', "\\'");
            let js = format!("__wsDispatchEvent({id}, '{etype}', '{escaped}')");
            let _ = engine.eval_safe(&js);
            ws_fired += 1;
        }
        if fired > 0 || dyn_executed > 0 || ws_fired > 0 {
            engine.run_jobs();
            idle_rounds = 0;
        } else {
            // M78.8 同款地平线语义：500ms 内有待到期 timer → 视为活动不推进
            // idle（点击回调常排 setTimeout(50-200ms)，3 个 idle tick ~15ms
            // 就退出的话永远等不到它）。
            let pending_soon = engine
                .eval_js_bool("__nextTimerDueInMs()<=500")
                .unwrap_or(false);
            if pending_soon {
                idle_rounds = 0;
            } else {
                engine.run_jobs();
                idle_rounds += 1;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    engine.run_jobs();
}

/// M66-B: 获取所有 JS shim 的 JS 字符串（引擎无关）。
/// 最小版本——只包含 QuickJS 验证所需的核心 shim。
/// 后续需要从 boa 的 shim 模块提取完整 JS 字符串。
#[cfg(feature = "quickjs")]
fn get_all_shim_js(_base_url: &Option<String>) -> Vec<(&'static str, String)> {
    vec![
        ("globals", QUICKJS_GLOBAL_SHIM.to_string()),
        // M93.14: WebCrypto（P-256 ECDH + AES-256-GCM + HKDF）——依赖 globals 段的
        // __sha256 与 __subtleTarget，须紧跟 globals 之后。
        ("webcrypto", QUICKJS_WEBCRYPTO_SHIM.to_string()),
        ("element", QUICKJS_ELEMENT_SHIM.to_string()),
        ("document", QUICKJS_DOCUMENT_SHIM.to_string()),
        ("xhr", QUICKJS_XHR_SHIM.to_string()),
        // M93: Web Worker（同步子 Context）——Anubis PoW 等依赖 Worker 的
        // 挑战/计算才能闭环。放最后：依赖 globals 段的 JSON/全局设施。
        ("worker", QUICKJS_WORKER_SHIM.to_string()),
    ]
}

/// M66-B: QuickJS 最小全局 shim（window/document/navigator/setTimeout 桩）。
/// 这是验证用的最小集——后续替换为 boa 的完整 5400 行 shim。
#[cfg(feature = "quickjs")]
const QUICKJS_GLOBAL_SHIM: &str = r#"
// window 全局对象
var window = globalThis;
var self = globalThis;
// M78.127: Element 构造器必须最先定义（赋值不提升——后续 Text/Comment/
// HTMLElement 等在 eval 期就引用 Element；顶层 function 声明有提升但
// non-configurable，WPT interface-objects 要求可 delete）。
// Event 同理：globals 段 __StorageEvent.prototype 在 eval 期引用 Event，
// 之前靠合并 eval 的函数提升（XHR 段声明）兜底；转赋值后须在此预定义
// （XHR 段稍后会用完整版本再次覆盖 + 挂原型静态方法）。
globalThis.Element = function Element(nodeId) { this.__nodeId = nodeId; };
globalThis.Event = function Event(type, opts) {
    opts = opts || {};
    this.type = String(type);
    this.target = null;
    this.currentTarget = null;
    this.bubbles = !!opts.bubbles;
    this.cancelable = !!opts.cancelable;
    this.composed = !!opts.composed;
    this.eventPhase = 2;
    this.defaultPrevented = false;
    this.isTrusted = false;
    this.cancelBubble = false;
    this.returnValue = true;
    this.timeStamp = (typeof performance !== 'undefined' && performance.now) ? performance.now() : Date.now();
};
	var top = globalThis;
	var parent = globalThis;
	
	// M70.6: 全局 onload/onerror 桩（防止 `onload is not defined` 报错）。
	// 某些框架（如 bing.com）直接引用 onload 全局变量而非 window.onload。
	var onload = null;
	var onerror = null;
	window.onload = null;
	window.onerror = null;
	
	// navigator
// M83: UA 与主请求（net::client）一致——掘金风控 SDK 会比对 navigator.userAgent
// 完整性（旧值 'Mozilla/5.0' 残缺，一眼非浏览器）。
// M93.15: screen——此前完全未定义，站点指纹采集 screen.width 直接
// ReferenceError（xcancel fp 报 screenResolution:"ERROR"）。spec 常规形状
// + macOS 主流值（与 UA/platform 的 MacIntel 声明一致——环境一致性）。
window.screen = {
    width: 1512, height: 982,
    availWidth: 1512, availHeight: 930,
    colorDepth: 24, pixelDepth: 24,
    isExtended: false,
    availLeft: 0, availTop: 25,
    orientation: { type: 'landscape-primary', angle: 0, onchange: null }
};
try { window.screen.orientation.type = 'landscape-primary'; } catch (eScr) {}

// M93.15: window.chrome——UA 声明 Chrome 而 window.chrome 缺失是环境
// 不一致信号（fp 检查 window.chrome 存在性）。现代 Chrome 的最小形状。
window.chrome = {
    app: { isInstalled: false, getDetails: function() { return null; }, getIsInstalled: function() { return false; }, installState: function() { return {installState:'disabled'}; }, runningState: function() { return 'cannot_run'; } },
    runtime: { OnInstalledReason: {}, PlatformArch: {}, PlatformNaclArch: {}, PlatformOs: {}, RequestUpdateCheckStatus: {} },
    csi: function() { return { startE: Date.now(), onloadT: Date.now(), pageT: 0, tran: 15 }; },
    loadTimes: function() { return { requestTime: Date.now() / 1000, startLoadTime: Date.now() / 1000, commitLoadTime: Date.now() / 1000, finishDocumentLoadTime: Date.now() / 1000, finishLoadTime: Date.now() / 1000, firstPaintTime: Date.now() / 1000, firstPaintAfterLoadTime: 0, navigationType: 'Other', wasFetchedViaSpdy: true, wasNpnNegotiated: true, npnNegotiatedProtocol: 'h2', wasAlternateProtocolAvailable: false, connectionInfo: 'h2' }; }
};

// M93.15: CacheStorage（caches）——spec 桩：空缓存语义。
window.caches = {
    open: function() { return Promise.reject(new Error('caches unavailable')); },
    keys: function() { return Promise.resolve([]); },
    has: function() { return Promise.resolve(false); },
    match: function() { return Promise.resolve(undefined); },
    delete: function() { return Promise.resolve(false); }
};

window.navigator = { userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36', platform: 'MacIntel', language: 'en-US', languages: ['en-US','en'], cookieEnabled: true, hardwareConcurrency: __hwConcurrency(), deviceMemory: 8, maxTouchPoints: 0 };
// M83: Plugin/MimeType 标准接口——core-js DOM collections 表 / 风控 SDK 环境检测
// 裸引用 PluginArray 会 ReferenceError 断掉脚本链（掘金 feed 不渲染根因①）。
// 空 PluginArray 语义（无插件环境，真实浏览器无插件时也是空数组）。
function Plugin(name, filename, description) {
    this.name = name || ''; this.filename = filename || ''; this.description = description || ''; this.length = 0;
}
function PluginArray() { this.length = 0; }
PluginArray.prototype.item = function() { return null; };
PluginArray.prototype.namedItem = function() { return null; };
PluginArray.prototype.refresh = function() {};
function MimeType(type, suffixes, description) {
    this.type = type || ''; this.suffixes = suffixes || ''; this.description = description || '';
}
function MimeTypeArray() { this.length = 0; }
MimeTypeArray.prototype.item = function() { return null; };
MimeTypeArray.prototype.namedItem = function() { return null; };
window.Plugin = Plugin;
window.PluginArray = PluginArray;
window.MimeType = MimeType;
window.MimeTypeArray = MimeTypeArray;
// M93.15: navigator.plugins/mimeTypes——Chrome 126 的公开常量默认表（5 个
// PDF 相关条目，所有正常 Chrome 一致；环境一致性而非个体身份）。此前空表
// 是无头特征（正常 Chrome 从不空表）。
(function() {
    var mt = function(t, s) { var m = new MimeType(); m.type = t; m.suffixes = s; m.description = ''; return m; };
    var mkPlugin = function(name, fn, desc, file, mimes) {
        var pl = new Plugin();
        pl.name = name; pl.filename = fn; pl.description = desc;
        pl.length = mimes.length;
        for (var i = 0; i < mimes.length; i++) {
            pl[i] = mimes[i]; pl[mimes[i].type] = mimes[i];
            mimes[i].enabledPlugin = pl;
        }
        return pl;
    };
    var pdfMime = mt('application/pdf', 'pdf');
    var textPdf = mt('text/pdf', 'pdf');
    var pPdf = mkPlugin('PDF Viewer', 'internal-pdf-viewer', 'Portable Document Format', 'internal-pdf-viewer', [pdfMime, textPdf]);
    var chromePdf = mkPlugin('Chrome PDF Viewer', 'internal-pdf-viewer', '', 'internal-pdf-viewer', [pdfMime, textPdf]);
    var chPdf = mkPlugin('Chromium PDF Viewer', 'internal-pdf-viewer', '', 'internal-pdf-viewer', [pdfMime, textPdf]);
    var msPdf = mkPlugin('Microsoft Edge PDF Viewer', 'internal-pdf-viewer', '', 'internal-pdf-viewer', [pdfMime, textPdf]);
    var wkPdf = mkPlugin('WebKit built-in PDF', 'internal-pdf-viewer', '', 'internal-pdf-viewer', [pdfMime, textPdf]);
    var arr = new PluginArray();
    var list = [pPdf, chromePdf, chPdf, msPdf, wkPdf];
    arr.length = list.length;
    for (var k = 0; k < list.length; k++) { arr[k] = list[k]; arr[list[k].name] = list[k]; }
    window.navigator.plugins = arr;
    var marr = new MimeTypeArray();
    marr.length = 2;
    marr[0] = pdfMime; marr['application/pdf'] = pdfMime;
    marr[1] = textPdf; marr['text/pdf'] = textPdf;
    window.navigator.mimeTypes = marr;
    window.navigator.pdfViewerEnabled = true;
})();
window.scrollTo = window.scroll = function() {};
window.scrollX = window.scrollY = window.pageXOffset = window.pageYOffset = 0;
window.innerWidth = 1024;
window.innerHeight = 768;
// M93.15: 窗口几何——outerWidth/outerHeight undefined 是非浏览器特征
//（真窗口必有值；headless 的 0 也被检测——取视口同尺寸的"有窗口"值）。
window.outerWidth = 1512;
window.outerHeight = 982;
window.screenX = 0;
window.screenY = 0;
window.screenLeft = 0;
window.screenTop = 25;
window.devicePixelRatio = 2;
window.visualViewport = { width: 1024, height: 768, offsetTop: 0, offsetLeft: 0, scale: 1 };
// Promise.allSettled（ES2020，QuickJS 原生支持但 shim 可能覆盖）
if (!Promise.allSettled) {
    Promise.allSettled = function(promises) {
        return Promise.all(promises.map(function(p) {
            return Promise.resolve(p).then(
                function(v) { return { status: 'fulfilled', value: v }; },
                function(e) { return { status: 'rejected', reason: e }; }
            );
        }));
    };
}

// M66: 异步 event loop —— setTimeout/setInterval 不立刻执行，
// 存到 __pendingTimers 队列，脚本执行完后由 Rust event loop 逐条触发。
// 这样 Promise.then 微任务能正确 drain，框架的异步渲染流程完整。
var __timerSeq = 0;
var __pendingTimers = [];
// M78.8: 下一个 timer 的到期倒计时（ms；无 pending 返回 Infinity）。
// event loop 的 idle 判定用它区分"真静默"与"还有 timer 在等"。
window.__nextTimerDueInMs = function() {
    var now = Date.now(), min = Infinity;
    for (var i = 0; i < __pendingTimers.length; i++) {
        var d = __pendingTimers[i].fireAt - now;
        if (d < min) min = d;
    }
    return min;
};
window.setTimeout = function(cb, delay) {
    if (typeof cb !== 'function') return 0;
    __timerSeq++;
    var id = __timerSeq;
    // M70.13: 在 window 上存强引用，防止 QuickJS GC 回收跨 eval 边界的回调。
    window['__cb_' + id] = cb;
    __pendingTimers.push({ id: id, cb: cb, type: 'timeout',
                           fireAt: Date.now() + (delay || 0), count: 0 });
    return id;
};
window.clearTimeout = function(id) {
    delete window['__cb_' + id];
    for (var i = 0; i < __pendingTimers.length; i++) {
        if (__pendingTimers[i].id === id) { __pendingTimers.splice(i, 1); break; }
    }
};
window.setInterval = function(cb, delay) {
    if (typeof cb !== 'function') return 0;
    __timerSeq++;
    var id = __timerSeq;
    window['__cb_' + id] = cb;
    __pendingTimers.push({ id: id, cb: cb, type: 'interval',
                           fireAt: Date.now() + (delay || 0), count: 0, interval: delay || 0 });
    return id;
};
window.clearInterval = function(id) { window.clearTimeout(id); };
window.requestAnimationFrame = function(cb) { return window.setTimeout(cb, 0); };
window.cancelAnimationFrame = function(id) {};

// M66: 触发到期的 timer 回调。返回本轮触发的回调数。
// 每个回调在新 call stack 执行（通过 eval 隔离），让 Promise microtask 能 drain。
// interval 回调触发后自动重新 schedule（最多 100 次）。
window.__drainDueTimers = function() {
    var fired = 0;
    // M92-fix: 每次迭代取 fresh Date.now()——旧版 now 冻结在轮次起点，
    // 回调执行期间（如 history.go 遍历 → firePopstate）新排队的 0 延迟
    // timer 会因 fireAt >= now 被跳到下一轮（毫秒边界竞态：007.html 的
    // popstate 时而进本轮时而掉到 load 之后）。guard 防 0 延迟自递归
    // timer 把单轮 drain 变死循环（外层 event loop 会继续跑剩余轮次）。
    var i = 0;
    var guard = 0;
    while (i < __pendingTimers.length) {
        if (++guard > 1000) { break; }
        var t = __pendingTimers[i];
        if (!t || Date.now() < t.fireAt) { i++; continue; }
        var now = Date.now();
        if (t.type === 'interval') {
            t.count++;
            if (t.count > 100) { __pendingTimers.splice(i, 1); continue; }
            t.fireAt = now + t.interval;
        } else {
            __pendingTimers.splice(i, 1);
        }
        try {
            if (typeof t.cb !== 'function') {
                if (typeof __log === 'function') __log('[timer] cb is ' + typeof t.cb + ' for id=' + t.id);
            } else {
                t.cb();
            }
        } catch(e) {
            var st = (e && e.stack) ? String(e.stack).split('\\n').slice(0,4).join(' | ') : '';
            if (typeof __log === 'function') __log('[timer] ' + (e.message || String(e)) + (st ? (' | ' + st) : ''));
            // M92: cross-realm 近似——回调带 __realmWin 标记（帧 Function
            // 包装的产出）时，异常上报到该帧的 onerror 而非顶层
            //（WPT settimeout/setinterval-cross-realm-callback-report-exception：
            // 异步回调异常记在回调的全局对象上）。
            try {
                var __rw = t.cb && t.cb.__realmWin;
                if (__rw && typeof __rw.onerror === 'function') {
                    __rw.onerror(e.message || String(e), '', 0, 0, e);
                }
            } catch (e2) {}
        }
        // M70.13: 清理 window 上的强引用（防止内存泄漏）。
        delete window['__cb_' + t.id];
        fired++;
    }
    return fired;
};

// M66: 检查是否还有短期内会触发的 pending timer（event loop 判断是否继续循环）。
// 忽略 fireAt > 2s 的长延迟定时器（analytics/telemetry 等，不阻塞退出）。
window.__hasPendingTimers = function() {
    var now = Date.now();
    var limit = now + 2000;
    for (var i = 0; i < __pendingTimers.length; i++) {
        if (__pendingTimers[i].fireAt <= limit) return true;
    }
    return false;
};
window.__pendingTimerCount = function() { return __pendingTimers.length; };
window.__pendingTimerNames = function() {
    var s = '';
    for (var i = 0; i < __pendingTimers.length && i < 5; i++) {
        s += (__pendingTimers[i].type || '?') + ',';
    }
    return s + '(' + __pendingTimers.length + ')';
};

// queueMicrotask
window.queueMicrotask = function(cb) { Promise.resolve().then(cb); };

// atob/btoa（Base64）
window.atob = function(s) {
    var chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
    var str = String(s).replace(/=+$/, '');
    var out = '';
    for (var i = 0; i < str.length; i += 4) {
        var n = (chars.indexOf(str[i]) << 18) | (chars.indexOf(str[i+1]) << 12) |
                ((str[i+2] ? chars.indexOf(str[i+2]) : 0) << 6) | (str[i+3] ? chars.indexOf(str[i+3]) : 0);
        out += String.fromCharCode((n >> 16) & 255) + (str.length > i+2 ? String.fromCharCode((n >> 8) & 255) : '') + (str.length > i+3 ? String.fromCharCode(n & 255) : '');
    }
    return out;
};
window.btoa = function(s) {
    var chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
    var out = '';
    for (var i = 0; i < s.length; i += 3) {
        var n = (s.charCodeAt(i) << 16) | ((i+1 < s.length ? s.charCodeAt(i+1) : 0) << 8) | (i+2 < s.length ? s.charCodeAt(i+2) : 0);
        out += chars[(n >> 18) & 63] + chars[(n >> 12) & 63] + (i+1 < s.length ? chars[(n >> 6) & 63] : '=') + (i+2 < s.length ? chars[n & 63] : '=');
    }
    return out;
};

// M93.12-diag: Promise 拒绝望远镜——then 源码级包装（安装期一致，VM 防篡改
// 不可见），任何流经异步链的 rejection 及错误信息打印出来（xcancel VM 拿到
// 挑战后 5 微任务死寂，唯一可观测缺口就是被吞的 rejection）。
(function() {
    var OT = Promise.prototype.then;
    Promise.prototype.then = function(onF, onR) {
        var wF = onF ? function(v) {
            try { return onF(v); }
            catch (e) {
                if (typeof __ctrace === 'function') { try { __ctrace('THROW@then ' + String((e && (e.message || e)) || e).slice(0, 120) + ' STACK=' + String((e && e.stack) || '').split('\n').slice(0, 5).join(' ~ ').slice(0, 400)); } catch (e2) {} }
                throw e;
            }
        } : onF;
        var wR = onR ? function(e) {
            if (typeof __ctrace === 'function') { try { __ctrace('CAUGHT@then ' + String((e && (e.message || e)) || e).slice(0, 180)); } catch (e2) {} }
            try { return onR(e); }
            catch (e3) {
                if (typeof __ctrace === 'function') { try { __ctrace('THROW@catch ' + String((e3 && (e3.message || e3)) || e3).slice(0, 180)); } catch (e4) {} }
                throw e3;
            }
        } : function(e) {
            // 无 onR：rejection 继续传播——记录（若永远无人接住=unhandled）
            if (typeof __ctrace === 'function') { try { __ctrace('PASS-REJECT ' + String((e && (e.message || e)) || e).slice(0, 180)); } catch (e5) {} }
            throw e;
        };
        return OT.call(this, wF, wR);
    };
})();

// M93.7: 纯 JS SHA-256（crypto.subtle.digest 的实现内核，零 Rust 依赖——G4 自研）。
// 主上下文与 worker env（engine_quickjs.rs worker_env_js）各持一份同源拷贝：
// 两处是独立 eval 空间，共享常量需跨模块传字符串，拷贝更稳。
var __sha256 = (function() {
    var K = [0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
             0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
             0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
             0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
             0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
             0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
             0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
             0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2];
    function rr(x, n) { return (x >>> n) | (x << (32 - n)); }
    return function(bytes) {
        var h = [0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19];
        var ml = bytes.length, w = new Array(64), i;
        var bitLenHi = Math.floor(ml / 0x20000000), bitLenLo = (ml << 3) >>> 0;
        var padded = [];
        for (i = 0; i < ml; i++) padded.push(bytes[i] & 255);
        padded.push(0x80);
        while (padded.length % 64 !== 56) padded.push(0);
        padded.push((bitLenHi>>>24)&255,(bitLenHi>>>16)&255,(bitLenHi>>>8)&255,bitLenHi&255,
                    (bitLenLo>>>24)&255,(bitLenLo>>>16)&255,(bitLenLo>>>8)&255,bitLenLo&255);
        for (var b = 0; b < padded.length; b += 64) {
            for (i = 0; i < 16; i++) w[i] = ((padded[b+4*i]&255)<<24)|((padded[b+4*i+1]&255)<<16)|((padded[b+4*i+2]&255)<<8)|(padded[b+4*i+3]&255);
            for (i = 16; i < 64; i++) {
                var s0 = rr(w[i-15],7)^rr(w[i-15],18)^(w[i-15]>>>3);
                var s1 = rr(w[i-2],17)^rr(w[i-2],19)^(w[i-2]>>>10);
                w[i] = (w[i-16]+s0+w[i-7]+s1)|0;
            }
            var a=h[0],bb=h[1],c=h[2],d=h[3],e=h[4],f=h[5],g=h[6],hh=h[7];
            for (i = 0; i < 64; i++) {
                var S1 = rr(e,6)^rr(e,11)^rr(e,25);
                var ch = (e&f)^(~e&g);
                var t1 = (hh+S1+ch+K[i]+w[i])|0;
                var S0 = rr(a,2)^rr(a,13)^rr(a,22);
                var mj = (a&bb)^(a&c)^(bb&c);
                var t2 = (S0+mj)|0;
                hh=g; g=f; f=e; e=(d+t1)|0; d=c; c=bb; bb=a; a=(t1+t2)|0;
            }
            h[0]=(h[0]+a)|0; h[1]=(h[1]+bb)|0; h[2]=(h[2]+c)|0; h[3]=(h[3]+d)|0;
            h[4]=(h[4]+e)|0; h[5]=(h[5]+f)|0; h[6]=(h[6]+g)|0; h[7]=(h[7]+hh)|0;
        }
        var out = new Uint8Array(32);
        for (i = 0; i < 8; i++) {
            out[i*4]=(h[i]>>>24)&255; out[i*4+1]=(h[i]>>>16)&255; out[i*4+2]=(h[i]>>>8)&255; out[i*4+3]=h[i]&255;
        }
        return out;
    };
})();

// M93.7: crypto.subtle.digest —— SHA-256 子集（cap.js 等 PoW 挑战的核心依赖；
// spec 接口：digest(algo, BufferSource) → Promise<ArrayBuffer>）。
function __subtleDigest(algo, data) {
    return new Promise(function(resolve, reject) {
        try {
            if (typeof __ctrace === 'function') { try { __ctrace('subtle.digest algo=' + JSON.stringify(algo)); } catch (eT) {} }
            var norm = String(algo).replace(/-/g, '').toUpperCase();
            if (norm !== 'SHA256') {
                var e1 = new Error("crypto.subtle.digest: unsupported algorithm '" + algo + "' (SHA-256 only)");
                e1.name = 'NotSupportedError';
                reject(e1);
                return;
            }
            var u8;
            if (data instanceof Uint8Array) { u8 = data; }
            else if (data && data.buffer instanceof ArrayBuffer) { u8 = new Uint8Array(data.buffer, data.byteOffset || 0, data.byteLength); }
            else if (data instanceof ArrayBuffer) { u8 = new Uint8Array(data); }
            else if (typeof data === 'string') { u8 = new TextEncoder().encode(data); }
            else {
                reject(new TypeError('crypto.subtle.digest: data must be BufferSource'));
                return;
            }
            resolve(__sha256(u8).buffer);
        } catch (err) { reject(err); }
    });
}

// M93.11: navigator.hardwareConcurrency——cap.js 用 Math.min(hardwareConcurrency, N)
// 决定 PoW Worker 数量；undefined → Math.min(NaN, N)=NaN → 零 Worker → 无限等待
// （xcancel 挑战拿到 200 后死寂的根因）。诚实值：宿主真实逻辑核数。
function __hwConcurrency() {
    if (typeof __hwCores === 'number' && __hwCores > 0) return __hwCores;
    return 8;
}

// crypto.getRandomValues（uuid 库需要）+ M93.7 subtle.digest（cap.js PoW）
// M93.11-diag: subtle 方法访问追踪（env 门控，shim 自身代码——VM 防篡改不可见）
// M93.14: target 拆为具名 var __subtleTarget——webcrypto 段（QUICKJS_WEBCRYPTO_SHIM）
// 在 install 期向 target 追加 generateKey/importKey/exportKey/deriveBits/deriveKey/
// encrypt/decrypt（P-256 ECDH + AES-256-GCM + HKDF），Proxy 的兜底逻辑保持不变。
var __subtleTarget = {
    digest: function(algo, data) { return __subtleDigest(algo, data); }
};
window.crypto = {
    getRandomValues: function(arr) {
        for (var i = 0; i < arr.length; i++) arr[i] = Math.floor(Math.random() * 256);
        return arr;
    },
    subtle: new Proxy(__subtleTarget, {
        get: function(target, prop) {
            if (typeof prop !== 'string') return target[prop];
            if (typeof target[prop] !== 'undefined') return target[prop];
            // M93.13-diag: 未实现的 subtle 方法——记参数后抛（逐步映射 VM 的
            // 完整 crypto 调用图：generateKey/importKey/exportKey/encrypt/...）
            return function() {
                var args = [];
                for (var i = 0; i < arguments.length; i++) {
                    try { args.push(typeof arguments[i] === 'object' ? JSON.stringify(arguments[i]).slice(0, 120) : String(arguments[i]).slice(0, 60)); }
                    catch (eS) { args.push('?obj'); }
                }
                if (typeof __ctrace === 'function') { try { __ctrace('subtle.CALL ' + prop + '(' + args.join(', ') + ')'); } catch (eT) {} }
                throw new TypeError('crypto.subtle.' + prop + ' is not implemented');
            };
        }
    })
};

// location 对象
var __locHref = (typeof __locationHref === 'function') ? __locationHref() : 'about:blank';
function __parseLoc(href) {
    // 区分 scheme://host (http://) 和 opaque (about:blank)
    var colonIdx = href.indexOf(':');
    var proto = colonIdx >= 0 ? href.slice(0, colonIdx + 1) : '';
    var afterProto = colonIdx >= 0 ? href.slice(colonIdx + 1) : href;
    // 去除可能的 //
    if (afterProto.indexOf('//') === 0) afterProto = afterProto.slice(2);
    var hashIdx = afterProto.indexOf('#');
    var hashPart = hashIdx >= 0 ? afterProto.slice(hashIdx) : '';
    var noHash = hashIdx >= 0 ? afterProto.slice(0, hashIdx) : afterProto;
    var searchIdx = noHash.indexOf('?');
    var searchPart = searchIdx >= 0 ? noHash.slice(searchIdx) : '';
    var noSearch = searchIdx >= 0 ? noHash.slice(0, searchIdx) : noHash;
    var host = noSearch.split('/')[0] || '';
    var hostNoPort = host.split(':')[0];
    var pathname;
    if (proto === 'about:') {
        pathname = noSearch; // 'blank'
    } else if (host) {
        pathname = '/' + noSearch.split('/').slice(1).join('/');
    } else {
        pathname = '/' + noSearch;
    }
    var loc = {
        protocol: proto,
        host: host,
        hostname: hostNoPort,
        port: host.split(':')[1] || '',
        pathname: pathname,
        search: searchPart,
        origin: proto + (host ? '//' + host : ''),
        reload: function() {},
        replace: function(u) { __locNavigate(u, 'replace'); },
        assign: function(u) { __locNavigate(u, 'assign'); },
        toString: function() { return __locHref; }
    };
    // M78.132: unforgeable 接口语义——valueOf / Symbol.toPrimitive 是 location
    // 的 own 不可配置属性（WPT Location valueOf/toPrimitive 断言 own 描述符；
    // 顶层脚本 getOwnPropertyDescriptor(location,'valueOf') 此前返回 undefined）。
    try {
        Object.defineProperty(loc, 'valueOf', {
            value: Object.prototype.valueOf,
            writable: false, enumerable: false, configurable: false
        });
        Object.defineProperty(loc, Symbol.toPrimitive, {
            value: undefined,
            writable: false, enumerable: false, configurable: false
        });
    } catch (e) {}
    // M78.135: protocol setter——scheme 语法校验（首字符必须 ASCII 字母、
    // 其后字母/数字/+/-/.；非法抛 SyntaxError DOMException——WPT
    // location-protocol-setter 48 个 "is not a scheme" 断言）。
    Object.defineProperty(loc, 'protocol', {
        get: function() { return proto; },
        set: function(v) {
            var s = String(v).replace(/:$/, '');
            if (!/^[a-zA-Z][a-zA-Z0-9+.-]*$/.test(s)) {
                throw new DOMException(
                    "Failed to set 'protocol' on 'Location': '" + String(v) +
                    "' is not a valid scheme", 'SyntaxError');
            }
            var rest = __locHref.slice(__locHref.indexOf(':') + 1);
            __setLocHref(s + ':' + rest);
        },
        enumerable: true, configurable: true
    });
    // M78: href/hash setter——赋值触发导航语义（相对解析 + hashchange）。
    Object.defineProperty(loc, 'href', {
        get: function() { return __locHref; },
        set: function(u) { __setLocHref(u); },
        enumerable: true, configurable: true
    });
    // M78.132: location.assign/replace 的 URL 严格校验——解析失败抛 SyntaxError
    // DOMException 且 location.href 不变（WPT "URL that fails to parse"：
    // "http://:" 这类空 host 的 http(s) URL 必须抛）。__parseLoc 宽松接受
    // host=':'，这里在导航前校验。
    window.__locNavigate = function(u, mode) {
        var s = String(u);
        var m = /^([a-zA-Z][a-zA-Z0-9+.-]*):/.exec(s);
        var proto = m ? m[1].toLowerCase() : '';
        if (proto === 'http' || proto === 'https') {
            var rest = s.slice(m[0].length);
            if (rest.indexOf('//') === 0) rest = rest.slice(2);
            var h = rest.split(/[/?#]/)[0];
            // 空 host 或裸 ':'（无 hostname）→ URL 解析失败。
            var hostOnly = h.split(':')[0];
            if (!hostOnly) {
                throw new DOMException("Failed to parse URL from '" + s + "'", 'SyntaxError');
            }
        }
        // M92: replace 语义 → 导航后替换当前 history 条目而非新增。
        window.__histReplaceNext = (mode === 'replace');
        __setLocHref(s);
    };
    Object.defineProperty(loc, 'hash', {
        get: function() { return hashPart; },
        set: function(h) {
            var v = String(h);
            if (v.charAt(0) !== '#') v = '#' + v;
            // M78.119: hash 值 URL 编码（空格等不兼容字符——WPT 断言）。
            // M78.132: % 加入 safe 表——已编码值不得二次编码（'a%20b' 旧版
            // 变 'a%2520b'；fragment percent-encode 集不含 %）。
            var encoded = '';
            for (var hi = 1; hi < v.length; hi++) {
                var ch = v.charAt(hi);
                var code = v.charCodeAt(hi);
                var safe = (code >= 65 && code <= 90) || (code >= 97 && code <= 122)
                    || (code >= 48 && code <= 57)
                    || '-_.!~*\'()/?:@&=+$,#%'.indexOf(ch) >= 0;
                encoded += safe ? ch : encodeURIComponent(ch);
            }
            __setLocHref(__locHref.split('#')[0] + '#' + encoded);
        },
        enumerable: true, configurable: true
    });
    // M92: ancestorOrigins——Chrome 扩展 API（WPT
    // location-ancestor-origins-new-object）。same-realm 近似：文档里有
    // 已连接 iframe 时返回 [父 origin]，否则 []；连接状态变化（移除）
    // 后重建新对象（断言"移除后是新对象、重复访问同一对象"）。
    // 连接性用 __frameIds 注册表 + __getParent 判断（__qsAll 对裸标签
    // 选择器会多匹配，计数不可靠）。
    Object.defineProperty(loc, 'ancestorOrigins', {
        get: function() {
            var connected = 0;
            try {
                var __fids = window.__frameIds || [];
                for (var ii = 0; ii < __fids.length; ii++) {
                    var __pid = (typeof __getParent === 'function') ? __getParent(__fids[ii]) : -1;
                    if (typeof __pid === 'number' && __pid >= 0) { connected++; }
                }
            } catch (e) {}
            if (this.__ancKey !== connected || !this.__ancArr) {
                this.__ancKey = connected;
                this.__ancArr = (connected > 0) ? [window.origin] : [];
            }
            return this.__ancArr;
        },
        enumerable: true, configurable: true
    });
    return loc;
}
function __setLocHref(u) {
    // M78: 相对 URL 解析（pushState('/x?y#z') 后 location.href 必须是绝对地址）。
    var resolved = String(u);
    if (typeof URL === 'function' && __locHref && __locHref.indexOf('about:') !== 0) {
        try { resolved = new URL(String(u), __locHref).href; } catch (e) {}
    }
    // M78: hash 变化 → 异步派发 hashchange（WPT history 系列依赖）。
    var oldHash = '';
    try { oldHash = window.location ? (window.location.hash || '') : ''; } catch (e) {}
    var oldNoHash = __locHref.split('#')[0];
    // M93: 文档级导航记录——非 hash 变化 && 非 history API 驱动（pushState/
    // replaceState/back/forward 都设 __histApiNav 旗标）= 浏览器会重新加载
    // 文档的导航（href 赋值 / assign / replace）。记到 Rust 侧队列，JS 阶段
    // 结束后由导航循环重新 fetch + 换树 + 重跑脚本（Anubis pass-challenge：
    // 解完 PoW 后 location.replace(Set-Cookie + 302 → 原页面真身)）。
    if (window.__histApiNav !== true && resolved.split('#')[0] !== oldNoHash
        && typeof __navRecord === 'function') {
        try { __navRecord(resolved); } catch (eNav) {}
    }
    __locHref = resolved;
    window.location = __parseLoc(resolved);
    var newHash = window.location.hash || '';
    if (oldHash !== newHash) {
        // M92: hash-only 导航入 history 栈（fragment navigation 语义——
        // 004.html 断言 location.hash 三次赋值后 history.go(-2)/go(-1) 能遍历
        // 回起点）。API 驱动（pushState/replaceState/遍历）时由调用方管理栈，
        // 跳过；location.replace 语义为替换当前条目而非新增。
        if (window.__histApiNav !== true && window.__histPushEntry
            && oldNoHash === resolved.split('#')[0]) {
            try {
                if (window.__histReplaceNext === true) {
                    window.__histReplaceNext = false;
                    if (window.__histReplaceEntry) { window.__histReplaceEntry({ url: resolved, state: null }); }
                } else {
                    window.__histPushEntry({ url: resolved, state: null });
                }
            } catch (e2) {}
        }
        setTimeout(function() {
            try {
                var ev = new Event('hashchange');
                ev.oldURL = oldHash; ev.newURL = resolved;
                window.dispatchEvent(ev);
            } catch (e) {}
        }, 0);
    }
}
window.location = __parseLoc(__locHref);
// M92: window.origin（序列化 origin——WPT ancestor-origins 断言
// ancestorOrigins 元素 === window.origin）。opaque（about:）源为 'null'。
try {
    Object.defineProperty(window, 'origin', {
        get: function() {
            var m0 = /^([a-zA-Z][a-zA-Z0-9+.-]*):\/\/([^\/?#]+)/.exec(__locHref);
            return m0 ? (m0[1] + '://' + m0[2]) : 'null';
        },
        configurable: true
    });
} catch (e) {}

// history API（docsify 路由需要 pushState/replaceState）
// length 是函数（对齐 boa navigation_shim：history.length() 返回栈深度）
window.history = (function() {
    // M78.17: 条目存 {url, state}——back/forward 恢复 state 并异步派发
    // popstate（浏览器语义：popstate 由跨条目导航触发，pushState 不触发）。
    var stack = [{ url: __locHref, state: null }];
    var cur = 0;
    var state = null;
    // M92: 栈操作导出——__setLocHref 的 hash-only 导航需要入栈/替换条目。
    function pushEntry(e) { stack = stack.slice(0, cur + 1); stack.push(e); cur = stack.length - 1; }
    window.__histPushEntry = pushEntry;
    window.__histReplaceEntry = function(e) { if (stack.length > 0) { stack[cur] = e; } };
    function firePopstate(st) {
        // M78.132: popstate 事件必须带 .state（PopStateEvent 语义——WPT
        // history_back/forward/go 系列 8 个断言 e.state === N 全军覆没的根因：
        // 参数 st 收了没用）。
        setTimeout(function() {
            try {
                var ev = new Event('popstate');
                ev.state = st;
                window.dispatchEvent(ev);
            } catch (e) {}
        }, 0);
    }
    function goEntry(idx) {
        var from = cur;
        cur = Math.max(0, Math.min(idx, stack.length - 1));
        if (cur === from) return;
        state = stack[cur].state;
        window.__histApiNav = true;
        try { __setLocHref(stack[cur].url); } catch (e3) {} finally { window.__histApiNav = false; }
        firePopstate(state);
    }
    // M92: 跨条目遍历排队执行（浏览器语义：history.go/back/forward 在独立
    // task 里生效，调用点不同步导航——004.html 断言 go(-2) 同步调用后
    // location.hash 不变、hashchange 计数为 0）。
    var __goQueue = [];
    function queueGo(n) {
        __goQueue.push(n);
        setTimeout(function() {
            var step = __goQueue.shift();
            if (step !== undefined) { goEntry(cur + step); }
        }, 0);
    }
    // M92: 跨源 URL pushState/replaceState 抛 SecurityError DOMException
    //（WPT history_pushstate_err / history_replacestate_err）。
    // opaque 源（about:blank，无 base_url 的 render-script 场景）没有可比较
    // 的 origin——跳过校验（M14.4 navigation-spa fixture 依赖 pushState 生效）。
    function assertSameOrigin(url) {
        if (__locHref.indexOf('about:') === 0) { return; }
        // 非绝对 URL（about:blank 基底上 pushState 相对路径后 href 停留在
        // path-only 形态）无 origin 可判定——跳过，保持 M14.4 fixture 行为。
        if (__locHref.indexOf('://') < 0) { return; }
        try {
            var u = new URL(String(url), __locHref);
            var p = new URL(__locHref);
            if ((u.protocol + '//' + (u.host || '')) !== (p.protocol + '//' + (p.host || ''))) {
                throw new DOMException(
                    "Failed to execute 'pushState' on 'History': A history state object with URL '" +
                    String(url) + "' cannot be created in a document with origin '" +
                    (p.protocol + '//' + (p.host || '')) + "'", 'SecurityError');
            }
        } catch (e4) {
            if (e4 && e4.name === 'SecurityError') throw e4;
        }
    }
    // M78: length 必须是 getter 属性（WPT history 断言 history.length 是数字）。
    var h = {
        get state() { return state; },
        pushState: function(s, title, url) {
            if (url !== undefined && url !== null && String(url) !== '') { assertSameOrigin(url); }
            stack = stack.slice(0, cur + 1);
            state = s;
            if (url) {
                window.__histApiNav = true;
                try { __setLocHref(url); } catch (e5) {} finally { window.__histApiNav = false; }
                stack.push({ url: url, state: s });
            }
            else { stack.push({ url: stack[cur].url, state: s }); }
            cur = stack.length - 1;
        },
        replaceState: function(s, title, url) {
            if (url !== undefined && url !== null && String(url) !== '') { assertSameOrigin(url); }
            state = s;
            if (url) {
                window.__histApiNav = true;
                try { __setLocHref(url); } catch (e5) {} finally { window.__histApiNav = false; }
                stack[cur] = { url: url, state: s };
            }
            else { stack[cur] = { url: stack[cur].url, state: s }; }
        },
        // M92: back/forward 保持同步导航——真实 SPA fixture（M14.4
        // navigation-spa）同步读 back() 后的 href；WPT back/forward 系列都是
        // 注册 listener 后再调用，同步/异步皆兼容。只有 go(n) 排队（004.html
        // 断言 go(-2) 调用点不同步生效）。
        back: function() { goEntry(cur - 1); },
        forward: function() { goEntry(cur + 1); },
        go: function(n) {
            if (n === undefined || n === 0) { return; }
            queueGo(n);
        },
        scrollRestoration: 'auto'
    };
    // 兼容：旧调用式 history.length()（M57 前 fixture/老站点写法）——getter 返回
    // 数字本身不可调用，包一层 valueOf 让 Number 包装下不再抛 TypeError。
    var __len = { valueOf: function() { return stack.length; }, toString: function() { return String(stack.length); } };
    Object.defineProperty(h, 'length', {
        get: function() { return stack.length; },
        // M78.9: setter 兜底——sloppy 模式下 history.length = x 被静默吞（无 setter）。
        set: function(v) { __len.valueOf(); },
        enumerable: true
    });
    return h;
})();

// document 占位（完整 document 在 document shim 里填充）
window.document = { createElement: function(tag) { return new Element(0); }, getElementById: function(id) { return null; } };

// M93.10: Page Visibility API——xcancel antibot VM 解码字符串实锤其探测
// visibilityState/hidden/visibilitychange：undefined ≠ 'visible' 被判
// "页面不可见"→ 静默等待可见 → 挑战永不发起（零副作用停滞的根因之一）。
// 我们主动渲染 DOM，'visible'/hidden=false 是诚实值。
try {
    Object.defineProperty(document, 'visibilityState', {
        get: function() { return 'visible'; },
        enumerable: true, configurable: true
    });
    Object.defineProperty(document, 'webkitVisibilityState', {
        get: function() { return 'visible'; },
        enumerable: true, configurable: true
    });
    Object.defineProperty(document, 'hidden', {
        get: function() { return false; },
        enumerable: true, configurable: true
    });
    Object.defineProperty(document, 'webkitHidden', {
        get: function() { return false; },
        enumerable: true, configurable: true
    });
} catch (eVis) {}

// M93.10: navigator.sendBeacon（VM 字符串含 /antibot/api/client-report，
// 上报通道大概率走 beacon）——同步 POST 真实现。
if (typeof navigator.sendBeacon !== 'function') {
    try {
        navigator.sendBeacon = function(url, data) {
            try {
                var body = (typeof data === 'string') ? data : String(data == null ? '' : data);
                __fetchSyncMethod(String(url), 'POST', body, null);
            } catch (eSb) {}
            return true;
        };
    } catch (eSb2) {}
}

// M93.10: HTMLMediaElement.canPlayType——VM 解码字符串含整段音视频 codec
// 探测表（audio/mp4 codecs=mp4a.40.2 等），用于构建平台编解码指纹。
// 全空表 = "什么都放不了"的退化指纹。按宿主平台（macOS + Chromium 系
// 编解码栈）的真实能力声明对齐 Chrome 的公开应答表——与 UA/Accept 同
// 类别的环境一致性，非伪装（我们不在页面内解码媒体，仅声明平台能力）。
if (typeof Element.prototype.canPlayType !== 'function') {
    (function() {
        var probably = [
            'audio/mp4', 'audio/mpeg', 'audio/aac', 'audio/webm', 'audio/ogg; codecs="vorbis"',
            'audio/wav', 'audio/flac', 'audio/ogg; codecs="flac"',
            'video/mp4', 'video/webm', 'video/ogg; codecs="theora"',
            'mp4a.40.2', 'avc1.42E01E', 'avc1.58A01E', 'avc1.4D401E', 'avc1.64001E',
            'vp8', 'vp9', 'av01', 'theora', 'vorbis', 'opus', 'flac', 'aac'
        ];
        var never = [
            'speex', 'dirac', 'mp4v.20.8', 'mp4v.20.240', 'x-matroska', '3gpp'
        ];
        Element.prototype.canPlayType = function(type) {
            var t = String(type || '');
            if (!t) return '';
            var tl = t.toLowerCase();
            for (var i = 0; i < never.length; i++) if (tl.indexOf(never[i].toLowerCase()) >= 0) return '';
            for (var j = 0; j < probably.length; j++) if (tl.indexOf(probably[j].toLowerCase()) >= 0) return 'probably';
            // 未列出的容器类型按 spec 返回 ''；Chrome 对裸容器带 codecs="unknown" 返回 ''
            return '';
        };
    })();
}



// __makeElement 工厂
// M78: 按 nodeId 缓存包装器——同一节点的两次 getElementById/querySelector/
// 命名访问必须 === 相等（WPT assert_equals 用严格相等）。纯 JS 数据缓存，
// 不持有原生引用（GC 安全，同 __cookieJar 模式）。
window.__elCache = {};
window.__makeElement = function(nodeId) {
    if (typeof nodeId === 'number' && nodeId >= 0) {
        var key = String(nodeId);
        if (!window.__elCache[key]) {
            var __el = new Element(nodeId);
            // M93.12: 自定义元素升级——createElement/解析出的元素命中
            // customElements registry 时，原型挂接 + 构造器执行（spec 的
            // custom element upgrade 语义；xcancel VM 对 <cap-widget> 调
            // .solve() 依赖此路径）。
            try {
                if (window.customElements && window.customElements.__registry) {
                    var __tag = (__el.tagName || '').toLowerCase();
                    var __ctor = window.customElements.__registry[__tag];
                    if (__ctor && !__el.__customUpgraded) {
                        var __real;
                        try { __real = new __ctor(); }
                        catch (eNew) {
                            if (typeof __ctrace === 'function') { try { __ctrace('CTOR-THROW ' + String((eNew && (eNew.message || eNew)) || eNew).slice(0, 160)); } catch (e8) {} }
                            throw eNew;
                        }
                        // M93.13-fix: 同 __upgradeOne——原型链继承，无拷贝
                        __real.__nodeId = __el.__nodeId;
                        __real.__customUpgraded = true;
                        __el = __real;
                    }
                }
            } catch (eMk) {}
            window.__elCache[key] = __el;
        }
        return window.__elCache[key];
    }
    return undefined;
};

// console — 覆盖原生（hook 到采集桥）
window.console = {
    log: function(){
        var m=Array.prototype.join.call(arguments,' ');
        try{__captureConsoleEvent('log',m);}catch(e){}
    },
    info: function(){
        var m=Array.prototype.join.call(arguments,' ');
        try{__captureConsoleEvent('info',m);}catch(e){}
    },
    warn: function(){
        var m=Array.prototype.join.call(arguments,' ');
        try{__captureConsoleEvent('warn',m);}catch(e){}
    },
    error: function(){
        var m=Array.prototype.join.call(arguments,' ');
        try{__captureConsoleEvent('error',m);}catch(e){}
    },
    debug: function(){
        var m=Array.prototype.join.call(arguments,' ');
        try{__captureConsoleEvent('debug',m);}catch(e){}
    },
    dir: function(){},
    table: function(){},
    group: function(){},
    groupEnd: function(){},
    trace: function(){},
    time: function(){},
    timeEnd: function(){},
    assert: function(){},
    count: function(){},
    clear: function(){}
};

// window EventTarget 方法（很多框架在 window 上注册事件）
// 用全局变量存监听器，避免 this 绑定问题
var __winListeners = {};
// M78.129: 与 __winListeners[type][i] 一一对应的 capture 标志（三阶段过滤用）。
var __winListenersCap = {};
window.addEventListener = function(type, cb, opt) {
    if (cb === null || cb === undefined) return;
    var wcap = (opt === true) || !!(opt && opt.capture);
    if (!__winListeners[type]) __winListeners[type] = [];
    if (!__winListenersCap[type]) __winListenersCap[type] = [];
    __winListeners[type].push(cb);
    __winListenersCap[type].push(wcap);
};
window.removeEventListener = function(type, cb) {};
window.dispatchEvent = function(ev) {
    if (ev && __winListeners[ev.type]) {
        var cbs = __winListeners[ev.type];
        for (var i = 0; i < cbs.length; i++) {
            try { cbs[i].call(window, ev); } catch(e) {}
        }
    }
    // M78: on* 事件处理器属性——浏览器标准行为：派发事件时除 addEventListener
    // 监听器外还要调用 window['on'+type]（WPT 大量测试用 window.onload = fn 启动）。
    if (ev && ev.type) {
        var __onh = window['on' + ev.type];
        if (typeof __onh === 'function') {
            try { __onh.call(window, ev); } catch(e) {}
        }
    }
};

// M66: customElements + HTMLElement（no-op，不存引用避免 GC 泄漏）
// M93.12: Shadow DOM 最小子集——cap-widget 的 connectedCallback 用
// this.attachShadow({mode:'open'}) + this.shadowRoot 构建组件 UI。
// 爬虫近似：影根 = detached 真 DOM 节点（arena 里的游离 div），天然继承
// 全部元素桥方法（appendChild/querySelector/innerHTML），子树查询用现有
// qs（支持 root id）。
Element.prototype.attachShadow = function(opts) {
    if (typeof __ctrace === 'function') { try { __ctrace('attachShadow this=' + String(this && this.tagName) + ' nid=' + (this && this.__nodeId)); } catch (eT1) {} }
    if (this.__shadowRoot) return this.__shadowRoot;
    try {
        var rootId = (typeof __createDetachedEl === 'function') ? __createDetachedEl('div') : null;
        var root = (typeof rootId === 'number' && typeof __makeElement === 'function') ? __makeElement(rootId) : null;
        if (root) {
            root.__isShadowRoot = true;
            this.__shadowRoot = root;
        }
    } catch (eAs) {}
    return this.__shadowRoot || null;
};
try {
    Object.defineProperty(Element.prototype, 'shadowRoot', {
        get: function() { return this.__shadowRoot || null; },
        enumerable: false, configurable: true
    });
} catch (eSh) {}

// M93.13: 直接构造的自定义元素实例（new CapWidget()）探测——沿原型链比对
// customElements registry 的构造器（实例无 tagName 无 __nodeId，唯一线索是
// 原型链）。返回注册名（小写 tag）或 null。
window.__customInstanceOf = function(obj) {
    try {
        var reg = window.customElements && window.customElements.__registry;
        if (!reg || !obj) return null;
        var proto = Object.getPrototypeOf(obj);
        var depth = 0;
        while (proto && depth < 12) {
            for (var name in reg) {
                if (Object.prototype.hasOwnProperty.call(reg, name) && reg[name] && reg[name].prototype === proto) {
                    return name;
                }
            }
            proto = Object.getPrototypeOf(proto);
            depth++;
        }
    } catch (e) {}
    return null;
};

// M93.12: customElements 真实现——xcancel antibot 的 VM 用
// `customElements.whenDefined('cap-widget').then(...solve())` 驱动挑战
// （cap.min.js 定义 <cap-widget> 自定义元素）。M66 的 no-op 让元素永远
// 没有 .solve() → VM 拿到挑战后 "not a function" 静默死。
// GC 纪律权衡：registry 挂 customElements 对象自身（页面生命周期常驻，
// runtime drop 时随全局一起回收——不挂 window 裸全局）。
window.customElements = {
    __registry: {},
    define: function(name, ctor, opts) {
        if (typeof __ctrace === 'function') { try { __ctrace('CE-define ' + String(name)); } catch (eT2) {} }
        this.__registry[name] = ctor;
        try {
            // 已存在同名元素升级（spec：define 触发已有实例 upgrade）
            var ids = (typeof __qsAll === 'function') ? String(__qsAll(name)).split(',') : [];
            for (var i = 0; i < ids.length; i++) {
                var nid = parseInt(ids[i], 10);
                if (nid) { this.__upgradeOne(nid, ctor); }
            }
        } catch (eUp) {}
    },
    __upgradeOne: function(nodeId, ctor) {
        var el = (typeof __makeElement === 'function') ? __makeElement(nodeId) : null;
        if (!el || el.__customUpgraded) return el;
        try {
            // M93.12: 真构造（new ctor()——类私有字段 #x 只能经正式构造安装，
            // ctor.call(el) 不行，cap-widget 的 this.#field 会炸）。DOM 桥
            // 方法从 Element.prototype 拷贝 + __nodeId 带过去；真实例替换
            // 缓存（getElementById 等返回同一对象，保持 === 同一性）。
            var real = new ctor();
            // M93.13-fix: 删除"Element.prototype 方法拷贝"——读原型上的 getter
            //（classList 等）会以 prototype 为 this 执行，把坏缓存（__nodeId
            // undefined 的 DOMTokenList Proxy）污染到 Element.prototype.__classList，
            // 全页 classList 随之炸 f64。真构造的原型链（ctor → HTMLElement =
            // Element）天然继承全部方法，拷贝本来就多余。
            real.__nodeId = el.__nodeId;
            real.__customUpgraded = true;
            var key = String(nodeId);
            if (window.__elCache) { window.__elCache[key] = real; }
            try {
                if (typeof real.connectedCallback === 'function') {
                    if (typeof __ctrace === 'function') { try { __ctrace('CC-upgrade nid=' + nodeId); } catch (eT4) {} }
                    var __ccr2 = real.connectedCallback();
                    if (__ccr2 && typeof __ccr2.catch === 'function') {
                        __ccr2.catch(function (eCcB) {
                            if (typeof __ctrace === 'function') { try { __ctrace('CCUP-ASYNC-THROW ' + String((eCcB && (eCcB.message || eCcB)) || eCcB).slice(0, 140) + ' STACK=' + String((eCcB && eCcB.stack) || '').split('\n').slice(0, 4).join('~').slice(0, 300)); } catch (e11) {} }
                        });
                    }
                }
            } catch (eCc) {
                if (typeof __ctrace === 'function') { try { __ctrace('UPCC-THROW ' + String((eCc && (eCc.message || eCc)) || eCc).slice(0, 160) + ' STACK=' + String((eCc && eCc.stack) || '').split('\n').slice(0, 4).join('~').slice(0, 320)); } catch (e7) {} }
            }
            return real;
        } catch (eS) { return el; }
    },
    get: function(name) { return this.__registry[name] || undefined; },
    upgrade: function(el) {
        var t = ((el && el.tagName) || '').toLowerCase();
        var ctor = this.__registry[t];
        if (ctor && el && typeof el.__nodeId === 'number') { this.__upgradeOne(el.__nodeId, ctor); }
    },
    whenDefined: function(name) {
        var self = this;
        return new Promise(function(resolve) {
            if (self.__registry[name]) { resolve(self.__registry[name]); return; }
            var tries = 0;
            var iv = setInterval(function() {
                tries++;
                if (self.__registry[name]) { clearInterval(iv); resolve(self.__registry[name]); return; }
                if (tries > 400) { clearInterval(iv); resolve(undefined); }
            }, 25);
        });
    }
};
if (typeof window.HTMLElement === 'undefined') { window.HTMLElement = Element; }
// M70.13: SVG/数学/表单元素构造器——框架（Vue/React）用 instanceof 检查元素类型。
// 对齐 Web 标准：所有 SVG 元素继承自 SVGElement → GraphicsElement → Element。
if (typeof window.SVGElement === 'undefined') { window.SVGElement = Element; }
if (typeof window.SVGSVGElement === 'undefined') { window.SVGSVGElement = Element; }
if (typeof window.HTMLCanvasElement === 'undefined') { window.HTMLCanvasElement = Element; }
// Canvas/WebGL stub：爬虫场景不要求像素渲染，但 getContext 必须返回不崩的 stub，
// 否则页面能力探测脚本（指纹/兼容检测）中断。M71.3 GAP-E/F。
Element.prototype.getContext = function(type) {
    if (type === '2d') {
        return window.__canvas2dStub();
    }
    if (type === 'webgl' || type === 'experimental-webgl' || type === 'webgl2') {
        return window.__webglStub(type);
    }
    return null;
};
Element.prototype.toDataURL = function() { return 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=='; };
Element.prototype.toBlob = function(cb) { if (typeof cb === 'function') cb(null); };
Element.prototype.captureStream = function() { return {}; };
if (typeof window.OffscreenCanvas === 'undefined') {
    window.OffscreenCanvas = function(w, h) { return { width: w||300, height: h||150, getContext: Element.prototype.getContext }; };
}
// 2D context stub：所有方法是 no-op，measureText.width 返回估算值。
window.__canvas2dStub = function() {
    var noop = function() {};
    return {
        canvas: null,
        fillStyle: '', strokeStyle: '', lineWidth: 1, font: '10px sans-serif',
        textAlign: 'start', textBaseline: 'alphabetic', globalAlpha: 1,
        globalCompositeOperation: 'source-over', lineCap: 'butt', lineJoin: 'miter',
        miterLimit: 10, shadowBlur: 0, shadowColor: 'rgba(0,0,0,0)',
        fillRect: noop, strokeRect: noop, clearRect: noop,
        beginPath: noop, closePath: noop, moveTo: noop, lineTo: noop,
        arc: noop, arcTo: noop, rect: noop, ellipse: noop, bezierCurveTo: noop,
        quadraticCurveTo: noop, fill: noop, stroke: noop, clip: noop,
        drawImage: noop, putImageData: noop,
        fillText: noop, strokeText: noop,
        measureText: function(t) { return { width: (String(t).length || 0) * 5, actualBoundingBoxAscent: 8, actualBoundingBoxDescent: 2 }; },
        save: noop, restore: noop, scale: noop, rotate: noop, translate: noop, transform: noop, setTransform: noop, resetTransform: noop,
        setLineDash: noop, getLineDash: function() { return []; },
        createLinearGradient: function() { return { addColorStop: noop }; },
        createRadialGradient: function() { return { addColorStop: noop }; },
        createPattern: function() { return {}; },
        getImageData: function(x,y,w,h) { return { width: w, height: h, data: new Uint8ClampedArray((w||0)*(h||0)*4) }; },
        isPointInPath: function() { return false; }, isPointInStroke: function() { return false; }
    };
};
// WebGL stub：getParameter 返回占位字符串/数字，方法返回 stub 对象。
window.__webglStub = function(type) {
    var noop = function() {};
    var stubObj = function() { return {}; };
    var ver = (type === 'webgl2') ? '2.0' : '1.0';
    var gl = {
        canvas: null, drawingBufferWidth: 300, drawingBufferHeight: 150,
        // 常量（部分）
        VERSION: 0x1F02, VENDOR: 0x1F00, RENDERER: 0x1F01, SHADING_LANGUAGE_VERSION: 0x8B8C,
        MAX_TEXTURE_SIZE: 0x0D33, MAX_VERTEX_ATTRIBS: 0x8869, MAX_VARYING_VECTORS: 0x8DFC,
        MAX_VERTEX_UNIFORM_VECTORS: 0x8DFB, MAX_FRAGMENT_UNIFORM_VECTORS: 0x8DFD,
        ALIASED_LINE_WIDTH_RANGE: 0x846E, ALIASED_POINT_SIZE_RANGE: 0x846D,
        // 方法
        getParameter: function(p) {
            if (p === 0x1F02) return 'WebGL ' + ver + ' (stub)';
            if (p === 0x1F00) return 'stub-vendor';
            if (p === 0x1F01) return 'stub-renderer';
            if (p === 0x8B8C) return 'WebGL GLSL ES ' + ver + ' (stub)';
            if (p === 0x0D33) return 16384;
            return null;
        },
        getSupportedExtensions: function() { return []; },
        getExtension: function() { return null; },
        createShader: stubObj, shaderSource: noop, compileShader: noop, getShaderParameter: function() { return true; },
        getShaderInfoLog: function() { return ''; }, deleteShader: noop,
        createProgram: stubObj, attachShader: noop, linkProgram: noop, useProgram: noop,
        getProgramParameter: function() { return true; }, getProgramInfoLog: function() { return ''; },
        deleteProgram: noop, validateProgram: noop,
        createBuffer: stubObj, bindBuffer: noop, bufferData: noop, deleteBuffer: noop,
        createTexture: stubObj, bindTexture: noop, texImage2D: noop, texParameteri: noop, deleteTexture: noop,
        createFramebuffer: stubObj, bindFramebuffer: noop, deleteFramebuffer: noop,
        createRenderbuffer: stubObj, bindRenderbuffer: noop, deleteRenderbuffer: noop,
        vertexAttribPointer: noop, enableVertexAttribArray: noop, disableVertexAttribArray: noop,
        drawArrays: noop, drawElements: noop, finish: noop, flush: noop,
        viewport: noop, clear: noop, clearColor: noop, enable: noop, disable: noop,
        depthFunc: noop, blendFunc: noop, cullFace: noop, frontFace: noop,
        getAttribLocation: function() { return 0; }, getUniformLocation: function() { return {}; },
        uniform1f: noop, uniform2f: noop, uniform3f: noop, uniform4f: noop,
        uniform1i: noop, uniform2i: noop, uniform3i: noop, uniform4i: noop,
        uniformMatrix4fv: noop, uniformMatrix3fv: noop,
        readPixels: noop
    };
    return gl;
};
// Intl（React/Intl.DateTimeFormat 等框架检测）
if (typeof Intl === 'undefined') {
    window.Intl = {
        DateTimeFormat: function() { return { format: function(d) { return String(d); } }; },
        NumberFormat: function() { return { format: function(n) { return String(n); } }; },
        Collator: function() { return { compare: function(a,b) { return String(a).localeCompare(b); } }; }
    };
}
if (typeof window.WebGLRenderingContext === 'undefined') {
    window.WebGLRenderingContext = function() {};
    window.WebGLRenderingContext.prototype = { VERSION: 0x1F02 };
}
if (typeof window.WebGL2RenderingContext === 'undefined') {
    window.WebGL2RenderingContext = function() {};
    window.WebGL2RenderingContext.prototype = { VERSION: 0x1F02 };
}
if (typeof window.HTMLInputElement === 'undefined') { window.HTMLInputElement = Element; }
if (typeof window.HTMLButtonElement === 'undefined') { window.HTMLButtonElement = Element; }
if (typeof window.HTMLDivElement === 'undefined') { window.HTMLDivElement = Element; }
if (typeof window.HTMLSpanElement === 'undefined') { window.HTMLSpanElement = Element; }
if (typeof window.HTMLAnchorElement === 'undefined') { window.HTMLAnchorElement = Element; }
if (typeof window.HTMLImageElement === 'undefined') { window.HTMLImageElement = Element; }
if (typeof window.HTMLIFrameElement === 'undefined') { window.HTMLIFrameElement = Element; }
// iframe 子文档能力 stub（M71.3 GAP-A 续）。爬虫场景：页面往 iframe 写内容再读，
// 需 contentDocument/contentWindow 返回可用对象。M71.3 iframe 69%→高。
Object.defineProperty(Element.prototype, 'contentDocument', {
    get: function() {
        // iframe 的 contentDocument：简易 document（含 body），页面可写内容。
        // M78.14: 惰性加载 src——data: URI 解析出 contentType（WPT
        // contentType 系列）；URL 记录解析后的地址（Document-URL 系列）。
        // 子文档 script 不执行（跨 realm 基建超 crawler-spa 目标）。
        if (this.tagName !== 'IFRAME') return null;
        if (!this.__contentDoc) {
            var doc = { body: null, documentElement: null,
                        URL: 'about:blank', documentURI: 'about:blank',
                        contentType: 'text/html',
                        write: function() {}, open: function() {}, close: function() {} };
            var src = __getAttr(this.__nodeId, 'src');
            if (src) {
                if (src.indexOf('data:') === 0) {
                    var dm = /data:([^;,]*)/i.exec(src);
                    if (dm && dm[1]) doc.contentType = dm[1];
                    doc.URL = src; doc.documentURI = src;
                } else if (typeof __fetchSync === 'function') {
                    var html = null;
                    try { html = __fetchSync(src); } catch (e) {}
                    // 扩展名近似 MIME（fetchSync 只回 body 拿不到头）
                    var em = /\.[a-z0-9]+$/i.exec(src.split('?')[0]);
                    if (em) {
                        var extMime = { '.txt': 'text/plain', '.html': 'text/html',
                            '.htm': 'text/html', '.xml': 'application/xml', '.xhtml': 'application/xhtml+xml',
                            '.svg': 'image/svg+xml', '.js': 'text/javascript', '.css': 'text/css' };
                        if (extMime[em[0].toLowerCase()]) doc.contentType = extMime[em[0].toLowerCase()];
                    }
                    try { doc.URL = new URL(src, location.href).href; } catch (e) { doc.URL = src; }
                    doc.documentURI = doc.URL;
                    this.__contentHtml = html;
                }
            }
            this.__contentDoc = doc;
        }
        return this.__contentDoc;
    },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'contentWindow', {
    get: function() {
        if (this.tagName !== 'IFRAME') return null;
        if (!this.__contentWin) {
            var self = this;
            // M92: 同一 iframe 反复取 contentWindow 必须同一对象（frames[i]
            // 跨访问 === 相等、frame0.onerror 赋值后 frames[0].setTimeout 内可见）。
            var __frameWin;
            // M92: detached 检查——iframe 从文档摘除后，其 window 的
            // setTimeout/setInterval 不再执行回调（WPT
            // settimeout-detached-iframe：attached 跑、detached 不跑不抛）。
            var __isConnected = function() {
                try { return !!self.parentNode; } catch (e) { return true; }
            };
            __frameWin = {
                document: self.contentDocument,
                frameElement: self,
                location: window.location,
                postMessage: function(msg) {
                    if (typeof window.onmessage === 'function') {
                        try { window.onmessage({ data: msg, origin: '*', source: __frameWin }); } catch(e) {}
                    }
                },
                // M92: timer 转发到共享 event loop；detached 后静默吞（不抛、
                // 回调不跑），仍返回数字 id（断言 typeof id === 'number'）。
                setTimeout: function(cb, delay) {
                    if (!__isConnected()) {
                        try { window.__timerSeq = (window.__timerSeq || 0) + 1; } catch (e) {}
                        return window.__timerSeq || 1;
                    }
                    return window.setTimeout(cb, delay);
                },
                setInterval: function(cb, delay) {
                    if (!__isConnected()) {
                        try { window.__timerSeq = (window.__timerSeq || 0) + 1; } catch (e) {}
                        return window.__timerSeq || 1;
                    }
                    return window.setInterval(cb, delay);
                },
                clearTimeout: function(id) { try { window.clearTimeout(id); } catch (e) {} },
                clearInterval: function(id) { try { window.clearTimeout(id); } catch (e) {} }
            };
            // M92: 跨 realm 近似——每帧独立 Function 包装：产出的函数带
            // __realmWin 标记，timer 回调抛错时向该帧 onerror 上报
            //（WPT settimeout/setinterval-cross-realm-callback-report-exception：
            // 异常上报到回调的全局而非调用方全局）。
            __frameWin.Function = function() {
                var args = Array.prototype.slice.call(arguments);
                var f = Function.apply(null, args);
                try {
                    Object.defineProperty(f, '__realmWin',
                        { value: __frameWin, configurable: true });
                } catch (e) {}
                return f;
            };
            __frameWin.Error = window.Error;
            this.__contentWin = __frameWin;
            // M92: frame 注册表——ancestorOrigins 连接性判断用（记录曾创建
            // 过 contentWindow 的 iframe nodeId，getter 里用 __getParent
            // 判断是否仍连接）。
            try {
                window.__frameIds = window.__frameIds || [];
                if (window.__frameIds.indexOf(this.__nodeId) < 0) {
                    window.__frameIds.push(this.__nodeId);
                }
            } catch (e) {}
            this.__contentDoc.defaultView = this.__contentWin;
        }
        return this.__contentWin;
    },
    enumerable: true, configurable: true
});
// srcdoc：iframe 的内嵌文档（页面常设 iframe.srcdoc='<div>..'</div>）。
Object.defineProperty(Element.prototype, 'srcdoc', {
    get: function() {
        try { return this.getAttribute('srcdoc') || ''; } catch(e) { return this.__srcdoc || ''; }
    },
    set: function(v) {
        this.__srcdoc = String(v);
        try { this.setAttribute('srcdoc', String(v)); } catch(e) {}
    },
    enumerable: true, configurable: true
});
// postMessage / onmessage：跨窗口消息（iframe 通信、SPA 路由）。M71.3 GAP-A。
window.postMessage = function(msg, _origin, _transfer) {
    // 同窗口 postMessage：触发 window.onmessage。异步语义简化为同步。
    if (typeof window.onmessage === 'function') {
        try { window.onmessage({ data: msg, origin: typeof location!=='undefined'?location.href:'*', source: window }); } catch(e) {}
    }
};
if (typeof window.onmessage === 'undefined') { window.onmessage = null; }
// M78.137: MessageChannel/MessagePort——React scheduler 的 work loop 用
// `new MessageChannel()` 做宏任务调度，缺定义时模块级抛错 → 水合清空 SSR
// 内容后客户端渲染永不执行 → render-url 空输出。peer 端口队列 + setTimeout
// 投递（onmessage 未设时消息排队——start() 或首次赋值时消费）。
function __MessagePort() {
    this.onmessage = null;
    this.__peer = null;
    this.__queue = [];
    this.__started = false;
}
__MessagePort.prototype.postMessage = function(msg) {
    var other = this.__peer;
    if (!other) return;
    var self = this;
    other.__queue.push({ msg: msg, ports: [self] });
    setTimeout(function() {
        if (!other.__queue.length) return;
        if (!other.__started && other.onmessage === null) return;
        other.__started = true;
        while (other.__queue.length) {
            var item = other.__queue.shift();
            if (typeof other.onmessage === 'function') {
                try {
                    other.onmessage({ data: item.msg, origin: '', source: null, ports: item.ports });
                } catch (e) {}
            }
        }
    }, 0);
};
__MessagePort.prototype.start = function() { this.__started = true; };
__MessagePort.prototype.close = function() { this.__started = false; this.onmessage = null; this.__queue = []; };
globalThis.MessagePort = __MessagePort;
globalThis.MessageChannel = function MessageChannel() {
    var p1 = new __MessagePort(), p2 = new __MessagePort();
    p1.__peer = p2; p2.__peer = p1;
    this.port1 = p1; this.port2 = p2;
};
try { Object.defineProperty(globalThis.MessageChannel.prototype, Symbol.toStringTag, { value: 'MessageChannel' }); } catch (e) {}
window.addEventListener = window.addEventListener || function(type, cb) {
    if (typeof cb === 'function' && type) {
        if (type === 'message') window.onmessage = cb;
    }
};
if (typeof window.HTMLOptionElement === 'undefined') { window.HTMLOptionElement = Element; }
if (typeof window.HTMLOptionsCollection === 'undefined') { window.HTMLOptionsCollection = Element; }
if (typeof window.HTMLLabelElement === 'undefined') { window.HTMLLabelElement = Element; }
if (typeof window.HTMLHeadingElement === 'undefined') { window.HTMLHeadingElement = Element; }
if (typeof window.HTMLParagraphElement === 'undefined') { window.HTMLParagraphElement = Element; }
if (typeof window.HTMLUListElement === 'undefined') { window.HTMLUListElement = Element; }
if (typeof window.HTMLLIElement === 'undefined') { window.HTMLLIElement = Element; }
if (typeof window.HTMLScriptElement === 'undefined') { window.HTMLScriptElement = Element; }
if (typeof window.HTMLLinkElement === 'undefined') { window.HTMLLinkElement = Element; }
if (typeof window.HTMLMetaElement === 'undefined') { window.HTMLMetaElement = Element; }
if (typeof window.HTMLStyleElement === 'undefined') { window.HTMLStyleElement = Element; }
if (typeof window.HTMLHeadElement === 'undefined') { window.HTMLHeadElement = Element; }
if (typeof window.HTMLBodyElement === 'undefined') { window.HTMLBodyElement = Element; }
if (typeof window.HTMLHtmlElement === 'undefined') { window.HTMLHtmlElement = Element; }
if (typeof window.HTMLSelectElement === 'undefined') { window.HTMLSelectElement = Element; }
if (typeof window.HTMLTextAreaElement === 'undefined') { window.HTMLTextAreaElement = Element; }
if (typeof window.HTMLFormElement === 'undefined') { window.HTMLFormElement = Element; }
if (typeof window.HTMLTableElement === 'undefined') { window.HTMLTableElement = Element; }
if (typeof window.HTMLUListElement === 'undefined') { window.HTMLUListElement = Element; }
if (typeof window.HTMLLIElement === 'undefined') { window.HTMLLIElement = Element; }
if (typeof window.HTMLOptionElement === 'undefined') { window.HTMLOptionElement = Element; }
if (typeof window.HTMLLabelElement === 'undefined') { window.HTMLLabelElement = Element; }
if (typeof window.HTMLHeadingElement === 'undefined') { window.HTMLHeadingElement = Element; }
if (typeof window.HTMLParagraphElement === 'undefined') { window.HTMLParagraphElement = Element; }
if (typeof window.HTMLBodyElement === 'undefined') { window.HTMLBodyElement = Element; }
if (typeof window.HTMLHeadElement === 'undefined') { window.HTMLHeadElement = Element; }
if (typeof window.HTMLScriptElement === 'undefined') { window.HTMLScriptElement = Element; }
if (typeof window.HTMLLinkElement === 'undefined') { window.HTMLLinkElement = Element; }
if (typeof window.HTMLStyleElement === 'undefined') { window.HTMLStyleElement = Element; }
if (typeof window.HTMLMetaElement === 'undefined') { window.HTMLMetaElement = Element; }
if (typeof window.HTMLDataListElement === 'undefined') { window.HTMLDataListElement = Element; }
if (typeof window.HTMLOutputElement === 'undefined') { window.HTMLOutputElement = Element; }
if (typeof window.HTMLProgressElement === 'undefined') { window.HTMLProgressElement = Element; }
if (typeof window.HTMLMeterElement === 'undefined') { window.HTMLMeterElement = Element; }
if (typeof window.HTMLFieldSetElement === 'undefined') { window.HTMLFieldSetElement = Element; }
if (typeof window.HTMLLegendElement === 'undefined') { window.HTMLLegendElement = Element; }

// M66: 框架全局变量桩（SSR hydration key / Next.js / Qwik 等）
// SvelteKit hydration key（svelte.dev）
if (typeof window.__sveltekit_1ntsbtp === 'undefined') { window.__sveltekit_1ntsbtp = {}; }
// Next.js 全局变量（react.dev / nextjs.org）
if (typeof window._N_E === 'undefined') { window._N_E = {}; }
// Qwik 全局变量（qwik.dev）
if (typeof window.__QI_KEY__ === 'undefined') { window.__QI_KEY__ = ''; }
if (typeof window.__QI_URL__ === 'undefined') { window.__QI_URL__ = ''; }
if (typeof window.__QI_BASE__ === 'undefined') { window.__QI_BASE__ = ''; }

// M80.28: requestIdleCallback / cancelIdleCallback——VueUse/框架调度依赖。
// 缺失时调用抛 ReferenceError 被组件吞掉 → 整个功能静默失效。用 setTimeout 近似。
if (typeof window.requestIdleCallback !== 'function') {
    window.requestIdleCallback = function(cb, options) {
        var opts = options || {};
        return setTimeout(function() {
            try { cb({ didTimeout: false, timeRemaining: function() { return Math.max(0, (opts.timeout || 50) - 1); } }); } catch (e) {}
        }, 1);
    };
}
if (typeof window.cancelIdleCallback !== 'function') {
    window.cancelIdleCallback = function(id) { clearTimeout(id); };
}
// M80.27: IntersectionObserver——爬虫语义：所有元素视为"已进入视口"，
// observe 后异步立即回调一次（isIntersecting=true），懒加载组件（vite
// sponsors、图片 lazy）立即渲染。旧 no-op 版让懒加载内容永不出现。
window.IntersectionObserver = function(callback, options) {
    this._cb = (typeof callback === 'function') ? callback : null;
    this._targets = [];
    var self = this;
    this.observe = function(el) {
        self._targets.push(el);
        // 异步触发一次回调（entries 含所有已观察元素，isIntersecting=true）
        setTimeout(function() {
            if (!self._cb) return;
            var entries = self._targets.map(function(t) {
                return {
                    target: t, isIntersecting: true, intersectionRatio: 1,
                    boundingClientRect: (t.getBoundingClientRect ? t.getBoundingClientRect() : {}),
                    intersectionRect: (t.getBoundingClientRect ? t.getBoundingClientRect() : {}),
                    rootBounds: null, time: performance.now()
                };
            });
            try { self._cb(entries, self); } catch (e) {}
        }, 0);
    };
    this.unobserve = function(el) {
        var i = self._targets.indexOf(el);
        if (i >= 0) self._targets.splice(i, 1);
    };
    this.disconnect = function() { self._targets = []; };
    this.takeRecords = function(){return [];};
};
window.ResizeObserver = function() { this.observe = function(){}; this.unobserve = function(){}; this.disconnect = function(){}; };

window.performance = {
    timing: { navigationStart: Date.now(), loadEventEnd: Date.now() },
    navigation: { type: 0, redirectCount: 0 },
    now: function() { return Date.now(); },
    getEntries: function() { return []; },
    getEntriesByName: function() { return []; },
    getEntriesByType: function() { return []; },
    mark: function() {},
    measure: function() {},
};

// window/document 的 parentElement + currentScript
// svelte 用 document.currentScript.parentElement
window.parentElement = null;
document.parentElement = null;
Object.defineProperty(document, 'currentScript', {
    get: function() {
        return { tagName: "SCRIPT", parentElement: null, src: window.location.href, hasAttribute: function() { return false; }, getAttribute: function() { return null; } };
    },
    configurable: true
});

// localStorage / sessionStorage
// M93.4: 存储区改为桥接 Rust StorageHandle（__storageGet/Set/Remove/Clear/
// Len/Key）——此前是纯 JS 对象，页面写入随每页引擎销毁而丢，导航循环层的
// 同源 storage 复用对页面不可见。桥接后写入落到 CURRENT_STORAGE 句柄上，
// 同源跳由导航循环复用句柄实现跨页保留；与 boa storage_shim 一致，两个
// area 共享同一后端。setItem/removeItem/clear 仍派发 StorageEvent（M78.22）。
function __StorageEvent(type, opts) {
    opts = opts || {};
    Event.call(this, type, opts);
    this.key = ('key' in opts) ? opts.key : null;
    this.oldValue = ('oldValue' in opts) ? opts.oldValue : null;
    this.newValue = ('newValue' in opts) ? opts.newValue : null;
    this.url = ('url' in opts) ? opts.url : '';
    this.storageArea = opts.storageArea || null;
}
__StorageEvent.prototype = Object.create(Event.prototype);
Object.defineProperty(__StorageEvent.prototype, Symbol.toStringTag, { value: 'StorageEvent' });
window.StorageEvent = __StorageEvent;
function __fireStorage(area, key, oldV, newV) {
    // M78.133: 规范——storage 事件不回发起变更的同一窗口。same-realm 近似：
    // 只在 iframe 子页脚本执行期间（__inIframeScript 标记）派发，父页自身
    // 的 setItem/clear 不触发（旧版父页 clear() 抢发 key=null 干扰断言顺序）。
    if (window.__inIframeScript !== true) return;
    // M92: url = 发起变更的文档 URL——same-realm 近似下取 __iframeSrcUrl
    //（iframe 派发器在子页脚本执行期间写入的子文档绝对地址），而非监听方
    //（父页）URL。WPT event_local_url / event_local_removeitem 断言
    // event.url === iframe 子文档 documentURI。
    var __evUrl = (typeof window.__iframeSrcUrl === 'string' && window.__iframeSrcUrl)
        ? window.__iframeSrcUrl
        : (typeof location !== 'undefined' ? location.href : '');
    setTimeout(function() {
        try {
            var ev = new __StorageEvent('storage', { key: key, oldValue: oldV,
                newValue: newV, url: __evUrl,
                storageArea: area });
            // M80.9: body onstorage 属性处理器——WPT event_basic 系列子页用
            // `<body onstorage="handleStorageEvent(event);">`（属性是 JS 代码
            // 字符串，需 new Function 包装；event 标识符由传参注入）。
            var __body = (typeof __getBody === 'function') ? __makeElement(__getBody(0)) : null;
            var __attr = (__body && typeof __getAttr === 'function') ? __getAttr(__body.__nodeId, 'onstorage') : null;
            if (__attr) {
                try {
                    var __fn = new Function('event', __attr);
                    __fn.call(__body, ev);
                } catch (pe) {}
            }
            window.dispatchEvent(ev);
        } catch (e) {}
    }, 0);
}
function __makeStorageArea(storeName) {
    function __stored(v) { return (v === null || v === undefined) ? null : v; }
    return {
        getItem: function(k) { return __stored(__storageGet(String(k))); },
        setItem: function(k, v) {
            k = String(k);
            var old = __stored(__storageGet(k));
            v = String(v);
            __storageSet(k, v);
            __fireStorage(this, k, old, v);
        },
        removeItem: function(k) {
            k = String(k);
            var old = __stored(__storageGet(k));
            __storageRemove(k);
            __fireStorage(this, k, old, null);
        },
        clear: function() {
            __storageClear();
            __fireStorage(this, null, null, null);
        },
        key: function(i) {
            return __stored(__storageKey(Number(i)));
        },
        get length() { return __storageLen(); }
    };
}
window.localStorage = __makeStorageArea('local');
window.sessionStorage = __makeStorageArea('session');

// URL 构造器（简化版——避免 QuickJS 不支持的复杂正则）
// M81.7: BroadcastChannel 桩——同进程全局事件总线（多 tab 场景单进程近似）。
if (typeof window.BroadcastChannel !== 'function') {
    var __bcChannels = {};
    window.BroadcastChannel = function(name) {
        var self = this;
        this.name = name;
        this.onmessage = null;
        this._id = Math.random().toString(36).slice(2);
        this.postMessage = function(msg) {
            var ch = __bcChannels[this.name];
            if (!ch) return;
            for (var i = 0; i < ch.length; i++) {
                if (ch[i]._id !== self._id && ch[i].onmessage) {
                    (function(oc, data) { setTimeout(function() { oc({ data: data }); }, 0); })(ch[i], msg);
                }
            }
        };
        this.close = function() {};
        if (!__bcChannels[name]) __bcChannels[name] = [];
        __bcChannels[name].push(this);
    };
    window.BroadcastChannel.prototype = { constructor: BroadcastChannel };
}
// M81.7: Notification 桩（permission 默认 denied，构造不抛错）。
if (typeof window.Notification === 'undefined') {
    window.Notification = function(title, opts) {
        this.title = String(title);
        this.body = (opts && opts.body) || '';
        this.close = function() {};
    };
    window.Notification.permission = 'denied';
    window.Notification.requestPermission = function(cb) {
        if (cb) setTimeout(function() { cb('denied'); }, 0);
        return Promise.resolve('denied');
    };
}
// M81.7: navigator.clipboard 桩（writeText 存全局，readText 返回空——权限 denied 语义）。
if (typeof navigator.clipboard === 'undefined') {
    var __clipboardText = '';
    navigator.clipboard = {
        writeText: function(t) { __clipboardText = String(t); return Promise.resolve(); },
        readText: function() { return Promise.resolve(__clipboardText); }
    };
}
var __blobUrls = {};
var __blobCounter = 0;

window.URL = function(input, base) {
    input = String(input);
    // M80.8: 孤立代理（ lone surrogate，如 \uD83D）→ U+FFFD 替换符
    //（WHATWG URL 规范：无效代理按替换符 percent-encode——WPT url-encoding
    // 期望 %EF%BF%BD；QuickJS String() 把孤立代理显示为 "U+d83d" 文本）。
    input = input.replace(/[\uD800-\uDFFF]/g, function(ch) {
        var c = ch.charCodeAt(0);
        if (c >= 0xD800 && c <= 0xDBFF) {
            // 高代理：看后一个是否低代理（合法对则保留）。
            var i = arguments[3];
            var next = input.charCodeAt(i + 1);
            if (next >= 0xDC00 && next <= 0xDFFF) return ch;
        }
        return '\uFFFD';
    });
    // M78.16: 纯 query / 纯 hash 的相对引用（WPT url-encoding）。
    function __encPart(str) {
        var out = '';
        for (var ci = 0; ci < str.length; ci++) {
            var ch = str.charAt(ci);
            var code = str.charCodeAt(ci);
            var safe = (code >= 65 && code <= 90) || (code >= 97 && code <= 122)
                || (code >= 48 && code <= 57)
                || '-_.!~*\'()/?:@&=+$,#%'.indexOf(ch) >= 0;
            out += safe ? ch : encodeURIComponent(ch);
        }
        return out;
    }
    if (base && (input.charAt(0) === '?' || input.charAt(0) === '#')) {
        var b0 = String(base);
        input = b0.split('?')[0].split('#')[0] + input;
    }
    if (base && input.indexOf('://') < 0 && !/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(input)) {
        var baseURL = String(base);
        if (input.charAt(0) === '.') {
            // 相对路径：取 base 的目录部分
            var baseDir = baseURL.substring(0, baseURL.lastIndexOf('/') + 1);
            input = baseDir + input.replace(/^\.\//, '');
        } else if (input.charAt(0) === '/') {
            // 绝对路径：取 base 的 origin
            var protoEnd = baseURL.indexOf('://');
            if (protoEnd > 0) {
                var hostPart = baseURL.substring(protoEnd + 3);
                var slashIdx = hostPart.indexOf('/');
                input = baseURL.substring(0, protoEnd + 3) + (slashIdx > 0 ? hostPart.substring(0, slashIdx) : hostPart) + input;
            }
        } else {
            // M78.133: 无前缀裸相对路径（'resources/child.html'）——旧版漏了
            // 这个分支原样返回，iframe src 解析全挂。取 base 目录拼接。
            var baseDir2 = baseURL.substring(0, baseURL.lastIndexOf('/') + 1);
            input = baseDir2 + input;
        }
    }
    this.href = input;
    this.protocol = (input.split('://')[0] || '') + ':';
    var afterProto = input.split('://')[1] || '';
    this.host = afterProto.split('/')[0] || '';
    this.hostname = this.host.split(':')[0];
    this.port = this.host.split(':')[1] || '';
    var afterHost = afterProto.substring(afterProto.indexOf('/') + 1);
    this.pathname = '/' + afterHost.split('?')[0].split('#')[0];
    var q = input.split('?')[1];
    var rawSearch = q ? q.split('#')[0] : '';
    // M78.16: query 非 ASCII 百分号编码（ß→%C3%9F，WHATWG 近似）。
    this.search = rawSearch ? '?' + __encPart(rawSearch) : '';
    // M78.16: href 同步编码后的 query（测试断言 .href）。
    if (rawSearch) {
        this.href = input.split('?')[0] + this.search + (input.indexOf('#') >= 0 ? '#' + input.split('#')[1] : '');
    }
    this.searchParams = new URLSearchParams(q || '');
    // M93: searchParams 变更回写锚点——set/append/delete/sort 后同步
    // URL.search/href（WHATWG 语义）。此前是构造时快照：Anubis 用
    // `v()` = new URL(...) + searchParams.set(...) 构造 pass-challenge
    // GET URL，query 全丢 → 服务端当无效请求打回挑战页。
    this.searchParams.__owner = this;
    this.hash = input.indexOf('#') >= 0 ? '#' + input.split('#')[1] : '';
    this.origin = this.protocol + '//' + this.host;
    this.toString = function() { return this.href; };
    this.toJSON = function() { return this.href; };
};

// URLSearchParams（WHATWG url-3x 子集，M79）
// 内部存 [key,value] 对数组：支持重复键（get→首个 / getAll→全部 / set→替换首个）。
// solid-router 的 parsePath 返回 searchParams 后调 forEach 转对象——此前缺 forEach
// 直接 TypeError "not a function"，module eval 中断 → solidjs.com 整页空白。
// M81.6: URL.createObjectURL/revokeObjectURL（blob URL 注册表）
window.URL.createObjectURL = function(obj) {
    var text = '';
    try { text = (typeof obj === 'string') ? obj : (obj.toString ? obj.toString() : ''); } catch(e) {}
    try { if (obj && obj._blob_text !== undefined) text = obj._blob_text; } catch(e) {}
    __blobCounter++;
    var url = 'blob:' + (typeof location !== 'undefined' && location.href ? location.href.split('#')[0] : 'null') + '/' + __blobCounter;
    __blobUrls[url] = text;
    return url;
};
window.URL.revokeObjectURL = function(url) { delete __blobUrls[url]; };

// M93: searchParams 变更回写所属 URL（WHATWG update steps 近似）——
// set/append/delete/sort 后同步 owner.search/href。仅当该 URLSearchParams
// 被 URL 构造器挂了 __owner 时生效（独立使用的实例不受影响）。
function __uspSyncOwner(sp) {
    var o = sp.__owner;
    if (!o) return;
    var qs;
    try { qs = sp.toString(); } catch (e) { return; }
    o.search = qs ? '?' + qs : '';
    var h = String(o.href || '');
    var noQ = h.split('?')[0].split('#')[0];
    var hash = (h.indexOf('#') >= 0) ? '#' + h.split('#')[1] : '';
    o.href = noQ + o.search + hash;
}
window.URLSearchParams = function(init) {
    this.__entries = [];
    var self = this;
    function pushDecoded(pair) {
        var kv = pair.split('=');
        var k = decodeURIComponent((kv[0] || '').replace(/\+/g, ' '));
        var v = decodeURIComponent((kv[1] || '').replace(/\+/g, ' '));
        self.__entries.push([k, v]);
    }
    if (typeof init === 'string') {
        var s = init.replace(/^\?/, '');
        if (s.length > 0) {
            var parts = s.split('&');
            for (var i = 0; i < parts.length; i++) {
                if (parts[i] !== '') pushDecoded(parts[i]);
            }
        }
    } else if (init && typeof init === 'object') {
        if (init.__entries) {
            // 另一个 URLSearchParams（拷贝）
            for (var j = 0; j < init.__entries.length; j++) {
                this.__entries.push([init.__entries[j][0], init.__entries[j][1]]);
            }
        } else if (Array.isArray(init)) {
            // sequence<sequence<USVString>>（如 Object.fromEntries 后的数组）
            for (var m = 0; m < init.length; m++) {
                if (Array.isArray(init[m]) && init[m].length >= 2) {
                    this.__entries.push([String(init[m][0]), String(init[m][1])]);
                }
            }
        } else {
            // record（普通对象）
            for (var key in init) {
                if (Object.prototype.hasOwnProperty.call(init, key)) {
                    this.__entries.push([String(key), String(init[key])]);
                }
            }
        }
    }
};
window.URLSearchParams.prototype.append = function(k, v) {
    this.__entries.push([String(k), String(v)]);
    __uspSyncOwner(this);
};
window.URLSearchParams.prototype['delete'] = function(k) {
    k = String(k);
    var out = [];
    for (var i = 0; i < this.__entries.length; i++) {
        if (this.__entries[i][0] !== k) out.push(this.__entries[i]);
    }
    this.__entries = out;
    __uspSyncOwner(this);
};
window.URLSearchParams.prototype.get = function(k) {
    k = String(k);
    for (var i = 0; i < this.__entries.length; i++) {
        if (this.__entries[i][0] === k) return this.__entries[i][1];
    }
    return null;
};
window.URLSearchParams.prototype.getAll = function(k) {
    k = String(k);
    var out = [];
    for (var i = 0; i < this.__entries.length; i++) {
        if (this.__entries[i][0] === k) out.push(this.__entries[i][1]);
    }
    return out;
};
window.URLSearchParams.prototype.has = function(k) {
    return this.get(String(k)) !== null;
};
window.URLSearchParams.prototype.set = function(k, v) {
    k = String(k);
    v = String(v);
    var found = false;
    var out = [];
    for (var i = 0; i < this.__entries.length; i++) {
        if (this.__entries[i][0] === k) {
            if (!found) {
                out.push([k, v]);
                found = true;
            }
        } else {
            out.push(this.__entries[i]);
        }
    }
    if (!found) out.push([k, v]);
    this.__entries = out;
    __uspSyncOwner(this);
};
window.URLSearchParams.prototype.sort = function() {
    this.__entries.sort(function(a, b) {
        return a[0] < b[0] ? -1 : (a[0] > b[0] ? 1 : 0);
    });
    __uspSyncOwner(this);
};
window.URLSearchParams.prototype.forEach = function(cb, thisArg) {
    for (var i = 0; i < this.__entries.length; i++) {
        cb.call(thisArg, this.__entries[i][1], this.__entries[i][0], this);
    }
};
// 迭代器协议：entries/keys/values + Symbol.iterator（解构 for-of / [...sp] 用）
function __uspMakeIter(get, len) {
    var idx = 0;
    var it = {
        next: function() {
            if (idx >= len()) return { done: true, value: undefined };
            return { done: false, value: get(idx++) };
        }
    };
    // 迭代器自身必须可迭代（协议：iterator[Symbol.iterator]() === iterator）
    it[Symbol.iterator] = function() { return it; };
    return it;
}
window.URLSearchParams.prototype.entries = function() {
    var e = this.__entries;
    return __uspMakeIter(function(i) { return [e[i][0], e[i][1]]; }, function() { return e.length; });
};
window.URLSearchParams.prototype.keys = function() {
    var e = this.__entries;
    return __uspMakeIter(function(i) { return e[i][0]; }, function() { return e.length; });
};
window.URLSearchParams.prototype.values = function() {
    var e = this.__entries;
    return __uspMakeIter(function(i) { return e[i][1]; }, function() { return e.length; });
};
window.URLSearchParams.prototype[Symbol.iterator] = window.URLSearchParams.prototype.entries;
window.URLSearchParams.prototype.toString = function() {
    function enc(s) {
        // application/x-www-form-urlencoded 序列化：空格→+，!'()* 也编码
        return encodeURIComponent(s)
            .replace(/%20/g, '+')
            .replace(/!/g, '%21').replace(/'/g, '%27')
            .replace(/\(/g, '%28').replace(/\)/g, '%29').replace(/\*/g, '%2A');
    }
    return this.__entries
        .map(function(e) { return enc(e[0]) + '=' + enc(e[1]); })
        .join('&');
};
Object.defineProperty(window.URLSearchParams.prototype, 'size', {
    get: function() { return this.__entries.length; }
});

// FileReader（爬虫场景：文件上传前读内容）
window.FileReader = function() {
    this.result = null;
    this.onload = null;
    this.readAsText = function(blob, encoding) {
        // 不真正读内容，几毫秒后触发 onload
        var self = this;
        setTimeout(function() {
            self.result = '';
            if (typeof self.onload === 'function') self.onload({ target: self, type: 'load' });
        }, 1);
    };
    this.readAsArrayBuffer = function(blob) { this.readAsText(blob); };
    this.readAsDataURL = function(blob) { this.result = 'data:,'; var self = this; setTimeout(function() { if (typeof self.onload === 'function') self.onload({ target: self, type: 'load' }); }, 1); };
    this.abort = function() {};
};

// MutationObserver（框架用，存回调但不触发）
// MutationObserver —— M78.11: 真实触发（近似）。observe 记录 target，
// DOM 变更入口 fire 挂起 observer 的回调（microtask 时机，records 近似）。
window.__activeObservers = [];
window.MutationObserver = function(cb) {
    var self = this;
    self.__cb = cb;
    self.__targets = [];
    self.observe = function(target, opts) {
        // M78.74: 参数校验（WPT MutationObserver-sanity）。
        opts = opts || {};
        // M78.88: 只有"三个选项都未提供且都为 falsy"才抛。
        // attributeOldValue/attributeFilter/characterDataOldValue 的 presence
        // auto-enables 对应观察（WPT 断言，不能 throw）。
        var hasAny = opts.childList || opts.attributes || opts.characterData ||
                     ('attributeOldValue' in opts) || ('attributeFilter' in opts) ||
                     ('characterDataOldValue' in opts);
        if (!hasAny) {
            throw new TypeError('MutationObserver: none of childList, attributes, or characterData are true');
        }
        // M78.95: 区分省略（auto-enable）和显式 false（throw）。
        if (opts.attributeOldValue === true && opts.attributes === false) {
            throw new TypeError('MutationObserver: attributeOldValue=true but attributes=false');
        }
        if (opts.attributeFilter && opts.attributes === false) {
            throw new TypeError('MutationObserver: attributeFilter but attributes=false');
        }
        if (opts.characterDataOldValue === true && opts.characterData === false) {
            throw new TypeError('MutationObserver: characterDataOldValue=true but characterData=false');
        }
        // auto-enable: attributeOldValue/attributeFilter 省略 attributes → attributes=true
        if (('attributeOldValue' in opts) || ('attributeFilter' in opts)) {
            if (opts.attributes === undefined) opts.attributes = true;
        }
        if ('characterDataOldValue' in opts) {
            if (opts.characterData === undefined) opts.characterData = true;
        }
        if (target && target.__nodeId !== undefined) {
            self.__targets.push(target);
            if (window.__activeObservers.indexOf(self) < 0) window.__activeObservers.push(self);
        } else if (target === document && typeof __findTag === 'function') {
            // M78.121: document 的 observe——通过 body 节点代理。
            var bodyId = __findTag('body');
            if (bodyId >= 0) {
                var bodyEl = __makeElement(bodyId);
                self.__targets.push(bodyEl);
                if (window.__activeObservers.indexOf(self) < 0) window.__activeObservers.push(self);
            }
        }
    };
    self.disconnect = function() {
        self.__targets = [];
        var i = window.__activeObservers.indexOf(self);
        if (i >= 0) window.__activeObservers.splice(i, 1);
    };
    self.takeRecords = function() { return []; };
};
// fire 变更通知：type ∈ attributes|childList，attributeName 可选。
window.__fireMutation = function(nodeId, type, attrName) {
    var observers = window.__activeObservers;
    if (!observers || !observers.length) return;
    for (var i = 0; i < observers.length; i++) {
        var obs = observers[i];
        for (var j = 0; j < obs.__targets.length; j++) {
            if (obs.__targets[j].__nodeId === nodeId) {
                var records = [{ type: type, target: obs.__targets[j],
                                 attributeName: attrName || null, addedNodes: [], removedNodes: [] }];
                (function(o, recs) {
                    Promise.resolve().then(function() {
                        try { o.__cb.call(o, recs, o); } catch (e) {}
                    });
                })(obs, records);
                break;
            }
        }
    }
};

// MatchMedia（CSS 媒体查询检测）
window.matchMedia = function(query) {
    // 解析 min-width/max-width 对比渲染宽度（默认 1280，桌面环境）。
    // 爬虫场景：框架用 matchMedia 做响应式判断，默认 match 桌面布局。
    var vw = 1280, vh = 720;
    var matched = true;
    try {
        var mw = query.match(/min-width\s*:\s*(\d+)/i);
        var Mw = query.match(/max-width\s*:\s*(\d+)/i);
        if (mw && vw < parseInt(mw[1], 10)) matched = false;
        if (Mw && vw > parseInt(Mw[1], 10)) matched = false;
        // prefers-color-scheme 等默认 false
        if (/prefers-color-scheme/i.test(query) && /dark/i.test(query)) matched = false;
        if (/prefers-reduced-motion/i.test(query)) matched = false;
    } catch(e) {}
    return { matches: matched, media: query, onchange: null, addListener: function(){}, removeListener: function(){}, addEventListener: function(){}, removeEventListener: function(){}, dispatchEvent: function() { return true; } };
};
// CSS 对象 + supports()：框架能力检测常用。M71.3 GAP-D。
if (typeof window.CSS === 'undefined') {
    window.CSS = {
        supports: function(prop, val) {
            // 爬虫场景：声明支持常见 CSS 属性，避免能力检测中断。
            if (arguments.length === 1) {
                // 单参数：整个声明，检测已知关键词
                return /flex|grid|transform|transition|animation|var\(|calc\(|position|display/i.test(prop);
            }
            return /flex|grid|transform|transition|animation/i.test(prop);
        },
        escape: function(s) { return String(s).replace(/([:.#])/g, '\\$1'); },
        registerProperty: function() {},
    };
}

// Event 构造器（强制覆盖——QuickJS 原生 Event 不设 bubbles/cancelable，框架依赖）
// 不用 if(typeof) 判断，直接覆盖确保 opts.bubbles 生效。
window.Event = function(type, opts) {
    this.type = type;
    this.bubbles = !!(opts && opts.bubbles);
    this.cancelable = !!(opts && opts.cancelable);
    this.target = null;
    this.currentTarget = null;
    this.defaultPrevented = false;
    this.timeStamp = Date.now();
};
window.Event.prototype.preventDefault = function() { this.defaultPrevented = true; };
window.Event.prototype.stopPropagation = function() { this.__stopPropagation = true; };
window.Event.prototype.stopImmediatePropagation = function() { this.__stopPropagation = true; };
window.CustomEvent = function(type, opts) { Event.call(this, type, opts); this.detail = (opts && opts.detail) || null; };
window.CustomEvent.prototype = Object.create(window.Event.prototype);

// === M71.1: 补齐 QuickJS 缺失的 Web API（纯 JS polyfill，爬虫场景够用）===

// structuredClone：深拷贝（JSON 实现，够用于普通对象/数组）
if (typeof structuredClone !== 'function') {
    window.structuredClone = function(obj) { return JSON.parse(JSON.stringify(obj)); };
}

// TextEncoder/TextDecoder（UTF-8，简化实现——爬虫场景不真正编码字节，
// 但 length/content 与原文一致，满足框架初始化检查）
if (typeof TextEncoder !== 'function') {
            window.TextEncoder = function() { this.encoding = 'utf-8'; };
            window.TextEncoder.prototype.encode = function(str) {
                str = str == null ? '' : String(str);
                // M93.7: 真 UTF-8 编码返回真 Uint8Array——旧版返回伪 Array
                //（length 对但类型错），cap.js 的 `crypto.subtle.digest(
                // 'SHA-256', g.encode(v))` 按 spec 要求 BufferSource。
                var out = [], i = 0;
                while (i < str.length) {
                    var c = str.charCodeAt(i++);
                    if (c >= 0xD800 && c <= 0xDBFF && i < str.length) {
                        var c2 = str.charCodeAt(i++);
                        if (c2 >= 0xDC00 && c2 <= 0xDFFF) { c = 0x10000 + ((c - 0xD800) << 10) + (c2 - 0xDC00); }
                        else { i--; c = 0xFFFD; }
                    }
                    if (c < 0x80) out.push(c);
                    else if (c < 0x800) out.push(0xC0 | (c >> 6), 0x80 | (c & 63));
                    else if (c < 0x10000) out.push(0xE0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
                    else out.push(0xF0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
                }
                return new Uint8Array(out);
            };
            window.TextEncoder.prototype.encodeInto = function(str, dst) {
                var e = this.encode(str);
                for (var i = 0; i < e.length && i < dst.length; i++) { dst[i] = e[i]; }
                return { read: e.length, written: Math.min(e.length, dst.length) };
            };
        }
if (typeof TextDecoder !== 'function') {
    window.TextDecoder = function(label) { this.encoding = (label || 'utf-8').toLowerCase(); };
    window.TextDecoder.prototype.decode = function(bytes) {
        if (!bytes) return '';
        if (typeof bytes === 'string') return bytes;
        // 伪解码：把 charCode 转回字符
        var s = '';
        var len = bytes.length || 0;
        for (var i = 0; i < len; i++) { s += String.fromCharCode(bytes[i]); }
        return s;
    };
}
// TextEncoderStream / TextDecoderStream（流式编码，框架特性检测用）
if (typeof TextEncoderStream !== 'function') {
    window.TextEncoderStream = function() { this.encoding = 'utf-8'; this.readable = { locked: false }; };
}
if (typeof TextDecoderStream !== 'function') {
    window.TextDecoderStream = function(label) { this.encoding = (label || 'utf-8').toLowerCase(); this.readable = { locked: false }; };
}

// Blob（构造器：存 size/type，爬虫不真正读内容）
if (typeof Blob !== 'function') {
    window.Blob = function(parts, opts) {
        // M93.7: 存内容（_blob_text）——cap.js 等用 `URL.createObjectURL(
        // new Blob([workerSrc]))` 构造 blob: Worker，此前桩不存内容，
        // createObjectURL 注册的是空串，Worker 拿到空源码。
        var text = '';
        if (parts) {
            for (var i = 0; i < parts.length; i++) {
                var p = parts[i];
                if (typeof p === 'string') { text += p; }
                else if (p && p.buffer instanceof ArrayBuffer) {
                    var u8 = new Uint8Array(p.buffer, p.byteOffset || 0, p.byteLength);
                    var s = '';
                    for (var j = 0; j < u8.length; j++) s += String.fromCharCode(u8[j]);
                    text += s;
                }
                else if (p instanceof ArrayBuffer) {
                    var u8b = new Uint8Array(p), sb = '';
                    for (var k = 0; k < u8b.length; k++) sb += String.fromCharCode(u8b[k]);
                    text += sb;
                }
                else if (p !== undefined && p !== null) { text += String(p); }
            }
        }
        this._blob_text = text;
        this.size = text.length;
        this.type = (opts && opts.type) || '';
    };
    window.Blob.prototype.text = function() { return Promise.resolve(this._blob_text || ''); };
    window.Blob.prototype.arrayBuffer = function() {
        var t = this._blob_text || '', u8 = new Uint8Array(t.length);
        for (var i = 0; i < t.length; i++) u8[i] = t.charCodeAt(i) & 255;
        return Promise.resolve(u8.buffer);
    };
}

// Headers（大小写不敏感的 get/set/append）
if (typeof Headers !== 'function') {
    window.Headers = function(init) {
        var store = {};
        function norm(k) { return String(k).toLowerCase(); }
        this.has = function(k) { return norm(k) in store; };
        this.get = function(k) { var v = store[norm(k)]; return v !== undefined ? v : null; };
        this.set = function(k, v) { store[norm(k)] = String(v); };
        this.append = function(k, v) {
            var n = norm(k);
            if (n in store) { store[n] = store[n] + ', ' + v; } else { store[n] = String(v); }
        };
        this.delete = function(k) { delete store[norm(k)]; };
        this.forEach = function(cb) { for (var k in store) { cb(store[k], k, this); } };
        if (init) {
            if (typeof init.forEach === 'function') { init.forEach(function(v, k) { this.append(k, v); }.bind(this)); }
            else { for (var k in init) { this.append(k, init[k]); } }
        }
    };
}
// FormData（key-value 表单数据）
if (typeof FormData !== 'function') {
    window.FormData = function() {
        var store = {};
        this.append = function(k, v) {
            if (!(k in store)) { store[k] = []; }
            store[k].push(String(v));
        };
        this.get = function(k) { return (k in store && store[k].length) ? store[k][0] : null; };
        this.getAll = function(k) { return store[k] || []; };
        this.has = function(k) { return k in store; };
        this.set = function(k, v) { store[k] = [String(v)]; };
        this.delete = function(k) { delete store[k]; };
        this.forEach = function(cb) { for (var k in store) { for (var i = 0; i < store[k].length; i++) { cb(store[k][i], k, this); } } };
    };
}

// document.createElementNS：存 namespaceURI + 创建元素（tagName 大写）
document.createElementNS = function(ns, tag) {
    var el = document.createElement(tag);
    if (el && typeof ns === 'string') {
        el.namespaceURI = ns;
        // M78.125: 非 HTML 命名空间的元素 tagName 不大写（SVG 语义）。
        if (ns !== 'http://www.w3.org/1999/xhtml') {
            el.__origTagName = tag;
        }
    }
    return el;
};

// Node 常量（框架常用 nodeType 判断；WPT 要求构造器与 prototype 双暴露）
window.Node = window.Node || function Node() {};
(function() {
    var consts = {
        ELEMENT_NODE: 1, ATTRIBUTE_NODE: 2, TEXT_NODE: 3, CDATA_SECTION_NODE: 4,
        ENTITY_REFERENCE_NODE: 5, ENTITY_NODE: 6, PROCESSING_INSTRUCTION_NODE: 7,
        COMMENT_NODE: 8, DOCUMENT_NODE: 9, DOCUMENT_TYPE_NODE: 10,
        DOCUMENT_FRAGMENT_NODE: 11, NOTATION_NODE: 12,
        DOCUMENT_POSITION_DISCONNECTED: 1, DOCUMENT_POSITION_PRECEDING: 2,
        DOCUMENT_POSITION_FOLLOWING: 4, DOCUMENT_POSITION_CONTAINS: 8,
        DOCUMENT_POSITION_CONTAINED_BY: 16, DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC: 32
    };
    for (var k in consts) {
        window.Node[k] = consts[k];
        window.Node.prototype[k] = consts[k];
    }
    Object.defineProperty(window.Node.prototype, Symbol.toStringTag, { value: 'Node' });
    Object.defineProperty(window.Node, Symbol.toStringTag, { value: 'Node' });
    window.Node.prototype.nodeType = 0;
})();
Element.prototype.webkitMatchesSelector = Element.prototype.matches;
// M78.46: lookupNamespaceURI / isDefaultNamespace——按 DOM Standard
// locate-a-namespace 算法（M90 对齐 WPT Node-lookupNamespaceURI 全集）：
// - Element：xml/xmlns 隐式绑定 → 自身 namespace+前缀匹配 → xmlns/xmlns:prefix
//   属性沿父元素链向上查
// - Document：委托 documentElement；DocumentType/DocumentFragment：恒 null
// - Attr：委托 ownerElement；其他节点：委托父元素
Element.prototype.lookupNamespaceURI = function(prefix) {
    if (prefix === '' || prefix === undefined) prefix = null;
    // fragment/doctype 伪节点：无命名空间（xml/xmlns 隐式绑定仅元素可用）
    if (this.__isFragment || this.__isDoctype) return null;
    if (typeof this.__nodeId !== 'number') return null;
    // 元素分支：隐式绑定 + 自身 namespace 前缀匹配（createElementNS 语义）
    var ownNs = this.namespaceURI;
    if (prefix === 'xml') return 'http://www.w3.org/XML/1998/namespace';
    if (prefix === 'xmlns') return 'http://www.w3.org/2000/xmlns/';
    if (ownNs) {
        var tn = String(this.tagName || '');
        var ownPrefix = (tn.indexOf(':') > 0) ? tn.slice(0, tn.indexOf(':')) : null;
        if (ownPrefix === prefix) return ownNs;
    }
    // xmlns 属性链查询（自身 + 祖先元素；祖先元素的自有 namespace 前缀
    // 匹配优先于 xmlns 属性——comment/text 委托父元素走完整元素算法）
    var cur = this;
    while (cur && typeof cur.__nodeId === 'number') {
        var ansNs = cur.namespaceURI;
        if (ansNs && cur.nodeType === 1 && !cur.__isFragment) {
            var atn = String(cur.tagName || '');
            var apx = (atn.indexOf(':') > 0) ? atn.slice(0, atn.indexOf(':')) : null;
            if (apx === prefix) return ansNs;
        }
        var attrs = (typeof __attrsOf === 'function') ? __attrsOf(cur.__nodeId) : '';
        var lines = (attrs || '').split(String.fromCharCode(10));
        if (prefix === null) {
            for (var i = 0; i < lines.length; i++) {
                var eq = lines[i].indexOf('=');
                if (eq > 0 && lines[i].slice(0, eq) === 'xmlns') return lines[i].slice(eq + 1);
            }
        } else {
            for (var j = 0; j < lines.length; j++) {
                var eq2 = lines[j].indexOf('=');
                if (eq2 > 0 && lines[j].slice(0, eq2) === 'xmlns:' + prefix) return lines[j].slice(eq2 + 1);
            }
        }
        var pid = (typeof __getParent === 'function') ? __getParent(cur.__nodeId) : -1;
        if (typeof pid !== 'number' || pid < 0) break;
        cur = __makeElement(pid);
    }
    return null;
};
Element.prototype.isDefaultNamespace = function(ns) {
    // DOM Standard：namespace 空串归一为 null；lookup(null) === ns 即 true。
    if (ns === '' || ns === undefined) ns = null;
    var found = this.lookupNamespaceURI(null);
    return found === ns;
};
Element.prototype.lookupPrefix = function(ns) {
    if (ns === null || ns === undefined) return null;
    var cur = this;
    while (cur && typeof cur.__nodeId === 'number') {
        var attrs = (typeof __attrsOf === 'function') ? __attrsOf(cur.__nodeId) : '';
        var lines = (attrs || '').split(String.fromCharCode(10));
        for (var i = 0; i < lines.length; i++) {
            var eq = lines[i].indexOf('=');
            if (eq > 0 && lines[i].slice(eq + 1) === ns) {
                var name = lines[i].slice(0, eq);
                return name === 'xmlns' ? null : name.slice(6);
            }
        }
        var pid = (typeof __getParent === 'function') ? __getParent(cur.__nodeId) : -1;
        if (typeof pid !== 'number' || pid < 0) break;
        cur = __makeElement(pid);
    }
    return null;
};
// M78.35: 节点等同（isSameNode 身份；isEqualNode tag+文本近似）+ composedPath。
// M78.61: baseURI = 文档 URL（无嵌入 base 变更场景下二者相等）。
Object.defineProperty(Element.prototype, 'baseURI', {
    get: function() {
        try { return document.URL || document.documentURI || ''; } catch (e) { return ''; }
    },
    enumerable: true, configurable: true
});
Element.prototype.isSameNode = function(other) { return !!other && other.__nodeId === this.__nodeId; };
Element.prototype.isEqualNode = function(other) {
    if (!other || other.__nodeId === undefined) return false;
    if (other.__nodeId === this.__nodeId) return true;
    if (__getTag(other.__nodeId) !== __getTag(this.__nodeId)) return false;
    return __getText(other.__nodeId) === __getText(this.__nodeId);
};
Event.prototype.composedPath = function() {
    var path = [];
    var t = this.target;
    while (t && t.__nodeId !== undefined) {
        path.push(t);
        var pid = __getParent(t.__nodeId);
        if (typeof pid !== 'number' || pid < 0) break;
        t = __makeElement(pid);
    }
    return path;
};
// M78.118: 缺失的接口构造器声明（WPT interface-objects 的 exist 断言）。
(function() {
    var missing = ['DOMImplementation', 'ProcessingInstruction', 'DocumentType',
                   'Attr', 'CharacterData', 'NodeIterator', 'CDATASection',
                   'EntityReference', 'Entity', 'Notation'];
    for (var i = 0; i < missing.length; i++) {
        var nm = missing[i];
        if (typeof window[nm] === 'undefined') {
            try {
                window[nm] = function() { throw new TypeError('Illegal constructor'); };
                Object.defineProperty(window[nm], 'name', { value: nm, writable: false, configurable: true });
                Object.defineProperty(window[nm].prototype, Symbol.toStringTag, { value: nm });
            } catch (e) {}
        }
    }
})();
// M78.112: DOMStringMap 全局构造器（只在全局暴露引用，不移除内部实现）。
if (typeof window.DOMStringMap === 'undefined') {
    window.DOMStringMap = function DOMStringMap() { throw new TypeError('Illegal constructor'); };
    Object.defineProperty(window.DOMStringMap.prototype, Symbol.toStringTag, { value: 'DOMStringMap' });
}
// M91: translate 反射——枚举属性 yes/no；无属性沿父链继承，默认 true
//（WPT translate-non-html-translation-mode：非 HTML 元素同样继承）。
Object.defineProperty(Element.prototype, 'translate', {
    get: function() {
        var cur = this;
        var guard = 0;
        while (cur && guard++ < 64) {
            var v = (typeof cur.__nodeId === 'number') ? __getAttr(cur.__nodeId, 'translate') : null;
            if (v !== null && v !== undefined) {
                var s = String(v).toLowerCase();
                if (s === 'yes' || s === '' ) return true;
                if (s === 'no') return false;
            }
            cur = cur.parentNode;
        }
        return true;
    },
    set: function(v) { __setAttr(this.__nodeId, 'translate', v ? 'yes' : 'no'); },
    enumerable: true, configurable: true
});
// M78.32: 反射属性批量（lang/dir/className/title/hidden/tabIndex/draggable）。
(function() {
    var refl = ['lang', 'dir', 'title', 'draggable'];
    for (var i = 0; i < refl.length; i++) {
        (function(name) {
            Object.defineProperty(Element.prototype, name, {
                get: function() { return __getAttr(this.__nodeId, name) || ''; },
                set: function(v) { __setAttr(this.__nodeId, name, String(v)); },
                enumerable: true, configurable: true
            });
        })(refl[i]);
    }
    Object.defineProperty(Element.prototype, 'className', {
        get: function() { return __getAttr(this.__nodeId, 'class') || ''; },
        set: function(v) { __setAttr(this.__nodeId, 'class', String(v)); },
        enumerable: true, configurable: true
    });
    // M80.18: __getAttr 原生桥对缺失属性返回 undefined（Rust None 过桥），
    // 不是 null——`!== null` 恒真，布尔反射属性全部假真。统一用 __hasAttrX。
    function __hasAttrX(el, name) {
        var v = __getAttr(el.__nodeId, name);
        return v !== null && v !== undefined;
    }
    Object.defineProperty(Element.prototype, 'hidden', {
        get: function() { return __hasAttrX(this, 'hidden'); },
        set: function(v) { if (v) __setAttr(this.__nodeId, 'hidden', ''); else __removeAttr(this.__nodeId, 'hidden'); },
        enumerable: true, configurable: true
    });
    Object.defineProperty(Element.prototype, 'tabIndex', {
        get: function() { var t = __getAttr(this.__nodeId, 'tabindex'); return t !== null ? parseInt(t, 10) : -1; },
        set: function(v) { __setAttr(this.__nodeId, 'tabindex', String(v)); },
        enumerable: true, configurable: true
    });
    // M78.87: 表单元素反射属性（value/checked/disabled/selected——M78.73 放错位置）。
    // M78.134b: value 反射仅表单元素语义（'value' in div 旧版为 true——
    // WPT textInput execCommand 用 `'value' in el` 分流读值，非表单元素
    // 应走 textContent，否则拿到空串断言失败）。
Object.defineProperty(Element.prototype, 'value', {
    get: function() {
        var tg = String((typeof __getTag === 'function') ? __getTag(this.__nodeId) : '').toLowerCase();
        var __formTags = '|input|textarea|select|option|button|meter|progress|param|li|';
        if (__formTags.indexOf('|' + tg + '|') >= 0) {
            return __getAttr(this.__nodeId, 'value') || '';
        }
        // M78.134b: 非表单元素回退 textContent（'value' in el 因原型反射恒
        // true，WPT textInput 用它分流读值——div 需要拿到内容而非空串）。
        return __getText(this.__nodeId) || '';
    },
    set: function(v) {
        var tg2 = String((typeof __getTag === 'function') ? __getTag(this.__nodeId) : '').toLowerCase();
        var __formTags2 = '|input|textarea|select|option|button|meter|progress|param|li|';
        if (__formTags2.indexOf('|' + tg2 + '|') >= 0) {
            __setAttr(this.__nodeId, 'value', String(v));
        }
    },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'checked', {
    // M80.18: undefined/null 双检（原生桥缺失属性返回 undefined）。
    get: function() { return __hasAttrX(this, 'checked'); },
    set: function(v) { if (v) __setAttr(this.__nodeId, 'checked', ''); else __removeAttr(this.__nodeId, 'checked'); },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'disabled', {
    get: function() { return __hasAttrX(this, 'disabled'); },
    set: function(v) { if (v) __setAttr(this.__nodeId, 'disabled', ''); else __removeAttr(this.__nodeId, 'disabled'); },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'selected', {
    get: function() { return __hasAttrX(this, 'selected'); },
    set: function(v) { if (v) __setAttr(this.__nodeId, 'selected', ''); else __removeAttr(this.__nodeId, 'selected'); },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'accessKey', {
        get: function() { return __getAttr(this.__nodeId, 'accesskey') || ''; },
        set: function(v) { __setAttr(this.__nodeId, 'accesskey', String(v)); },
        enumerable: true, configurable: true
    });
})();
// M78.18: Text/Comment 全局构造器（insertion-removing-steps 系列依赖
// `new Text(...)`）。
// M78.127: WebIDL 接口用赋值定义（configurable），顶层 function 声明是
// non-configurable 且无法 redefine（delete 返回 false，WPT interface-objects）。
globalThis.Text = function Text(data) { var n = document.createTextNode(data); return n; };
Text.prototype = Object.create(Element.prototype);
Object.defineProperty(Text.prototype, Symbol.toStringTag, { value: 'Text' });
window.Text = Text;
globalThis.Comment = function Comment(data) { var n = document.createComment(data); return n; };
Comment.prototype = Object.create(Element.prototype);
Object.defineProperty(Comment.prototype, Symbol.toStringTag, { value: 'Comment' });
window.Comment = Comment;
// M78.18: nodeValue/data 反射（textNode 的读写）。
Object.defineProperty(Element.prototype, 'nodeValue', {
    get: function() {
        // M78.51: 缓存判定结果（tag 是否文本），值仍实时（文本可变）。
        if (this.__isTextNd === undefined) {
            var tag0 = (typeof __getTag === 'function') ? __getTag(this.__nodeId) : '';
            this.__isTextNd = (!tag0 || tag0 === '__text__') ? 1 : 0;
        }
        if (this.__isTextNd) {
            var td = (typeof __textData === 'function') ? __textData(this.__nodeId) : '';
            return td || __getText(this.__nodeId);
        }
        return null;
    },
    set: function(v) {
        var tag = __getTag(this.__nodeId);
        if (!tag || tag === '__text__') __setText(this.__nodeId, String(v));
    },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'data', {
    get: function() { return this.nodeValue; },
    set: function(v) { this.nodeValue = v; },
    enumerable: true, configurable: true
});
// M78.18: document.domain（读写近似——同源恒等于 location.hostname）。
try {
    Object.defineProperty(document, 'domain', {
        get: function() {
            try { return location.hostname || 'localhost'; } catch (e) { return 'localhost'; }
        },
        set: function(v) { /* no-op 近似 */ },
        enumerable: true, configurable: true
    });
} catch (e) {}
// M78.15: nodeName/nodeType 按节点类型映射（#text/#comment/#document）。
Object.defineProperty(Element.prototype, 'nodeName', {
    get: function() {
        if (this.__nnCache !== undefined) return this.__nnCache;
        var tag = (typeof __getTag === 'function') ? __getTag(this.__nodeId) : '';
        var nn = (!tag || tag === '__text__') ? '#text' : this.tagName;
        this.__nnCache = nn;
        return nn;
    },
    enumerable: true, configurable: true
});
// M78.15: 元素级导航（firstElementChild/lastElementChild/childElementCount）。
(function() {
    function elementChildrenOf(id) {
        var cs = __children(id);
        if (!cs) return [];
        var out = [];
        var ids = cs.split(',');
        for (var i = 0; i < ids.length; i++) {
            if (!ids[i]) continue;
            var cid = parseInt(ids[i], 10);
            var tag = __getTag(cid);
            if (tag && tag !== '__text__') out.push(cid);
        }
        return out;
    }
    Object.defineProperty(Element.prototype, 'firstElementChild', {
        get: function() { var c = elementChildrenOf(this.__nodeId); return c.length ? __makeElement(c[0]) : null; },
        enumerable: true, configurable: true
    });
    Object.defineProperty(Element.prototype, 'lastElementChild', {
        get: function() { var c = elementChildrenOf(this.__nodeId); return c.length ? __makeElement(c[c.length - 1]) : null; },
        enumerable: true, configurable: true
    });
    Object.defineProperty(Element.prototype, 'childElementCount', {
        get: function() { return elementChildrenOf(this.__nodeId).length; },
        enumerable: true, configurable: true
    });
    Object.defineProperty(Element.prototype, 'previousElementSibling', {
        get: function() {
            var pid = __getParent(this.__nodeId);
            if (pid < 0) return null;
            var sibs = elementChildrenOf(pid), prev = null;
            for (var i = 0; i < sibs.length; i++) {
                if (sibs[i] === this.__nodeId) return prev ? __makeElement(prev) : null;
                prev = sibs[i];
            }
            return null;
        },
        enumerable: true, configurable: true
    });
    Object.defineProperty(Element.prototype, 'nextElementSibling', {
        get: function() {
            var pid = __getParent(this.__nodeId);
            if (pid < 0) return null;
            var sibs = elementChildrenOf(pid);
            for (var i = 0; i < sibs.length; i++) {
                if (sibs[i] === this.__nodeId) {
                    return (i + 1 < sibs.length) ? __makeElement(sibs[i + 1]) : null;
                }
            }
            return null;
        },
        enumerable: true, configurable: true
    });
})();

// AbortSignal（React/Next.js 在 GitHub 检查）
if (typeof AbortSignal === 'undefined') {
    window.AbortSignal = function() {
        this.aborted = false;
        this.reason = undefined;
        this.onabort = null;
        this.__listeners = {};
    };
    window.AbortSignal.prototype = Object.create(Object.prototype);
    AbortSignal.prototype.throwIfAborted = function() {
        if (this.aborted) throw this.reason;
    };
    AbortSignal.prototype.abort = function(reason) {
        if (this.aborted) return;
        this.aborted = true;
        this.reason = (reason !== undefined) ? reason : new DOMException('signal is aborted without reason', 'AbortError');
        var self = this;
        setTimeout(function() {
            if (typeof self.onabort === 'function') { try { self.onabort.call(self, new Event('abort')); } catch (e) {} }
            var cbs = self.__listeners && self.__listeners['abort'];
            if (cbs) { for (var i = 0; i < cbs.length; i++) { try { cbs[i].call(self, new Event('abort')); } catch (e) {} } }
        }, 0);
    };
    AbortSignal.prototype.addEventListener = function(t, cb) {
        (this.__listeners[t] = this.__listeners[t] || []).push(cb);
    };
    AbortSignal.prototype.removeEventListener = function(t, cb) {
        var a = this.__listeners[t];
        if (a) { var i = a.indexOf(cb); if (i >= 0) a.splice(i, 1); }
    };
    AbortSignal.prototype.dispatchEvent = function() { return true; };
    // M78.25: AbortSignal.timeout(ms)——定时自动 abort。
    AbortSignal.timeout = function(ms) {
        var sig = new AbortSignal();
        var err = new DOMException('signal timed out', 'TimeoutError');
        setTimeout(function() { sig.abort(err); }, Number(ms) || 0);
        return sig;
    };
    AbortSignal.any = function(signals) {
        var combined = new AbortSignal();
        (signals || []).forEach(function(s) {
            if (!s) return;
            if (s.aborted) { combined.abort(s.reason); return; }
            s.addEventListener('abort', function() { combined.abort(s.reason); });
        });
        return combined;
    };
    window.AbortController = function() { this.signal = new AbortSignal(); };
    window.AbortController.prototype.abort = function(reason) { this.signal.abort(reason); };
}

// EventTarget（Webpack chunk 加载器检查）
if (typeof EventTarget === 'undefined') {
    window.EventTarget = function() {
        this.__listeners = {};
    };
    EventTarget.prototype.addEventListener = function(type, cb) {
        if (!this.__listeners) this.__listeners = {};
        if (!this.__listeners[type]) this.__listeners[type] = [];
        this.__listeners[type].push(cb);
    };
    EventTarget.prototype.removeEventListener = function(type, cb) {};
    EventTarget.prototype.dispatchEvent = function(ev) {
        if (this.__listeners && this.__listeners[ev && ev.type]) {
            var cbs = this.__listeners[ev.type];
            for (var i = 0; i < cbs.length; i++) cbs[i].call(this, ev);
        }
        return true;
    };
}

// Document（框架检查 instanceof Document）
// M78.10: 真实体——WPT dom/common.js setupRangeTests 用 xmlDocument.
// createCDATASection/createComment/appendChild 链（空壳让 setup 崩，
// Range 簇整页 harness-not-run）。元素挂到独立子树（__createEl 默认挂 body，
// 可接受：WPT 只读节点属性/树形，不要求脱离主文档）。
// M92: location own accessor——Document.location 是 [Unforgeable] 属性，
// 必须是 own 描述符（get+set），且 get/set 在实例间同一函数对象
//（WPT document_location "Attribute getter/setter deduplication"）。
var __docLocGet = function() {
    return (this && this.__subLoc !== undefined) ? this.__subLoc : window.location;
};
var __docLocSet = function(v) {
    var loc = (this && this.__subLoc !== undefined) ? this.__subLoc : window.location;
    if (loc && typeof v === 'string') { try { loc.href = v; } catch (e) {} }
};
if (typeof Document === 'undefined') {
    window.Document = function Document() {
        this.nodeType = 9;
        this.nodeName = '#document';
        this.readyState = 'complete';
        this.contentType = 'application/xml';
        Object.defineProperty(this, 'location', {
            get: __docLocGet, set: __docLocSet, enumerable: true, configurable: false
        });
    };
    Document.prototype = Object.create(Object.prototype);
    Object.defineProperty(Document.prototype, Symbol.toStringTag, { value: 'Document' });
    Document.prototype.body = null;
    Document.prototype.documentElement = null;
    Document.prototype.addEventListener = function() {};
    Document.prototype.removeEventListener = function() {};
    Document.prototype.dispatchEvent = function() { return true; };
    Document.prototype.createElement = function(tag) { return document.createElement(tag); };
    Document.prototype.createElementNS = function(ns, tag) { return document.createElement(tag); };
    Document.prototype.createTextNode = function(t) { return document.createTextNode(t); };
    Document.prototype.createComment = function(t) { return document.createComment(t); };
    Document.prototype.createCDATASection = function(t) {
        return { nodeType: 4, nodeName: '#cdata-section', data: String(t), textContent: String(t) };
    };
    Document.prototype.createProcessingInstruction = function(target, data) {
        return { nodeType: 7, nodeName: String(target), data: String(data), target: String(target) };
    };
    Document.prototype.createDocumentFragment = function() { return document.createDocumentFragment(); };
    Document.prototype.createRange = function() { return new Range(); };
    Document.prototype.createEvent = function(t) { return document.createEvent(t); };
    Document.prototype.appendChild = function(child) {
        if (child && typeof child.__nodeId === 'number') __appendChild(this.__rootId || __getBody(0), child.__nodeId);
        return child;
    };
    Document.prototype.getElementsByTagName = function() { return []; };
    Document.prototype.getElementById = function() { return null; };
    Document.prototype.querySelector = function() { return null; };
    Document.prototype.querySelectorAll = function() { return []; };
    // M90: namespace 查询——documentElement 为 null 时 locate 返回 null
    //（WPT Node-lookupNamespaceURI 的 new Document() 断言）。
    Document.prototype.lookupNamespaceURI = function(prefix) {
        if (prefix === '' || prefix === undefined) prefix = null;
        var de = this.documentElement;
        if (!de || typeof de.lookupNamespaceURI !== 'function') return null;
        return de.lookupNamespaceURI(prefix);
    };
    Document.prototype.isDefaultNamespace = function(ns) {
        if (ns === '' || ns === undefined) ns = null;
        return this.lookupNamespaceURI(null) === ns;
    };
    Document.prototype.lookupPrefix = function() { return null; };
}
// M78.10: document.implementation —— dom/common.js L78 用
// implementation.createHTMLDocument（Node-removeChild 系列也依赖）。
// M78.54: document.doctype——DocumentType 伪节点（lookupNamespaceURI 返回
// null 即可，WPT 断言集）。
document.doctype = (function() {
    var dt = document.createElement('doctype');
    try { delete dt.__ntCache; dt.__isDoctype = true; } catch (e) {}
    return dt;
})();
Object.defineProperty(document.doctype, 'nodeType', {
    get: function() { return 10; },
    enumerable: true, configurable: true
});
document.doctype.lookupNamespaceURI = function() { return null; };
// M90: doctype isDefaultNamespace——locate(null)=null，ns 归一后 null 即 true
//（WPT 断言 isDefaultNamespace(null/'') === true）。
document.doctype.isDefaultNamespace = function(ns) {
    if (ns === '' || ns === undefined) ns = null;
    return ns === null;
};
document.doctype.lookupPrefix = function() { return null; };
document.doctype.name = 'html';
document.doctype.publicId = '';
document.doctype.systemId = '';
document.implementation = {
    createHTMLDocument: function(title) { return document.createHTMLDocument(title); },
    // M90: createDocument——XML 文档语义：documentElement.tagName 保留原始
    // 大小写（__origTagName）；importNode 进 HTML 文档后 tagName 大写
    //（cloneNode 不拷 __origTagName → tagName getter 自动 toUpperCase）。
    createDocument: function(ns, qname, doctype) {
        // M91: qname 为 null/'' → 无 documentElement（WPT
        // Document-createAttribute 的 createDocument(null, null, null)）。
        var root = null;
        if (qname !== null && qname !== undefined && qname !== '') {
            var name = String(qname);
            root = document.createElement(name);
            try { root.__origTagName = name; } catch (e) {}
            if (typeof ns === 'string' && ns) { try { root.namespaceURI = ns; } catch (e2) {} }
        }
        var xdoc = {
            nodeType: 9,
            nodeName: '#document',
            documentElement: root,
            contentType: 'application/xml',
            createElement: function(t) { return document.createElement(t); },
            createElementNS: function(nsn, t) { return document.createElementNS(nsn, t); },
            createTextNode: function(s) { return document.createTextNode(s); },
            createComment: function(s) { return document.createComment(s); },
            createAttribute: function(name2) { return __createAttributeNode(name2, false, xdoc); },
            createDocumentFragment: function() { return document.createDocumentFragment(); },
            importNode: function(n, deep) { return document.importNode(n, deep); },
            addEventListener: function() {},
            removeEventListener: function() {},
            appendChild: function(c) { return c; },
            querySelector: function() { return null; },
            querySelectorAll: function() { return []; },
            lookupNamespaceURI: function(prefix) {
                if (prefix === '' || prefix === undefined) prefix = null;
                return (prefix === null && typeof ns === 'string' && ns) ? ns : null;
            },
            isDefaultNamespace: function(nsn) {
                if (nsn === '' || nsn === undefined) nsn = null;
                return this.lookupNamespaceURI(null) === nsn;
            },
            lookupPrefix: function() { return null; }
        };
        try { root.ownerDocument = xdoc; } catch (e3) {}
        return xdoc;
    },
    hasFeature: function() { return true; }
};

// DocumentFragment
if (typeof DocumentFragment === 'undefined') {
    window.DocumentFragment = function() {};
    DocumentFragment.prototype = Object.create(Object.prototype);
    DocumentFragment.prototype.nodeType = 11;
    DocumentFragment.prototype.appendChild = function(child) { return child; };
    DocumentFragment.prototype.querySelector = function() { return null; };
    DocumentFragment.prototype.querySelectorAll = function() { return []; };
}

// HTMLTemplateElement
if (typeof HTMLTemplateElement === 'undefined') {
    window.HTMLTemplateElement = function() {};
    HTMLTemplateElement.prototype = Object.create(Object.prototype);
    HTMLTemplateElement.prototype.content = null;
}

// HTMLScriptElement（GitHub 类型检查）
if (typeof HTMLScriptElement === 'undefined') {
    window.HTMLScriptElement = function() {};
    HTMLScriptElement.prototype = Object.create(Object.prototype);
    HTMLScriptElement.prototype.src = '';
    HTMLScriptElement.prototype.type = '';
    HTMLScriptElement.prototype.defer = false;
}

undefined;
"#;

/// M93.14: 纯 JS WebCrypto 子集（G4 自研，零 Rust 依赖）——P-256 ECDH +
/// AES-256-GCM + HMAC-SHA256/HKDF。挂载到 globals 段暴露的 __subtleTarget
/// （window.crypto.subtle 的 Proxy target）。真实场景：xcancel 反自动化 VM 用
/// ECDH P-256 派生共享密钥 → HKDF → AES-GCM 加密指纹上报。向量验证：
/// RFC 5903 §8.1 / RFC 5114 A.6（ECDH）、NIST GCM App. B（AES-GCM）、
/// RFC 5869 A.1/A.3（HKDF），另经 Python/OpenSSL 独立实现交叉核对
/// （见 crates/cli/tests/integration_webcrypto.rs）。
#[cfg(feature = "quickjs")]
const QUICKJS_WEBCRYPTO_SHIM: &str = r#"
// ============================================================
// M93.14: 纯 JS WebCrypto 子集（G4 自研，零 Rust 依赖，二进制不涨）
// - P-256（secp256r1/NIST）ECDH：Jacobian 坐标点运算 + 标量乘
// - AES-GCM：AES 块加密（GF(2^8) 运行时生成 S-box，免手抄常量表）
//   + GHASH（GF(2^128) 右移法）+ CTR + tag 校验
// - HMAC-SHA256 / HKDF（RFC 5869）——复用 globals 段的 __sha256
// 全部挂在 __subtleTarget（window.crypto.subtle 的 Proxy target，见 globals 段）。
// 向量验证：RFC 5903 §8.1 / RFC 5114 A.6（ECDH）、NIST GCM Appendix B（AES-GCM）、
// RFC 5869 A.1/A.3（HKDF），另经 Go/OpenSSL 交叉核对。
// ============================================================
(function() {
    function err(name, msg) { var e = new Error(msg); e.name = name; return e; }
    function toU8(data) {
        if (data instanceof Uint8Array) return data;
        if (data instanceof ArrayBuffer) return new Uint8Array(data);
        if (data && data.buffer instanceof ArrayBuffer) {
            return new Uint8Array(data.buffer, data.byteOffset || 0, data.byteLength);
        }
        throw err('TypeError', 'crypto.subtle: data must be BufferSource');
    }
    function hexToBytes(h) {
        if (h.length % 2) h = '0' + h;
        var out = new Uint8Array(h.length / 2);
        for (var i = 0; i < out.length; i++) out[i] = parseInt(h.substr(2 * i, 2), 16);
        return out;
    }
    function bytesToHex(b) {
        var s = '';
        for (var i = 0; i < b.length; i++) s += (b[i] & 255).toString(16).padStart(2, '0');
        return s;
    }
    function biToBytes(bi, len) {
        var out = new Uint8Array(len);
        for (var i = len - 1; i >= 0; i--) { out[i] = Number(bi & 0xffn); bi >>= 8n; }
        return out;
    }
    function bytesToBi(b) {
        var v = 0n;
        for (var i = 0; i < b.length; i++) v = (v << 8n) | BigInt(b[i] & 255);
        return v;
    }
    function randBytes(n) {
        var out = new Uint8Array(n);
        if (typeof window !== 'undefined' && window.crypto && window.crypto.getRandomValues) {
            window.crypto.getRandomValues(out);
        } else {
            for (var i = 0; i < n; i++) out[i] = Math.floor(Math.random() * 256);
        }
        return out;
    }
    function concatBytes() {
        var len = 0, i;
        for (i = 0; i < arguments.length; i++) len += arguments[i].length;
        var out = new Uint8Array(len), off = 0;
        for (i = 0; i < arguments.length; i++) { out.set(arguments[i], off); off += arguments[i].length; }
        return out;
    }
    function b64url(bytes) {
        var s = '';
        for (var i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i] & 255);
        return (typeof window !== 'undefined' && window.btoa ? window.btoa : btoa)(s)
            .replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
    }
    function b64urlDecode(s) {
        s = String(s).replace(/-/g, '+').replace(/_/g, '/');
        while (s.length % 4) s += '=';
        var raw = (typeof window !== 'undefined' && window.atob ? window.atob : atob)(s);
        var out = new Uint8Array(raw.length);
        for (var i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
        return out;
    }
    function modPow(b, e, m) {
        var r = 1n;
        b %= m;
        while (e > 0n) {
            if (e & 1n) r = (r * b) % m;
            b = (b * b) % m;
            e >>= 1n;
        }
        return r;
    }
    function normName(algo) {
        var n = (algo && algo.name) ? String(algo.name) : String(algo || '');
        return n.replace(/-/g, '').toUpperCase();
    }
    function mkKey(type, extractable, algorithm, usages, material) {
        return { type: type, extractable: !!extractable, algorithm: algorithm, usages: usages || [], __material: material };
    }
    function checkUsage(key, want) {
        if (key && key.usages && key.usages.length && key.usages.indexOf(want) < 0) {
            throw err('InvalidAccessError', 'crypto.subtle: key usages do not permit ' + want);
        }
    }

    // ---------- P-256（secp256r1，RFC 5114 §2.6 域参数）----------
    var Pp = 0xffffffff00000001000000000000000000000000ffffffffffffffffffffffffn;
    var Pa = 0xffffffff00000001000000000000000000000000fffffffffffffffffffffffcn;
    var Pb = 0x5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604bn;
    var Pg = { x: 0x6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296n,
               y: 0x4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5n };
    var Pn = 0xffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551n;

    function modP(v) { var r = v % Pp; return r < 0n ? r + Pp : r; }
    var INF = { x: 1n, y: 1n, z: 0n };

    // Jacobian 倍点（a = -3 专用公式）
    function ecDouble(pt) {
        if (pt.z === 0n || pt.y === 0n) return INF;
        var delta = modP(pt.z * pt.z);
        var gamma = modP(pt.y * pt.y);
        var beta = modP(pt.x * gamma);
        var alpha = modP(3n * modP(pt.x - delta) * modP(pt.x + delta));
        var x3 = modP(alpha * alpha - 8n * beta);
        var ys = modP(pt.y + pt.z);
        var z3 = modP(ys * ys - gamma - delta);
        var y3 = modP(alpha * (4n * beta - x3) - 8n * modP(gamma * gamma));
        return { x: x3, y: y3, z: z3 };
    }
    // Jacobian 一般加法（EFD add-1998-cmo；u1==u2 时转倍点/无穷远）
    function ecAdd(p1, p2) {
        if (p1.z === 0n) return p2;
        if (p2.z === 0n) return p1;
        var z1z1 = modP(p1.z * p1.z), z2z2 = modP(p2.z * p2.z);
        var u1 = modP(p1.x * z2z2), u2 = modP(p2.x * z1z1);
        var s1 = modP(p1.y * p2.z * z2z2), s2 = modP(p2.y * p1.z * z1z1);
        if (u1 === u2) return (s1 === s2) ? ecDouble(p1) : INF;
        var h = modP(u2 - u1);
        var i = modP(4n * h * h);           // I = (2H)^2
        var j = modP(h * i);                // J = H·I
        var r = modP(2n * modP(s2 - s1));   // r = 2(S2−S1)
        var v = modP(u1 * i);               // V = U1·I
        var x3 = modP(r * r - j - 2n * v);
        var y3 = modP(r * (v - x3) - 2n * modP(s1 * j));
        var z3 = modP(2n * p1.z * p2.z * h);
        return { x: x3, y: y3, z: z3 };
    }
    // 标量乘：LSB-first double-and-add（数据量小、恒定时间非目标——爬虫场景）
    function ecMul(k, pt) {
        var r = INF, q = { x: pt.x, y: pt.y, z: pt.z === undefined ? 1n : pt.z };
        while (k > 0n) {
            if (k & 1n) r = ecAdd(r, q);
            q = ecDouble(q);
            k >>= 1n;
        }
        return r;
    }
    function ecAffine(pt) {
        if (pt.z === 0n) return null;
        var zi = modPow(pt.z, Pp - 2n, Pp);
        var z2 = modP(zi * zi);
        return { x: modP(pt.x * z2), y: modP(modP(pt.y * z2) * zi) };
    }
    function onCurve(x, y) {
        if (x < 0n || x >= Pp || y < 0n || y >= Pp) return false;
        return modP(y * y) === modP((x * x + Pa) * x + Pb);
    }
    function randomScalar() {
        for (var guard = 0; guard < 64; guard++) {
            var k = bytesToBi(randBytes(32));
            if (k >= 1n && k < Pn) return k;
        }
        return 1n; // 不可达（拒绝概率 2^-32 级），防御性兜底
    }
    function ecPubToRaw(m) {
        var out = new Uint8Array(65);
        out[0] = 4;
        out.set(biToBytes(m.x, 32), 1);
        out.set(biToBytes(m.y, 32), 33);
        return out;
    }
    function rawToEcPub(raw) {
        if (raw.length !== 65 || raw[0] !== 4) {
            throw err('DataError', 'crypto.subtle: EC raw public key must be 65-byte uncompressed (0x04||X||Y)');
        }
        var x = bytesToBi(raw.subarray(1, 33)), y = bytesToBi(raw.subarray(33, 65));
        if (!onCurve(x, y)) throw err('DataError', 'crypto.subtle: point is not on the P-256 curve');
        return { x: x, y: y };
    }

    // ---------- AES（运行时生成 S-box/逆表 + 密钥扩展 + 块加密）----------
    var AES_SBOX = (function() {
        function xt(a) { return ((a << 1) ^ ((a & 0x80) ? 0x1b : 0)) & 0xff; }
        var exp = new Array(256), log = new Array(256);
        exp[0] = 1;
        // 生成元 3（2 的阶只有 51，不能当生成元——exp/log 表会撞环）
        for (var i = 1; i < 256; i++) { exp[i] = exp[i - 1] ^ xt(exp[i - 1]); log[exp[i]] = i; }
        var s = new Uint8Array(256), inv = new Uint8Array(256);
        for (var i = 0; i < 256; i++) {
            var b = i ? exp[255 - log[i]] : 0;
            var t = b ^ ((b << 1 | b >>> 7) & 255) ^ ((b << 2 | b >>> 6) & 255)
                      ^ ((b << 3 | b >>> 5) & 255) ^ ((b << 4 | b >>> 4) & 255) ^ 0x63;
            s[i] = t & 255;
            inv[t & 255] = i;
        }
        return { s: s };
    })();
    function xt2(a) { return ((a << 1) ^ ((a & 0x80) ? 0x1b : 0)) & 0xff; }

    function aesKeyExpansion(key) {
        var nk = key.length >> 2, nr = nk + 6;
        if (nk !== 4 && nk !== 6 && nk !== 8) {
            throw err('DataError', 'crypto.subtle: AES key length must be 128/192/256 bits');
        }
        var w = [];
        for (var i = 0; i < nk; i++) w.push([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]]);
        var rcon = 1;
        for (var i = nk; i < 4 * (nr + 1); i++) {
            var t = w[i - 1].slice();
            if (i % nk === 0) {
                t = [AES_SBOX.s[t[1]], AES_SBOX.s[t[2]], AES_SBOX.s[t[3]], AES_SBOX.s[t[0]]];
                t[0] ^= rcon;
                rcon = xt2(rcon);
            } else if (nk > 6 && i % nk === 4) {
                for (var j = 0; j < 4; j++) t[j] = AES_SBOX.s[t[j]];
            }
            var p = w[i - nk];
            w.push([t[0] ^ p[0], t[1] ^ p[1], t[2] ^ p[2], t[3] ^ p[3]]);
        }
        return { w: w, nr: nr };
    }
    function aesAddRoundKey(s, w, rd) {
        for (var c = 0; c < 4; c++) for (var r = 0; r < 4; r++) s[c][r] ^= w[rd * 4 + c][r];
    }
    function aesSubBytes(s) {
        for (var c = 0; c < 4; c++) for (var r = 0; r < 4; r++) s[c][r] = AES_SBOX.s[s[c][r]];
    }
    function aesShiftRows(s) {
        for (var r = 1; r < 4; r++) {
            var row = [s[0][r], s[1][r], s[2][r], s[3][r]];
            for (var c = 0; c < 4; c++) s[c][r] = row[(c + r) % 4];
        }
    }
    function aesMixColumns(s) {
        for (var c = 0; c < 4; c++) {
            var a = s[c], t = a[0] ^ a[1] ^ a[2] ^ a[3];
            var a0 = a[0], a1 = a[1], a2 = a[2], a3 = a[3];
            a[0] = a0 ^ t ^ xt2(a0 ^ a1);
            a[1] = a1 ^ t ^ xt2(a1 ^ a2);
            a[2] = a2 ^ t ^ xt2(a2 ^ a3);
            a[3] = a3 ^ t ^ xt2(a3 ^ a0);
        }
    }
    function aesEncryptBlock(ctx, input) {
        var s = [], c, r;
        for (c = 0; c < 4; c++) s.push([input[4 * c], input[4 * c + 1], input[4 * c + 2], input[4 * c + 3]]);
        aesAddRoundKey(s, ctx.w, 0);
        for (var round = 1; round < ctx.nr; round++) {
            aesSubBytes(s);
            aesShiftRows(s);
            aesMixColumns(s);
            aesAddRoundKey(s, ctx.w, round);
        }
        aesSubBytes(s);
        aesShiftRows(s);
        aesAddRoundKey(s, ctx.w, ctx.nr);
        var out = new Uint8Array(16);
        for (c = 0; c < 4; c++) for (r = 0; r < 4; r++) out[4 * c + r] = s[c][r];
        return out;
    }

    // ---------- GHASH：GF(2^128) 乘法（右移法，NIST SP 800-38D）----------
    function bytesToWords(b) {
        var w = [];
        for (var i = 0; i < b.length; i += 4) {
            w.push((((b[i] || 0) << 24) | ((b[i + 1] || 0) << 16) | ((b[i + 2] || 0) << 8) | (b[i + 3] || 0)) >>> 0);
        }
        return w;
    }
    function wordsToBytes(w) {
        var out = new Uint8Array(4 * w.length);
        for (var i = 0; i < w.length; i++) {
            out[4 * i] = (w[i] >>> 24) & 255;
            out[4 * i + 1] = (w[i] >>> 16) & 255;
            out[4 * i + 2] = (w[i] >>> 8) & 255;
            out[4 * i + 3] = w[i] & 255;
        }
        return out;
    }
    function xorWords(a, b) { return [a[0] ^ b[0], a[1] ^ b[1], a[2] ^ b[2], a[3] ^ b[3]]; }
    function gfMul128(x, y) {
        var z = [0, 0, 0, 0], v = y.slice();
        for (var i = 0; i < 128; i++) {
            if ((x[i >> 5] >>> (31 - (i & 31))) & 1) {
                z = xorWords(z, v);
            }
            var lsb = v[3] & 1;
            v[3] = (v[3] >>> 1) | ((v[2] & 1) << 31);
            v[2] = (v[2] >>> 1) | ((v[1] & 1) << 31);
            v[1] = (v[1] >>> 1) | ((v[0] & 1) << 31);
            v[0] = v[0] >>> 1;
            if (lsb) v[0] ^= 0xe1000000; // R = 0xE1 || 0^120
        }
        return z;
    }
    function ghashBlocks(h, parts) {
        var y = [0, 0, 0, 0];
        for (var pi = 0; pi < parts.length; pi++) {
            var b = parts[pi];
            var padded = (b.length % 16 === 0) ? b : concatBytes(b, new Uint8Array(16 - (b.length % 16)));
            for (var off = 0; off < padded.length; off += 16) {
                y = gfMul128(xorWords(y, bytesToWords(padded.subarray(off, off + 16))), h);
            }
        }
        return y;
    }
    // J0：96-bit IV 直接拼接；其余长度 GHASH_H(IV||pad||0^64||len(IV)_64)
    function gcmJ0(h, iv) {
        if (iv.length === 12) {
            var w = bytesToWords(iv);
            return [w[0], w[1], w[2], 1];
        }
        if (iv.length < 1) throw err('DataError', 'crypto.subtle: AES-GCM iv must not be empty');
        var lenBlock = new Uint8Array(16);
        var bits = iv.length * 8;
        lenBlock[12] = (bits >>> 24) & 255;
        lenBlock[13] = (bits >>> 16) & 255;
        lenBlock[14] = (bits >>> 8) & 255;
        lenBlock[15] = bits & 255;
        return ghashBlocks(h, [iv, lenBlock]);
    }
    function gcmCtx(keyRaw, iv) {
        var ctx = aesKeyExpansion(keyRaw);
        var h = bytesToWords(aesEncryptBlock(ctx, new Uint8Array(16)));
        return { ctx: ctx, h: h, j0: gcmJ0(h, iv) };
    }
    function gcmKeystream(g, data) {
        var out = new Uint8Array(data.length);
        var cb = g.j0.slice();
        for (var off = 0; off < data.length; off += 16) {
            cb[3] = (cb[3] + 1) >>> 0; // inc32
            var ks = aesEncryptBlock(g.ctx, wordsToBytes(cb));
            var n = Math.min(16, data.length - off);
            for (var i = 0; i < n; i++) out[off + i] = data[off + i] ^ ks[i];
        }
        return out;
    }
    function gcmTag(g, aad, ct, tagLenBytes) {
        // GHASH 输入 = A||pad || C||pad || [len(A)]_64 || [len(C)]_64（SP 800-38D 7.1）
        var lenBlock = new Uint8Array(16);
        var aBits = aad.length * 8, cBits = ct.length * 8;
        lenBlock[4] = (aBits >>> 24) & 255;
        lenBlock[5] = (aBits >>> 16) & 255;
        lenBlock[6] = (aBits >>> 8) & 255;
        lenBlock[7] = aBits & 255;
        lenBlock[12] = (cBits >>> 24) & 255;
        lenBlock[13] = (cBits >>> 16) & 255;
        lenBlock[14] = (cBits >>> 8) & 255;
        lenBlock[15] = cBits & 255;
        var s = ghashBlocks(g.h, [aad, ct, lenBlock]);
        var ekJ0 = bytesToWords(aesEncryptBlock(g.ctx, wordsToBytes(g.j0)));
        var t = xorWords(s, ekJ0);
        return wordsToBytes(t).subarray(0, tagLenBytes);
    }
    function tagEqual(a, b) {
        if (a.length !== b.length) return false;
        var d = 0;
        for (var i = 0; i < a.length; i++) d |= a[i] ^ b[i];
        return d === 0;
    }

    // ---------- HMAC-SHA256 / HKDF（RFC 5869）----------
    function hmacSha256(key, msg) {
        if (key.length > 64) key = __sha256(key);
        var k = new Uint8Array(64);
        k.set(key);
        var ipad = new Uint8Array(64 + msg.length);
        var opad = new Uint8Array(64 + 32);
        for (var i = 0; i < 64; i++) { ipad[i] = k[i] ^ 0x36; opad[i] = k[i] ^ 0x5c; }
        ipad.set(msg, 64);
        opad.set(__sha256(ipad), 64);
        return __sha256(opad);
    }
    function hkdfExtract(salt, ikm) { return hmacSha256(salt, ikm); }
    function hkdfExpand(prk, info, L) {
        var t = new Uint8Array(0), okm = new Uint8Array(0), i = 1;
        while (okm.length < L) {
            t = hmacSha256(prk, concatBytes(t, info, new Uint8Array([i])));
            okm = concatBytes(okm, t);
            i++;
            if (i > 255) throw err('OperationError', 'crypto.subtle: HKDF-Expand length too large');
        }
        return okm.subarray(0, L);
    }

    // ---------- crypto.subtle 方法挂载（Proxy target 上）----------
    function trace(method, detail) {
        if (typeof __ctrace === 'function') {
            try { __ctrace('subtle.' + method + ' ' + (detail || '')); } catch (eT) {}
        }
    }
    var TAG_LENS = [32, 64, 96, 104, 112, 120, 128];

    function isEcdh(alg) { return alg && alg.name && normName(alg) === 'ECDH'; }
    function isAesGcm(alg) { var n = normName(alg); return n === 'AESGCM' || n === 'AES'; }
    function aesAlgoName(len) { return { name: 'AES-GCM', length: len }; }

    __subtleTarget.generateKey = function(algo, extractable, usages) {
        return new Promise(function(resolve, reject) {
            try {
                var name = normName(algo);
                if (name === 'ECDH') {
                    trace('generateKey ECDH');
                    if (!algo.namedCurve || String(algo.namedCurve).toUpperCase() !== 'P-256') {
                        throw err('NotSupportedError', 'crypto.subtle.generateKey: only namedCurve P-256 is supported');
                    }
                    for (var i = 0; i < (usages || []).length; i++) {
                        if (usages[i] !== 'deriveBits' && usages[i] !== 'deriveKey') {
                            throw err('DataError', 'crypto.subtle.generateKey: invalid ECDH usage ' + usages[i]);
                        }
                    }
                    if (usages && !usages.length) throw err('SyntaxError', 'crypto.subtle.generateKey: usages cannot be empty');
                    var d = randomScalar();
                    var pub = ecAffine(ecMul(d, Pg));
                    resolve({
                        publicKey: mkKey('public', true, { name: 'ECDH', namedCurve: 'P-256' }, [], { x: pub.x, y: pub.y }),
                        privateKey: mkKey('private', extractable, { name: 'ECDH', namedCurve: 'P-256' }, usages || [], { d: d })
                    });
                    return;
                }
                if (name === 'AESGCM' || name === 'AES') {
                    var len = algo && algo.length ? algo.length : 256;
                    if (len !== 128 && len !== 192 && len !== 256) {
                        throw err('DataError', 'crypto.subtle.generateKey: AES length must be 128/192/256');
                    }
                    trace('generateKey AES-' + len);
                    resolve(mkKey('secret', extractable, aesAlgoName(len), usages || [], { raw: randBytes(len >> 3) }));
                    return;
                }
                throw err('NotSupportedError', "crypto.subtle.generateKey: unsupported algorithm '" + name + "'");
            } catch (e) { reject(e); }
        });
    };

    __subtleTarget.exportKey = function(format, key) {
        return new Promise(function(resolve, reject) {
            try {
                format = String(format).toLowerCase();
                var alg = (key && key.algorithm) ? normName(key.algorithm) : '';
                var m = key ? key.__material : null;
                if (!m) throw err('InvalidAccessError', 'crypto.subtle.exportKey: not a CryptoKey');
                if (format === 'raw') {
                    if (alg === 'ECDH') {
                        if (key.type !== 'public') {
                            throw err('InvalidAccessError', 'crypto.subtle.exportKey(raw): only EC public keys are exportable (use jwk/pkcs8 for private)');
                        }
                        trace('exportKey ECDH raw');
                        resolve(ecPubToRaw(m).buffer);
                        return;
                    }
                    if (alg === 'AESGCM' || alg === 'AES') {
                        trace('exportKey AES raw');
                        resolve(m.raw.slice().buffer);
                        return;
                    }
                } else if (format === 'jwk') {
                    if (alg === 'ECDH') {
                        trace('exportKey ECDH jwk');
                        var jwk = { kty: 'EC', crv: 'P-256', key_ops: key.usages.slice(), ext: !!key.extractable };
                        if (key.type === 'public') {
                            jwk.x = b64url(biToBytes(m.x, 32));
                            jwk.y = b64url(biToBytes(m.y, 32));
                        } else {
                            var pub = ecAffine(ecMul(m.d, Pg));
                            jwk.x = b64url(biToBytes(pub.x, 32));
                            jwk.y = b64url(biToBytes(pub.y, 32));
                            jwk.d = b64url(biToBytes(m.d, 32));
                        }
                        resolve(jwk);
                        return;
                    }
                    if (alg === 'AESGCM' || alg === 'AES') {
                        trace('exportKey AES jwk');
                        resolve({
                            kty: 'oct', k: b64url(m.raw),
                            key_ops: key.usages.slice(), ext: !!key.extractable
                        });
                        return;
                    }
                }
                throw err('NotSupportedError', "crypto.subtle.exportKey: format '" + format + "' not supported for " + alg);
            } catch (e) { reject(e); }
        });
    };

    __subtleTarget.importKey = function(format, keyData, algo, extractable, usages) {
        return new Promise(function(resolve, reject) {
            try {
                format = String(format).toLowerCase();
                var name = normName(algo);
                if (name === 'ECDH') {
                    if (!algo.namedCurve || String(algo.namedCurve).toUpperCase() !== 'P-256') {
                        throw err('NotSupportedError', 'crypto.subtle.importKey: only namedCurve P-256 is supported');
                    }
                    if (format === 'raw') {
                        var pub = rawToEcPub(toU8(keyData));
                        if (usages && usages.length) {
                            throw err('DataError', 'crypto.subtle.importKey: EC public key usages must be empty');
                        }
                        trace('importKey ECDH raw public');
                        resolve(mkKey('public', extractable, { name: 'ECDH', namedCurve: 'P-256' }, [], pub));
                        return;
                    }
                    if (format === 'jwk') {
                        var jwk = keyData;
                        if (!jwk || jwk.kty !== 'EC' || String(jwk.crv || '').toUpperCase() !== 'P-256') {
                            throw err('DataError', 'crypto.subtle.importKey: JWK must be {kty:EC, crv:P-256}');
                        }
                        if (jwk.d) {
                            for (var i = 0; i < (usages || []).length; i++) {
                                if (usages[i] !== 'deriveBits' && usages[i] !== 'deriveKey') {
                                    throw err('DataError', 'crypto.subtle.importKey: invalid ECDH private usage ' + usages[i]);
                                }
                            }
                            var d = bytesToBi(b64urlDecode(jwk.d));
                            if (d < 1n || d >= Pn) throw err('DataError', 'crypto.subtle.importKey: JWK d out of range');
                            trace('importKey ECDH jwk private');
                            resolve(mkKey('private', extractable, { name: 'ECDH', namedCurve: 'P-256' }, usages || [], { d: d }));
                            return;
                        }
                        if (jwk.x && jwk.y) {
                            var x = bytesToBi(b64urlDecode(jwk.x)), y = bytesToBi(b64urlDecode(jwk.y));
                            if (!onCurve(x, y)) throw err('DataError', 'crypto.subtle.importKey: JWK point is not on the P-256 curve');
                            if (usages && usages.length) {
                                throw err('DataError', 'crypto.subtle.importKey: EC public key usages must be empty');
                            }
                            trace('importKey ECDH jwk public');
                            resolve(mkKey('public', extractable, { name: 'ECDH', namedCurve: 'P-256' }, [], { x: x, y: y }));
                            return;
                        }
                        throw err('DataError', 'crypto.subtle.importKey: EC JWK must contain d (private) or x/y (public)');
                    }
                    throw err('NotSupportedError', "crypto.subtle.importKey: format '" + format + "' not supported for ECDH");
                }
                if (name === 'AESGCM' || name === 'AES') {
                    var raw;
                    if (format === 'raw') raw = toU8(keyData).slice();
                    else if (format === 'jwk') {
                        if (!keyData || keyData.kty !== 'oct' || !keyData.k) {
                            throw err('DataError', 'crypto.subtle.importKey: AES JWK must be {kty:oct, k:base64url}');
                        }
                        raw = b64urlDecode(keyData.k);
                    } else {
                        throw err('NotSupportedError', "crypto.subtle.importKey: format '" + format + "' not supported for AES-GCM");
                    }
                    var len = raw.length * 8;
                    if (len !== 128 && len !== 192 && len !== 256) {
                        throw err('DataError', 'crypto.subtle.importKey: AES key length must be 128/192/256 bits');
                    }
                    for (var j = 0; j < (usages || []).length; j++) {
                        if (usages[j] !== 'encrypt' && usages[j] !== 'decrypt') {
                            throw err('DataError', 'crypto.subtle.importKey: invalid AES-GCM usage ' + usages[j]);
                        }
                    }
                    if (usages && !usages.length) {
                        throw err('SyntaxError', 'crypto.subtle.importKey: usages cannot be empty for secret keys');
                    }
                    trace('importKey AES-' + len);
                    resolve(mkKey('secret', extractable, aesAlgoName(len), usages || [], { raw: raw }));
                    return;
                }
                if (name === 'HKDF') {
                    if (format !== 'raw') {
                        throw err('NotSupportedError', "crypto.subtle.importKey: format '" + format + "' not supported for HKDF");
                    }
                    for (var k = 0; k < (usages || []).length; k++) {
                        if (usages[k] !== 'deriveBits' && usages[k] !== 'deriveKey') {
                            throw err('DataError', 'crypto.subtle.importKey: invalid HKDF usage ' + usages[k]);
                        }
                    }
                    if (usages && !usages.length) {
                        throw err('SyntaxError', 'crypto.subtle.importKey: usages cannot be empty for HKDF keys');
                    }
                    trace('importKey HKDF raw ' + toU8(keyData).length + 'B');
                    // 标准语义：keyData 本身是 IKM 材料；salt 在 deriveKey 的参数里
                    resolve(mkKey('secret', false, { name: 'HKDF' }, usages || [], { ikm: toU8(keyData).slice() }));
                    return;
                }
                throw err('NotSupportedError', "crypto.subtle.importKey: unsupported algorithm '" + name + "'");
            } catch (e) { reject(e); }
        });
    };

    function deriveEcdhBits(algo, baseKey, length) {
        if (!baseKey || baseKey.type !== 'private' || !isEcdh(baseKey.algorithm)) {
            throw err('InvalidAccessError', 'crypto.subtle.deriveBits: baseKey must be an ECDH private key');
        }
        checkUsage(baseKey, 'deriveBits');
        var peer = algo && algo.public;
        if (!peer || !peer.__material || (peer.type !== 'public')) {
            throw err('InvalidAccessError', 'crypto.subtle.deriveBits: algo.public must be an ECDH public key');
        }
        var shared = ecAffine(ecMul(baseKey.__material.d, peer.__material));
        if (!shared) throw err('OperationError', 'crypto.subtle.deriveBits: shared point at infinity');
        if (length % 8 !== 0) throw err('DataError', 'crypto.subtle.deriveBits: length must be a multiple of 8');
        if (length > 256) throw err('DataError', 'crypto.subtle.deriveBits: ECDH P-256 yields at most 256 bits');
        return biToBytes(shared.x, 32).subarray(0, length >> 3);
    }

    __subtleTarget.deriveBits = function(algo, baseKey, length) {
        return new Promise(function(resolve, reject) {
            try {
                var name = normName(algo);
                if (name === 'HKDF') checkUsage(baseKey, 'deriveBits');
                if (name === 'ECDH') {
                    trace('deriveBits ECDH ' + length);
                    resolve(deriveEcdhBits(algo, baseKey, length).buffer);
                    return;
                }
                if (name === 'HKDF') {
                    trace('deriveBits HKDF ' + length);
                    var salt = algo.salt ? toU8(algo.salt) : new Uint8Array(0);
                    var info = algo.info ? toU8(algo.info) : new Uint8Array(0);
                    var prk = hkdfExtract(salt, baseKey.__material.ikm);
                    resolve(hkdfExpand(prk, info, length >> 3).slice().buffer);
                    return;
                }
                throw err('NotSupportedError', "crypto.subtle.deriveBits: unsupported algorithm '" + name + "'");
            } catch (e) { reject(e); }
        });
    };

    __subtleTarget.deriveKey = function(algo, baseKey, derivedAlgo, extractable, usages) {
        return new Promise(function(resolve, reject) {
            try {
                var name = normName(algo);
                var dname = normName(derivedAlgo);
                if (name === 'ECDH') checkUsage(baseKey, 'deriveKey');
                if (name === 'HKDF') checkUsage(baseKey, 'deriveKey');
                var bits;
                if (name === 'ECDH') {
                    trace('deriveKey ECDH -> ' + dname);
                    bits = deriveEcdhBits(algo, baseKey, 256);
                } else if (name === 'HKDF') {
                    trace('deriveKey HKDF -> ' + dname);
                    var salt = algo.salt ? toU8(algo.salt) : new Uint8Array(0);
                    var info = algo.info ? toU8(algo.info) : new Uint8Array(0);
                    var prk = hkdfExtract(salt, baseKey.__material.ikm);
                    var len = (derivedAlgo && derivedAlgo.length) ? derivedAlgo.length : 256;
                    if (len !== 128 && len !== 192 && len !== 256) {
                        throw err('DataError', 'crypto.subtle.deriveKey: AES length must be 128/192/256');
                    }
                    bits = hkdfExpand(prk, info, len >> 3);
                } else {
                    throw err('NotSupportedError', "crypto.subtle.deriveKey: unsupported algorithm '" + name + "'");
                }
                if (dname === 'AESGCM' || dname === 'AES') {
                    resolve(mkKey('secret', extractable, aesAlgoName(bits.length * 8), usages || [], { raw: bits.slice() }));
                    return;
                }
                if (name === 'HKDF' && dname === 'HKDF') {
                    resolve(mkKey('secret', false, { name: 'HKDF' }, usages || [], { ikm: bits.slice() }));
                    return;
                }
                throw err('NotSupportedError', "crypto.subtle.deriveKey: unsupported derived algorithm '" + dname + "'");
            } catch (e) { reject(e); }
        });
    };

    function gcmParams(algo) {
        if (!algo || !algo.iv) throw err('TypeError', 'crypto.subtle: AES-GCM requires iv');
        var iv = toU8(algo.iv);
        if (iv.length < 1) throw err('DataError', 'crypto.subtle: AES-GCM iv must not be empty');
        var aad = algo.additionalData ? toU8(algo.additionalData) : new Uint8Array(0);
        var tagLen = (algo.tagLength === undefined || algo.tagLength === null) ? 128 : algo.tagLength;
        if (TAG_LENS.indexOf(tagLen) < 0) {
            throw err('OperationError', 'crypto.subtle: AES-GCM tagLength ' + tagLen + ' not supported');
        }
        return { iv: iv, aad: aad, tagLenBytes: tagLen >> 3 };
    }

    __subtleTarget.encrypt = function(algo, key, data) {
        return new Promise(function(resolve, reject) {
            try {
                var name = normName(algo);
                if (name === 'AESGCM' || name === 'AES') {
                    checkUsage(key, 'encrypt');
                    if (!key || key.type !== 'secret' || !key.__material || !key.__material.raw) {
                        throw err('InvalidAccessError', 'crypto.subtle.encrypt: key must be an AES secret key');
                    }
                    var p = gcmParams(algo);
                    var g = gcmCtx(key.__material.raw, p.iv);
                    var plain = toU8(data);
                    // M93.14-diag: fp 明文捕获（区分 spec 缺口 vs 身份信号——
                    // VM 不可见的 shim 源码级）
                    if (plain.length > 0 && plain.length < 4096 && typeof __ctrace === 'function') {
                        try {
                            var __txt = '';
                            for (var ti = 0; ti < plain.length; ti++) __txt += String.fromCharCode(plain[ti]);
                            __ctrace('FP-PLAIN ' + __txt.slice(0, 1200));
                        } catch (eFp) {}
                    }
                    var ct = gcmKeystream(g, plain);
                    var tag = gcmTag(g, p.aad, ct, p.tagLenBytes);
                    trace('encrypt AES-GCM ' + plain.length + 'B iv=' + p.iv.length + 'B');
                    resolve(concatBytes(ct, tag).buffer);
                    return;
                }
                throw err('NotSupportedError', "crypto.subtle.encrypt: unsupported algorithm '" + name + "'");
            } catch (e) { reject(e); }
        });
    };

    __subtleTarget.decrypt = function(algo, key, data) {
        return new Promise(function(resolve, reject) {
            try {
                var name = normName(algo);
                if (name === 'AESGCM' || name === 'AES') {
                    checkUsage(key, 'decrypt');
                    if (!key || key.type !== 'secret' || !key.__material || !key.__material.raw) {
                        throw err('InvalidAccessError', 'crypto.subtle.decrypt: key must be an AES secret key');
                    }
                    var p = gcmParams(algo);
                    var all = toU8(data);
                    if (all.length < p.tagLenBytes) {
                        throw err('OperationError', 'crypto.subtle.decrypt: data shorter than tag');
                    }
                    var g = gcmCtx(key.__material.raw, p.iv);
                    var ct = all.subarray(0, all.length - p.tagLenBytes);
                    var tag = all.subarray(all.length - p.tagLenBytes);
                    if (!tagEqual(gcmTag(g, p.aad, ct, p.tagLenBytes), tag)) {
                        throw err('OperationError', 'crypto.subtle.decrypt: authentication tag mismatch');
                    }
                    var pt = gcmKeystream(g, ct);
                    trace('decrypt AES-GCM ' + pt.length + 'B');
                    resolve(pt.slice().buffer);
                    return;
                }
                throw err('NotSupportedError', "crypto.subtle.decrypt: unsupported algorithm '" + name + "'");
            } catch (e) { reject(e); }
        });
    };
})();
"#;

/// M66-B: QuickJS Element shim（和 boa element_shim 的核心逻辑相同）。
#[cfg(feature = "quickjs")]
const QUICKJS_ELEMENT_SHIM: &str = r#"
// M78.127: Element 构造器已移到 GLOBAL shim 顶部（赋值不提升，此处仅为
// 旧定义位——所有 Element.prototype.* 挂载照常）。
Element.prototype.hasAttributes = function() {
    var raw = (typeof __attrsOf === 'function') ? __attrsOf(this.__nodeId) : '';
    return (raw || '').length > 0;
};
Element.prototype.hasAttribute = function(key) {
    var v = __getAttr(this.__nodeId, String(key));
    return v !== null && v !== undefined;
};
Element.prototype.hasAttributeNS = function(ns, key) { return this.hasAttribute(key); };
Element.prototype.getAttribute = function(key) {
    var v = __getAttr(this.__nodeId, key);
    return (v === null || v === undefined) ? null : String(v);
};
Element.prototype.setAttribute = function(key, val) { __setAttr(this.__nodeId, key, String(val)); try { window.__fireMutation(this.__nodeId, 'attributes', String(key)); } catch(e) {} };
Element.prototype.appendChild = function(child) {
    // M93.13: 直构自定义元素收养——new CapWidget() 的实例不是 arena 节点
    // （无 __nodeId），appendChild 会因 f64 转换炸掉。浏览器语义：自定义元素
    // 构造即产生真实元素节点；此处近似：检测到注册表实例时创建 detached
    // 节点收养（__nodeId 绑定 + __elCache 登记），后续 DOM 桥全部可用。
    if (child && typeof child.__nodeId !== 'number' && !child.__isFragment) {
        var __ctag = (typeof window.__customInstanceOf === 'function') ? window.__customInstanceOf(child) : null;
        if (__ctag) {
            try {
                var __cid = (typeof __createDetachedEl === 'function') ? __createDetachedEl(__ctag) : null;
                if (typeof __cid === 'number') {
                    child.__nodeId = __cid;
                    child.__customUpgraded = true;
                    if (window.__elCache) { window.__elCache[String(__cid)] = child; }
                }
            } catch (eAdopt) {}
        }
    }
    if (child && typeof child.__nodeId === 'number') {
        // GAP-K: DocumentFragment 插入时展开子节点（Web 标准行为）。
        // fragment 的子节点逐个移动到 this，fragment 本身变空（不插入）。
        // __children 返回逗号分隔 NodeId 字符串，需 split 成数组。
        // 先拷贝 children 数组再遍历——__appendChild 是 move 语义，边遍历边移会错位。
        if (child.__isFragment) {
            var fragChildrenStr = __children(child.__nodeId);
            if (fragChildrenStr) {
                var fragIds = fragChildrenStr.split(',').filter(function(s) { return s; });
                for (var _ci = 0; _ci < fragIds.length; _ci++) {
                    __appendChild(this.__nodeId, parseInt(fragIds[_ci], 10));
                }
            }
            return child;
        }
        __appendChild(this.__nodeId, child.__nodeId);
        try { window.__fireMutation(this.__nodeId, 'childList'); } catch(e) {}
        // M93.12: 自定义元素 connectedCallback（spec：连接到文档时派发一次）。
        // M93.13: 条件放宽为注册表 tag 命中（直构 new ctor() 实例无
        // __customUpgraded 标记，但其原型链上的 connectedCallback 同样必须派发）。
        try {
            var __ccTag = '';
            try { __ccTag = String((child && child.tagName) || (typeof __getTag === 'function' ? __getTag(child.__nodeId) : '')).toLowerCase(); } catch (eTg) {}
            var __ccHit = child.__customUpgraded ||
                (__ccTag && window.customElements && window.customElements.__registry && window.customElements.__registry[__ccTag]);
            if (__ccHit && !child.__ccDone && typeof child.connectedCallback === 'function') {
                child.__ccDone = true;
                if (typeof __ctrace === 'function') { try { __ctrace('CC-dispatch tag=' + __ccTag + ' nid=' + child.__nodeId); } catch (eT3) {} }
                // M93.13: CC 可能是 async——同步 try/catch 抓不到内部异常
                //（变成 rejected Promise 静默丢）。catch 返回的 Promise 记录。
                try {
                    var __ccr = child.connectedCallback();
                    if (__ccr && typeof __ccr.catch === 'function') {
                        __ccr.catch(function (eCcA) {
                            if (typeof __ctrace === 'function') { try { __ctrace('CC-ASYNC-THROW ' + String((eCcA && (eCcA.message || eCcA)) || eCcA).slice(0, 140) + ' STACK=' + String((eCcA && eCcA.stack) || '').split('\n').slice(0, 4).join('~').slice(0, 300)); } catch (e10) {} }
                        });
                    }
                } catch (eCc3) {
                    if (typeof __ctrace === 'function') { try { __ctrace('CC-THROW ' + String((eCc3 && (eCc3.message || eCc3)) || eCc3).slice(0, 160) + ' STACK=' + String((eCc3 && eCc3.stack) || '').split('\n').slice(0, 4).join('~').slice(0, 300)); } catch (e9) {} }
                }
            }
        } catch (eCc2) {}
        // M69: 动态 script 执行。webpack/vite 等前端工程化站点把业务代码打包成
        // 独立 chunk，在运行时用 createElement("script") + head.appendChild(s)
        // 动态加载。浏览器语义：appendChild 一个 script 元素时，若它有 src 则
        // fetch 远程 JS 执行，若它有 textContent 则执行内联代码；执行完触发 onload。
        var tag = (typeof __getTag === 'function') ? String(__getTag(child.__nodeId)) : '';
        if (tag && tag.toLowerCase() === 'script') {
            // QuickJS shim 无反射属性系统：s.src=x 只设 JS 属性，不写 DOM attrs。
            // 所以这里双 fallback——先读 DOM attr（setAttribute 路径），再读 JS 属性。
            var src = __getAttr(child.__nodeId, 'src');
            if (!src && typeof child.src === 'string') src = child.src;
            if (src) {
                // PERF-M80: 外链 script 不再在 appendChild 内同步 fetch（M78 的
                // __fetchScriptMimeOk + __fetchSync 双重拉取且串行阻塞——webpack
                // 一 tick 连挂多个 chunk 时逐个等 TLS 往返，react.dev 实测 3 chunk
                // 串行 ~6.6s、9 条首屏外链串行 56s）。改为入队 URL 标记，pump
                // （drain_and_eval_dynamic_scripts）并行 fetch + 按入队顺序 eval，
                // eval 后经 __dynPending 触发 onload/onerror。
                // MIME 强制（WPT block-mime）：pump 侧 fetch_external_script 检查，
                // 被禁 MIME → __dynStatus='failed' → 下面回调走 onerror 分支。
                __enqueueDynamicScript("\u0001DYNURL\u0001" + src);
                window.__dynPending = window.__dynPending || {};
                window.__dynStatus = window.__dynStatus || {};
                var _s = child;
                window.__dynPending[src] = function() {
                    if (window.__dynStatus[src] === 'failed') {
                        if (typeof _s.onerror === 'function') {
                            try { _s.onerror.call(_s, { type: 'error', target: _s }); } catch (e1) {}
                        }
                    } else if (typeof _s.onload === 'function') {
                        try { _s.onload.call(_s, { type: 'load', target: _s }); } catch (e2) {}
                    }
                };
                return child;
            }
            // inline script：读 textContent（同样双 fallback：DOM + JS 属性）。
            var code = __getText(child.__nodeId);
            if (!code && typeof child.textContent === 'string') code = child.textContent;
            if (code) {
                // M75: 跳过含 JSX 语法（<div>）的 chunk——非 ES 规范产物。
                // Chrome/V8 也不解析 JSX，生产构建时 Babel/SWC 编译掉。
                if (code.indexOf('<') >= 0 && code.indexOf('>') >= 0) {
                    var _jsx_err = child;
                    setTimeout(function() {
                        if (typeof _jsx_err.onerror === 'function') {
                            try { _jsx_err.onerror.call(_jsx_err, { type: 'error', target: _jsx_err, message: 'JSX not supported' }); } catch(e) {}
                        }
                    }, 0);
                    return child;
                }
                // 入队，pump 循环（run_scripts_quickjs 里的 loop）取出 eval_safe。
                __enqueueDynamicScript(code);
                // 异步触发 onload（推迟到 event loop 下一轮，符合 HTML5 语义）。
                var _s2 = child;
                setTimeout(function() {
                    if (typeof _s2.onload === 'function') {
                        try { _s2.onload.call(_s2, { type: 'load', target: _s2 }); } catch(e) {}
                    }
                }, 0);
            }
        }
    }
    return child;
};
Element.prototype.insertBefore = function(child, ref) {
    if (child && typeof child.__nodeId === 'number') {
        var refId = (ref && typeof ref.__nodeId === 'number') ? ref.__nodeId : -1;
        __insertBefore(this.__nodeId, child.__nodeId, refId);
    }
    return child;
};
Element.prototype.removeChild = function(child) {
    // M78.15: 规范语义——null/非节点 TypeError；非本节点子节点 NotFoundError。
    if (child === null || child === undefined || typeof child.__nodeId !== 'number') {
        // M78.53: 文档对象（有 createElement 的伪 Node）按规范抛 NotFound。
        if (child && typeof child.createElement === 'function') {
            throw new DOMException('The object can not be found here.', 'NotFoundError');
        }
        throw new TypeError('Argument 1 is not an object.');
    }
    var cs = (__children(this.__nodeId) || '').split(',');
    var mine = false;
    for (var i = 0; i < cs.length; i++) {
        if (parseInt(cs[i], 10) === child.__nodeId) { mine = true; break; }
    }
    if (!mine) {
        // M78.45: 子节点校验（不在本节点下=NotFoundError；含"无子节点"情形）。
        throw new DOMException('The object can not be found here.', 'NotFoundError');
    }
    __removeChild(this.__nodeId, child.__nodeId);
    return child;
};
Element.prototype.append = function() {
    for (var i = 0; i < arguments.length; i++) {
        var n = arguments[i];
        if (n === null || n === undefined) continue;
        if (typeof n === 'string') {
            var tn = __createEl('__text__');
            __setText(tn, n);
            __appendChild(this.__nodeId, tn);
        } else if (typeof n.__nodeId === 'number') {
            __appendChild(this.__nodeId, n.__nodeId);
        }
    }
};
// M78.130: remove 真摘除（旧 no-op 让 :dir() 等 first-strong 语义失效——
// WPT dir-selector-auto: div2_1.remove() 后 div2 不再扫到希伯来文本）。
Element.prototype.remove = function() {
    var pid = (typeof __getParent === 'function') ? __getParent(this.__nodeId) : -1;
    if (typeof pid === 'number' && pid >= 0) __removeChild(pid, this.__nodeId);
};
Element.prototype.addEventListener = function(type, cb, opt) {
    if (cb === null || cb === undefined) return;
    // M78.129: 记录 capture 标志（dispatchEvent 三阶段过滤用）。
    var ecapture = (opt === true) || !!(opt && opt.capture);
    if (!this.__listeners) this.__listeners = {};
    if (!this.__listeners[type]) this.__listeners[type] = [];
    if (!this.__listenerCaps) this.__listenerCaps = {};
    if (!this.__listenerCaps[type]) this.__listenerCaps[type] = [];
    this.__listeners[type].push(cb);
    this.__listenerCaps[type].push(ecapture);
};
Element.prototype.cloneNode = function(deep) {
    var tag = String(__getTag(this.__nodeId) || 'div');
    var newId = __createEl(tag);
    if (newId < 0) return null;
    var copy = __makeElement(newId);
    // M79: 深拷贝改为克隆**全部子节点类型**（元素 + 文本 + 注释）。
    // 旧版 GAP-J 只克隆元素子节点、叶子文本靠 __setText 兜底——注释子节点
    // 全部丢失。solid 等编译型框架的模板克隆（template().content.firstChild
    // .cloneNode(true)）依赖注释占位符 `<!--#-->` 保留，节点链
    // （z.firstChild.nextSibling...）缺一环即 TypeError（nextSibling of null）。
    // 全量克隆后叶子文本自然被克隆，不再需要旧 __setText 特例（避免双份文本）。
    if (deep !== false) {
        try {
            var cs = __children(this.__nodeId);
            if (cs) {
                var ids = cs.split(',').filter(function(s) { return s; });
                for (var i = 0; i < ids.length; i++) {
                    var cid = parseInt(ids[i], 10);
                    var ctag = __getTag(cid);
                    var childCopy = null;
                    if (ctag === '__text__') {
                        var td = (typeof __textData === 'function') ? __textData(cid) : '';
                        childCopy = document.createTextNode(String(td || __getText(cid) || ''));
                    } else if (ctag === '__comment__') {
                        var cd = (typeof __textData === 'function') ? __textData(cid) : '';
                        childCopy = document.createComment(String(cd || __getText(cid) || ''));
                    } else if (ctag) {
                        childCopy = __makeElement(cid) ? __makeElement(cid).cloneNode(true) : null;
                    } else {
                        // M79: getTag 空串 = 真解析的 Text（textData 非空）或真 Comment
                        // （textData 恒空——现有 bridge 读不出 Comment 数据，按空注释克隆；
                        // solid 标记靠节点身份而非内容，nextSibling 链不因数据缺失断裂）。
                        var td2 = (typeof __textData === 'function') ? __textData(cid) : '';
                        childCopy = td2 ? document.createTextNode(td2)
                                        : document.createComment('');
                    }
                    if (childCopy) {
                        try { __appendChild(newId, childCopy.__nodeId); } catch(e2) {}
                    }
                }
            }
        } catch(e3) {}
    }
    return copy;
};
Object.defineProperty(Element.prototype, 'tagName', {
    get: function() {
        if (this.__origTagName) return this.__origTagName;
        return String(__getTag(this.__nodeId)).toUpperCase();
    },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'textContent', {
    get: function() { return __getText(this.__nodeId); },
    set: function(v) {
        // M78.128: 空串清全部子节点且不留空 Text（WPT: textContent='' 后
        // childNodes.length === 0；__setText 清子建子会留一个空 Text）。
        var s = String(v);
        if (s === '') {
            while (true) {
                var kids = (__children(this.__nodeId) || '').split(',').filter(function(x) { return x; });
                if (!kids.length) break;
                __removeChild(this.__nodeId, parseInt(kids[0], 10));
            }
            return;
        }
        __setText(this.__nodeId, s);
    },
    enumerable: true, configurable: true
});
window.__serAttrsOf = function(id) {
    if (typeof __attrsOf !== 'function') return '';
    var out = '';
    var raw = __attrsOf(id) || '';
    raw.split('\n').forEach(function(line) {
        var eq = line.indexOf('=');
        if (eq > 0) out += ' ' + line.slice(0, eq) + '="' + line.slice(eq + 1) + '"';
    });
    return out;
};
Object.defineProperty(Element.prototype, 'innerHTML', {
    get: function() {
        // 从 Rust Tree 读子节点的文本拼接（近似 innerHTML）
        var cs = __children(this.__nodeId);
        if (!cs) return '';
        var ids = cs.split(',').filter(function(s) { return s; });
        var out = '';
        var __voidTags = { br:1, hr:1, img:1, input:1, meta:1, link:1, area:1,
            base:1, col:1, embed:1, source:1, track:1, wbr:1 };
        // M80.21: 递归序列化子树——旧版只处理直接子节点（元素子节点用
        // __getText 纯文本，嵌套的 h3/a/p/属性全部丢失——todomvc learn-bar
        // 的 aside.outerHTML 缺臂断腿再注入后结构破坏的根因）。
        function __serNode(id, tag) {
            var out = '';
            var attrsStr = '';
            if (typeof __attrsOf === 'function') {
                var raw = __attrsOf(id);
                (raw || '').split('\n').forEach(function(line) {
                    var eq = line.indexOf('=');
                    if (eq > 0) attrsStr += ' ' + line.slice(0, eq) + '="' + line.slice(eq + 1) + '"';
                });
            }
            var low = tag.toLowerCase();
            if (__voidTags[low]) return '<' + low + attrsStr + '>';
            var inner = '';
            var kids = (__children(id) || '').split(',').filter(function(x) { return x; });
            for (var k = 0; k < kids.length; k++) {
                var kid = parseInt(kids[k], 10);
                var ktag = __getTag(kid);
                if (!ktag || ktag === '__text__') {
                    var td = (typeof __textData === 'function') ? __textData(kid) : '';
                    inner += td || __getText(kid);
                } else if (ktag === '__comment__') {
                    // 注释不可见（同 html_ser.rs）
                } else {
                    inner += __serNode(kid, ktag);
                }
            }
            out += '<' + low + attrsStr + '>' + inner + '</' + low + '>';
            return out;
        }
        for (var i = 0; i < ids.length; i++) {
            var id = parseInt(ids[i], 10);
            var tag = __getTag(id);
            // M78.10: 真 Text 节点（__parseHtml/html5ever 插入）getTag 返回空串，
            // 与 shim 的 '__text__' 伪标签同等按文本输出。
            if (!tag || tag === '__text__') {
                // M78.38: 文本值优先 __textData（节点自身 data）；__getText 聚合
                // 子树对文本节点本身返回空（M78.21 重写时曾丢失此 fallback）。
                var td = (typeof __textData === 'function') ? __textData(id) : '';
                var txt = td || __getText(id);
                out += txt;
            } else if (tag === '__comment__') {
                // M79: 注释节点不可见于 innerHTML（与序列化器 html_ser.rs 对齐）
            } else {
                out += __serNode(id, tag);
            }
        }
        return out;
    },
    set: function(v) {
        var s = String(v);
        if (s.length === 0) {
            // M78.60: 空串清空全部子节点（__setText 会留一个空 Text 子节点，
            // 使 firstChild 变成 nt3 空 Text——WPT innerText 系列的 e=null 根因）。
            while (true) {
                var kids = (__children(this.__nodeId) || '').split(',').filter(function(x) { return x; });
                if (!kids.length) break;
                __removeChild(this.__nodeId, parseInt(kids[0], 10));
            }
            return;
        }
        if (typeof __parseHtml === 'function' && s.length > 0) {
            __parseHtml(this.__nodeId, s);
        } else {
            __setText(this.__nodeId, s);
        }
    },
    enumerable: true, configurable: true
});
// M78.10: innerText —— getter 带布局感知近似（块级边界插 \n + display:none
// 子树排除 + <br>→\n，纯 JS 遍历）；setter 按规范语义：文本 HTML 转义 +
// 换行拆分插 <br> + 替换全部子节点。text-transform 类真排版需求超目标。
function __innerTextWalk(nodeId, out, tf) {
    var cs = __children(nodeId);
    if (!cs) return;
    var ids = cs.split(',');
    var __blockTags = { DIV:1, P:1, UL:1, OL:1, LI:1, H1:1, H2:1, H3:1, H4:1, H5:1, H6:1,
        SECTION:1, ARTICLE:1, HEADER:1, FOOTER:1, NAV:1, BLOCKQUOTE:1, PRE:1,
        TABLE:1, TR:1, ADDRESS:1, MAIN:1, ASIDE:1, FIGURE:1, FIELDSET:1, DETAILS:1 };
    for (var i = 0; i < ids.length; i++) {
        if (!ids[i]) continue;
        var id = parseInt(ids[i], 10);
        var tag = (__getTag(id) || '').toUpperCase();
        if (!tag || tag === '__TEXT__') {
            // M78.62b: td 优先 + gt fallback——原生 Text 的 __getText 返回空
            // （collect_text 只聚合子树不读自身）；__setText 写过的节点则相反
            // （td 空 gt 真）。双 fallback 覆盖两形态。
            var tv = (typeof __textData === 'function') ? __textData(id) : '';
            var txt = tv || __getText(id);
            // M91: text-transform:uppercase（祖先链任一命中即大写——WPT
            // dynamic-getter 断言 innerText 应用 transform）。
            if (tf) txt = String(txt).toUpperCase();
            out.push(txt);
        } else if (tag === 'BR') {
            out.push('\n');
        } else if (tag === 'SCRIPT' || tag === 'STYLE' || tag === 'NOSCRIPT' || tag === 'TEMPLATE') {
            // 不可见子树
        } else {
            // display:none 子树排除：inline style 属性 + style 代理动态状态
            //（el.style.display='none' 只写 styleObj 不写属性——M91 修正）。
            var st = __getAttr(id, 'style') || '';
            var dynEl = (typeof __makeElement === 'function') ? __makeElement(id) : null;
            var sp = (dynEl && dynEl.__styleProxy) ? dynEl.__styleProxy : null;
            var disp = (sp && sp.__display) ? String(sp.__display) : '';
            if (/display\s*:\s*none/i.test(st) || /^\s*none\s*;?$/i.test(disp)) continue;
            // M91: text-transform：内联属性或动态 style 代理（watchProps 走
            // '__'+prop 键，任意键走原名——style['text-transform'] 存在
            // 'text-transform'；text-transform 继承，祖先链任一 uppercase 生效）。
            var ttDyn = sp ? String(sp['__text-transform'] || sp['text-transform'] || sp['__textTransform'] || '') : '';
            var ntf = tf || /uppercase/i.test(ttDyn) || /text-transform\s*:\s*uppercase/i.test(st);
            var isBlock = __blockTags[tag] === 1;
            if (isBlock) out.push('\n');
            __innerTextWalk(id, out, ntf);
            if (isBlock) out.push('\n');
        }
    }
}
Object.defineProperty(Element.prototype, 'innerText', {
    get: function() {
        // M78.50: SVG/MathML 元素不支持 innerText（返回空，WPT 断言）。
        var tn = (this.tagName || '').toLowerCase();
        if (tn === 'svg' || tn === 'math') return '';
        // M91: text-transform 继承——先沿祖先链查（父元素 uppercase 作用于
        // 本子树；WPT dynamic-getter「parent element」断言）。
        var initTf = false;
        var anc = this;
        var ag = 0;
        while (anc && ag++ < 64) {
            var aw = (anc.__styleProxy) ? anc.__styleProxy : null;
            var att = aw ? String(aw['__text-transform'] || aw['text-transform'] || aw['__textTransform'] || '') : '';
            var ast = (typeof anc.__nodeId === 'number') ? (__getAttr(anc.__nodeId, 'style') || '') : '';
            if (/uppercase/i.test(att) || /text-transform\s*:\s*uppercase/i.test(ast)) { initTf = true; break; }
            anc = anc.parentNode;
        }
        var out = [];
        __innerTextWalk(this.__nodeId, out, initTf);
        var joined = out.join('');
        // 规范：仅去首尾换行；空格/制表符保留（"Leading whitespace preserved"）。
        return joined.replace(/^\n/, '').replace(/\n$/, '');
    },
    set: function(v) {
        // M78.60: undefined 显式赋值序列化为 "undefined"（String(undefined)）；
        // null 为空串。
        var text = (v === undefined) ? 'undefined' : String(v == null ? '' : v);
        // M78.50: SVG/MathML 不支持 innerText setter（no-op）。
        var tn0 = (this.tagName || '').toLowerCase();
        if (tn0 === 'svg' || tn0 === 'math') return;
        function esc(s) {
            return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
        }
        // M78.56: 无换行时建单个真 Text 节点——不经 HTML 解析（NUL 字符、
        // 首空白、空串均按 data 原样保留；WPT assertNewSingleTextNode 链）。
        if (text.indexOf(String.fromCharCode(10)) < 0 && text.indexOf(String.fromCharCode(13)) < 0) {
            // M78.58: 清空现有子节点（__removeChild 逐个，每轮重读防错位）。
            while (true) {
                var kids0 = (__children(this.__nodeId) || '').split(',').filter(function(x) { return x; });
                if (!kids0.length) break;
                __removeChild(this.__nodeId, parseInt(kids0[0], 10));
            }
            // M78.58: 空串/null 不留空 Text 节点（WPT: Should not have empty
            // text nodes）；非空直建真 Text。
            if (text.length > 0) {
                var tid = __createEl('__text__');
                __setText(tid, text);
                __appendChild(this.__nodeId, tid);
            }
            return;
        }
        // M78.39: 规范换行集——LF / CRLF / CR 都转为 <br>（HTML 序列化标准）。
        var lines = text.split(/\r\n|\r|\n/);
        var html = '';
        for (var i = 0; i < lines.length; i++) {
            if (i > 0) html += '<br>';
            html += esc(lines[i]);
        }
        if (typeof __parseHtml === 'function' && html.length > 0) {
            __parseHtml(this.__nodeId, html);
        } else {
            __setText(this.__nodeId, text);
        }
    },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'id', {
    get: function() { return __getAttr(this.__nodeId, 'id') || ''; },
    set: function(v) { __setAttr(this.__nodeId, 'id', String(v)); },
    enumerable: true, configurable: true
});
// href 属性反射（a/area/link 标签用，爬虫 docsify sidebar sort 依赖）
Object.defineProperty(Element.prototype, 'href', {
    get: function() { return __getAttr(this.__nodeId, 'href') || ''; },
    set: function(v) { __setAttr(this.__nodeId, 'href', String(v)); },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'children', {
    get: function() {
        // M91: children 返回活 HTMLCollection（live proxy——item coercion、
        // named property、ownKeys 语义；WPT Element-children 断言）。
        // 仅元素子节点（过滤 __text__/__comment__ 伪节点）。
        var self = this;
        if (typeof window.__makeLiveCollection === 'function') {
            return window.__makeLiveCollection(function() {
                var cs = __children(self.__nodeId);
                if (!cs) return [];
                return cs.split(',').filter(function(s) { return s; })
                    .map(function(s) { return __makeElement(parseInt(s, 10)); })
                    .filter(function(el) { return el && el.nodeType === 1; });
            });
        }
        var cs = __children(this.__nodeId);
        if (!cs) return [];
        return cs.split(',').filter(function(s) { return s; }).map(function(s) { return __makeElement(parseInt(s, 10)); });
    },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'className', {
    get: function() { return __getAttr(this.__nodeId, 'class') || ''; },
    set: function(v) { __setAttr(this.__nodeId, 'class', String(v)); },
    enumerable: true, configurable: true
});
// M78: DOMTokenList —— 真类实现（Symbol.toStringTag + 惰性缓存 + value/迭代）。
// WPT assert_class_string 用 {}.toString.call(obj) 检查 [object DOMTokenList]。
globalThis.DOMTokenList = function DOMTokenList(nodeId) { this.__nodeId = nodeId; };
Object.defineProperty(DOMTokenList.prototype, Symbol.toStringTag, { value: 'DOMTokenList' });
DOMTokenList.prototype.__tokens = function() {
    // M78.44: 去重 + 保序（DOMTokenList 语义：token 集合无重复）。
    var raw = (__getAttr(this.__nodeId, 'class') || '').split(/\s+/).filter(function(s) { return s; });
    var seen = {}, out = [];
    for (var i = 0; i < raw.length; i++) {
        if (!seen[raw[i]]) { seen[raw[i]] = 1; out.push(raw[i]); }
    }
    return out;
};
DOMTokenList.prototype.__write = function(arr) { __setAttr(this.__nodeId, 'class', arr.join(' ')); };
DOMTokenList.prototype.add = function() {
    var cls = this.__tokens();
    for (var i = 0; i < arguments.length; i++) {
        var c = String(arguments[i]);
        if (cls.indexOf(c) < 0) cls.push(c);
    }
    this.__write(cls);
};
DOMTokenList.prototype.remove = function() {
    var cls = this.__tokens();
    for (var i = 0; i < arguments.length; i++) {
        var idx = cls.indexOf(String(arguments[i]));
        if (idx >= 0) cls.splice(idx, 1);
    }
    this.__write(cls);
};
DOMTokenList.prototype.toggle = function(c, force) {
    c = String(c);
    var cls = this.__tokens();
    var has = cls.indexOf(c) >= 0;
    if (force === true || (!has && force !== false)) {
        if (!has) cls.push(c);
    } else if (has) {
        cls.splice(cls.indexOf(c), 1);
    }
    this.__write(cls);
    return cls.indexOf(c) >= 0;
};
DOMTokenList.prototype.contains = function(c) { return this.__tokens().indexOf(String(c)) >= 0; };
DOMTokenList.prototype.item = function(i) { var t = this.__tokens(); return (i >= 0 && i < t.length) ? t[i] : null; };
DOMTokenList.prototype.replace = function(a, b) {
    var cls = this.__tokens();
    var idx = cls.indexOf(String(a));
    if (idx >= 0) { cls[idx] = String(b); this.__write(cls); return true; }
    return false;
};
DOMTokenList.prototype.toString = function() { return __getAttr(this.__nodeId, 'class') || ''; };
Object.defineProperty(DOMTokenList.prototype, 'length', { get: function() { return this.__tokens().length; }, enumerable: true, configurable: true });
Object.defineProperty(DOMTokenList.prototype, 'value', {
    get: function() { return __getAttr(this.__nodeId, 'class') || ''; },
    set: function(v) { __setAttr(this.__nodeId, 'class', String(v)); },
    enumerable: true, configurable: true
});
// M91: DOMTokenList 继承 Array.prototype（WPT DOMTokenList-iteration 断言
// keys/values/entries/forEach/Symbol.iterator 与 Array.prototype **同一函数**）。
// 旧 own 实现（String 索引迭代）删除，索引访问交给 classList 返回的 Proxy。
Object.setPrototypeOf(DOMTokenList.prototype, Array.prototype);
window.DOMTokenList = DOMTokenList;
Object.defineProperty(Element.prototype, 'classList', {
    get: function() {
        // 惰性缓存：同一元素的 classList 必须身份相等（===）。
        // M91: Proxy 包装——数字索引 + length 直读 token（Array 迭代器
        // 经继承的 Array.prototype 方法操作）。
        // M93.13-fix: own-property 判定——防止原型上的意外缓存（__classList
        // 沿原型链查到别人的缓存会返回错误的 DOMTokenList）。
        if (!Object.prototype.hasOwnProperty.call(this, '__classList')) {
            var inst = new DOMTokenList(this.__nodeId);
            this.__classList = new Proxy(inst, {
                get: function(t, k) {
                    if (typeof k === 'string' && /^\d+$/.test(k)) {
                        var i = +k;
                        var toks = t.__tokens();
                        return (i >= 0 && i < toks.length) ? toks[i] : undefined;
                    }
                    if (k === 'length') return t.__tokens().length;
                    return Reflect.get(t, k);
                },
                has: function(t, k) {
                    if (typeof k === 'string' && /^\d+$/.test(k)) return +k < t.__tokens().length;
                    return Reflect.has(t, k);
                }
            });
        }
        return this.__classList;
    },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'firstChild', {
    get: function() {
        var cs = __children(this.__nodeId);
        if (!cs) return null;
        var ids = cs.split(',').filter(function(s) { return s; });
        return ids.length > 0 ? __makeElement(parseInt(ids[0], 10)) : null;
    },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'lastChild', {
    get: function() {
        var cs = __children(this.__nodeId);
        if (!cs) return null;
        var ids = cs.split(',').filter(function(s) { return s; });
        return ids.length > 0 ? __makeElement(parseInt(ids[ids.length-1], 10)) : null;
    },
    enumerable: true, configurable: true
});
// M91: nextSibling/previousSibling 真实现（旧恒 null——WPT TreeWalker 用
// subTree.previousSibling 定位，SPA 也常用）。
Object.defineProperty(Element.prototype, 'nextSibling', {
    get: function() {
        var pid = __getParent(this.__nodeId);
        if (typeof pid !== 'number' || pid < 0) return null;
        var ids = (__children(pid) || '').split(',').filter(function(s) { return s; });
        for (var i = 0; i < ids.length; i++) {
            if (parseInt(ids[i], 10) === this.__nodeId) {
                return (i + 1 < ids.length) ? __makeElement(parseInt(ids[i + 1], 10)) : null;
            }
        }
        return null;
    },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'previousSibling', {
    get: function() {
        var pid = __getParent(this.__nodeId);
        if (typeof pid !== 'number' || pid < 0) return null;
        var ids = (__children(pid) || '').split(',').filter(function(s) { return s; });
        for (var i = 0; i < ids.length; i++) {
            if (parseInt(ids[i], 10) === this.__nodeId) {
                return (i > 0) ? __makeElement(parseInt(ids[i - 1], 10)) : null;
            }
        }
        return null;
    },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'parentNode', {
    get: function() {
        var pid = __getParent(this.__nodeId);
        return (pid >= 0) ? __makeElement(pid) : null;
    },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'parentElement', {
    get: function() {
        try {
            if (!this || typeof this.__nodeId !== 'number') return null;
            var pid = __getParent(this.__nodeId);
            if (pid < 0) return null;
            var tag = __getTag(pid);
            return tag ? __makeElement(pid) : null;
        } catch(e) { return null; }
    },
    enumerable: true, configurable: true
});
// M78.108: Element 实例也能访问 Node 常量（WPT Node-constants 断言）。
(function() {
    var nodeConsts = { ELEMENT_NODE:1, ATTRIBUTE_NODE:2, TEXT_NODE:3,
        CDATA_SECTION_NODE:4, ENTITY_REFERENCE_NODE:5, ENTITY_NODE:6,
        PROCESSING_INSTRUCTION_NODE:7, COMMENT_NODE:8, DOCUMENT_NODE:9,
        DOCUMENT_TYPE_NODE:10, DOCUMENT_FRAGMENT_NODE:11, NOTATION_NODE:12,
        DOCUMENT_POSITION_DISCONNECTED:1, DOCUMENT_POSITION_PRECEDING:2,
        DOCUMENT_POSITION_FOLLOWING:4, DOCUMENT_POSITION_CONTAINS:8,
        DOCUMENT_POSITION_CONTAINED_BY:16, DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC:32 };
    for (var k in nodeConsts) {
        Element.prototype[k] = nodeConsts[k];
    }
})();
Object.defineProperty(Element.prototype, 'nodeType', {
    get: function() {
        // M78.48: 真 Text 节点（getTag 空）与 shim 伪文本（__text__）都是 3。
        // M78.51: 实例缓存——React 渲染热路径每次 access 都走桥让 react.dev
        // 的脚本阶段 3.7s→22s（6 倍）。节点类型不可变，缓存安全。
        if (this.__ntCache !== undefined) return this.__ntCache;
        var nt = 1;
        if (this.__isFragment) nt = 11;
        else {
            var tag = (typeof __getTag === 'function') ? __getTag(this.__nodeId) : 'div';
            if (!tag || tag === '__text__') nt = 3;
            else if (tag === '__comment__') nt = 8;
        }
        this.__ntCache = nt;
        return nt;
    },
    enumerable: true, configurable: true
});
// M77: ownerDocument——React 事件系统检查 rootContainerElement.ownerDocument
// 如果返回 undefined，React 的 `!== null` 检查会误判（undefined !== null = true），
// 然后试图在 undefined 上设 _reactListening 属性，抛 "cannot read property of undefined"。
Object.defineProperty(Element.prototype, 'ownerDocument', {
    get: function() {
        // M78.53: __ownerDoc 优先（createHTMLDocument 子文档的元素）。
        if (this.__ownerDoc) return this.__ownerDoc;
        return typeof document !== 'undefined' ? document : null;
    },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'style', {
    get: function() {
        // M70.13: 返回一个可写 style 对象，属性变更时触发 __onStyleChange（CSS transition 仿真）。
        var self = this;
        if (self.__styleProxy) return self.__styleProxy;
        var styleObj = {
            getPropertyValue: function(p) { return styleObj[p] || ''; },
            setProperty: function(p, v) {
                var old = styleObj[p];
                styleObj[p] = v;
                __onStyleChange(self, p, old, v);
            },
            removeProperty: function(p) {
                var old = styleObj[p];
                delete styleObj[p];
                __onStyleChange(self, p, old, undefined);
            },
            get cssText() { return __getAttr(self.__nodeId, 'style') || ''; },
            set cssText(v) { __setAttr(self.__nodeId, 'style', String(v)); }
        };
        // M70.13: 拦截常用 CSS 属性的赋值 → 通知 transition manager。
        // 用 defineProperty 给 styleObj 加 getter/setter，赋值时触发 __onStyleChange。
        var watchProps = ['opacity', 'transform', 'display', 'visibility',
                          'width', 'height', 'left', 'top', 'transition',
                          'WebkitTransition', 'color', 'backgroundColor',
                          'margin', 'padding', 'position'];
        for (var i = 0; i < watchProps.length; i++) {
            (function(prop) {
                Object.defineProperty(styleObj, prop, {
                    get: function() { return styleObj['__' + prop] || ''; },
                    set: function(v) {
                        var old = styleObj['__' + prop];
                        styleObj['__' + prop] = v;
                        __onStyleChange(self, prop, old, v);
                    },
                    enumerable: true, configurable: true
                });
            })(watchProps[i]);
        }
        self.__styleProxy = styleObj;
        return styleObj;
    },
    enumerable: true, configurable: true
});
// M91: 游离子树查询兜底——createElement 创建的 detached 子树不在文档，
// __qs/__qsAll 查不到（WPT traversal-skip-most 在 detached 根上
// querySelectorAll('#B3')）。JS 侧 DFS 子树 + 简单复合选择器匹配。
function __subtreeSelectIds(rootId, sel) {
    var out = [];
    var parts = String(sel).split(',').map(function(s) { return s.trim(); }).filter(function(s) { return s; });
    function compoundMatch(id, q) {
        var tagM = q.match(/^[a-zA-Z][a-zA-Z0-9-]*/);
        var idsM = q.match(/#[a-zA-Z0-9_-]+/g) || [];
        var clsM = q.match(/\.[a-zA-Z0-9_-]+/g) || [];
        if (tagM) {
            var tg = String(__getTag(id)).toLowerCase();
            if (tg !== '__text__' && tg !== '__comment__' && tg !== tagM[0].toLowerCase()) return false;
        }
        for (var i = 0; i < idsM.length; i++) {
            if (__getAttr(id, 'id') !== idsM[i].slice(1)) return false;
        }
        if (clsM.length) {
            var cls = (__getAttr(id, 'class') || '').split(/\s+/);
            for (var j = 0; j < clsM.length; j++) {
                if (cls.indexOf(clsM[j].slice(1)) < 0) return false;
            }
        }
        return true;
    }
    function matched(id) {
        for (var i = 0; i < parts.length; i++) {
            if (compoundMatch(id, parts[i])) return true;
        }
        return false;
    }
    (function dfs(id) {
        var tg = String(__getTag(id));
        if (tg !== '__text__' && tg !== '__comment__' && matched(id)) out.push(id);
        var cs = (__children(id) || '').split(',').filter(function(s) { return s; });
        for (var i = 0; i < cs.length; i++) dfs(parseInt(cs[i], 10));
    })(rootId);
    return out;
}
function __isDetached(id) {
    var p = __getParent(id);
    return !(typeof p === 'number' && p >= 0);
}
// M91: 子树归属判定（querySelector scoping root 语义——结果必须是 this
// 的后代，不含 this 自身）。
function __inSubtreeOf(rootId, id) {
    var cur = id;
    var guard = 0;
    while (cur >= 0 && guard++ < 512) {
        cur = __getParent(cur);
        if (typeof cur !== 'number' || cur < 0) return false;
        if (cur === rootId) return true;
    }
    return false;
}
Element.prototype.querySelector = function(sel) {
    // M91: detached 子树 / DocumentFragment 走 JS 子树搜索（fragment 的
    // querySelector 语义 = 仅搜自身子树；文档内元素仍走 __qs 全局查询）。
    if (typeof this.__nodeId === 'number' && (this.__isFragment || __isDetached(this.__nodeId))) {
        var r = __subtreeSelectIds(this.__nodeId, String(sel));
        return r.length ? __makeElement(r[0]) : null;
    }
    // M91: scoping root——用 __qsAll 取树序全量命中，返回**子树内**首个
    //（全局首个可能在子树外——WPT svg-template-querySelector 嵌套用例）。
    var ids = __qsAll(String(sel));
    if (!ids) return null;
    var arr = ids.split(',').filter(function(s) { return s; });
    for (var i = 0; i < arr.length; i++) {
        var id0 = parseInt(arr[i], 10);
        if (__inSubtreeOf(this.__nodeId, id0)) return __makeElement(id0);
    }
    return null;
};
Element.prototype.querySelectorAll = function(sel) {
    if (typeof this.__nodeId === 'number' && (this.__isFragment || __isDetached(this.__nodeId))) {
        return __subtreeSelectIds(this.__nodeId, String(sel)).map(function(x) { return __makeElement(x); });
    }
    var ids = __qsAll(String(sel));
    if (!ids) return [];
    // M91: scoping root 过滤。
    var root = this.__nodeId;
    return ids.split(',').filter(function(s) { return s; })
        .map(function(s) { return parseInt(s, 10); })
        .filter(function(id) { return __inSubtreeOf(root, id); })
        .map(function(id) { return __makeElement(id); });
};
Element.prototype.contains = function(node) {
    // M78.31: 子树包含判定（自含或后代）。沿 parent 链上溯。
    if (!node || typeof node.__nodeId !== 'number') return false;
    var cur = node.__nodeId;
    while (cur >= 0 && cur !== undefined) {
        if (cur === this.__nodeId) return true;
        try { cur = __getParent(cur); } catch (e) { return false; }
        if (typeof cur !== 'number' || cur < 0) return false;
    }
    return false;
};
// M78: DOMException —— WPT testharness 的 assert_throws_dom 检查
// e.constructor === window.DOMException 且 name/code 正确（SyntaxError=12）。
function DOMException(message, name) {
    this.message = String(message || '');
    this.name = String(name || 'Error');
    var codes = {
        IndexSizeError: 1, HierarchyRequestError: 3, WrongDocumentError: 4,
        InvalidCharacterError: 5, NoModificationAllowedError: 7, NotFoundError: 8,
        NotSupportedError: 9, InUseAttributeError: 10, InvalidStateError: 11,
        SyntaxError: 12, InvalidModificationError: 13, NamespaceError: 14,
        InvalidAccessError: 15, TypeMismatchError: 17, SecurityError: 18,
        NetworkError: 19, AbortError: 20, URLMismatchError: 21, TimeoutError: 23,
        InvalidNodeTypeError: 24, DataCloneError: 25, QuotaExceededError: 22,
        EvalError: 27, RangeError: 27, ReferenceError: 27, TypeError: 27, URIError: 27
    };
    this.code = codes[this.name] || 0;
}
DOMException.prototype.toString = function() { return this.name + ': ' + this.message; };
window.DOMException = DOMException;
// M78: 选择器语法预检 —— 非法选择器抛 SYNTAX_ERR（对齐浏览器 querySelector 行为）。
function __qsThrowIfInvalid(sel) {
    if (typeof __qsCheck === 'function' && !__qsCheck(String(sel))) {
        throw new DOMException(String(sel) + " is not a valid selector.", 'SyntaxError');
    }
}
// matches/closest：CSS 选择器匹配。依赖 __qsMatch/__qsClosest bridge。
Element.prototype.matches = function(sel) {
    __qsThrowIfInvalid(sel);
    if (typeof __qsMatch === 'function') {
        try { return !!__qsMatch(this.__nodeId, String(sel)); } catch(e) { return false; }
    }
    return false;
};
Element.prototype.closest = function(sel) {
    __qsThrowIfInvalid(sel);
    if (typeof __qsClosest === 'function') {
        try {
            var id = __qsClosest(this.__nodeId, String(sel));
            return (id >= 0) ? __makeElement(id) : null;
        } catch(e) { return null; }
    }
    return null;
};
// childNodes：返回子节点的伪数组（框架常读 childNodes.length）。从 __children 反射。
Object.defineProperty(Element.prototype, 'childNodes', {
    get: function() {
        try {
            var cs = __children(this.__nodeId);
            var ids = cs ? cs.split(',').filter(function(s) { return s; }) : [];
            // M78.128: 返回 Element 包装（elCache 缓存）——裸 NodeId 数字上
            // localName/data/nodeType 等全部 undefined（WPT outerText 系列断言）。
            var arr = ids.map(function(s) { return __makeElement(parseInt(s, 10)); });
            arr.item = function(i) { return (i >= 0 && i < arr.length) ? arr[i] : null; };
            return arr;
        } catch(e) { return []; }
    },
    enumerable: true, configurable: true
});
// <template>.content：返回一个 DocumentFragment（nodeType=11）。爬虫场景空 fragment 够用。
Object.defineProperty(Element.prototype, 'content', {
    get: function() {
        if (this.tagName === 'TEMPLATE') {
            // M91: content 以模板自身子树呈现——新建独立包装（不污染 elCache
            // 缓存的模板包装/fragment 包装；__isFragment 使 nodeType=11）。
            // querySelector 走子树搜索分支，能查到解析出的 <svg> 等内容节点。
            var f = new Element(this.__nodeId);
            f.__isFragment = true;
            f.__isTemplateContent = true;
            return f;
        }
        return undefined;
    },
    enumerable: true, configurable: true
});
// importNode：简化为 cloneNode（爬虫场景够用）。
document.importNode = function(node, deep) {
    try { return node.cloneNode(deep !== false); } catch(e) { return null; }
};
Element.prototype.removeEventListener = function(type, cb) {};
// M81: HTMLElement.click()——合成 click MouseEvent 并 dispatch
// （bubbles/cancelable 对齐浏览器；坐标 0——合成点击无真实指针）。
// MouseEvent 构造器在 XHR shim 段定义，调用期解析（eval 期不引用）。
Element.prototype.click = function() {
    var ev = new MouseEvent('click', { bubbles: true, cancelable: true,
        view: (typeof window !== 'undefined') ? window : null });
    var notCanceled = this.dispatchEvent(ev);
    // M80.19: <a href> 点击默认行为——未 preventDefault 时执行导航
    //（hash 链接 → __setLocHref 触发 hashchange；其他 → location.href）。
    // docsify/vue-router 等 hash 路由 SPA 的侧栏点击依赖此语义。
    if (notCanceled && this.tagName === 'A') {
        var href = null;
        try { href = __getAttr(this.__nodeId, 'href'); } catch (e) {}
        if (href && href.charAt(0) === '#') {
            try { __setLocHref(href); } catch (e2) {}
        } else if (href && href.indexOf('javascript:') !== 0) {
            try { __setLocHref(href); } catch (e2) {}
        }
    }
};
// M78.129: capture 标志登记（__listenerCaps[type][i] 与 __listeners[type][i] 一一对应）。
// 供 dispatchEvent 三阶段过滤：capture listener 只在 capture 相位触发，
// 非 capture 只在 at-target/bubble 相位触发（at-target 全部按注册序触发）。
Element.prototype.dispatchEvent = function(ev) {
    // M78.113: 三阶段 dispatch（capture → target → bubble）。
    // M78.129: 路径补 window/document（DOM 传播路径：window → document →
    // 根元素 → … → 父节点 → target，bubble 反向）；stopPropagation 语义改为
    // 延迟生效（本节点剩余 listener 照跑，之后不再前进）；stopImmediatePropagation
    // 立即终止（本节点剩余 listener 也不跑）。
    if (!ev) return true;
    ev.target = this;
    // M80.18: checkbox 合成点击的 pre-click activation——派发**前**翻转
    // checked（Chrome 语义：listener 内读到的已是翻转值，WPT
    // dispatchEvent.click.checkbox 断言依赖）；preventDefault 生效则在
    // 派发结束后回退。radio 的同组互斥语义未实现，不在此处理。
    var __ckToggled = false;
    if (ev.type === 'click') {
        try {
            var __ckTag = String(__getTag(this.__nodeId) || '').toLowerCase();
            if (__ckTag === 'input') {
                var __ckType = __getAttr(this.__nodeId, 'type');
                if (typeof __ckType === 'string' && __ckType.toLowerCase() === 'checkbox') {
                    this.checked = !this.checked;
                    __ckToggled = true;
                }
            }
        } catch (__cke) {}
    }
    // 元素祖先链（target 在最前；父节点非元素 = document 节点 → 链到此为止）
    var chain = [this];
    var cur = this;
    var guard = 0;
    while (guard++ < 64) {
        var pid;
        try { pid = __getParent(cur.__nodeId); } catch (pe) { break; }
        if (typeof pid !== 'number' || pid < 0) break;
        var ptag;
        try { ptag = __getTag(pid); } catch (te) { break; }
        if (typeof ptag !== 'string' || !ptag) break;
        try { cur = __makeElement(pid); } catch (me) { break; }
        if (!cur) break;
        chain.push(cur);
    }
    function __stopped() {
        return !!(ev.__immediate || ev.__stopPropagation || ev.cancelBubble);
    }
    // 在一个节点上触发一次 visit。repPhase = eventPhase 上报值；
    // filter: 1=只 capture listener，3=只非 capture，2=不过滤。
    function __visit(target, repPhase, filter, lst, caps) {
        if (!lst) return;
        ev.currentTarget = target;
        ev.eventPhase = repPhase;
        var snap = lst.slice();
        for (var k = 0; k < snap.length; k++) {
            if (filter !== 2 && caps) {
                var isc = !!caps[k];
                if (filter === 1 ? !isc : isc) continue;
            }
            try { snap[k].call(target, ev); } catch (e) {
                // M91: listener 抛错 → window.onerror（字符串 message），
                // 后续 listener 照常执行（WPT Event-dispatch-throwing）。
                try {
                    var msg0 = (e && e.message !== undefined) ? String(e.message) : String(e);
                    if (typeof window.onerror === 'function') window.onerror(msg0, '', 0, 0, e);
                } catch (e2) {}
            }
            if (ev.__immediate) break;
        }
    }
    function __elVisit(target, repPhase, filter) {
        if (!target || !target.__listeners) return;
        __visit(target, repPhase, filter, target.__listeners[ev.type],
            (target.__listenerCaps && target.__listenerCaps[ev.type]) || null);
    }
    var __winLst = (typeof __winListeners !== 'undefined' && __winListeners[ev.type]) || null;
    var __winCap = (typeof __winListenersCap !== 'undefined' && __winListenersCap[ev.type]) || null;
    var __docLst = (document.__listeners && document.__listeners[ev.type]) || null;
    var __docCap = (document.__listenerCaps && document.__listenerCaps[ev.type]) || null;
    // Phase 1: capture（window → document → 根 → target 的父节点）
    if (!__stopped()) __visit(window, 1, 1, __winLst, __winCap);
    if (!__stopped()) __visit(document, 1, 1, __docLst, __docCap);
    for (var ci = chain.length - 1; ci >= 1; ci--) {
        if (__stopped()) break;
        __elVisit(chain[ci], 1, 1);
    }
    // Phase 2: target 双 visit（capture listener → 非 capture listener，均报
    // AT_TARGET；bubbles=false 也触发；两次 visit 之间检查 stop 标志——
    // WPT Event-stopPropagation-cancel-bubbling）。
    if (!__stopped()) __elVisit(this, 2, 1);
    if (!__stopped()) __elVisit(this, 2, 3);
    // M81: on* 处理器——target 相位在 addEventListener 监听器之后触发
    // （浏览器语义：attribute/property handler 是按注册序排最后的
    // bubble-phase listener）。两种来源：property 赋值（el.onclick = fn）
    // 优先；否则读 DOM 树属性表（<button onclick="...">，__getAttr），
    // 以 `event` 为参数名 new Function 编译（全局作用域，无词法捕获）。
    // 局部变量不外存——QuickJS GC 安全。
    if (!__stopped() && ev && ev.type) {
        var __onh = this['on' + ev.type];
        if (typeof __onh !== 'function') {
            try {
                var __onattr = __getAttr(this.__nodeId, 'on' + ev.type);
                if (typeof __onattr === 'string' && __onattr) {
                    try { __onh = new Function('event', __onattr); } catch (__cfe) { __onh = null; }
                }
            } catch (__gfe) { __onh = null; }
        }
        if (typeof __onh === 'function') {
            ev.currentTarget = this;
            try { __onh.call(this, ev); } catch (__one) {
                try {
                    var __om = (__one && __one.message !== undefined) ? String(__one.message) : String(__one);
                    if (typeof window.onerror === 'function') window.onerror(__om, '', 0, 0, __one);
                } catch (__oe2) {}
            }
        }
    }
    // Phase 3: bubble（target 父 → 根 → document → window，仅 bubbles=true）
    if (ev.bubbles) {
        for (var bi = 1; bi < chain.length; bi++) {
            if (__stopped()) break;
            __elVisit(chain[bi], 3, 3);
        }
        if (!__stopped()) __visit(document, 3, 3, __docLst, __docCap);
        if (!__stopped()) __visit(window, 3, 3, __winLst, __winCap);
    }
    // M80.18: preventDefault 生效 → 回退派发前的 checked 翻转（canceled
    // activation steps；returnValue setter 已保证不可 cancel 时不置位）。
    if (__ckToggled && ev.defaultPrevented) {
        try { this.checked = !this.checked; } catch (__cre) {}
    }
    // M78.129: dispatch 前预设的 stop 标志抑制全部 listener（propagation-stopped）；
    // dispatch 结束清标志——同一 event 可再次 dispatch（multiple-cancelBubble）。
    ev.eventPhase = 0; ev.currentTarget = null;
    ev.cancelBubble = false; ev.__stopPropagation = false;
    try { delete ev.__immediate; } catch (de) {}
    return true;
};
// getComputedStyle：返回一个只读 style 对象（爬虫场景，不需像素精确）。
window.getComputedStyle = function(el) {
    if (!el) return null;
    // GAP-L: 按 tag 返回合理默认 computed style（爬虫场景，不需像素精确）。
    // 这样 getPropertyValue('display')/'color' 等能力探测不返回空，避免页面脚本中断。
    var tagName = (typeof el.tagName === 'string') ? el.tagName.toUpperCase() : '';
    var __defaults = {
        DIV: 'block', P: 'block', H1: 'block', H2: 'block', H3: 'block', H4: 'block',
        H5: 'block', H6: 'block', UL: 'block', OL: 'block', LI: 'list-item',
        SECTION: 'block', ARTICLE: 'block', HEADER: 'block', FOOTER: 'block',
        NAV: 'block', ASIDE: 'block', MAIN: 'block', FORM: 'block', FIELDSET: 'block',
        TABLE: 'table', TR: 'table-row', TD: 'table-cell', TH: 'table-cell',
        SPAN: 'inline', A: 'inline', B: 'inline', I: 'inline', EM: 'inline',
        STRONG: 'inline', IMG: 'inline', LABEL: 'inline', CODE: 'inline',
        INPUT: 'inline-block', BUTTON: 'inline-block', SELECT: 'inline-block',
        TEXTAREA: 'inline-block', CANVAS: 'inline-block'
    };
    var styleObj = {
        getPropertyValue: function(p) { return styleObj[p] || ''; },
        getPropertyPriority: function() { return ''; },
        setProperty: function() {},
        removeProperty: function() {},
        length: 0,
        item: function() { return '' },
        // 默认值（能力探测不返回空）
        display: __defaults[tagName] || 'block',
        color: 'rgb(0, 0, 0)',
        visibility: 'visible',
        opacity: '1',
        position: 'static',
        zIndex: 'auto',
        overflow: 'visible'
    };
    // 从 el.style 反射已知 inline style（覆盖默认值）
    try {
        var cs = el.style;
        if (cs) {
            for (var p in cs) {
                if (typeof cs[p] === 'string' && cs[p]) styleObj[p] = cs[p];
            }
            styleObj.cssText = cs.cssText || '';
        }
    } catch(e) {}
    return styleObj;
};
Element.prototype.insertAdjacentHTML = function(pos, html) {
    // M78.142: 规范语义——beforeend 把片段解析为真实子节点并追加。
    // 旧实现 __setAttr('innerHTML') 是 AGENTS.md 规则 18 点名的反模式：
    // set_attr 只写属性表（M66-fix 同款），内容永不进 DOM（渲染缺内容），
    // 还在元素上留 innerHTML="<markup>" 垃圾属性污染序列化输出。
    // 实现：临时 div 承载 __parseHtml 产出的真实子节点，再逐个 move 到
    // 目标（__appendChild 是 move 语义，先快照 children 再遍历，同 GAP-K）。
    // M80.20: 4 位置全支持（M80.18 只实现了 beforeend——base.js 的
    // insertAdjacentHTML('afterBegin', aside.outerHTML) 被静默 return，
    // todomvc learn-bar 侧栏注入整段丢失）。位置语义镜像
    // insertAdjacentElement 的既有实现（afterbegin/beforebegin/afterend
    // 用 __insertBefore 定位，wrap 子节点快照后 move）。
    var posL = String(pos || '').toLowerCase();
    if (posL !== 'beforeend' && posL !== 'afterbegin' && posL !== 'beforebegin' && posL !== 'afterend') return;
    var s = (html == null) ? '' : String(html);
    if (!s.length) return;
    if (typeof __parseHtml !== 'function' || typeof __createEl !== 'function') {
        // 无解析桥（极端环境）：退化为文本插入，保证内容可见不静默丢失
        var _tid = __createEl('__text__');
        __setText(_tid, s);
        if (posL === 'beforebegin' || posL === 'afterend') {
            var _pid = __getParent(this.__nodeId);
            if (_pid >= 0) __appendChild(_pid, _tid); else __appendChild(this.__nodeId, _tid);
        } else {
            __appendChild(this.__nodeId, _tid);
        }
        return;
    }
    var wrapId = __createEl('div');
    __parseHtml(wrapId, s);
    var wrapParent = __getParent(wrapId);
    var kidsStr = __children(wrapId) || '';
    var kids = kidsStr.split(',').filter(function(x) { return x; });
    // 目标挂载点：beforeend/afterbegin → 本盒；beforebegin/afterend → 父盒
    var mountPid = (posL === 'beforebegin' || posL === 'afterend')
        ? __getParent(this.__nodeId) : this.__nodeId;
    if (mountPid < 0) { if (wrapParent >= 0) __removeChild(wrapParent, wrapId); return; }
    // 参考节点：beforebegin→本节点；afterend→本节点的下一兄弟；其余 → null（追加）
    var ref = null;
    if (posL === 'beforebegin') ref = this.__nodeId;
    else if (posL === 'afterend') {
        var sibs = (__children(mountPid) || '').split(',').filter(function(x) { return x; });
        for (var si = 0; si < sibs.length; si++) {
            if (parseInt(sibs[si], 10) === this.__nodeId) {
                ref = (si + 1 < sibs.length) ? parseInt(sibs[si + 1], 10) : null;
                break;
            }
        }
    }
    // 逐个 move（__appendChild 是 move 语义）。afterend 用 __insertBefore 对
    // ref 定位；其余按顺序 append（beforeend 尾插 / afterbegin 头插需逆序）。
    if (posL === 'afterbegin' && kids.length > 1) kids = kids.reverse();
    for (var i = 0; i < kids.length; i++) {
        var kidId = parseInt(kids[i], 10);
        if (posL === 'beforebegin' || posL === 'afterend') {
            __insertBefore(mountPid, kidId, ref);
        } else {
            __appendChild(mountPid, kidId);
        }
    }
    // 清理临时 wrap（__createEl 会把节点挂到 body 下）
    if (wrapParent >= 0) __removeChild(wrapParent, wrapId);
    try { window.__fireMutation(this.__nodeId, 'childList'); } catch(e) {}
};
// M78.13: insertAdjacentElement —— 镜像 insertAdjacentText 的位置逻辑。
Element.prototype.insertAdjacentElement = function(pos, el) {
    if (!el || typeof el.__nodeId !== 'number') return null;
    var self = this;
    function nextSiblingId(pid) {
        var ids = (__children(pid) || '').split(',');
        for (var i = 0; i < ids.length; i++) {
            if (parseInt(ids[i], 10) === self.__nodeId) {
                return (i + 1 < ids.length) ? parseInt(ids[i + 1], 10) : -1;
            }
        }
        return -1;
    }
    try {
        if (pos === 'beforeend') {
            __appendChild(self.__nodeId, el.__nodeId);
        } else if (pos === 'afterbegin') {
            var first = parseInt((__children(self.__nodeId) || '').split(',')[0], 10);
            __insertBefore(self.__nodeId, el.__nodeId, isNaN(first) ? -1 : first);
        } else if (pos === 'beforebegin' || pos === 'afterend') {
            var pid = __getParent(self.__nodeId);
            if (pid < 0) return null;
            var ref = (pos === 'beforebegin') ? self.__nodeId : nextSiblingId(pid);
            __insertBefore(pid, el.__nodeId, ref);
        }
    } catch (e) { return null; }
    return el;
};
// M78: insertAdjacentText —— testharness.js 的输出渲染依赖它（4 个位置全支持）。
Element.prototype.insertAdjacentText = function(pos, text) {
    text = String(text == null ? '' : text);
    // M78.76: 位置校验（WPT: 无效位置抛 SyntaxError）。
    var validPos = ['beforebegin', 'afterbegin', 'beforeend', 'afterend'];
    if (validPos.indexOf(String(pos)) < 0) {
        throw new DOMException('The position provided must be one of "beforebegin", "afterbegin", "beforeend", or "afterend".', 'SyntaxError');
    }
    if (!text) return;
    var self = this;
    // M91: beforebegin/afterend 无父元素或父为 document 节点（documentElement，
    // arena root id 0 的 tag 为空串）→ HierarchyRequestError（WPT
    // Element-insertAdjacentText 断言）。必须在 try 外抛——下方 catch 会吞异常。
    if ((pos === 'beforebegin' || pos === 'afterend')) {
        var pidChk = __getParent(self.__nodeId);
        var pidIsDoc = (typeof pidChk === 'number' && pidChk >= 0 && String(__getTag(pidChk) || '') === '');
        if (!(typeof pidChk === 'number' && pidChk >= 0) || pidIsDoc) {
            throw new DOMException("the node has no parent", 'HierarchyRequestError');
        }
    }
    function makeTextNode() { return document.createTextNode(text); }
    function nextSiblingId(pid) {
        var ids = (__children(pid) || '').split(',');
        for (var i = 0; i < ids.length; i++) {
            if (parseInt(ids[i], 10) === self.__nodeId) {
                return (i + 1 < ids.length) ? parseInt(ids[i + 1], 10) : -1;
            }
        }
        return -1;
    }
    try {
        if (pos === 'beforeend') {
            __appendChild(self.__nodeId, makeTextNode().__nodeId);
        } else if (pos === 'afterbegin') {
            var first = parseInt((__children(self.__nodeId) || '').split(',')[0], 10);
            __insertBefore(self.__nodeId, makeTextNode().__nodeId, isNaN(first) ? -1 : first);
        } else if (pos === 'beforebegin' || pos === 'afterend') {
            var pid = __getParent(self.__nodeId);
            if (pid < 0) return;
            var ref = (pos === 'beforebegin') ? self.__nodeId : nextSiblingId(pid);
            __insertBefore(pid, makeTextNode().__nodeId, ref);
        }
    } catch (e) { /* 静默：文本插入失败不阻断测试主流程 */ }
};
Element.prototype.getBoundingClientRect = function() {
    return { x:0, y:0, top:0, left:0, right:0, bottom:0, width:0, height:0 };
};
// M78: offsetWidth —— 经 __offsetWidth 桥做 mini 级联（<style> 规则 → width px）。
// 近似：只有显式 px 宽才返回非 0（WPT :lang 系列测试的断言路径）。
Object.defineProperty(Element.prototype, 'offsetWidth', {
    get: function() {
        try {
            var w = (typeof __offsetWidth === 'function') ? __offsetWidth(this.__nodeId) : 0;
            return (typeof w === 'number' && w >= 0) ? w : 0;
        } catch(e) { return 0; }
    },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'offsetHeight', {
    get: function() { return 0; },
    enumerable: true, configurable: true
});
// M78.134: focus/activeElement 追踪 + execCommand('insertText')——WPT
// uievents/textInput：execCommand 在焦点元素插入文本并同步派发 input 事件
// （不派发 textInput）。insertText 无光标语义，值写入 value（表单）或
// 追加文本子节点（contenteditable）。
window.__activeEl = null;
// M81.4: focus/blur 派发真实事件——旧版只改 __activeEl 不派发事件
//（WPT focus 事件系列 + 框架的 focus 监听依赖）。同步派发 focusin/focusout
//（冒泡版，delegated 监听依赖）。相关事件类型用 FocusEvent。
Element.prototype.focus = function() {
    var prev = window.__activeEl;
    if (prev === this) return;
    // M81(B1): 上报焦点 NodeId 给宿主（CDP dispatchKeyEvent 的 activeElement
    // 同步——每个 eval 会话的 __activeEl 不跨会话存续，由 cdp drain 到
    // PageState.focused_node）。__psReportFocus 缺失（boa）时静默忽略。
    try {
        if (typeof this.__nodeId === 'number') __psReportFocus(this.__nodeId);
    } catch (e) {}
    if (prev && typeof prev.dispatchEvent === 'function') {
        try { prev.dispatchEvent(new FocusEvent('blur', { bubbles: false })); } catch (e) {}
        try { prev.dispatchEvent(new FocusEvent('focusout', { bubbles: true })); } catch (e) {}
    }
    window.__activeEl = this;
    try { this.dispatchEvent(new FocusEvent('focus', { bubbles: false })); } catch (e) {}
    try { this.dispatchEvent(new FocusEvent('focusin', { bubbles: true })); } catch (e) {}
};
Element.prototype.blur = function() {
    if (window.__activeEl !== this) return;
    try { this.dispatchEvent(new FocusEvent('blur', { bubbles: false })); } catch (e) {}
    try { this.dispatchEvent(new FocusEvent('focusout', { bubbles: true })); } catch (e) {}
    window.__activeEl = null;
};
Object.defineProperty(document, 'activeElement', {
    get: function() { return window.__activeEl || document.body || null; },
    enumerable: true, configurable: true
});
document.execCommand = function(cmd, ui, value) {
    if (cmd !== 'insertText') return false;
    var el = window.__activeEl;
    if (!el || typeof el.__nodeId !== 'number') return false;
    var tag = String(__getTag(el.__nodeId) || '').toLowerCase();
    var inserted = false;
    if (tag === 'input' || tag === 'textarea') {
        __setAttr(el.__nodeId, 'value', String(value));
        try { el.value = String(value); } catch (e) {}
        inserted = true;
    } else {
        var ce = __getAttr(el.__nodeId, 'contenteditable');
        // M78.134b: contenteditable=""（空串）也要命中——旧 if (ce !== null)
        // 逻辑本身对，但保险起见显式区分 undefined/null 与值（含空串）。
        if (ce !== null && ce !== undefined) {
            var tn = __createDetachedEl('__text__');
            __setText(tn, String(value));
            __appendChild(el.__nodeId, tn);
            inserted = true;
        }
    }
    if (inserted) {
        try {
            var ev = new Event('input', { bubbles: true });
            ev.data = value;
            el.dispatchEvent(ev);
        } catch (e) {}
    }
    return inserted;
};
Element.prototype.scrollIntoView = function() {};
// dataset（框架常用 data-* 属性）——Proxy 动态反射到 data-* attribute。
// dataset.fooBar → __getAttr(nodeId, 'data-foo-bar')，写同步 __setAttr。
// 不依赖枚举所有属性（QuickJS bridge 无列属性 API），惰性按 key 反射。
Object.defineProperty(Element.prototype, 'dataset', {
    get: function() {
        var self = this;
        // M91: dataset 仅 HTML/SVG/MathML 元素有——createElementNS 随机
        // 命名空间返回 undefined（WPT dataset.html 断言）。
        var ownNs = this.namespaceURI;
        if (ownNs && ownNs !== 'http://www.w3.org/1999/xhtml' &&
            ownNs !== 'http://www.w3.org/2000/svg' &&
            ownNs !== 'http://www.w3.org/1998/Math/MathML') {
            return undefined;
        }
        // M91: 原型挂 DOMStringMap.prototype（instanceof 断言）。
        var proto = (typeof DOMStringMap !== 'undefined' && DOMStringMap.prototype)
            ? DOMStringMap.prototype : Object.prototype;
        var cache = Object.create(proto);
        // 驼峰 ↔ kebab：fooBar ↔ data-foo-bar
        function toKebab(k) { return 'data-' + String(k).replace(/([A-Z])/g, function(_, c) { return '-' + c.toLowerCase(); }); }
        function toCamel(k) { return k.slice(5).replace(/-([a-z])/g, function(_, c) { return c.toUpperCase(); }); }
        // M91: supported property name——含「-小写字母」的名字（如 '-foo'）
        // 不是 supported name：get→undefined、has→false、delete no-op
        //（set 已抛 SyntaxError）。WPT dataset-get/delete 断言。
        function isSupported(k) {
            var s = String(k);
            for (var i = 0; i < s.length - 1; i++) {
                if (s.charAt(i) === '-') {
                    var nx = s.charAt(i + 1);
                    if (nx >= 'a' && nx <= 'z') return false;
                }
            }
            return true;
        }
        try {
            return new Proxy(cache, {
                get: function(t, k) {
                    if (typeof k === 'string' && Object.prototype.hasOwnProperty.call(t, k)) return t[k];
                    if (typeof k !== 'string' || !isSupported(k)) return undefined;
                    var v = __getAttr(self.__nodeId, toKebab(k));
                    return (v === null || v === undefined) ? undefined : v;
                },
                deleteProperty: function(t, k) {
                    if (typeof k === 'string' && isSupported(k)) {
                        try { __removeAttr(self.__nodeId, toKebab(k)); } catch (e) {}
                    }
                    delete t[k];
                    return true;
                },
                // M78.42: ownKeys——枚举 data-* 属性（驼峰，属性树顺序）。
                ownKeys: function(t) {
                    var keys = Object.keys(t);
                    try {
                        var raw = (typeof __attrsOf === 'function') ? __attrsOf(self.__nodeId) : '';
                        (raw || '').split('\n').forEach(function(line) {
                            var eq = line.indexOf('=');
                            if (eq > 0 && line.slice(0, 5) === 'data-') {
                                var dk = line.slice(5, eq);
                                keys.push(dk.replace(/-([a-z])/g, function(_, ch) { return ch.toUpperCase(); }));
                            }
                        });
                    } catch (e) {}
                    return keys;
                },
                getOwnPropertyDescriptor: function(t, k) {
                    if (typeof k === 'string' && Object.prototype.hasOwnProperty.call(t, k)) return Object.getOwnPropertyDescriptor(t, k);
                    if (typeof k !== 'string' || !isSupported(k)) return undefined;
                    var v = __getAttr(self.__nodeId, toKebab(k));
                    if (v !== null && v !== undefined) {
                        return { value: v, writable: true, enumerable: true, configurable: true };
                    }
                    return undefined;
                },
                has: function(t, k) {
                    if (typeof k !== 'string') return false;
                    if (Object.prototype.hasOwnProperty.call(t, k)) return true;
                    if (!isSupported(k)) return false;
                    var v = __getAttr(self.__nodeId, toKebab(k));
                    return v !== null && v !== undefined;
                },
                set: function(t, k, v) {
                    if (typeof k === 'string') {
                        // M91: DOMStringMap setter 校验（HTML Standard 顺序）：
                        // 1) name 含 "-小写字母" → SyntaxError（'-foo' 抛、'-' 与
                        //    '-Foo' 不抛）；2) 驼峰转 kebab；3) data- 前缀；
                        // 4) 非法 attribute local name → InvalidCharacterError
                        //（'foo ' 空格抛）。
                        var dname = String(k);
                        for (var si = 0; si < dname.length - 1; si++) {
                            if (dname.charAt(si) === '-') {
                                var nx = dname.charAt(si + 1);
                                if (nx >= 'a' && nx <= 'z') {
                                    throw new DOMException('the name contains a hyphen followed by a lowercase letter', 'SyntaxError');
                                }
                            }
                        }
                        var attrName = toKebab(dname);
                        var bad = attrName.length === 0;
                        if (!bad) {
                            if (attrName.charCodeAt(0) === 0) bad = true;
                            for (var vi = 0; vi < attrName.length; vi++) {
                                var cc = attrName.charAt(vi);
                                if (cc === ' ' || cc === '\t' || cc === '\n' || cc === '\r' || cc === '\f' || cc === '/' || cc === '=' || cc === '>') { bad = true; break; }
                            }
                        }
                        if (bad) {
                            throw new DOMException('the name is not a valid attribute local name', 'InvalidCharacterError');
                        }
                        __setAttr(self.__nodeId, attrName, String(v));
                        t[k] = String(v);
                    }
                    return true;
                }
            });
        } catch(e) {
            // 无 Proxy——返回空对象（爬虫读场景少）
            return cache;
        }
    },
    enumerable: true, configurable: true
});
// outerHTML setter（docsify/框架用 outerHTML 替换节点）
// M78.49: outerText（setter = innerText + 替换自身）/ replaceWith /
// before / after / normalize（相邻文本合并——WPT outerText 系列断言）。
Object.defineProperty(Element.prototype, 'outerText', {
    get: function() { return this.innerText; },
    set: function(v) {
        var pid = (typeof __getParent === 'function') ? __getParent(this.__nodeId) : -1;
        if (typeof pid !== 'number' || pid < 0) {
            // M78.128: 游离节点 setter 抛 NoModificationAllowedError（WPT 断言）。
            throw new DOMException('newText argument is invalid', 'NoModificationAllowedError');
        }
        // M78.128: SVG/MathML 不支持 outerText——no-op（WPT: innerHTML 保持原样）。
        var lg = (typeof this.localName === 'string') ? this.localName : String(__getTag(this.__nodeId)).toLowerCase();
        var nsS = String(this.namespaceURI || '');
        if (lg === 'svg' || lg === 'math' || nsS.indexOf('svg') >= 0 || nsS.indexOf('MathML') >= 0) return;
        // M91: 按规范重写——换行→<br>，替换自身；只与**相邻**前后 Text 合并
        //（不做全规范化），空串也创建空 Text 节点（WPT outertext-setter）。
        var text = (v === undefined) ? 'undefined' : String(v == null ? '' : v);
        var parts = text.split(/\r\n|\r|\n/);
        var newIds = [];
        for (var pi = 0; pi < parts.length; pi++) {
            if (pi > 0) {
                var br = document.createElement('br');
                if (br && typeof br.__nodeId === 'number') newIds.push(br.__nodeId);
            }
            // M91: 空串整体（parts=['']）也创建空 Text 节点（规范语义）；
            // 多段时跳过空段（由 <br> 承担分隔）。
            if (parts[pi] !== '' || parts.length === 1) {
                var tx = document.createTextNode(parts[pi]);
                if (tx && typeof tx.__nodeId === 'number') newIds.push(tx.__nodeId);
            }
        }
        var ref = this.__nodeId;
        for (var i2 = 0; i2 < newIds.length; i2++) {
            __insertBefore(pid, newIds[i2], ref);
        }
        __removeChild(pid, ref);
        function isTextId(id) { var t = String(__getTag(id)); return t === '__text__' || (!t && id !== 0); }
        function textOf(id) { return String((__textData(id) || __getText(id) || '')); }
        function sibOfId(id, delta) {
            var ids = (__children(pid) || '').split(',').filter(function(s) { return s; });
            for (var i = 0; i < ids.length; i++) {
                if (parseInt(ids[i], 10) === id) {
                    var j = i + delta;
                    return (j >= 0 && j < ids.length) ? parseInt(ids[j], 10) : -1;
                }
            }
            return -1;
        }
        var firstText = -1, lastText = -1;
        for (var k2 = 0; k2 < newIds.length; k2++) {
            if (isTextId(newIds[k2])) {
                if (firstText < 0) firstText = newIds[k2];
                lastText = newIds[k2];
            }
        }
        if (firstText >= 0) {
            var prevId = sibOfId(firstText, -1);
            if (prevId >= 0 && isTextId(prevId)) {
                __setText(firstText, textOf(prevId) + textOf(firstText));
                __removeChild(pid, prevId);
            }
        }
        if (lastText >= 0) {
            var nextId = sibOfId(lastText, 1);
            if (nextId >= 0 && isTextId(nextId)) {
                __setText(lastText, textOf(lastText) + textOf(nextId));
                __removeChild(pid, nextId);
            }
        }
    },
    enumerable: true, configurable: true
});
// M78.128: localName getter——元素小写（HTML ns）、SVG/MathML 原始大小写
// （__origTagName，createElementNS 保留）、伪文本 #text / 伪注释 #comment。
Object.defineProperty(Element.prototype, 'localName', {
    get: function() {
        if (this.__origTagName) return String(this.__origTagName);
        var t = String((typeof __getTag === 'function') ? __getTag(this.__nodeId) : '');
        if (!t || t === '__text__') return '#text';
        if (t === '__comment__') return '#comment';
        return t.toLowerCase();
    },
    enumerable: true, configurable: true
});

Element.prototype.replaceWith = function() {
    var pid = (typeof __getParent === 'function') ? __getParent(this.__nodeId) : -1;
    if (typeof pid !== 'number' || pid < 0) return;
    var ref = this.__nodeId;
    for (var i = 0; i < arguments.length; i++) {
        var n = arguments[i];
        if (typeof n === 'string') {
            var t = document.createTextNode(n);
            __insertBefore(pid, t.__nodeId, ref);
        } else if (n && typeof n.__nodeId === 'number') {
            __insertBefore(pid, n.__nodeId, ref);
        }
    }
    __removeChild(pid, this.__nodeId);
    __normalizeParent(pid);
};
Element.prototype.before = function() {
    var pid = (typeof __getParent === 'function') ? __getParent(this.__nodeId) : -1;
    if (typeof pid !== 'number' || pid < 0) return;
    for (var i = 0; i < arguments.length; i++) {
        var n = arguments[i];
        if (typeof n === 'string') {
            var t = document.createTextNode(n);
            __insertBefore(pid, t.__nodeId, this.__nodeId);
        } else if (n && typeof n.__nodeId === 'number') {
            __insertBefore(pid, n.__nodeId, this.__nodeId);
        }
    }
};
Element.prototype.after = function() {
    var pid = (typeof __getParent === 'function') ? __getParent(this.__nodeId) : -1;
    if (typeof pid !== 'number' || pid < 0) return;
    var sib = (__children(pid) || '').split(',');
    var ref = -1;
    for (var i = 0; i < sib.length; i++) {
        if (parseInt(sib[i], 10) === this.__nodeId) { ref = (i + 1 < sib.length) ? parseInt(sib[i + 1], 10) : -1; break; }
    }
    for (var j = 0; j < arguments.length; j++) {
        var n2 = arguments[j];
        if (typeof n2 === 'string') {
            var t2 = document.createTextNode(n2);
            __insertBefore(pid, t2.__nodeId, ref);
        } else if (n2 && typeof n2.__nodeId === 'number') {
            __insertBefore(pid, n2.__nodeId, ref);
        }
    }
};
Element.prototype.normalize = function() { __normalizeParent(this.__nodeId); };
window.__normalizeParent = function(pid) {
    // M78.58-修一: 每轮重读 children（__removeChild 改数组，快照会错位）；
    // 合并读值统一 __getText 聚合（__setText 清子建子：写过 td 空 gt 真）。
    var i = 0;
    while (true) {
        var kids = (__children(pid) || '').split(',').filter(function(x) { return x; });
        if (i >= kids.length) break;
        var id = parseInt(kids[i], 10);
        var tag = __getTag(id);
        if (!tag || tag === '__text__') {
            if (i + 1 < kids.length) {
                var id2 = parseInt(kids[i + 1], 10);
                var tag2 = __getTag(id2);
                if (!tag2 || tag2 === '__text__') {
                    // M78.62b: 双 fallback 读值（同 walk——getText 对原生 Text 空）。
                    var v1 = (__textData(id) || '') || __getText(id);
                    var v2 = (__textData(id2) || '') || __getText(id2);
                    __setText(id, v1 + v2);
                    __removeChild(pid, id2);
                    continue;
                }
            }
        }
        i += 1;
    }
};
Object.defineProperty(Element.prototype, 'outerHTML', {
    // M80.21: 规范语义——outerHTML 含自身标签（<aside>outer</aside>）。
    // 旧版只回 innerHTML——base.js 的 aside.outerHTML 注入丢掉 aside 壳。
    get: function() {
        var tg = (typeof __getTag === 'function') ? __getTag(this.__nodeId) : '';
        if (!tg || tg === '__text__') return this.innerHTML || '';
        var low = tg.toLowerCase();
        var attrs = '';
        try { attrs = window.__serAttrsOf(this.__nodeId); } catch (e) {}
        return '<' + low + attrs + '>' + (this.innerHTML || '') + '</' + low + '>';
    },
    set: function(v) {
        // 简化：等同 innerHTML（爬虫够用）
        this.innerHTML = v;
    },
    enumerable: true, configurable: true
});

// ── M70.13: CSS Transition 仿真（DOM 层通用，非特定站 hack）──
//
// 原理：很多 SPA 框架（Docsify cover、Vue <transition>、React Framer Motion）
// 依赖 CSS transition + transitionend 事件来推进渲染流程。
// 我们没有 CSS 引擎，但当 JS 修改 element.style.opacity/transform/display 时，
// 我们可以读取该元素的 transition 定义（inline style 或 __pendingTransitions），
// 在预估 duration 后自动 dispatch transitionend 事件。
//
// 不 hack 任何特定站点——任何用 CSS transition 的框架都受益。

// TransitionEvent 构造器（对齐 Web 标准）
window.TransitionEvent = function(type, options) {
    this.type = type;
    this.propertyName = (options && options.propertyName) || '';
    this.elapsedTime = (options && options.elapsedTime) || 0;
    this.pseudoElement = (options && options.pseudoElement) || '';
    this.bubbles = true;
    this.cancelable = true;
    this.target = null;
    this.currentTarget = null;
};
window.TransitionEvent.prototype = Object.create(window.Event ? window.Event.prototype : {});

// AnimationEvent（框架常用，如 Animate.css / Web Animations API fallback）
window.AnimationEvent = function(type, options) {
    this.type = type;
    this.animationName = (options && options.animationName) || '';
    this.elapsedTime = (options && options.elapsedTime) || 0;
    this.bubbles = true;
};
window.AnimationEvent.prototype = Object.create(window.Event ? window.Event.prototype : {});

// 全局 pending transition 队列（在事件循环里 drain）
window.__pendingTransitions = [];

// __onStyleChange：当 style 属性变更时检测 transition 并入队。
// 在 Element.style 的 Proxy/setter 里调用。
window.__onStyleChange = function(el, prop, oldVal, newVal) {
    if (oldVal === newVal) return;
    if (!el || typeof el.__nodeId !== 'number') return;
    // 只关心 transition 相关属性
    var transitionProps = ['opacity', 'transform', 'display', 'visibility',
                           'width', 'height', 'left', 'top', 'right', 'bottom',
                           'margin', 'padding', 'color', 'background-color'];
    if (transitionProps.indexOf(prop) < 0) return;

    // 读取该元素的 transition 定义
    var style = el.style || {};
    var tDef = style.transition || style.WebkitTransition || '';
    if (!tDef || tDef === 'none' || tDef === 'all 0s') return;

    // 解析 "opacity 0.3s ease 0s" 或 "all 0.4s"
    // 格式: <property> <duration> <timing-function> <delay>
    var parts = String(tDef).trim().split(/\s+/);
    var tProp = parts[0] || 'all';
    var tDuration = parseFloat(parts[1]) || 0;
    var tDelay = parseFloat(parts[3]) || 0;

    if (tDuration <= 0) return;
    // 如果 transition 只针对特定属性，检查 prop 是否匹配
    if (tProp !== 'all' && tProp !== prop) return;

    var totalMs = (tDuration + tDelay) * 1000;
    // 入队：{ element, propertyName, fireAt }
    window.__pendingTransitions.push({
        element: el,
        propertyName: prop,
        fireAt: Date.now() + totalMs
    });
};

// __drainDueTransitions：触发到期的 transitionend 事件。
// 由事件循环每轮调用（和 __drainDueTimers 类似）。
window.__drainDueTransitions = function() {
    var now = Date.now();
    var remaining = [];
    var fired = 0;
    for (var i = 0; i < window.__pendingTransitions.length; i++) {
        var t = window.__pendingTransitions[i];
        if (now >= t.fireAt) {
            // dispatch transitionend
            try {
                var ev = new TransitionEvent('transitionend', {
                    propertyName: t.propertyName,
                    elapsedTime: 0.3
                });
                ev.target = t.element;
                t.element.dispatchEvent(ev);
                // 也触发 animationend（部分框架用它）
                var aev = new AnimationEvent('animationend', {
                    animationName: t.propertyName,
                    elapsedTime: 0.3
                });
                t.element.dispatchEvent(aev);
            } catch(e) {
                if (typeof __log === 'function') __log('[transition] dispatch error: ' + e.message);
            }
            fired++;
        } else {
            remaining.push(t);
        }
    }
    window.__pendingTransitions = remaining;
    return fired;
};

window.__hasPendingTransitions = function() {
    return window.__pendingTransitions.length > 0;
};
// M78: window 命名访问 —— HTML 规范：带 id 的元素可在 window 上裸引用
// （WPT 测试大量使用 `div2_3` 这类裸引用）。shim 安装时 DOM 已解析，
// 为每个 id 惰性定义 getter。动态新建元素不覆盖（已知子集，记录于 PROGRESS）。
try {
    // M78.63: 接口对象批量 non-enumerable（WPT: for..in window 不应枚举到
    // Event/Node 等——浏览器全局接口默认不可枚举）。
    try {
        var __iface = ['Event','CustomEvent','EventTarget','AbortController','AbortSignal',
            'Node','Document','DOMImplementation','DocumentFragment','ProcessingInstruction',
            'DocumentType','Element','Attr','CharacterData','Text','Comment','NodeIterator',
            'TreeWalker','NodeFilter','NodeList','HTMLCollection','DOMTokenList','UIEvent',
            'MouseEvent','KeyboardEvent','WheelEvent','InputEvent','CompositionEvent',
            'TextEvent','PointerEvent','FocusEvent','StorageEvent','Range','StaticRange',
            'MutationObserver','NamedNodeMap','DOMException','Response','Request',
            'XMLHttpRequest','Window'];
        for (var ii = 0; ii < __iface.length; ii++) {
            var nm = __iface[ii];
            if (globalThis[nm] !== undefined) {
                try {
                    var val = globalThis[nm];
                    Object.defineProperty(globalThis, nm, { value: val, writable: true,
                        configurable: true, enumerable: false });
                } catch (e) {}
            }
        }
    } catch (e) {}
    // M78.103: Location 构造器（WPT location-prototype 系列）。
try { window.Location = function Location() { throw new TypeError('Illegal constructor'); }; } catch(e) {}
try { Object.defineProperty(window.Location.prototype, Symbol.toStringTag, { value: 'Location' }); } catch(e) {}
// M92: Document 构造器不在此重复定义——element shim 里有完整实现
//（nodeType/createCDATASection 等），location own accessor 也已并入。
// M78.10: 命名访问——白名单外的 id 才定义。常见全局名（testharness 的
    // test/setup/done、浏览器自身属性）不定义 accessor：<div id=test> 会把
    // self.test = fn 变成 getter 调用（sloppy 静默吞赋值）或吞 var 声明，
    // 曾致 html_dom 类 246→90 的整片回归（var x 声明被 accessor 挡住后
    // 读取得 undefined）。
    var __reservedGlobals = { test:1, setup:1, done:1, name:1, top:1, self:1,
        parent:1, opener:1, closed:1, status:1, length:1, history:1, location:1,
        document:1, window:1, navigator:1, frame:1, frames:1, origin:1, close:1,
        open:1, focus:1, blur:1, print:1, stop:1, postMessage:1, alert:1,
        confirm:1, prompt:1, scroll:1, scrollTo:1, scrollBy:1, getComputedStyle:1,
        matchMedia:1, requestAnimationFrame:1, cancelAnimationFrame:1 };
    var __namedIds = (typeof __allIds === 'function') ? String(__allIds() || '') : '';
    __namedIds.split(',').forEach(function(id) {
        if (id && !(id in window) && !__reservedGlobals[id]) {
            try {
                Object.defineProperty(window, id, {
                    get: function() { return document.getElementById(id); },
                    set: function(v) {
                        // 真实全局赋值优先：转数据属性（named access 是 fallback）。
                        try { delete window[id]; } catch (e) {}
                        try { window[id] = v; } catch (e) {}
                    },
                    configurable: true, enumerable: false
                });
            } catch (e) {}
        }
    });
} catch (e) {}
undefined;
"#;

/// M66-B: QuickJS document shim。
#[cfg(feature = "quickjs")]
const QUICKJS_DOCUMENT_SHIM: &str = r#"
document.createElement = function(tag) {
    // M78.45: 游离语义——createElement 的元素不在文档中（WPT removeChild
    /// insertBefore 照常插入）。旧 __createEl 直接挂 body 违反规范。
    var id = (typeof __createDetachedEl === 'function')
        ? __createDetachedEl(String(tag || 'div'))
        : __createEl(String(tag || 'div'));
    return __makeElement(id);
};
document.createElementNS = function(ns, tag) {
    var __el = document.createElement(tag);
    if (ns && typeof ns === 'string') {
        try { __el.namespaceURI = ns; } catch(e) {}
        // M78.125: 非 HTML 命名空间的元素保留原始 tagName（不大写）。
        if (ns !== 'http://www.w3.org/1999/xhtml') {
            try { __el.__origTagName = String(tag); } catch(e) {}
        }
    } else if (ns === '') {
        // M91: createElementNS('', tag)——空串命名空间也是显式非 HTML ns，
        // 记录空串（named property 的 name 属性、dataset 等据此排除）。
        try { __el.namespaceURI = ''; } catch(e) {}
        try { __el.__origTagName = String(tag); } catch(e) {}
    }
    return __el;
};
document.createTextNode = function(text) {
    // M78.62: 游离（__createDetachedEl）——createTextNode 的新节点不在文档中
    // （removeChild 对其应抛 NotFound；旧 __createEl 直接挂 body）。
    var id = (typeof __createDetachedEl === 'function')
        ? __createDetachedEl('__text__')
        : __createEl('__text__');
    __setText(id, String(text || ''));
    return __makeElement(id);
};
document.createDocumentFragment = function() {
    // 真正的 DocumentFragment：nodeType=11，appendChild 可用。
    // 用 Element 创建后打 __isFragment 标记，nodeType getter 据此返回 11。
    var frag = document.createElement('div');
    frag.__isFragment = true;
    return frag;
};
// M79: 真 Comment 节点。旧版返回 div——solid 模板克隆依赖 Comment 节点身份
// （cloneNode 保留占位符、insert 以注释为锚点、nodeType===8 检查）。
// 实现：`__comment__` 伪元素（nodeType getter 映射 8、nodeName '#comment'、
// innerHTML/collect_text 均输出空——不污染爬取文本）。注释数据存 JS 包装
// 对象 __data（现有 bridge 读不出树内 Comment 数据，克隆时空数据可接受）。
document.createComment = function(text) {
    var id = (typeof __createDetachedEl === 'function')
        ? __createDetachedEl('__comment__')
        : __createEl('__comment__');
    var node = __makeElement(id);
    if (node) node.__data = String(text || '');
    return node;
};
document.getElementById = function(id) {
    var nodeId = __getElById(String(id));
    return (nodeId >= 0) ? __makeElement(nodeId) : null;
};
// M78.61: 主 document 的 URL/documentURI（location 是权威源）。
// M78.89: document.location = window.location（同一对象引用）。
Object.defineProperty(document, 'location', {
    get: function() { return window.location; },
    set: function(v) { if (v && typeof v.href === 'string') { window.location = v; } else if (typeof v === 'string') { __setLocHref(v); } },
    enumerable: true, configurable: true
});
Object.defineProperty(document, 'URL', {
    get: function() { try { return location.href || ''; } catch (e) { return ''; } },
    enumerable: true, configurable: true
});
Object.defineProperty(document, 'documentURI', {
    get: function() { try { return location.href || ''; } catch (e) { return ''; } },
    enumerable: true, configurable: true
});
// M78.61: Attr 节点（createAttribute 此前完全缺失——baseURI 页因 not a
// function 中断）。getAttributeNode 一并补。
// M90: setAttributeNS/getAttributeNS/removeAttributeNS——本 DOM 无多命名空间
// 属性表，按 qualified name 降级到普通 attribute API（WPT
// Node-lookupNamespaceURI 依赖 setAttributeNS(XMLNS_NS, 'xmlns', v) 设置
// xmlns 绑定）。
Element.prototype.setAttributeNS = function(ns, qname, value) {
    return this.setAttribute(String(qname), value);
};
Element.prototype.getAttributeNS = function(ns, localName) { return this.getAttribute(String(localName)); };
Element.prototype.removeAttributeNS = function(ns, localName) { this.removeAttribute(String(localName)); };
// M91: Attr 节点工厂——value/nodeValue/textContent/localName/prefix/name/
// nodeName/specified/ownerElement 全套（WPT Document-createAttribute
// attr_is 断言）；HTML 文档小写化，非法名（空串/空白//=/>）抛
// InvalidCharacterError。
function __createAttributeNode(rawName, isHTMLDoc, ownerDoc) {
    var n0 = String(rawName);
    var bad = n0 === '' || n0.charCodeAt(0) === 0;
    if (!bad) {
        for (var ci = 0; ci < n0.length; ci++) {
            var cc = n0.charAt(ci);
            if (cc === ' ' || cc === '\t' || cc === '\n' || cc === '\r' || cc === '\f' || cc === '/' || cc === '=' || cc === '>') { bad = true; break; }
        }
    }
    if (bad) throw new DOMException('invalid attribute name', 'InvalidCharacterError');
    var lname = isHTMLDoc ? n0.toLowerCase() : n0;
    return { nodeType: 2, name: lname, nodeName: lname, localName: lname, prefix: null,
             namespaceURI: null, value: '', nodeValue: '', textContent: '',
             specified: true, ownerElement: null, ownerDocument: ownerDoc,
             lookupNamespaceURI: function(prefix) {
                 if (prefix === '' || prefix === undefined) prefix = null;
                 var el = this.ownerElement;
                 if (!el || typeof el.lookupNamespaceURI !== 'function') return null;
                 return el.lookupNamespaceURI(prefix);
             },
             isDefaultNamespace: function(nsn) {
                 if (nsn === '' || nsn === undefined) nsn = null;
                 return this.lookupNamespaceURI(null) === nsn;
             },
             lookupPrefix: function() { return null; },
             get baseURI() { return (ownerDoc && ownerDoc.URL) || ''; } };
}
document.createAttribute = function(name) {
    return __createAttributeNode(name, true, document);
};
Element.prototype.getAttributeNode = function(name) {
    var v = this.getAttribute(name);
    if (v === null || v === undefined) return null;
    return { name: String(name), value: v, specified: true, nodeType: 2,
             ownerDocument: document, ownerElement: this,
             get baseURI() { return document.URL || ''; } };
};
Element.prototype.setAttributeNode = function(attr) {
    if (attr && attr.name) this.setAttribute(attr.name, attr.value || '');
    if (attr) { try { attr.ownerElement = this; } catch (e) {} }
    return attr || null;
};
// M90: 主 document 的 namespace 查询——null 前缀返回 XHTML 命名空间
//（html 元素自身 namespace 的近似），其余前缀沿 documentElement 属性链查。
document.lookupNamespaceURI = function(prefix) {
    if (prefix === '' || prefix === undefined) prefix = null;
    if (prefix === null) return 'http://www.w3.org/1999/xhtml';
    var de = document.documentElement;
    if (!de || typeof de.lookupNamespaceURI !== 'function') return null;
    return de.lookupNamespaceURI(prefix);
};
document.isDefaultNamespace = function(ns) {
    if (ns === '' || ns === undefined) ns = null;
    return this.lookupNamespaceURI(null) === ns;
};
document.lookupPrefix = function() { return null; };
// M90: document.appendChild——规范上把节点挂到 document 节点下；本实现无
// 独立 document 节点，no-op 返回 child（WPT 断言挂到 document 的 comment
// 无命名空间继承即可）。
document.appendChild = function(child) { return child; };
// M78.120: Element.querySelector/querySelectorAll 的 :scope 处理——
// 替换为 this 自身的 id 选择器（如果无 id 则用临时 UUID）。
(function() {
    function resolveScope(sel) {
        if (typeof sel !== 'string' || sel.indexOf(':scope') < 0) return sel;
        if (!this || typeof this.__nodeId !== 'number') return sel;
        var scopeId = this.getAttribute('id');
        if (!scopeId) {
            scopeId = '__scope_' + this.__nodeId;
            this.setAttribute('id', scopeId);
        }
        return sel.replace(/:scope/g, '#' + scopeId);
    }
    var _origQS = Element.prototype.querySelector;
    Element.prototype.querySelector = function(sel) { return _origQS.call(this, resolveScope.call(this, sel)); };
    var _origQSA = Element.prototype.querySelectorAll;
    Element.prototype.querySelectorAll = function(sel) { return _origQSA.call(this, resolveScope.call(this, sel)); };
})();
document.querySelector = function(sel) {
    __qsThrowIfInvalid(sel);
    var nodeId = __qs(String(sel));
    // M91: document 级查询过滤非文档内节点（removeChild 残留等）。
    if (nodeId >= 0 && !__inDocOf(nodeId)) return null;
    return (nodeId >= 0) ? __makeElement(nodeId) : null;
};
// M91: 在文档内的判定——游离子树（createElement）与已 removeChild 的节点
// 仍留在 arena（Element 数据不清理），全局 __qsAll 会误命中；document 级
// 查询按「祖先链顶到 arena 根（id 0）」过滤。
function __inDocOf(id) {
    var cur = id;
    var guard = 0;
    while (cur >= 0 && guard++ < 512) {
        var p = __getParent(cur);
        if (typeof p !== 'number' || p < 0) return false;
        if (p === 0) return true;
        cur = p;
    }
    return false;
}
document.querySelectorAll = function(sel) {
    __qsThrowIfInvalid(sel);
    var ids = __qsAll(String(sel));
    if (!ids) return [];
    return ids.split(',').filter(function(s) { return s; })
        .map(function(s) { return parseInt(s, 10); })
        .filter(function(id) { return __inDocOf(id); })
        .map(function(id) { return __makeElement(id); });
};
document.getElementsByTagName = function(tag) {
    return document.querySelectorAll(tag);
};
document.getElementsByClassName = function(cls) {
    return document.querySelectorAll('.' + cls);
};
// M80.29: DataTransfer 桩——drag 事件的 dataTransfer 字段载体。
window.DataTransfer = function() {
    this._data = {};
    this.dropEffect = 'move';
    this.effectAllowed = 'all';
};
DataTransfer.prototype.setData = function(type, v) { this._data[type] = String(v); };
DataTransfer.prototype.getData = function(type) { return this._data[type] || ''; };
DataTransfer.prototype.clearData = function(type) { if (type) delete this._data[type]; else this._data = {}; };
// M81: elementFromPoint/elementsFromPoint——合成 hover 的命中测试配套。
// 本引擎 JS 会话内没有布局树（getBoundingClientRect 是零桩），真实命中
// 测试做不了；近似：矩形含点测试（rect 未来升级为真实值后自动变准），
// 零面积矩形（桩值）视为"无命中数据"跳过；无命中退回 body/
// documentElement——调用方（菜单/工具提示逻辑）通常只要求返回非 null
// 以继续执行。
function __rectHasPoint(r, x, y) {
    if (!r) return false;
    var w = (r.right || 0) - (r.left || 0), h = (r.bottom || 0) - (r.top || 0);
    if (w <= 0 || h <= 0) return false;
    return x >= (r.left || 0) && x <= (r.right || 0) && y >= (r.top || 0) && y <= (r.bottom || 0);
}
document.elementsFromPoint = function(x, y) {
    x = +x || 0; y = +y || 0;
    var hits = [];
    try {
        var els = document.querySelectorAll('*') || [];
        for (var i = 0; i < els.length; i++) {
            var r = null;
            try { r = els[i].getBoundingClientRect(); } catch (re) { r = null; }
            if (__rectHasPoint(r, x, y)) hits.push(els[i]);
        }
    } catch (qe) {}
    if (hits.length === 0) {
        var fb = document.body || document.documentElement;
        if (fb) hits.push(fb);
    }
    return hits;
};
document.elementFromPoint = function(x, y) {
    var hits = document.elementsFromPoint(x, y);
    return hits.length ? hits[hits.length - 1] : null;
};
// M78.42: live HTMLCollection——named property 语义（WebIDL legacy platform
// object）。Proxy set 返回 false 精确复刻赋值语义：sloppy 静默 / strict
// TypeError；named 未命中时创建 own 属性（后续 get 优先 own）。
window.__makeLiveCollection = function(queryFn) {
    var target = { __own: {} };
    var proxyRef = null;  // M91: receiver 品牌检查用（延迟捕获 proxy 本体）
    function lookup(k) {
        var arr = queryFn();
        if (typeof k === 'string' && /^\d+$/.test(k)) {
            var i = +k;
            return (i >= 0 && i < arr.length) ? { el: arr[i] } : null;
        }
        for (var j = 0; j < arr.length; j++) {
            var el = arr[j];
            var id = (typeof el.getAttribute === 'function') ? el.getAttribute('id') : null;
            var nm = (typeof el.getAttribute === 'function') ? el.getAttribute('name') : null;
            if (id === k) return { el: el };
            // name 属性只对 HTML 命名空间元素生效（WPT Element-children：
            // createElementNS('', 'img')[name] 不参与 named property）。
            if (nm === k) {
                var ns = el.namespaceURI;
                if (ns === undefined || ns === 'http://www.w3.org/1999/xhtml') return { el: el };
            }
        }
        return null;
    }
    proxyRef = new Proxy(target, {
        get: function(t, k, receiver) {
            // M91: WebIDL 品牌检查——接口属性（length/item/namedItem）经
            // 原型链（Object.create(collection)）访问时 this 非法 → TypeError。
            if ((k === 'length' || k === 'item' || k === 'namedItem') && receiver !== proxyRef) {
                throw new TypeError('Illegal invocation');
            }
            if (k === 'length') return queryFn().length;
            if (k === 'item') return function(i) {
                var a = queryFn();
                // M91: WebIDL unsigned long 转换——item('foo') → NaN → 0
                //（WPT Element-children item('foo') 返回第 0 个元素）。
                var n = Number(i);
                if (isNaN(n)) n = 0;
                return (n >= 0 && n < a.length) ? a[n] : null;
            };
            if (k === 'namedItem') return function(n) { var r = lookup(n); return r ? r.el : null; };
            if (k === Symbol.toStringTag) return 'HTMLCollection';
            if (typeof k === 'string' && Object.prototype.hasOwnProperty.call(t.__own, k)) return t.__own[k];
            var r = lookup(k);
            if (r) return r.el;
            // M91: 未命中回落原型链（此前显式返回 undefined 吞掉了
            // hasOwnProperty/toString 等 Object.prototype 方法）。
            return Reflect.get(t, k);
        },
        set: function(t, k, v, receiver) {
            // M91: 索引键恒拒（legacy platform object 无索引 setter——
            // sloppy 静默 / strict TypeError，WPT HTMLCollection-own-props）。
            if (typeof k === 'string' && /^\d+$/.test(k)) return false;
            if (typeof k === 'string' && lookup(k)) {
                // named 已存在：collection 自身赋值拒绝；派生 receiver 走
                // OrdinarySet 语义（在 receiver 上建 own 属性）。
                if (receiver !== proxyRef) {
                    try {
                        Object.defineProperty(receiver, k, { value: v, writable: true, enumerable: true, configurable: true });
                    } catch (e) {}
                    return true;
                }
                return false;
            }
            t.__own[k] = v;
            return true;
        },
        has: function(t, k) {
            if (Object.prototype.hasOwnProperty.call(t.__own, k)) return true;
            return !!lookup(k);
        },
        // M91: ownKeys——索引键 + named 键（不含 length：length 是接口属性，
        // 非 own——WPT getOwnPropertyNames 断言）。name 属性仅 HTML ns。
        ownKeys: function(t) {
            var arr = queryFn();
            var keys = [];
            for (var i = 0; i < arr.length; i++) keys.push(String(i));
            var seen = {};
            for (var j = 0; j < arr.length; j++) {
                var el = arr[j];
                var id = (typeof el.getAttribute === 'function') ? el.getAttribute('id') : null;
                var nm = (typeof el.getAttribute === 'function') ? el.getAttribute('name') : null;
                if (nm) {
                    var elNs = el.namespaceURI;
                    if (!(elNs === undefined || elNs === 'http://www.w3.org/1999/xhtml')) nm = null;
                }
                if (id && !seen[id]) { keys.push(id); seen[id] = 1; }
                if (nm && !seen[nm]) { keys.push(nm); seen[nm] = 1; }
            }
            return keys;
        },
            getOwnPropertyDescriptor: function(t, k) {
                if (typeof k === 'string' && Object.prototype.hasOwnProperty.call(t.__own, k)) {
                    return Object.getOwnPropertyDescriptor(t.__own, k);
                }
                var r = lookup(k);
                // M91: 索引键可枚举 / named 键不可枚举（for-in + hasOwnProperty
                // 只出索引键，`in` 与 hasOwnProperty 对 named 均真）。
                if (r) {
                    var isIdx = (typeof k === 'string') && /^\d+$/.test(k);
                    return { value: r.el, writable: false, enumerable: !!isIdx, configurable: true };
                }
                if (k === 'length') return { value: queryFn().length, writable: false, enumerable: true, configurable: true };
                return undefined;
            }
    });
    return proxyRef;
};
document.getElementsByTagName = function(tag) {
    return window.__makeLiveCollection(function() { return document.querySelectorAll(tag); });
};
document.getElementsByClassName = function(cls) {
    return window.__makeLiveCollection(function() { return document.querySelectorAll('.' + cls); });
};
Object.defineProperty(document, 'body', {
    get: function() { return __makeElement(__getBody(0)); },
    enumerable: true, configurable: true
});
Object.defineProperty(document, 'documentElement', {
    get: function() { return __makeElement(__findTag('html')); },
    enumerable: true, configurable: true
});
Object.defineProperty(document, 'head', {
    // M78.23: 返回真实 <head>（旧实现误返回 body）。
    get: function() {
        var n = (typeof __findTag === 'function') ? __findTag('head') : -1;
        return (typeof n === 'number' && n >= 0) ? __makeElement(n) : null;
    },
    enumerable: true, configurable: true
});
Object.defineProperty(document, 'title', {
    get: function() {
        // M67: 对齐 boa —— 读第一个 <title> 节点的文本。CDP evaluate
        // （document.title）依赖这个，不能返回空字符串。
        // 用 __findTag（字符串→NodeId）而非 __getTag（NodeId→tag 名）。
        var n = (typeof __findTag === 'function') ? __findTag('title') : -1;
        return (typeof n === 'number' && n >= 0 && typeof __getText === 'function')
            ? (__getText(n) || '')
            : '';
    },
    set: function(v) {
        // M68: document.title setter —— 把值写回 <title> 节点（对齐浏览器：
        // 改 title 会更新 <title> 元素文本，getter 再读时反映新值）。
        var n = (typeof __findTag === 'function') ? __findTag('title') : -1;
        if (typeof n === 'number' && n >= 0 && typeof __setText === 'function') {
            __setText(n, String(v));
        }
    },
    enumerable: true, configurable: true
});

var __cookieJar = {};
Object.defineProperty(document, 'cookie', {
    get: function() {
        var parts = [];
        for (var k in __cookieJar) {
            if (__cookieJar.hasOwnProperty(k)) {
                parts.push(k + '=' + __cookieJar[k]);
            }
        }
        return parts.join('; ');
    },
    set: function(v) {
        if (typeof v === 'string') {
            var eq = v.indexOf('=');
            if (eq > 0) {
                var name = v.substring(0, eq).trim();
                var value = v.substring(eq + 1).split(';')[0];
                __cookieJar[name] = value;
            }
        }
    },
    enumerable: true, configurable: true
});
// M93.8: readyState 真语义（spec：loading → interactive（DCL）→ complete（load））。
// 旧版硬编码 'complete'——xcancel antibot 的编排器（混淆 VM）在模块加载时检查
// `document.readyState` 决定"等 DOMContentLoaded"还是"立即执行 main"：旧值让它
// 在 Pass 1（cap.min.js 等经典脚本尚未执行）直跑 main，依赖缺失 → 静默死锁，
// 挑战永不启动。真语义下 main 等 DCL（所有脚本后派发）→ 依赖就绪。
var __docReadyState = 'loading';
Object.defineProperty(document, 'readyState', {
    get: function() { return __docReadyState; },
    enumerable: true, configurable: true
});
document.addEventListener = function(type, cb, opt) {
    // M78.129: 记录 capture 标志（dispatchEvent 三阶段过滤用）。
    var dcap = (opt === true) || !!(opt && opt.capture);
    if (!this.__listeners) this.__listeners = {};
    if (!this.__listeners[type]) this.__listeners[type] = [];
    if (!this.__listenerCaps) this.__listenerCaps = {};
    if (!this.__listenerCaps[type]) this.__listenerCaps[type] = [];
    this.__listeners[type].push(cb);
    this.__listenerCaps[type].push(dcap);
};
document.removeEventListener = function(type, cb) {};
document.dispatchEvent = function(ev) {
    // M78.129: 对齐 dispatch 语义——document 为 target（全部 listener 按注册序、
    // AT_TARGET），bubbles=true 冒泡到 window；stop 标志生效；结束清标志
    //（同一 event 可再次 dispatch，WPT Event-dispatch-multiple-cancelBubble）。
    if (!ev) return true;
    ev.target = this;
    function __dStopped() {
        return !!(ev.__immediate || ev.__stopPropagation || ev.cancelBubble);
    }
    if (!__dStopped()) {
        ev.currentTarget = this; ev.eventPhase = 2;
        var cbs = (this.__listeners && this.__listeners[ev.type]) || null;
        if (cbs) {
            var snap = cbs.slice();
            for (var i = 0; i < snap.length; i++) {
                try { snap[i].call(this, ev); } catch (e) {}
                if (ev.__immediate) break;
            }
        }
    }
    if (ev.bubbles && !__dStopped()) {
        ev.currentTarget = window; ev.eventPhase = 3;
        var wcbs = (typeof __winListeners !== 'undefined' && __winListeners[ev.type]) || null;
        var wcap = (typeof __winListenersCap !== 'undefined' && __winListenersCap[ev.type]) || null;
        if (wcbs) {
            var wsnap = wcbs.slice();
            for (var wi = 0; wi < wsnap.length; wi++) {
                if (wcap && wcap[wi]) continue;
                try { wsnap[wi].call(window, ev); } catch (e) {}
                if (ev.__immediate) break;
            }
        }
    }
    ev.eventPhase = 0; ev.currentTarget = null;
    ev.cancelBubble = false; ev.__stopPropagation = false;
    try { delete ev.__immediate; } catch (de) {}
    return true;
};
// ===== M78: WPT 高频 DOM API 补齐（html_dom 类循环 5）=====
// document.contentType（Document-contentType 系列）
if (document.contentType === undefined) {
    Object.defineProperty(document, 'contentType', {
        get: function() { return 'text/html'; }, enumerable: true, configurable: true
    });
}
// document.images / document.scripts（dom-tree-accessors）
document.images = document.querySelectorAll('img');
// M78.28: HTMLCollection 近似——querySelectorAll/getElementsByTagName 的
// 返回值补 namedItem/item（WPT HTMLCollection 断言）。数组原型扩展法
// （不改每次返回的数组，挂在 Array.prototype 由 name/id 反射）。
window.HTMLCollection = function() {};
Object.defineProperty(window.HTMLCollection.prototype, Symbol.toStringTag, { value: 'HTMLCollection' });
// M78.30: NodeList 近似——querySelectorAll 返回数组的 item/valueOf 语义。
window.NodeList = function() {};
Object.defineProperty(window.NodeList.prototype, Symbol.toStringTag, { value: 'NodeList' });
Array.prototype.item = function(i) { return (i >= 0 && i < this.length) ? this[i] : null; };
Array.prototype.entries = Array.prototype.entries || function() {
    var self = this, i = 0;
    return { next: function() { return i < self.length ? { value: [i, self[i++]], done: false } : { value: undefined, done: true }; } };
};
document.styleSheets = [];
document.alinkColor = ''; document.linkColor = ''; document.vlinkColor = '';
document.bgColor = ''; document.fgColor = '';
Array.prototype.namedItem = function(name) {
    if (name === undefined || name === null) return null;
    name = String(name);
    for (var i = 0; i < this.length; i++) {
        var el = this[i];
        if (el && el.getAttribute) {
            if (el.getAttribute('id') === name || el.getAttribute('name') === name) return el;
        }
    }
    return null;
};
// M78.28: window 遗留可写属性（HTMLCollection 测试的 loose/strict 赋值目标）。
if (window.status === undefined) { window.status = ''; }
if (window.name === undefined) { window.name = ''; }
window.closed = false;
window.length = 0;
// M78.91: frames 按索引返回 iframe 的 contentWindow（Node-removeChild 等依赖）。
window.frames = new Proxy([], {
    get: function(t, k) {
        if (typeof k === 'string' && /^\d+$/.test(k)) {
            var iframes = (typeof __qsAll === 'function') ? __qsAll('iframe') : '';
            var ids = (iframes || '').split(',').filter(function(x) { return x; });
            var idx = parseInt(k, 10);
            if (idx < ids.length) {
                var el = __makeElement(parseInt(ids[idx], 10));
                return el.contentWindow;
            }
            return undefined;
        }
        return t[k];
    }
});
window.opener = null;
// M92: 顶层窗口的 self/parent/top 都是自己（WPT cross-realm 系列在回调体里
// eval `parent.frames[i]`——parent 未定义直接 ReferenceError）。
if (typeof window.parent === 'undefined') { window.parent = window; }
if (typeof window.top === 'undefined') { window.top = window; }
if (typeof window.self === 'undefined') { window.self = window; }
document.scripts = document.querySelectorAll('script');
// document.createTreeWalker：DFS 顺序的基本实现（NodeIterator 同理最小桩）。
// M78.127: 赋值定义（configurable——WPT interface-objects delete 断言）。
// M91: TreeWalker——按 DOM Standard 遍历算法重写（旧实现预计算序列 +
// 裸索引，不认 filter/root 边界/previousSibling）。
globalThis.TreeWalker = function TreeWalker(root, whatToShow, filter) {
    this.root = root;
    this.__cur = root;
    this.whatToShow = (whatToShow === undefined) ? 0xFFFFFFFF : whatToShow;
    this.filter = (filter === undefined) ? null : filter;
}
Object.defineProperty(TreeWalker.prototype, 'currentNode', {
    get: function() { return this.__cur; },
    set: function(v) {
        // WPT：currentNode = null / {} / window 必须 TypeError。
        var ok = v && (typeof v.__nodeId === 'number' ||
                 (typeof Node === 'function' && v instanceof Node));
        if (!ok) throw new TypeError('currentNode must be a Node');
        this.__cur = v;
    },
    enumerable: true, configurable: true
});
(function() {
    function kidsOf(n) {
        if (!n || typeof n.__nodeId !== 'number') return [];
        var s = __children(n.__nodeId);
        if (!s) return [];
        return s.split(',').filter(function(x) { return x !== ''; })
                .map(function(x) { return __makeElement(parseInt(x, 10)); })
                .filter(function(x) { return x && typeof x.__nodeId === 'number'; });
    }
    function sibOf(n, delta) {
        var pid = (n && typeof n.__nodeId === 'number') ? __getParent(n.__nodeId) : -1;
        if (typeof pid !== 'number' || pid < 0) return null;
        var ks = kidsOf(__makeElement(pid));
        for (var i = 0; i < ks.length; i++) {
            if (ks[i].__nodeId === n.__nodeId) {
                var j = i + delta;
                return (j >= 0 && j < ks.length) ? ks[j] : null;
            }
        }
        return null;
    }
    function match(self, n) {
        var nt = (n && n.nodeType) || 1;
        if (nt === 11 || nt === 9 || nt === 10) { /* fragment/document/doctype 位 */ }
        var bit = 1 << (nt - 1);
        if (!(self.whatToShow & bit)) return NodeFilter.FILTER_SKIP;
        var f = self.filter;
        if (!f) return NodeFilter.FILTER_ACCEPT;
        var r;
        if (typeof f === 'function') r = f.call(f, n);
        else if (f && typeof f.acceptNode === 'function') r = f.acceptNode.call(f, n);
        else throw new TypeError('filter must be a function or acceptNode object');
        return (r === undefined || r === null) ? NodeFilter.FILTER_ACCEPT : +r;
    }
    function contains(self, n) {
        // root 是否 n 的祖先（含自身）
        var c = n;
        var guard = 0;
        while (c && guard++ < 256) {
            if (c === self.root) return true;
            c = (typeof c.__nodeId === 'number') ? __makeElement(__getParent(c.__nodeId)) : null;
            if (c && typeof c.__nodeId !== 'number') c = null;
        }
        return false;
    }
    TreeWalker.prototype.parentNode = function() {
        var cur = this.__cur;
        // 根节点/游离于 root 子树外 → null（不更新 currentNode）
        if (cur === this.root || !contains(this, cur)) return null;
        var pid = __getParent(cur.__nodeId);
        if (typeof pid !== 'number' || pid < 0) return null;
        var p = __makeElement(pid);
        this.__cur = p;
        return p;
    };
    TreeWalker.prototype.firstChild = function() {
        var ks = kidsOf(this.__cur);
        for (var i = 0; i < ks.length; i++) {
            var n = ks[i];
            var m = match(this, n);
            if (m === NodeFilter.FILTER_ACCEPT) { this.__cur = n; return n; }
            if (m === NodeFilter.FILTER_SKIP) {
                var save = this.__cur;
                this.__cur = n;
                var d = this.firstChild();
                this.__cur = d || save;
                if (d) return d;
            }
            // REJECT → 跳过子树（继续下一兄弟）
        }
        return null;
    };
    TreeWalker.prototype.lastChild = function() {
        var ks = kidsOf(this.__cur);
        for (var i = ks.length - 1; i >= 0; i--) {
            var n = ks[i];
            var m = match(this, n);
            if (m === NodeFilter.FILTER_ACCEPT) { this.__cur = n; return n; }
            if (m === NodeFilter.FILTER_SKIP) {
                var save = this.__cur;
                this.__cur = n;
                var d = this.lastChild();
                this.__cur = d || save;
                if (d) return d;
            }
        }
        return null;
    };
    TreeWalker.prototype.nextSibling = function() {
        var n = sibOf(this.__cur, 1);
        var guard = 0;
        while (n && guard++ < 4096) {
            var m = match(this, n);
            if (m === NodeFilter.FILTER_ACCEPT) { this.__cur = n; return n; }
            if (m === NodeFilter.FILTER_SKIP) {
                var save = this.__cur;
                this.__cur = n;
                var d = this.firstChild();
                this.__cur = d || save;
                if (d) return d;
            }
            n = sibOf(n, 1);
        }
        return null;
    };
    TreeWalker.prototype.previousSibling = function() {
        var n = sibOf(this.__cur, -1);
        var guard = 0;
        while (n && guard++ < 4096) {
            var m = match(this, n);
            if (m === NodeFilter.FILTER_ACCEPT) { this.__cur = n; return n; }
            if (m === NodeFilter.FILTER_SKIP) {
                var save = this.__cur;
                this.__cur = n;
                var d = this.lastChild();
                this.__cur = d || save;
                if (d) return d;
            }
            n = sibOf(n, -1);
        }
        return null;
    };
    TreeWalker.prototype.nextNode = function() {
        var guard = 0;
        var descend = true;
        var n = this.__cur;
        while (guard++ < 8192) {
            var next = null;
            if (descend) {
                var ks = kidsOf(n);
                if (ks.length) next = ks[0];
            }
            if (!next) {
                // 上溯找右兄弟；穿过 root 仍无 → null
                var cur = n;
                while (cur) {
                    if (cur === this.root) return null;
                    var sib = sibOf(cur, 1);
                    if (sib) { next = sib; break; }
                    cur = (typeof cur.__nodeId === 'number') ? __makeElement(__getParent(cur.__nodeId)) : null;
                    if (cur && typeof cur.__nodeId !== 'number') cur = null;
                }
                if (!next) return null;
            }
            n = next;
            var m = match(this, n);
            if (m === NodeFilter.FILTER_ACCEPT) { this.__cur = n; return n; }
            descend = (m !== NodeFilter.FILTER_REJECT);  // REJECT 不进子树
        }
        return null;
    };
    TreeWalker.prototype.previousNode = function() {
        var guard = 0;
        var n = this.__cur;
        while (guard++ < 8192) {
            var prev = null;
            // 先试左兄弟的最后一棵子树（REJECT 场景 currentNode 子树不回溯——
            // previousNode 语义：前一个树序节点）
            var sib = sibOf(n, -1);
            if (sib) {
                // 下潜到 sib 最后叶子
                var d = sib;
                var dg = 0;
                while (dg++ < 1024) {
                    var ks = kidsOf(d);
                    if (!ks.length) break;
                    d = ks[ks.length - 1];
                }
                prev = d;
            } else {
                var cur = n;
                while (cur) {
                    if (cur === this.root) return null;
                    var par = (typeof cur.__nodeId === 'number') ? __makeElement(__getParent(cur.__nodeId)) : null;
                    if (!par || typeof par.__nodeId !== 'number') return null;
                    prev = par;
                    break;
                }
            }
            if (!prev) return null;
            n = prev;
            var m = match(this, n);
            if (m === NodeFilter.FILTER_ACCEPT) { this.__cur = n; return n; }
            // REJECT/SKIP：继续前溯（上一轮 sibOf(n,-1) 已定位）
        }
        return null;
    };
})();
// M78.115: NodeFilter 常量（WPT TreeWalker 断言依赖）。
window.NodeFilter = {
    SHOW_ALL: 0xFFFFFFFF, SHOW_ELEMENT: 1, SHOW_ATTRIBUTE: 2, SHOW_TEXT: 4,
    SHOW_CDATA_SECTION: 8, SHOW_ENTITY_REFERENCE: 16, SHOW_ENTITY: 32,
    SHOW_PROCESSING_INSTRUCTION: 64, SHOW_COMMENT: 128, SHOW_DOCUMENT: 256,
    SHOW_DOCUMENT_TYPE: 512, SHOW_DOCUMENT_FRAGMENT: 1024, SHOW_NOTATION: 2048,
    FILTER_ACCEPT: 1, FILTER_REJECT: 2, FILTER_SKIP: 3
};
document.createTreeWalker = function(root, whatToShow, filter) { return new TreeWalker(root, whatToShow, filter); };
// M78: document.createEvent —— WPT Event-constants/老式 API 依赖。
document.createEvent = function(type) {
    var t = String(type || 'Event');
    // M80.18: 单数 'MouseEvent' 是 WPT uievents/legacy-domevents 实际用法
    // （dispatchEvent.click.checkbox 等），此前只认复数 'MouseEvents'，
    // 单数落 new Event('') → 产物无 initMouseEvent → no-results。
    if (t === 'MouseEvent' || t === 'MouseEvents') return new MouseEvent('click');
    if (t === 'UIEvents' || t === 'HTMLEvents') return new Event('load');
    if (t === 'CustomEvent') return new CustomEvent('');
    if (t === 'TextEvent') {
        // M78.64: createEvent 绕过构造器的 TypeError（工厂路径合法）。
        var te = Object.create(TextEvent.prototype);
        Event.call(te, 'textInput');
        return te;
    }
    return new Event('');
};
document.createComment = document.createComment || function(text) { return document.createElement('div'); };
// M78: Range —— WPT dom/ranges 系列依赖（构造器四属性 + 常用方法桩）。
function Range() {
    this.startContainer = document;
    this.endContainer = document;
    this.startOffset = 0;
    this.endOffset = 0;
    this.collapsed = true;
    this.commonAncestorContainer = document;
}
Range.prototype.setStart = function(node, offset) {
    this.startContainer = node; this.startOffset = offset; this.collapsed = this._recalc();
};
Range.prototype.setEnd = function(node, offset) {
    this.endContainer = node; this.endOffset = offset; this.collapsed = this._recalc();
};
Range.prototype._recalc = function() {
    return this.startContainer === this.endContainer && this.startOffset === this.endOffset;
};
Range.prototype.collapse = function(toStart) {
    if (toStart) { this.endContainer = this.startContainer; this.endOffset = this.startOffset; }
    else { this.startContainer = this.endContainer; this.startOffset = this.endOffset; }
    this.collapsed = true;
};
Range.prototype.cloneRange = function() {
    var r = new Range();
    r.startContainer = this.startContainer; r.endContainer = this.endContainer;
    r.startOffset = this.startOffset; r.endOffset = this.endOffset; r.collapsed = this.collapsed;
    return r;
};
Range.prototype.selectNodeContents = function(node) {
    // M91: endOffset = 节点长度（元素=子节点数，文本=data 长度）——此前写 0
    // 导致 collapsed 恒 true（WPT Range-stringifier 首断言）。
    this.startContainer = node; this.endContainer = node; this.startOffset = 0;
    var len = 0;
    try {
        var nid = node.__nodeId;
        var tg = String(__getTag(nid));
        if (tg === '__text__' || !tg) len = String((__textData(nid) || __getText(nid) || '')).length;
        else len = (__children(nid) || '').split(',').filter(function(x) { return x !== ''; }).length;
    } catch (e) { len = 0; }
    this.endOffset = len;
    this.collapsed = this._recalc();
};
Range.prototype.deleteContents = function() { this.collapse(true); };
Range.prototype.cloneContents = function() { return document.createDocumentFragment(); };
Range.prototype.extractContents = function() { return document.createDocumentFragment(); };
Range.prototype.insertNode = function() {};
Range.prototype.getBoundingClientRect = function() { return { x:0, y:0, top:0, left:0, right:0, bottom:0, width:0, height:0 }; };
Range.prototype.detach = function() {};
// M78.116 + M91: Range.toString()——按 DOM Standard「get a string of a live
// range」语义：同容器文本取子串；跨节点时按树序拼叶子文本，元素边界插 LF。
Range.prototype.toString = function() {
    try {
        var sc = this.startContainer, ec = this.endContainer;
        var so = this.startOffset | 0, eo = this.endOffset | 0;
        var scId = (sc && typeof sc.__nodeId === 'number') ? sc.__nodeId : -1;
        var ecId = (ec && typeof ec.__nodeId === 'number') ? ec.__nodeId : -1;
        if (scId < 0 || ecId < 0) return '';
        var dataOf = function(id) { return String((__textData(id) || __getText(id) || '')); };
        var isTextId = function(id) { var t = String(__getTag(id)); return t === '__text__' || t === '__comment__' || !t; };
        var kids = function(id) { return (__children(id) || '').split(',').filter(function(x) { return x !== ''; }).map(function(x) { return parseInt(x, 10); }); };
        var parentOf = function(id) { var p = __getParent(id); return (typeof p === 'number' && p >= 0) ? p : -1; };
        // 同文本容器：直接子串
        if (scId === ecId && isTextId(scId)) return dataOf(scId).slice(so, eo);
        // 公共祖先（用 id 链求）
        var chain = {};
        var a = scId;
        while (a >= 0) { chain[a] = true; a = parentOf(a); }
        var common = ecId;
        while (common >= 0 && !chain[common]) common = parentOf(common);
        if (common < 0) common = 0;
        // 树序收集文本叶子
        var leaves = [];
        (function dfs(id) {
            if (isTextId(id)) { leaves.push(id); return; }
            var ck = kids(id);
            for (var i = 0; i < ck.length; i++) dfs(ck[i]);
        })(common);
        // 前序编号（树序比较用）
        var rank = {};
        (function num(id) {
            rank[id] = Object.keys(rank).length;
            var ck = kids(id);
            for (var i = 0; i < ck.length; i++) num(ck[i]);
        })(common);
        // 叶子相对起点 (nid, off)：'partial'（叶子即 nid）/ 'full'（含）/ 'before'（排除）
        var posStart = function(leafId, nid, off) {
            if (leafId === nid) return (off > 0) ? 'partial' : 'full';
            var c = leafId, p = parentOf(c), foundAnc = false;
            while (p >= 0) {
                if (p === nid) { foundAnc = true; break; }
                c = p; p = parentOf(c);
            }
            if (foundAnc) return (kids(nid).indexOf(c) >= off) ? 'full' : 'before';
            return (rank[leafId] > rank[nid]) ? 'full' : 'before';
        };
        // 叶子相对终点 (nid, off)：'partial'/'full'（含）/ 'after'（排除）
        var posEnd = function(leafId, nid, off) {
            if (leafId === nid) return 'partial';
            var c = leafId, p = parentOf(c), foundAnc = false;
            while (p >= 0) {
                if (p === nid) { foundAnc = true; break; }
                c = p; p = parentOf(c);
            }
            if (foundAnc) return (kids(nid).indexOf(c) < off) ? 'full' : 'after';
            return (rank[leafId] < rank[nid]) ? 'full' : 'after';
        };
        var s = '';
        var emitted = 0, lastLeaf = -1, ended = false;
        for (var li = 0; li < leaves.length && !ended; li++) {
            var t = leaves[li];
            var ps = posStart(t, scId, so);
            if (ps === 'before') continue;
            var pe = posEnd(t, ecId, eo);
            if (pe === 'after') break;
            var data = dataOf(t);
            if (ps === 'partial') data = data.slice(so);
            if (pe === 'partial') { data = data.slice(0, eo); ended = true; }
            // M91-fix: 纯文本拼接——预期里的 LF 来自源码空白文本节点，
            // 不做合成换行（WPT Range-stringifier 断言）。
            s += data;
            emitted++; lastLeaf = t;
        }
        return s;
    } catch (e) { return ''; }
};
window.Range = Range;
document.createRange = function() { return new Range(); };
document.createNodeIterator = function(root, whatToShow) { return new TreeWalker(root, whatToShow); };
// document.createHTMLDocument：独立 document 对象（元素挂到根，不进 body）。
document.createHTMLDocument = function(title) {
    var d = Object.create(Object.getPrototypeOf(document));
    // M78.126: 子文档 creator 全部走游离语义（__createDetachedEl）——旧版用
    // __createEl 直接挂主文档 body，导致 body.removeChild(s) 不抛 NotFound；
    // createTextNode 委托主文档导致 ownerDocument 断言失败（期望 d 得到主
    // document）。全部挂 __ownerDoc，并补齐 createComment（__comment__ 伪标签）。
    var __mkSub = function(kind, val) {
        var id = (typeof __createDetachedEl === 'function')
            ? __createDetachedEl(kind)
            : __createEl(kind);
        if (kind === '__text__' || kind === '__comment__') {
            try { __setText(id, String(val || '')); } catch (e) {}
        }
        var el = __makeElement(id);
        try { el.__ownerDoc = d; } catch (e) {}
        return el;
    };
    d.createElement = function(tag) { return __mkSub(String(tag || 'div'), ''); };
    d.createElementNS = function(ns, tag) { return d.createElement(tag); };
    d.createTextNode = function(t) { return __mkSub('__text__', t); };
    d.createComment = function(t) { return __mkSub('__comment__', t); };
    d.createDocumentFragment = function() { return document.createDocumentFragment(); };
    d.createEvent = function(t) { return new Event(t === 'UIEvents' ? 'UIEvent' : (t || '')); };
    d.body = d.createElement('body');
    d.documentElement = d.createElement('html');
    // M78.71: title 空白规范化(连续空白折叠为单空格——HTML title 语义)。
    d.title = String(title === undefined ? '' : title === null ? 'null' : title).replace(/\s+/g, ' ').trim();
    d.addEventListener = function() {};
    d.removeEventListener = function() {};
    d.getElementsByTagName = function(tag) { return []; };
    d.getElementById = function() { return null; };
    d.querySelector = function() { return null; };
    d.querySelectorAll = function() { return []; };
    // M92: 子文档无 browsing context → location 为 null（WPT
    // document_location "document not in a browsing context"）。
    d.location = null;
    return d;
};
// M78.72 + M91-fix: DOMStringMap 全局构造器 + removeAttribute 兜底。
// （此前误嵌在 createHTMLDocument 函数体内——只有调用 createHTMLDocument
// 才定义，页面顶层 `div.dataset instanceof DOMStringMap` 报 not defined、
// `div.removeAttribute` not a function。移到 shim 顶层。）
globalThis.DOMStringMap = function DOMStringMap() { throw new TypeError('Illegal constructor'); };
Object.defineProperty(window.DOMStringMap.prototype, Symbol.toStringTag, { value: 'DOMStringMap' });
Element.prototype.removeAttribute = Element.prototype.removeAttribute || function(name) {
    __removeAttr(this.__nodeId, String(name));
};
// element.attributes：基本 NamedNodeMap（length/item/getNamedItem）。
function NamedNodeMap(nodeId) { this.__nodeId = nodeId; }
Object.defineProperty(NamedNodeMap.prototype, Symbol.toStringTag, { value: 'NamedNodeMap' });
NamedNodeMap.prototype.__pairs = function() {
    var out = [];
    var s = (typeof __attrsOf === 'function') ? __attrsOf(this.__nodeId) : '';
    (s || '').split('\n').forEach(function(line) {
        var eq = line.indexOf('=');
        if (eq > 0) out.push({ name: line.slice(0, eq), value: line.slice(eq + 1), specified: true });
    });
    return out;
};
NamedNodeMap.prototype.getNamedItem = function(name) {
    var ps = this.__pairs();
    for (var i = 0; i < ps.length; i++) if (ps[i].name === String(name)) return ps[i];
    return null;
};
NamedNodeMap.prototype.item = function(i) { return this.__pairs()[i] || null; };
NamedNodeMap.prototype.setNamedItem = function(attr) {
    if (attr && attr.name) __setAttr(this.__nodeId, attr.name, attr.value || '');
    return attr || null;
};
NamedNodeMap.prototype.removeNamedItem = function(name) { __removeAttr(this.__nodeId, String(name)); return null; };
Object.defineProperty(NamedNodeMap.prototype, 'length', { get: function() { return this.__pairs().length; } });
// M78.43: ownKeys/gOPD——Object.getOwnPropertyNames(attrs) 枚举属性名。
Object.defineProperty(NamedNodeMap.prototype, Symbol.toStringTag, { value: 'NamedNodeMap' });
NamedNodeMap.prototype[Symbol.iterator] = function() {
    var pairs = this.__pairs(), i = 0;
    return { next: function() { return i < pairs.length ? { value: pairs[i++], done: false } : { value: undefined, done: true }; } };
};
NamedNodeMap.prototype.forEach = function(fn, thisArg) {
    var pairs = this.__pairs();
    for (var i = 0; i < pairs.length; i++) fn.call(thisArg, pairs[i], String(i), this);
};
Object.defineProperty(Element.prototype, 'attributes', {
    get: function() {
        if (!this.__attrs) this.__attrs = new NamedNodeMap(this.__nodeId);
        return this.__attrs;
    },
    enumerable: true, configurable: true
});
window.NamedNodeMap = NamedNodeMap;
undefined;
"#;

/// M66-B: QuickJS XHR + Event + fetch shim（docsify 核心依赖）。
#[cfg(feature = "quickjs")]
const QUICKJS_XHR_SHIM: &str = r#"
// Event 构造器（M78: 对齐 WPT 断言——Symbol.toStringTag/常量/phase 属性）
// M78.127: 赋值定义（configurable——WPT interface-objects delete 断言）。
globalThis.Event = function Event(type, opts) {
    opts = opts || {};
    this.type = String(type);
    this.target = null;
    this.currentTarget = null;
    this.bubbles = !!opts.bubbles;
    this.cancelable = !!opts.cancelable;
    this.composed = !!opts.composed;
    this.eventPhase = Event.AT_TARGET;
    this.defaultPrevented = false;
    this.isTrusted = false;
    this.cancelBubble = false;
    this.returnValue = true;
    this.timeStamp = (typeof performance !== 'undefined' && performance.now) ? performance.now() : Date.now();
}
Object.defineProperty(Event.prototype, Symbol.toStringTag, { value: 'Event' });
Event.NONE = 0; Event.CAPTURING_PHASE = 1; Event.AT_TARGET = 2; Event.BUBBLING_PHASE = 3;
Event.prototype.NONE = 0; Event.prototype.CAPTURING_PHASE = 1;
Event.prototype.AT_TARGET = 2; Event.prototype.BUBBLING_PHASE = 3;
Event.prototype.preventDefault = function() { this.defaultPrevented = true; };
Event.prototype.stopPropagation = function() { this.cancelBubble = true; };
Event.prototype.stopImmediatePropagation = function() { this.cancelBubble = true; this.__immediate = true; };
Event.prototype.initEvent = function(type, bubbles, cancelable) {
    // M78.65: init 后允许重新 dispatch——重置派发状态。
    this.type = String(type); this.bubbles = !!bubbles; this.cancelable = !!cancelable;
    this.defaultPrevented = false;
    this.target = null; this.currentTarget = null; this.eventPhase = Event.AT_TARGET;
    this.__dispatched = false; delete this.__immediate;
    delete this.__stopPropagation; this.cancelBubble = false; this.returnValue = true;
};
// M78.47: srcElement = target 别名；returnValue=false 等价 preventDefault
// （dispatch 期间与之后都要反映——WPT Event-defaultPrevented 系列）。
Object.defineProperty(Event.prototype, 'srcElement', {
    get: function() { return this.target; },
    enumerable: true, configurable: true
});
Object.defineProperty(Event.prototype, 'returnValue', {
    get: function() { return !this.defaultPrevented; },
    set: function(v) { if (v === false && this.cancelable) this.defaultPrevented = true; },
    enumerable: true, configurable: true
});
globalThis.CustomEvent = function CustomEvent(type, opts) {
    Event.call(this, type, opts);
    this.detail = (opts && opts.detail !== undefined) ? opts.detail : null;
}
CustomEvent.prototype = Object.create(Event.prototype);
Object.defineProperty(CustomEvent.prototype, Symbol.toStringTag, { value: CustomEvent.name || 'CustomEvent' });
function MouseEvent(type, opts) {
    Event.call(this, type, opts);
    opts = opts || {};
    this.clientX = opts.clientX || 0; this.clientY = opts.clientY || 0;
    this.button = (opts.button === undefined) ? 0 : opts.button;
    this.buttons = opts.buttons || 0;
}
MouseEvent.prototype = Object.create(Event.prototype);
Object.defineProperty(MouseEvent.prototype, Symbol.toStringTag, { value: 'MouseEvent' });
window.MouseEvent = MouseEvent;
function KeyboardEvent(type, opts) {
    Event.call(this, type, opts);
    opts = opts || {};
    this.key = opts.key || ''; this.code = opts.code || '';
    this.ctrlKey = !!opts.ctrlKey; this.altKey = !!opts.altKey;
    this.shiftKey = !!opts.shiftKey; this.metaKey = !!opts.metaKey;
}
KeyboardEvent.prototype = Object.create(Event.prototype);
Object.defineProperty(KeyboardEvent.prototype, Symbol.toStringTag, { value: 'KeyboardEvent' });
window.KeyboardEvent = KeyboardEvent;
// M78.29: KeyboardEvent DOM_KEY_LOCATION 常量（构造器与 prototype 双暴露）。
(function() {
    var loc = { DOM_KEY_LOCATION_STANDARD: 0, DOM_KEY_LOCATION_LEFT: 1,
        DOM_KEY_LOCATION_RIGHT: 2, DOM_KEY_LOCATION_NUMPAD: 3 };
    for (var k in loc) {
        window.KeyboardEvent[k] = loc[k];
        window.KeyboardEvent.prototype[k] = loc[k];
    }
})();
// MouseEvent 常量（button 掩码）。
window.MouseEvent.NONE = 0; window.MouseEvent.LEFT = 1; window.MouseEvent.MIDDLE = 2; window.MouseEvent.RIGHT = 3;
function FocusEvent(type, opts) { Event.call(this, type, opts); }
FocusEvent.prototype = Object.create(Event.prototype);
window.FocusEvent = FocusEvent;
// M78.9: Event 家族补全（WPT Event-subclasses 构造器/初始化测试）。
function UIEvent(type, opts) {
    Event.call(this, type, opts);
    opts = opts || {};
    this.view = opts.view || null;
    this.detail = (opts.detail === undefined) ? 0 : opts.detail;
}
UIEvent.prototype = Object.create(Event.prototype);
Object.defineProperty(UIEvent.prototype, Symbol.toStringTag, { value: 'UIEvent' });
UIEvent.prototype.initUIEvent = function(type, bubbles, cancelable, view, detail) { if (arguments.length < 1) throw new TypeError('Argument 1 is required.');
    this.initEvent(type, bubbles, cancelable);
    this.view = view || null; this.detail = (detail === undefined) ? 0 : detail;
};
window.UIEvent = UIEvent;
function WheelEvent(type, opts) {
    MouseEvent.call(this, type, opts);
    opts = opts || {};
    this.deltaX = opts.deltaX || 0; this.deltaY = opts.deltaY || 0; this.deltaZ = opts.deltaZ || 0;
    this.deltaMode = opts.deltaMode || 0;
}
WheelEvent.prototype = Object.create(MouseEvent.prototype);
Object.defineProperty(WheelEvent.prototype, Symbol.toStringTag, { value: 'WheelEvent' });
window.WheelEvent = WheelEvent;
function InputEvent(type, opts) {
    UIEvent.call(this, type, opts);
    opts = opts || {};
    this.data = ('data' in opts) ? opts.data : null;
    this.isComposing = !!opts.isComposing;
    this.inputType = opts.inputType || '';
}
InputEvent.prototype = Object.create(UIEvent.prototype);
Object.defineProperty(InputEvent.prototype, Symbol.toStringTag, { value: 'InputEvent' });
window.InputEvent = InputEvent;
function CompositionEvent(type, opts) {
    UIEvent.call(this, type, opts);
    opts = opts || {};
    this.data = ('data' in opts) ? opts.data : null;
    this.locale = opts.locale || '';
}
CompositionEvent.prototype = Object.create(UIEvent.prototype);
Object.defineProperty(CompositionEvent.prototype, Symbol.toStringTag, { value: 'CompositionEvent' });
CompositionEvent.prototype.initCompositionEvent = function(type, b, c, v, data, locale) { if (arguments.length < 1) throw new TypeError('Argument 1 is required.');
    this.initEvent(type, b, c);
    this.data = data; this.locale = locale || '';
};
window.CompositionEvent = CompositionEvent;
function TextEvent(type, opts) {
    // M78.64: TextEvent 是废弃接口——new 抛 TypeError（WPT 断言）。
    throw new TypeError('Illegal constructor');
}
TextEvent.prototype = Object.create(UIEvent.prototype);
Object.defineProperty(TextEvent.prototype, Symbol.toStringTag, { value: 'TextEvent' });
Object.defineProperty(TextEvent.prototype, 'data', { value: '', writable: true, enumerable: true, configurable: true });
Object.defineProperty(TextEvent.prototype, 'inputMethod', { value: 0, writable: true, enumerable: true, configurable: true });
Object.defineProperty(TextEvent.prototype, 'locale', { value: '', writable: true, enumerable: true, configurable: true });
TextEvent.prototype.initTextEvent = function(type, b, c, v, data, m, locale) {
    // M78.111: 无 type 时 throw TypeError；view 默认 null。
    if (arguments.length < 1) throw new TypeError('Argument 1 is required.');
    this.initEvent(type, b, c);
    this.view = v || null;
    // M78.129: data 缺省 → 字符串 'undefined'（WebIDL DOMString 转换语义，
    // WPT uievents/textInput/api.html 断言 initTextEvent('foo') 后 data === 'undefined'）。
    this.data = (data === undefined) ? 'undefined' : String(data);
    this.locale = locale || '';
};
window.TextEvent = TextEvent;
function PointerEvent(type, opts) {
    MouseEvent.call(this, type, opts);
    opts = opts || {};
    this.pointerId = opts.pointerId || 1;
    this.pointerType = opts.pointerType || '';
    this.isPrimary = !!opts.isPrimary;
    this.pressure = opts.pressure || 0;
}
PointerEvent.prototype = Object.create(MouseEvent.prototype);
Object.defineProperty(PointerEvent.prototype, Symbol.toStringTag, { value: 'PointerEvent' });
window.PointerEvent = PointerEvent;
// MouseEvent 键位修饰 + KeyboardEvent.location + FocusEvent.relatedTarget。
(function upgradeEventFamily() {
    var _ME = MouseEvent;
    function MouseEvent2(type, opts) {
        Event.call(this, type, opts);
        opts = opts || {};
        this.clientX = opts.clientX || 0; this.clientY = opts.clientY || 0;
        this.button = (opts.button === undefined) ? 0 : opts.button;
        this.buttons = opts.buttons || 0;
        this.ctrlKey = !!opts.ctrlKey; this.altKey = !!opts.altKey;
        this.shiftKey = !!opts.shiftKey; this.metaKey = !!opts.metaKey;
        this.relatedTarget = opts.relatedTarget || null;
    }
    MouseEvent2.prototype = Object.create(Event.prototype);
    Object.defineProperty(MouseEvent2.prototype, Symbol.toStringTag, { value: 'MouseEvent' });
    MouseEvent2.prototype.initMouseEvent = function(type, b, c, v, detail, x, y, cx, cy, ctrl, alt, shift, meta, btn, rel) { if (arguments.length < 1) throw new TypeError('Argument 1 is required.');
        this.initEvent(type, b, c);
        this.clientX = x || 0; this.clientY = y || 0; this.detail = detail || 0;
        this.ctrlKey = !!ctrl; this.altKey = !!alt; this.shiftKey = !!shift; this.metaKey = !!meta;
        this.button = btn || 0; this.relatedTarget = rel || null;
    };
    window.MouseEvent = MouseEvent2;
    var _KE = KeyboardEvent;
    function KeyboardEvent2(type, opts) {
        Event.call(this, type, opts);
        opts = opts || {};
        this.key = opts.key || ''; this.code = opts.code || '';
        this.location = (opts.location === undefined) ? 0 : opts.location;
        this.ctrlKey = !!opts.ctrlKey; this.altKey = !!opts.altKey;
        this.shiftKey = !!opts.shiftKey; this.metaKey = !!opts.metaKey;
        this.repeat = !!opts.repeat; this.isComposing = !!opts.isComposing;
        this.charCode = opts.charCode || 0; this.keyCode = opts.keyCode || 0;
    }
    KeyboardEvent2.prototype = Object.create(Event.prototype);
    Object.defineProperty(KeyboardEvent2.prototype, Symbol.toStringTag, { value: 'KeyboardEvent' });
    KeyboardEvent2.prototype.initKeyboardEvent = function(type, b, c, v, key, locale, loc, m, r, cHist) { if (arguments.length < 1) throw new TypeError('Argument 1 is required.');
        this.initEvent(type, b, c); this.key = key || ''; this.location = loc || 0;
    };
    window.KeyboardEvent = KeyboardEvent2;
    var _FE = FocusEvent;
    function FocusEvent2(type, opts) {
        UIEvent.call(this, type, opts);
        opts = opts || {};
        this.relatedTarget = ('relatedTarget' in opts) ? opts.relatedTarget : null;
    }
    FocusEvent2.prototype = Object.create(UIEvent.prototype);
    Object.defineProperty(FocusEvent2.prototype, Symbol.toStringTag, { value: 'FocusEvent' });
    window.FocusEvent = FocusEvent2;
    // M78.65: 构造器 length=1（WPT 断言——正式参数只有 type）。
    try {
        ['UIEvent','WheelEvent','InputEvent','CompositionEvent','TextEvent',
         'PointerEvent','MouseEvent','KeyboardEvent','FocusEvent'].forEach(function(nm) {
            var fn = window[nm];
            if (typeof fn === 'function') {
                Object.defineProperty(fn, 'length', { value: 1, writable: false, configurable: true });
            }
        });
    } catch (e) {}
    // M80.18: 局部绑定同步升级——createEvent/HTMLElement.click 等内部闭包
    // 解析到的是局部原始构造器（无 initMouseEvent/initKeyboardEvent），与
    // 页面可见的 window.MouseEvent 不是同一套类。不重绑则 createEvent 产物
    // 缺 init*，legacy 事件测试在第二段监听器前断裂。
    MouseEvent = MouseEvent2;
    KeyboardEvent = KeyboardEvent2;
    FocusEvent = FocusEvent2;
    var _unused = [_ME, _KE, _FE]; // 保留旧引用防 GC 提示（未被闭包捕获则编译期裁剪）
})();

// XMLHttpRequest（底层走 __fetchSyncMethod——支持任意 method + body + 真实 status）
// M83 重写：旧版 send 忽略 method/body（永远 GET）、setRequestHeader no-op、
// status 硬编码 200——axios（掘金等）POST feed API 全部失效且静默。
// 新语义：POST/PUT/DELETE 带 body + Content-Type；status 用桥返回的真实值
// （axios 2xx resolve / 非 2xx reject 依赖它）；responseType='json' 解析。
// 同步执行真正的网络请求（__fetchSyncMethod 阻塞），异步回调经 setTimeout(1)
// ——事件循环（scripts.rs QuickJS el loop）会 drain。
var __xhrSeq = 0;
function XMLHttpRequest() {
    __xhrSeq++;
    this.__id = __xhrSeq;
    this.__async = true;
    this.readyState = 0;
    this.status = 0;
    this.statusText = '';
    this.responseText = '';
    this.response = '';
    this.responseType = '';
    this.__listeners = {};
    this.__headers = {};
}
XMLHttpRequest.prototype.open = function(method, url, async) {
    this.__url = url;
    this.__method = (method || 'GET').toUpperCase();
    this.__async = (async !== false);
    this.readyState = 1;
};
XMLHttpRequest.prototype.setRequestHeader = function(key, val) {
    this.__headers[key] = val;
};
XMLHttpRequest.prototype.send = function(body) {
    var self = this;
    var url = this.__url;
    var method = this.__method || 'GET';
    var sync = (this.__async === false);
    function doSend() {
        var raw = null;
        var status = 0;
        if (typeof __fetchSyncMethod === 'function') {
            var ct = self.__headers['Content-Type'] || self.__headers['content-type'] || null;
            var bodyStr = (body === undefined || body === null) ? null : String(body);
            var r = __fetchSyncMethod(url, method, bodyStr, ct);
            if (typeof r === 'string' && r.length > 0) {
                var nl = r.indexOf('\n');
                if (nl > 0) {
                    status = parseInt(r.substring(0, nl), 10) || 0;
                    raw = r.substring(nl + 1);
                } else {
                    raw = r;
                    status = 200;
                }
            }
        } else if (method === 'GET' && typeof __fetchSync === 'function') {
            // 桥降级：只有 GET 同步 fetch 可用
            raw = __fetchSync(url);
            status = (raw !== null && raw !== undefined) ? 200 : 0;
        }
        if (typeof __log === 'function') {
            __log('[xhr] ' + method + ' ' + url + ' → ' + status + ' (' + (raw ? raw.length : 0) + ' bytes)');
        }
        if (raw !== null && raw !== undefined) {
            self.responseText = raw;
            self.response = (self.responseType === 'json')
                ? (function() { try { return JSON.parse(raw); } catch (e) { return null; } })()
                : raw;
        }
        self.status = status;
        self.statusText = (status >= 200 && status < 300) ? 'OK' : String(status);
        self.readyState = 4;
        if (typeof self.onreadystatechange === 'function') {
            try { self.onreadystatechange.call(self); } catch(e) {
                if (typeof __log === 'function') __log('[xhr] rsc threw: ' + e.message);
            }
        }
        var ev = new Event('load');
        ev.target = self;
        ev.currentTarget = self;
        if (self.__listeners && self.__listeners['load']) {
            for (var i = 0; i < self.__listeners['load'].length; i++) {
                try { self.__listeners['load'][i].call(self, ev); } catch(e) {}
            }
        }
        if (typeof self.onload === 'function') {
            try { self.onload.call(self, ev); } catch(e) {}
        }
    }
    if (sync) { doSend(); } else { setTimeout(doSend, 1); }
};
XMLHttpRequest.prototype.abort = function() {};
XMLHttpRequest.prototype.getResponseHeader = function(name) { return null; };
XMLHttpRequest.prototype.getAllResponseHeaders = function() { return ''; };
XMLHttpRequest.prototype.addEventListener = function(type, cb) {
    if (!this.__listeners[type]) this.__listeners[type] = [];
    this.__listeners[type].push(cb);
};
XMLHttpRequest.prototype.removeEventListener = function(type, cb) {};
XMLHttpRequest.prototype.getResponseHeader = function(name) { return null; };
XMLHttpRequest.prototype.getResponseText = function() { return this.responseText; };
XMLHttpRequest.prototype.overrideMimeType = function(mime) {};
XMLHttpRequest.prototype.upload = {};
XMLHttpRequest.prototype.withCredentials = false;

// WebSocket（复用 boa 的后台线程 WsManager）
var __wsInstances = {};
function WebSocket(url, protocols) {
    if (!url) throw new TypeError('Failed to construct "WebSocket": 1 argument required');
    this.__wsId = (typeof __wsCreate === 'function') ? __wsCreate(url) : -1;
    this.url = url;
    this.readyState = 0; // CONNECTING
    this.bufferedAmount = 0;
    this.extensions = '';
    this.protocol = protocols || '';
    this.binaryType = 'blob';
    this.__listeners = {};
    if (this.__wsId >= 0) {
        __wsInstances[this.__wsId] = this;
    }
}
WebSocket.CONNECTING = 0;
WebSocket.OPEN = 1;
WebSocket.CLOSING = 2;
WebSocket.CLOSED = 3;
WebSocket.prototype.send = function(data) {
    if (this.__wsId >= 0 && typeof __wsSend === 'function') {
        __wsSend(this.__wsId, String(data));
    }
};
WebSocket.prototype.close = function() {
    if (this.__wsId >= 0 && typeof __wsClose === 'function') {
        __wsClose(this.__wsId);
    }
};
WebSocket.prototype.addEventListener = function(type, cb) {
    if (!this.__listeners[type]) this.__listeners[type] = [];
    this.__listeners[type].push(cb);
};
// 后台 WS 事件分派器（Rust event loop 每轮 eval 调用）
window.__wsDispatchEvent = function(id, type, data) {
    var ws = __wsInstances[id];
    if (!ws) return;
    var ev = { type: type, data: data, target: ws, currentTarget: ws };
    if (type === 'open') {
        ws.readyState = 1; // OPEN
        if (typeof ws.onopen === 'function') {
            try { ws.onopen.call(ws, ev); } catch(e) {}
        }
    } else if (type === 'message') {
        if (typeof ws.onmessage === 'function') {
            try { ws.onmessage.call(ws, ev); } catch(e) {}
        }
    } else if (type === 'close') {
        ws.readyState = 3; // CLOSED
        if (typeof ws.onclose === 'function') {
            try { ws.onclose.call(ws, ev); } catch(e) {}
        }
        delete __wsInstances[id];
    } else if (type === 'error') {
        ws.readyState = 3; // CLOSED on error
        if (typeof ws.onerror === 'function') {
            try { ws.onerror.call(ws, ev); } catch(e) {}
        }
        delete __wsInstances[id];
    }
};
window.WebSocket = WebSocket;

// fetch（Promise-based，内部同步 fetch）
// M78.13: Response/Request 全局构造器（WPT fetch/response-form-data 等 14
// 子测试依赖 window.Response 存在 + instanceof 语义）。
function Response(body, init) {
    init = init || {};
    this.type = 'default';
    this.url = '';
    this.redirected = false;
    this.status = (init.status === undefined) ? 200 : init.status;
    this.statusText = init.statusText || '';
    this.ok = this.status >= 200 && this.status < 300;
    this.bodyUsed = false;
    // M78.19: 构造 headers 近似对象——init.headers 的 Content-Type 供
    // formData() 取 boundary（普通对象/Headers-like 都读）。
    var __hdrMap = {};
    var ih = init.headers;
    if (ih) {
        if (Array.isArray(ih)) {
            for (var ai = 0; ai < ih.length; ai++) {
                if (Array.isArray(ih[ai]) && ih[ai].length >= 2) {
                    __hdrMap[String(ih[ai][0]).toLowerCase()] = String(ih[ai][1]);
                }
            }
        } else if (typeof ih.forEach === 'function') {
            try { ih.forEach(function(v, k) { __hdrMap[String(k).toLowerCase()] = String(v); }); } catch (e) {}
        } else {
            for (var hk in ih) { if (ih.hasOwnProperty(hk)) __hdrMap[String(hk).toLowerCase()] = String(ih[hk]); }
        }
    }
    var self2 = this;
    this.headers = {
        get: function(k) { return __hdrMap[String(k).toLowerCase()] !== undefined ? __hdrMap[String(k).toLowerCase()] : null; },
        has: function(k) { return __hdrMap[String(k).toLowerCase()] !== undefined; },
        forEach: function(fn) { for (var k in __hdrMap) fn(__hdrMap[k], k); }
    };
    this.__multipartBoundary = __hdrMap['content-type'] || '';
    // M78.129: FormData body → multipart 序列化（WPT fetch response-form-data
    // "Empty form data"：new Response(new FormData).text() 必须以 '--' 开头、
    // 含 close delimiter，可回读解析出空 FormData）。
    if (body && body.__pairs) {
        var __fb = '----FormBoundary' + Date.now().toString(36) + Math.random().toString(36).slice(2, 10);
        var __fparts = [];
        for (var __fi = 0; __fi < body.__pairs.length; __fi++) {
            __fparts.push(__fb + '\r\n' + 'Content-Disposition: form-data; name="' + body.__pairs[__fi][0] + '"\r\n\r\n' + body.__pairs[__fi][1] + '\r\n');
        }
        this.__body = __fparts.join('') + __fb + '--\r\n';
        if (!__hdrMap['content-type']) __hdrMap['content-type'] = 'multipart/form-data; boundary=' + __fb.slice(2);
    } else {
        this.__body = (body === undefined || body === null) ? '' : String(body);
    }
}
Object.defineProperty(Response.prototype, Symbol.toStringTag, { value: 'Response' });
Response.prototype.text = function() { this.bodyUsed = true; return Promise.resolve(this.__body); };
Response.prototype.json = function() { this.bodyUsed = true; return Promise.resolve(JSON.parse(this.__body)); };
Response.prototype.clone = function() { return new Response(this.__body, { status: this.status, statusText: this.statusText }); };
Response.prototype.arrayBuffer = function() { return Promise.resolve(new ArrayBuffer(0)); };
Response.prototype.blob = function() { return Promise.resolve({}); };
Response.prototype.formData = function() {
    // M78.68: FormData 类（此前完全缺失——Response.formData 的依赖）。
function FormData() {
    this.__pairs = [];
}
Object.defineProperty(FormData.prototype, Symbol.toStringTag, { value: 'FormData' });
FormData.prototype.append = function(k, v) {
    this.__pairs.push([String(k), String(v)]);
};
FormData.prototype.get = function(k) {
    for (var i = 0; i < this.__pairs.length; i++) if (this.__pairs[i][0] === String(k)) return this.__pairs[i][1];
    return null;
};
FormData.prototype.getAll = function(k) {
    var out = [];
    for (var i = 0; i < this.__pairs.length; i++) if (this.__pairs[i][0] === String(k)) out.push(this.__pairs[i][1]);
    return out;
};
FormData.prototype.has = function(k) { return this.get(k) !== null; };
FormData.prototype.set = function(k, v) {
    for (var i = 0; i < this.__pairs.length; i++) {
        if (this.__pairs[i][0] === String(k)) { this.__pairs[i][1] = String(v); return; }
    }
    this.append(k, v);
};
FormData.prototype.delete = function(k) {
    for (var i = this.__pairs.length - 1; i >= 0; i--) {
        if (this.__pairs[i][0] === String(k)) this.__pairs.splice(i, 1);
    }
};
FormData.prototype.forEach = function(fn, thisArg) {
    for (var i = 0; i < this.__pairs.length; i++) fn.call(thisArg, this.__pairs[i][1], this.__pairs[i][0], this);
};
FormData.prototype.entries = function() {
    var idx = 0; var pairs = this.__pairs;
    var iter = { next: function() { return idx < pairs.length ? { value: pairs[idx++], done: false } : { value: undefined, done: true }; } };
    iter[Symbol.iterator] = function() { return iter; };
    return iter;
};
FormData.prototype.keys = function() {
    var idx = 0; var pairs = this.__pairs;
    var iter = { next: function() { return idx < pairs.length ? { value: pairs[idx++][0], done: false } : { value: undefined, done: true }; } };
    iter[Symbol.iterator] = function() { return iter; };
    return iter;
};
FormData.prototype.values = function() {
    var idx = 0; var pairs = this.__pairs;
    var iter = { next: function() { return idx < pairs.length ? { value: pairs[idx++][1], done: false } : { value: undefined, done: true }; } };
    iter[Symbol.iterator] = function() { return iter; };
    return iter;
};
FormData.prototype[Symbol.iterator] = FormData.prototype.entries;
window.FormData = FormData;
// M78.111: execCommand stub（textInput 测试的依赖）。
document.execCommand = function(cmd, ui, value) { return false; };
window.find = window.find || function() { return false; };
// M78.129: multipart 严格状态机解析（对齐 fetch 规范——畸形 body 必须 reject）：
// body 必须以 dash-boundary 开头；delimiter 后只允许 transport padding（空格/Tab）
// + CRLF（开 part）或 '--'（关 delimiter）；每个 part 必须有带 name 的
// Content-Disposition；关闭 delimiter 后只允许 padding + 单个 CRLF + EOF。
    var self = this;
    return Promise.resolve().then(function() {
        var ctype = (self.headers && self.headers.get) ? (self.headers.get('content-type') || '') : (self.__multipartBoundary || '');
        if (ctype.indexOf('multipart/form-data') < 0) {
            throw new TypeError('FormData: not multipart/form-data');
        }
        var bm = /boundary=([^;\s]+)/i.exec(ctype);
        if (!bm) throw new TypeError('FormData: missing boundary');
        var bstr = '--' + bm[1];
        var CRLF = String.fromCharCode(13, 10);
        var body = self.__body || '';
        if (body.indexOf(bstr) !== 0) throw new TypeError('FormData: missing opening boundary');
        var pos = bstr.length;
        var fd = new FormData();
        while (true) {
            var pad = pos;
            while (pad < body.length && (body.charAt(pad) === ' ' || body.charAt(pad) === '\t')) pad++;
            if (body.indexOf(CRLF, pad) === pad) {
                // 开 part delimiter → 解析 headers + content
                pos = pad + 2;
                var cd = null;
                while (true) {
                    if (body.indexOf(CRLF, pos) === pos) { pos += 2; break; }
                    var le = body.indexOf(CRLF, pos);
                    if (le < 0) throw new TypeError('FormData: unterminated part headers');
                    var hline = body.slice(pos, le);
                    if (hline.indexOf(String.fromCharCode(13)) >= 0 || hline.indexOf(String.fromCharCode(10)) >= 0) {
                        throw new TypeError('FormData: bare CR or LF in part headers');
                    }
                    var hci = hline.indexOf(':');
                    if (hci > 0 && hline.slice(0, hci).trim().toLowerCase() === 'content-disposition') {
                        cd = hline.slice(hci + 1).trim();
                    }
                    pos = le + 2;
                }
                if (cd === null) throw new TypeError('FormData: part missing Content-Disposition');
                var nm = /name="([^"]*)"/i.exec(cd);
                if (!nm) throw new TypeError('FormData: missing name in Content-Disposition');
                var dpos = body.indexOf(CRLF + bstr, pos);
                if (dpos < 0) throw new TypeError('FormData: unterminated part');
                var value = body.slice(pos, dpos);
                // content 里 bare CR / LF（不成对 CRLF）→ 非法（保留 M78.106 语义）
                for (var vi = 0; vi < value.length; vi++) {
                    var vch = value.charAt(vi);
                    if (vch === String.fromCharCode(13) && value.charAt(vi + 1) !== String.fromCharCode(10)) {
                        throw new TypeError('FormData: bare CR in part data');
                    }
                    if (vch === String.fromCharCode(10) && value.charAt(vi - 1) !== String.fromCharCode(13)) {
                        throw new TypeError('FormData: bare LF in part data');
                    }
                }
                fd.append(nm[1], value);
                pos = dpos + 2 + bstr.length;
            } else if (body.indexOf('--', pad) === pad) {
                // 关闭 delimiter：padding 后可选单个 CRLF，之后必须 EOF
                var q = pad + 2;
                while (q < body.length && (body.charAt(q) === ' ' || body.charAt(q) === '\t')) q++;
                if (body.indexOf(CRLF, q) === q) q += 2;
                if (q !== body.length) throw new TypeError('FormData: junk after closing boundary');
                return fd;
            } else {
                throw new TypeError('FormData: malformed boundary delimiter');
            }
        }
    });
};
Response.error = function() { return new Response('', { status: 0 }); };
Response.redirect = function(url, status) { var r = new Response('', { status: status || 302 }); r.url = url; r.redirected = true; return r; };
window.Response = Response;
function Request(input, init) {
    init = init || {};
    var url = (typeof input === 'string') ? input : (input && input.url) || String(input);
    // M78.66: URL normalize + query encode.
    if (typeof URL === 'function' && location && location.href) {
        try { url = new URL(url, location.href).href; } catch (e) {}
    }
    this.url = url;
    this.method = (init.method || (input && input.method) || 'GET').toUpperCase();
    this.headers = init.headers || {};
    this.body = init.body || null;
    this.credentials = init.credentials || 'same-origin';
    this.mode = init.mode || 'cors';
    this.redirect = init.redirect || 'follow';
}
Object.defineProperty(Request.prototype, Symbol.toStringTag, { value: 'Request' });
Request.prototype.clone = function() { return new Request(this.url, { method: this.method, headers: this.headers, body: this.body }); };
window.Request = Request;

window.fetch = function(input, options) {
    var url = (typeof input === 'string') ? input : (input && input.url) || String(input);
    // M78.66: URL normalize + query encode for fetch.
    if (typeof URL === 'function' && location && location.href) {
        try { url = new URL(url, location.href).href; } catch (e) {}
    }
    options = options || {};
    var method = options.method || 'GET';
    var body = options.body || null;
    // M93.11: fetch headers 全量透传（spec）——平面对象/Headers 实例均可，
    // 序列化成 JSON 交给 bridge；Content-Type 单独抽取保持旧路径。
    var hdrJson = null;
    try {
        var hObj = options.headers || null;
        var flat = {};
        if (hObj) {
            if (typeof hObj.forEach === 'function' && typeof hObj.has === 'function') {
                hObj.forEach(function(v, k) { flat[k] = v; });
            } else if (typeof hObj === 'object') {
                for (var hk in hObj) { if (Object.prototype.hasOwnProperty.call(hObj, hk)) flat[hk] = hObj[hk]; }
            }
        }
        var keys = Object.keys(flat);
        if (keys.length > 0) { hdrJson = JSON.stringify(flat); }
    } catch (eHdr) {}
    var ct = options.headers ? (options.headers['Content-Type'] || options.headers['content-type'] || (hdrJson ? undefined : null)) : null;
    if (ct === undefined) {
        try { ct = (options.headers && options.headers.get && options.headers.get('Content-Type')) || null; } catch (eCt) { ct = null; }
    }
    return new Promise(function(resolve, reject) {
        var raw = null;
        var statusCode = 200;
        // POST/PUT/DELETE → __fetchSyncMethod（支持 method/body，返回 "{status}\n{body}"）
        // GET → __fetchSync（现有同步 fetch）
        if (method !== 'GET' && typeof __fetchSyncMethod === 'function') {
            raw = __fetchSyncMethod(url, method, body, ct, hdrJson);
            if (raw && raw.indexOf('\n') > 0) {
                statusCode = parseInt(raw.split('\n')[0], 10);
                raw = raw.slice(raw.indexOf('\n') + 1);
            }
        } else if (typeof __fetchSync === 'function') {
            raw = __fetchSync(url);
        }
        if (raw === null || raw === undefined) {
            reject(new TypeError('Failed to fetch ' + url));
        } else {
            // M93.5: 静态资产形状的 GET 成功响应写 SCRIPT_CACHE——同 URL 的
            // Worker 源码加载（worker_run → fetch_sync 读 SCRIPT_CACHE）直接
            // 命中缓存，去重 Anubis main.mjs 预取 sha256.mjs + Worker 再取的
            // 双倍请求（限流站点双倍配额）。形状判断在 Rust 侧 __cacheAsset
            // 内：仅 js/mjs/css/版本参数 URL；API JSON/HTML 不进缓存（M80 纪律）。
            if (method === 'GET' && typeof __cacheAsset === 'function') {
                __cacheAsset(url, raw);
            }
            var resp = new Response(raw, { status: statusCode, statusText: statusCode === 200 ? 'OK' : String(statusCode) });
            resp.url = url;
            resolve(resp);
        }
    });
};

// M78.127: WebIDL 接口对象属性语义——{writable, enumerable: false, configurable: true}。
// 顶层 function 声明是 enumerable + non-configurable（delete 返回 false），
// WPT dom/interface-objects.html 要求 for-in 不可见且可 delete。
(function() {
    var ifaces = ['Event', 'CustomEvent', 'EventTarget', 'AbortController', 'AbortSignal',
        'Node', 'Document', 'DOMImplementation', 'DocumentFragment', 'ProcessingInstruction',
        'DocumentType', 'Element', 'Attr', 'CharacterData', 'Text', 'Comment',
        'NodeIterator', 'TreeWalker', 'NodeFilter', 'NodeList', 'HTMLCollection', 'DOMTokenList',
        'UIEvent', 'MouseEvent', 'KeyboardEvent', 'FocusEvent', 'WheelEvent', 'InputEvent',
        'MutationObserver', 'NamedNodeMap', 'DOMStringMap', 'Range', 'Selection',
        'XMLHttpRequest', 'FormData', 'Headers', 'Request', 'Response', 'FetchController',
        'StorageEvent', 'PopStateEvent', 'HashChangeEvent', 'ProgressEvent', 'ErrorEvent',
        'HTMLElement', 'Image', 'Option', 'WebSocket', 'MessageChannel', 'MessagePort',
        'TextEvent', 'File', 'Blob', 'URL', 'URLSearchParams', 'DOMParser',
        'Plugin', 'PluginArray', 'MimeType', 'MimeTypeArray'];
    for (var i = 0; i < ifaces.length; i++) {
        var n = ifaces[i], v;
        try { v = window[n]; } catch (e) { continue; }
        if (typeof v === 'function') {
            try {
                Object.defineProperty(window, n, {
                    value: v, writable: true, enumerable: false, configurable: true
                });
            } catch (e) {}
        }
    }
})();

undefined;
"#;

/// M93: Web Worker shim（爬虫够用子集）。
///
/// 真实 Worker 是独立线程 + 结构化克隆消息。爬虫场景实现为**同步子
/// Context**：`postMessage(msg)` 调用即触发 `__workerRun(url, json)`——
/// Rust 侧创建独立 QuickJS Runtime 执行 worker 源码、分发消息、drain
/// microtask，返回 outbox（JSON 文本数组）；本 shim 把每条消息
/// `JSON.parse` 后同步投递给 `onmessage({data})`。
///
/// 覆盖的真实场景：Anubis PoW（fast 算法在 worker 里跑纯 JS sha256，
/// difficulty=4 ≈ 6.5 万次哈希，解完 postMessage 结果回主线程 →
/// location.replace(pass-challenge)）。主线程侧只用到 `new Worker(url)` +
/// `onmessage` 属性 + `postMessage` + `terminate`（Anubis main.mjs 的
/// 消费方式），addEventListener 形式也一并提供。
///
/// 限制（爬虫够用原则）：无并行性（同步阻塞计算）、消息是 JSON 近似而非
/// 结构化克隆、无 importScripts/SharedWorker/ServiceWorker。
#[cfg(feature = "quickjs")]
const QUICKJS_WORKER_SHIM: &str = r#"
(function() {
    function Worker(url) {
        this.__src = String(url);
        this.__srcCode = null;
        // M93.7: blob: URL ——从 M81.6 的 __blobUrls 注册表解析源码
        //（cap.js 的 Worker 就是 `new Worker(URL.createObjectURL(new Blob([...])))`）。
        if (this.__src.indexOf('blob:') === 0 && typeof __blobUrls !== 'undefined') {
            var cached = __blobUrls[this.__src];
            if (typeof cached === 'string' && cached.length > 0) this.__srcCode = cached;
        }
        this.onmessage = null;
        this.onerror = null;
        this.onmessageerror = null;
        this.__terminated = false;
    }
    Worker.prototype.postMessage = function(msg) {
        if (this.__terminated) return;
        var runner = (this.__srcCode !== null && typeof __workerRunSrc === 'function')
            ? null : ((typeof __workerRun === 'function') ? __workerRun : null);
        if (runner === null && this.__srcCode === null) {
            if (this.onerror) { try { this.onerror({ message: 'Worker bridge unavailable' }); } catch (e) {} }
            return;
        }
        var payload;
        try { payload = JSON.stringify(msg); } catch (e1) { payload = 'null'; }
        var raw;
        try {
            raw = (this.__srcCode !== null && typeof __workerRunSrc === 'function')
                ? __workerRunSrc(this.__srcCode, payload)
                : __workerRun(this.__src, payload);
        } catch (e2) {
            if (this.onerror) { try { this.onerror({ message: String((e2 && e2.message) || e2) }); } catch (e3) {} }
            return;
        }
        var res;
        try { res = JSON.parse(raw); } catch (e4) {
            if (this.onerror) { try { this.onerror({ message: 'worker result parse error' }); } catch (e5) {} }
            return;
        }
        if (!res || res.ok !== true) {
            if (this.onerror) { try { this.onerror({ message: (res && res.error) || 'worker failed' }); } catch (e6) {} }
            return;
        }
        if (typeof this.onmessage !== 'function') return;
        var msgs = res.messages || [];
        for (var i = 0; i < msgs.length; i++) {
            var d;
            try { d = JSON.parse(msgs[i]); } catch (e7) { d = msgs[i]; }
            try { this.onmessage({ data: d }); }
            catch (e8) {
                if (this.onerror) { try { this.onerror({ message: String((e8 && e8.message) || e8) }); } catch (e9) {} }
            }
        }
    };
    Worker.prototype.terminate = function() { this.__terminated = true; };
    Worker.prototype.addEventListener = function(type, fn) {
        if (type === 'message') { this.onmessage = fn; }
        else if (type === 'error') { this.onerror = fn; }
    };
    Worker.prototype.removeEventListener = function(type) {
        if (type === 'message') { this.onmessage = null; }
        else if (type === 'error') { this.onerror = null; }
    };
    globalThis.Worker = Worker;
})();
undefined;
"#;

/// safety cap is hit. Returns the number of callbacks invoked.
/// M16.4: 每轮 tick 先 `ctx.run_jobs()`（执行 Promise then 回调 microtask），
/// 再 drain 到期 timer。两者交叉驱动，直到都 idle。
/// `ctx.eval` 不返回值我们也不关心（回调的副作用在 DOM 上，不在返回值）。
#[cfg(feature = "boa")]
fn pump_event_loop(ctx: &mut Context) -> usize {
    const MAX_TICKS: usize = 1000;
    // M70.13: 硬超时从 8s 降到 3s——爬虫不需要等 analytics timer。
    const MAX_TOTAL: std::time::Duration = std::time::Duration::from_secs(3);
    const MAX_SLEEP_MS: u64 = 50;
    // M65: networkidle 检测——连续 IDLE_ROUNDS 轮无任何事件（timer/WS/Promise）
    // 就提前退出。大多数 SPA 在 DOMContentLoaded 后 1-2 秒就稳定了，
    // 不必等满 3 秒 hard timeout。Puppeteer 的 networkidle0/2 也是类似策略。
    const IDLE_ROUNDS: u32 = 3;
    // M65: idle 检测的宽限期——允许页面初始的 setTimeout 链跑完再开始计数。
    // M70.13: 从 800ms 降到 300ms。
    const IDLE_GRACE: std::time::Duration = std::time::Duration::from_millis(300);
    let started_at = std::time::Instant::now();
    let mut invoked = 0;
    // M23.5: WS 是长连接异步，握手/收消息在后台线程。即使 timer idle，
    // 也要 poll WS 事件直到所有连接关闭（否则 onopen/onmessage 永不触发）。
    let mut ws_idle_polls = 0u32;
    const WS_MAX_IDLE_POLLS: u32 = 500; // ~2.5s（500 × 5ms）安全裕度
    let mut idle_rounds: u32 = 0;
    for _ in 0..MAX_TICKS {
        if started_at.elapsed() >= MAX_TOTAL {
            eprintln!(
                "[js-runtime] event loop hard timeout ({}s)",
                MAX_TOTAL.as_secs()
            );
            break;
        }
        // M82: 全局 deadline 到 → 事件循环立即退出。
        if crate::bridge::js_deadline_exceeded() {
            eprintln!("[js-runtime] global JS deadline exceeded — stop event loop");
            break;
        }
        // M16.4: 先执行 Promise microtask（then 回调）。可能 schedule 新 timer。
        let _ = ctx.run_jobs(); // 0.21: 返回 JsResult，drain microtask 失败忽略
        let mut tick_invoked = 0;
        // M69: drain 动态 script（appendChild(script) 入队的 chunk）。
        // eval 出的代码可能又入队新 script（webpack 链式加载），下一轮 tick 处理。
        // 必须在 timer 之前 eval——chunk 里 schedule 的 onload/setTimeout 才能进队列。
        for code in crate::bridge::drain_dynamic_scripts() {
            if has_ts_syntax(&code) {
                continue;
            }
            if let Err(e) = ctx.eval(Source::from_bytes(&code)) {
                eprintln!("[js] [boa dynamic] {e}");
            }
            invoked += 1;
            tick_invoked += 1;
        }
        // Timer 回调（setTimeout）。
        let due = crate::bridge::drain_due_timer_callbacks();
        for callback in due {
            // 调用 setTimeout 回调：this = undefined，无参。
            // 回调内部如果 schedule 新 timer 或修改 DOM，会在下一轮 tick 处理。
            if let Err(e) = callback.call(&JsValue::undefined(), &[], ctx) {
                eprintln!("[js-runtime] timer callback error: {e:?}");
            }
            invoked += 1;
            tick_invoked += 1;
        }
        // M23.5: WebSocket 事件（Open/Text/Binary/Closed/Error）。
        let ws_events = crate::bridge::drain_ws_events();
        for (id, etype, data) in ws_events {
            let escaped = escape_js_ws_data(&data);
            let js = format!("__wsDispatchEvent({id}, '{etype}', '{escaped}')");
            if let Err(e) = ctx.eval(Source::from_bytes(&js)) {
                eprintln!("[js-runtime] ws dispatch error: {e}");
            }
            invoked += 1;
            tick_invoked += 1;
        }
        if tick_invoked > 0 {
            // 有事件触发（可能 schedule 新操作），重置 idle 计数继续。
            ws_idle_polls = 0;
            idle_rounds = 0;
            continue;
        }
        // M37: 没事件。但可能有 pending timer 尚未到期。
        // 此时必须 sleep 到最近 deadline 再 drain，否则永远 break
        // 而不会触发 setTimeout 回调（之前的 bug：直接 break）。
        if let Some(deadline) = crate::bridge::next_timer_deadline() {
            let now = std::time::Instant::now();
            if deadline > now {
                let wait = deadline - now;
                // 限制单次等待与总等待，避免页面通过超长 timer 长时间卡住。
                let mut wait = wait.min(std::time::Duration::from_millis(MAX_SLEEP_MS));
                let left = MAX_TOTAL.saturating_sub(started_at.elapsed());
                wait = wait.min(left);
                if wait > std::time::Duration::ZERO {
                    std::thread::sleep(wait);
                }
            }
            ws_idle_polls = 0;
            // M65: networkidle 检测。过了宽限期后，连续 IDLE_ROUNDS 轮只有
            // pending timer 但无到期 timer 也无新事件 → 页面已稳定，提前退出。
            // pending timer 的 setTimeout 回调大多是 UI 动画/轮询，爬虫不需要。
            if started_at.elapsed() >= IDLE_GRACE {
                idle_rounds += 1;
                if idle_rounds >= IDLE_ROUNDS {
                    break;
                }
            }
            continue; // sleep 后回到循环顶部 drain 到期 timer
        }
        // 没有 pending timer。判断是否还有活跃 WS 连接。
        if crate::bridge::ws_connection_count() == 0 {
            break; // timer + WS 都 idle，结束
        }
        // 有活跃 WS 但暂无事件：短暂 sleep 让后台线程收消息，再 poll。
        if ws_idle_polls >= WS_MAX_IDLE_POLLS {
            eprintln!("[js-runtime] WS idle poll 超时（{WS_MAX_IDLE_POLLS} 轮），连接可能未关闭");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        ws_idle_polls += 1;
    }
    invoked
}

/// M23.5: 转义 WS 消息载荷为安全的 JS 字符串字面量（单引号包裹）。
/// 处理反斜杠/单引号/换行/回车/制表符，避免 eval 注入或语法错误。
#[allow(dead_code)]
fn escape_js_ws_data(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// Convenience: install bridge + execute scripts in one call.
/// Returns (executed_count, SharedTree). The caller can drop the
/// `Rc<RefCell<Tree>>` clones when done rendering.
#[must_use]
pub fn run_scripts(tree: Tree) -> (crate::bridge::SharedTree, usize) {
    run_scripts_with_base(tree, None)
}

/// Same as [`run_scripts`], but installs a base URL for relative-URL
/// resolution in `__fetch*` calls.
#[must_use]
pub fn run_scripts_with_base(
    tree: Tree,
    base_url: Option<String>,
) -> (crate::bridge::SharedTree, usize) {
    // M71.1: 无 boa feature 时回退到 QuickJS（默认引擎）。
    #[cfg(feature = "boa")]
    {
        run_scripts_with_base_engine(tree, base_url, &crate::engine::EngineKind::Boa)
    }
    #[cfg(not(feature = "boa"))]
    {
        run_scripts_with_base_engine(tree, base_url, &crate::engine::EngineKind::QuickJs)
    }
}

/// M66: 引擎可切换版本。通过 EngineKind 选择 JS 引擎（boa / quickjs）。
#[must_use]
pub fn run_scripts_with_base_engine(
    tree: Tree,
    base_url: Option<String>,
    engine_kind: &crate::engine::EngineKind,
) -> (crate::bridge::SharedTree, usize) {
    run_scripts_with_post_exprs(tree, base_url, engine_kind, &[])
}

/// M81: [`run_scripts_with_base_engine`] 的 `--click`/`--hover` 扩展版——
/// 页面脚本 + 事件循环跑完后，在**同一引擎会话**内按序 eval `post_exprs`
/// （合成点击/悬停）。addEventListener 监听器注册在会话内 `__elCache`
/// 缓存的元素包装上，引擎 drop 即失效——合成事件必须留在本会话，不能
/// 事后开新引擎补 eval。当前仅 QuickJS 实现；boa 引擎忽略（告警）。
#[must_use]
pub fn run_scripts_with_post_exprs(
    tree: Tree,
    base_url: Option<String>,
    engine_kind: &crate::engine::EngineKind,
    post_exprs: &[String],
) -> (crate::bridge::SharedTree, usize) {
    use std::cell::RefCell;
    use std::rc::Rc;
    let shared: crate::bridge::SharedTree = Rc::new(RefCell::new(tree));

    // M66-B/M93: QuickJS 走独立执行路径 + 文档导航循环。引擎创建移入循环
    // 内部（每页一个新引擎——导航 = 全新 JS 全局空间），不在此预建。
    #[cfg(feature = "quickjs")]
    if matches!(engine_kind, crate::engine::EngineKind::QuickJs) {
        return run_scripts_quickjs(shared, base_url, engine_kind, post_exprs);
    }
    #[cfg(not(feature = "quickjs"))]
    if matches!(engine_kind, crate::engine::EngineKind::QuickJs) {
        eprintln!("[js-runtime] QuickJS requested but feature not enabled, using boa");
    }

    // M71.1: boa 路径仅在 --features boa 时编译。无 boa 时不可能走到这里
    //（QuickJS 分支已 return，或 EngineKind 只有 QuickJs）。
    #[cfg(feature = "boa")]
    {
        // M64: 预扫描是否有 ESM module 脚本。如果有，用 HttpModuleLoader 创建 Context。
        let has_module = {
            let borrowed = shared.borrow();
            extract_script_entries(&borrowed).iter().any(|e| {
                matches!(
                    e,
                    ScriptEntry::ExternalModule(_) | ScriptEntry::InlineModule(_)
                )
            })
        };
        let origin = base_url
            .as_deref()
            .map(url_origin)
            .unwrap_or_else(|| "about:blank".to_string());
        let esm_origin = if has_module {
            Some(origin.as_str())
        } else {
            None
        };
        let mut engine = engine_kind.create(esm_origin);
        let engine_name = engine.name();
        if !post_exprs.is_empty() {
            eprintln!(
                "[js-runtime] --click/--hover post evals not supported on boa engine; ignored"
            );
        }
        #[allow(clippy::needless_return)]
        return run_scripts_with_base_boa(shared, base_url, engine, &engine_name);
    }
    #[cfg(not(feature = "boa"))]
    {
        let _ = engine_kind;
        let _ = post_exprs;
        // 不可能到达：engine_kind 只能是 QuickJs，上面已 return。
        (shared, 0)
    }
}

/// M71.1: boa 路径（从 run_scripts_with_base_engine 抽出）。cfg 门控。
#[cfg(feature = "boa")]
fn run_scripts_with_base_boa(
    shared: crate::bridge::SharedTree,
    base_url: Option<String>,
    mut engine: Box<dyn crate::engine::JsEngine>,
    engine_name: &str,
) -> (crate::bridge::SharedTree, usize) {
    // M66: 获取底层 boa Context（所有 bridge/shim 仍直接操作 Context）。
    let ctx = engine.ctx_mut();
    let trace_scripts = std::env::var("BROWSER_TRACE_SCRIPTS").is_ok();
    {
        let limits = ctx.runtime_limits_mut();
        limits.set_loop_iteration_limit(JS_LOOP_ITERATION_LIMIT);
        limits.set_stack_size_limit(JS_STACK_SIZE_LIMIT);
        limits.set_recursion_limit(JS_RECURSION_LIMIT);
    }
    install(ctx);
    // M13.3: 安装 localStorage / sessionStorage 后端 + JS 对象
    let storage = browser_storage::new_storage();
    crate::bridge::install_storage(storage);
    let _ = crate::storage_shim::install_storage_globals(ctx);
    // M14.3: 安装 history / location 后端 + JS 对象（初始 URL = base_url）
    let initial_url = base_url
        .clone()
        .unwrap_or_else(|| "about:blank".to_string());
    let nav = browser_navigation::new_navigation(&initial_url);
    crate::bridge::install_navigation(nav);
    let _ = crate::navigation_shim::install_navigation_globals(ctx);
    // M15.3/M15.4: 安装 cookie jar 后端。如果 cli 已装（主请求）复用，否则新建。
    crate::bridge::ensure_cookie_jar();
    // M17.2: 安装 XMLHttpRequest 全局构造器。
    let _ = crate::xhr_shim::install_xml_http_request(ctx);
    // M19.1: 安装全局 fetch（标准 Promise-based API）。
    let _ = crate::fetch_shim::install_fetch(ctx);
    // M23.5: 安装 WebSocket 全局构造器（ws:// 实时连接）。
    let _ = crate::ws_shim::install_websocket(ctx);
    // M28.1: 安装 navigator 全局对象（userAgent/platform/language，反爬必需）。
    let _ = crate::navigator_shim::install_navigator(ctx);
    // M28.2: 安装 window 全局对象（window===globalThis 自引用 + 视口数据属性）。
    // 必须在 navigator/location 等之后，window.navigator 经 globalThis 自动可见。
    let _ = crate::window_shim::install_window(ctx);
    // M28.3: 安装 document 全局对象（getElementById/querySelector/createElement
    // 包装 __* 桥 + body/head/cookie/title/location 数据属性）。必须在 window
    // 之后（document.location 指向 location 全局对象）。
    let _ = crate::document_shim::install_document(ctx);
    // M37: 安装 Element 对象（包装 NodeId + textContent/id/tagName getter/setter
    // 反射到 bridge）。必须在 document 之后（document.getElementById 返回 Element）。
    let _ = crate::element_shim::install_element(ctx);
    // M28.4: 安装 screen 全局对象（width/height/colorDepth/orientation，响应式布局
    // 特性检测常用）。静态默认值（无显示器环境）。
    let _ = crate::screen_shim::install_screen(ctx);
    // M41: Image 构造器（爬虫友好——设 src 后 setTimeout(0) 假触发 onload，不 fetch）。
    // 消除 baidu SPA 实测的 `Image is not defined` 错误。
    let _ = crate::image_shim::install_image(ctx);
    // M57: 基础兼容 shim，补齐常见前端运行时入口。
    let _ = crate::compat_shim::install_compat_shims(ctx);

    let count = execute_scripts_with_base(&shared, ctx, base_url);
    let _ = engine_name; // M66: 用于 --profile 日志
    let _ = engine; // 引擎在函数结束时 drop（释放 Context）
    (shared, count)
}

/// M48: 构建一个装好全部 shims（document/window/navigator/fetch/...）的 boa
/// `Context`，供 `run_scripts_with_base` 和 CDP `Runtime.evaluate`/`callFunctionOn`
/// 复用。返回的 ctx 还没绑定任何 tree —— 调用方需用
/// `bridge::install_shared_with_base` 装 tree guard 后才能 eval。
///
/// `base_url` 用于 location/navigation 后端的初始 URL（None → about:blank）。
#[cfg(feature = "boa")]
fn build_shimmed_context(base_url: &Option<String>) -> Context {
    let mut ctx = Context::default();
    {
        let limits = ctx.runtime_limits_mut();
        limits.set_loop_iteration_limit(JS_LOOP_ITERATION_LIMIT);
        limits.set_stack_size_limit(JS_STACK_SIZE_LIMIT);
        limits.set_recursion_limit(JS_RECURSION_LIMIT);
    }
    install(&mut ctx);
    let storage = browser_storage::new_storage();
    crate::bridge::install_storage(storage);
    let _ = crate::storage_shim::install_storage_globals(&mut ctx);
    let initial_url = base_url
        .clone()
        .unwrap_or_else(|| "about:blank".to_string());
    let nav = browser_navigation::new_navigation(&initial_url);
    crate::bridge::install_navigation(nav);
    let _ = crate::navigation_shim::install_navigation_globals(&mut ctx);
    crate::bridge::ensure_cookie_jar();
    let _ = crate::xhr_shim::install_xml_http_request(&mut ctx);
    let _ = crate::fetch_shim::install_fetch(&mut ctx);
    let _ = crate::ws_shim::install_websocket(&mut ctx);
    let _ = crate::navigator_shim::install_navigator(&mut ctx);
    let _ = crate::window_shim::install_window(&mut ctx);
    let _ = crate::document_shim::install_document(&mut ctx);
    let _ = crate::element_shim::install_element(&mut ctx);
    let _ = crate::screen_shim::install_screen(&mut ctx);
    let _ = crate::image_shim::install_image(&mut ctx);
    let _ = crate::compat_shim::install_compat_shims(&mut ctx);
    ctx
}

/// M48: 在一个**已构建好的 DOM tree** 上执行单个 JS 表达式，返回结果字符串
/// （boa `display()` 格式）。供 CDP `Runtime.evaluate` / `callFunctionOn` 用 ——
/// 让 `document.title`、`document.querySelector` 等能访问真实页面 DOM。
///
/// 与 `run_scripts_with_base` 不同：不执行页面里的 `<script>`，只 eval 调用方
/// 传入的表达式。tree 的 thread-local 安装在函数返回时由 `TreeGuard::drop` 清理。
///
/// # Errors
/// 返回 `Err(msg)` 如果 JS 解析或执行失败。
pub fn eval_in_tree(tree: Tree, base_url: Option<String>, expr: &str) -> Result<String, String> {
    // M71.1: 无 boa feature 时回退到 QuickJS（默认引擎）。
    #[cfg(feature = "boa")]
    {
        eval_in_tree_engine(tree, base_url, expr, &crate::engine::EngineKind::Boa)
    }
    #[cfg(not(feature = "boa"))]
    {
        eval_in_tree_engine(tree, base_url, expr, &crate::engine::EngineKind::QuickJs)
    }
}

/// M67: `eval_in_tree` 的引擎可选版本。供 CDP `Runtime.evaluate` /
/// `callFunctionOn` 用 —— 让 `document.title`、`document.querySelector` 等能
/// 访问真实页面 DOM。与 `run_scripts_with_base` 不同：不执行页面里的
/// `<script>`，只 eval 调用方传入的表达式。tree 的 thread-local 安装在函数
/// 返回时由 `TreeGuard::drop` 清理。
///
/// **返回值格式约定**（两引擎统一，便于 CDP `classify_value` 复用）：
/// - 字符串结果**带双引号**（模拟 boa `display()`：`"hello"`）
/// - number / bool / undefined / null 原样 `to_string()`（`5` / `true` / `undefined`）
///
/// # Errors
/// 返回 `Err(msg)` 如果 JS 解析或执行失败。
pub fn eval_in_tree_engine(
    tree: Tree,
    base_url: Option<String>,
    expr: &str,
    engine_kind: &crate::engine::EngineKind,
) -> Result<String, String> {
    eval_in_tree_engine_await(tree, base_url, expr, engine_kind, false)
}

/// M81(B4): [`eval_in_tree_engine`] 的 **awaitPromise** 版。`await_promise=true`
/// 时（仅 QuickJS 引擎生效），表达式完成值若是 Promise，Rust 侧驱动
/// microtask + timer 至 Fulfilled/Rejected，返回 resolved 值的 display 字符串；
/// Rejected → Err（CDP 层转 exceptionDetails）。boa 引擎忽略该参数（0.21 的
/// promise job 执行模型不同，不在此实现——行为同 `await_promise=false`）。
///
/// # Errors
/// 返回 `Err(msg)` 如果 JS 解析/执行失败，或 awaitPromise 的 Promise 被拒绝。
pub fn eval_in_tree_engine_await(
    tree: Tree,
    base_url: Option<String>,
    expr: &str,
    engine_kind: &crate::engine::EngineKind,
    await_promise: bool,
) -> Result<String, String> {
    use std::cell::RefCell;
    use std::rc::Rc;

    let shared: crate::bridge::SharedTree = Rc::new(RefCell::new(tree));

    // ── QuickJS 分支 ──
    #[cfg(feature = "quickjs")]
    if matches!(engine_kind, crate::engine::EngineKind::QuickJs) {
        return eval_in_tree_quickjs(shared, base_url, expr, await_promise);
    }
    // 非 QuickJS（boa）或 quickjs feature 未启用时的回退。
    let _ = engine_kind;
    let _ = await_promise;

    // ── boa 分支（默认 + QuickJS feature 未启用时的回退）──
    #[cfg(feature = "boa")]
    {
        let mut ctx = build_shimmed_context(&base_url);
        // 安装 tree guard：让 document/window shims 的 __* 桥能访问 DOM。
        // guard 在作用域结束时自动清理 thread-local slot。
        let _guard = crate::bridge::install_shared_with_base(shared, base_url);
        let result: JsValue = ctx
            .eval(Source::from_bytes(expr))
            .map_err(|e| format!("js eval error: {e}"))?;
        Ok(result.display().to_string())
    }
    #[cfg(not(feature = "boa"))]
    {
        // 无 boa feature：QuickJS 分支已 return，这里不可能到达。
        let _ = shared;
        let _ = base_url;
        let _ = expr;
        Err("no JS engine available".to_string())
    }
}

/// M67: QuickJS 版 `eval_in_tree`。复用 `run_scripts_quickjs` 的 setup 模式
/// （install_shared + storage/nav/cookie + shim install + 裸变量声明），但只
/// eval 调用方传入的单表达式，不执行页面 `<script>`。
///
/// 结果格式对齐 boa `display()`：字符串结果带双引号，其余原样 to_string()。
#[cfg(feature = "quickjs")]
fn eval_in_tree_quickjs(
    shared: crate::bridge::SharedTree,
    base_url: Option<String>,
    expr: &str,
    await_promise: bool,
) -> Result<String, String> {
    let mut engine_box = crate::engine::EngineKind::QuickJs.create(None);
    let wrapper: &mut crate::engine_quickjs::QuickJsEngineWrapper = (*engine_box)
        .as_any_mut()
        .downcast_mut::<crate::engine_quickjs::QuickJsEngineWrapper>()
        .expect("engine_name was quickjs but type mismatch");
    let engine = wrapper.engine();

    // 安装 thread_local DOM 后端（和 run_scripts_quickjs 一致）。
    let _guard = crate::bridge::install_shared_with_base(shared, base_url.clone());
    let storage = browser_storage::new_storage();
    crate::bridge::install_storage(storage);
    let initial_url = base_url
        .clone()
        .unwrap_or_else(|| "about:blank".to_string());
    let nav = browser_navigation::new_navigation(&initial_url);
    crate::bridge::install_navigation(nav);
    crate::bridge::ensure_cookie_jar();

    // 安装 JS shim —— 所有 shim 拼接成一个大字符串一次 eval（QuickJS ctx.eval
    // 每次是独立 scope，var 不跨 eval 泄漏，必须拼接）。
    let shims = get_all_shim_js(&base_url);
    let combined_shim: String = shims
        .iter()
        .map(|(_, js)| js.as_str())
        .collect::<Vec<_>>()
        .join("\n;\n");
    // M78.128-debug: eval_safe 打印异常 message（eval 的 Debug 格式只有 Exception）。
    if let Err(e) = engine.eval_safe(&combined_shim) {
        eprintln!("[js-runtime] QuickJS combined shim install failed: {e}");
        // M78.36-debug: 逐段定位 + 段内二分找首个失败行。
        for (name, js) in &shims {
            if let Err(se) = engine.eval_safe(js) {
                eprintln!("[js-runtime] shim 段 [{name}] 失败: {se}");
                let lines: Vec<&str> = js.split('\n').collect();
                let (mut lo, mut hi) = (0usize, lines.len() - 1);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    if engine.eval(&lines[..=mid].join("\n")).is_ok() {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                eprintln!(
                    "[js-runtime] [{name}] 首个失败行 ≈ {}: {}",
                    hi + 1,
                    lines[hi].trim()
                );
            }
        }
    }
    // 裸变量声明（globalThis.xxx 不会被解析为裸变量 xxx）。
    let _ = engine.eval(
        r#"var document = globalThis.document;
var navigator = globalThis.navigator;
var location = globalThis.location;
var history = globalThis.history;
var localStorage = globalThis.localStorage;
var sessionStorage = globalThis.sessionStorage;
var Event = globalThis.Event;
var CustomEvent = globalThis.CustomEvent;
var URL = globalThis.URL;
var URLSearchParams = globalThis.URLSearchParams;
var fetch = globalThis.fetch;
var setTimeout = globalThis.setTimeout;
var clearTimeout = globalThis.clearTimeout;
var setInterval = globalThis.setInterval;
var clearInterval = globalThis.clearInterval;
var requestAnimationFrame = globalThis.requestAnimationFrame;
var queueMicrotask = globalThis.queueMicrotask;
var atob = globalThis.atob;
var btoa = globalThis.btoa;
var crypto = globalThis.crypto;
var self = globalThis;
"#,
    );

    // eval 调用方表达式 —— 用封装好的 eval_display_string（对齐 boa display 格式）。
    // M81(B4): await_promise=true 时用 awaitPromise 版（完成值是 Promise 则
    // Rust 侧驱动至 Fulfilled/Rejected 取真实值）。
    let result = if await_promise {
        engine.eval_display_string_with_await(expr)
    } else {
        engine.eval_display_string(expr)
    };
    // engine 在作用域结束时 drop（释放 QuickJS runtime）
    drop(engine_box);
    result
}

#[cfg(all(test, feature = "boa"))]
mod tests {
    use super::*;
    use crate::bridge::body_text_content;

    fn parse(html: &str) -> Tree {
        browser_html_parser::parse(html)
    }

    #[test]
    fn extract_no_scripts() {
        let tree = parse("<html><body><p>hi</p></body></html>");
        assert!(extract_scripts(&tree).is_empty());
    }

    #[test]
    fn extract_one_inline_script() {
        let tree = parse("<html><body><script>__setBody('x')</script></body></html>");
        let scripts = extract_scripts(&tree);
        assert_eq!(scripts.len(), 1);
        assert_eq!(scripts[0], "__setBody('x')");
    }

    #[test]
    fn extract_multiple_scripts_in_order() {
        let tree = parse(
            "<html><body>\
             <script>a()</script>\
             <script>b()</script>\
             <script>c()</script>\
             </body></html>",
        );
        let scripts = extract_scripts(&tree);
        assert_eq!(scripts.len(), 3);
        assert!(scripts[0].contains("a()"));
        assert!(scripts[1].contains("b()"));
        assert!(scripts[2].contains("c()"));
    }

    #[test]
    fn extract_skips_json_and_template_scripts() {
        let tree = parse(
            "<html><body>\
             <script type=\"application/json\">{\"x\":1}</script>\
             <script type=\"text/template\">{{name}}</script>\
             <script>__setBody('ok')</script>\
             </body></html>",
        );
        let scripts = extract_scripts(&tree);
        assert_eq!(scripts.len(), 1);
        assert!(scripts[0].contains("__setBody('ok')"));
    }

    #[test]
    fn extract_accepts_js_and_module_scripts() {
        let tree = parse(
            "<html><body>\
             <script type=\"text/javascript\">__setBody('js')</script>\
             <script type=\"module\">__appendBody('module')</script>\
             </body></html>",
        );
        let scripts = extract_scripts(&tree);
        assert_eq!(scripts.len(), 2);
        assert!(scripts[0].contains("__setBody('js')"));
        assert!(scripts[1].contains("__appendBody('module')"));
    }

    #[test]
    fn extract_skips_empty_scripts() {
        let tree = parse(
            "<html><body>\
             <script>  </script>\
             <script>real()</script>\
             <script></script>\
             </body></html>",
        );
        let scripts = extract_scripts(&tree);
        assert_eq!(scripts.len(), 1);
        assert!(scripts[0].contains("real"));
    }

    #[test]
    fn execute_mutates_dom_via_bridge() {
        let html = "<html><body>\
                    <script>__setBody(\"dynamic content\")</script>\
                    </body></html>";
        let (shared, executed) = run_scripts(parse(html));
        assert!(
            executed >= 1,
            "should execute at least 1 script, got {executed}"
        );
        assert_eq!(body_text_content(&shared.borrow()), "dynamic content");
    }

    #[test]
    fn execute_chained_scripts_share_dom_state() {
        let html = "<html><body>\
                    <script>__setBody(\"first\")</script>\
                    <script>__appendBody(\" second\")</script>\
                    </body></html>";
        let (shared, executed) = run_scripts(parse(html));
        assert!(
            executed >= 2,
            "should execute at least 2 scripts, got {executed}"
        );
        assert_eq!(body_text_content(&shared.borrow()), "first second");
    }

    #[test]
    fn execute_script_error_does_not_abort_run() {
        let html = "<html><body>\
                    <script>throw new Error('boom')</script>\
                    <script>__setBody('recovered')</script>\
                    </body></html>";
        let (shared, _executed) = run_scripts(parse(html));
        // Each script is wrapped in try/catch (M57 compat), so the thrown
        // error is logged but does not abort the run. The key guarantee this
        // test verifies: a later script still runs and its DOM mutation
        // survives — the throw must not poison the pipeline.
        assert_eq!(body_text_content(&shared.borrow()), "recovered");
    }

    #[test]
    fn execute_preserves_non_script_dom() {
        // Static content should remain visible even with a script that
        // mutates a different part of the tree.
        let html = "<html><body>\
                    <p>static</p>\
                    <script>__setTitle('dynamic title')</script>\
                    </body></html>";
        let (shared, _) = run_scripts(parse(html));
        let text = body_text_content(&shared.borrow());
        assert!(text.contains("static"), "got: {text}");
    }

    #[test]
    fn infinite_loop_script_is_bounded_by_runtime_limit() {
        // M-cls.2: a runaway `while(true)` must throw (loop iteration limit)
        // instead of hanging or OOMing. The script sets a sentinel *before*
        // the loop; because per-script eval is wrapped in try/catch by
        // execute_scripts_with_base, the throw is swallowed and the next
        // script's sentinel still runs — proving the loop did not hang.
        let html = "<html><body>\
                    <script>__appendBody('before')</script>\
                    <script>var i=0; while(true){i++;}</script>\
                    <script>__appendBody('after')</script>\
                    </body></html>";
        let start = std::time::Instant::now();
        let (shared, _) = run_scripts(parse(html));
        let elapsed = start.elapsed();
        let text = body_text_content(&shared.borrow());
        assert!(text.contains("before"), "got: {text}");
        assert!(text.contains("after"), "got: {text}");
        // Must finish fast (limit kicks in), not hang for seconds.
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "runaway loop took {elapsed:?}, limit not enforced"
        );
    }
}

#[cfg(all(test, feature = "boa"))]
mod m69_proto_rel_diag {
    use super::resolve_script_url;

    #[test]
    fn diag_protocol_relative_url_resolution() {
        // 协议相对 URL（//host/path）在各种 base 下的解析行为
        let src = "//o.alicdn.com/x.js";
        // 有 base URL
        let r1 = resolve_script_url(src, Some("http://localhost:8899/page.html"));
        eprintln!("proto-rel with http base: {:?}", r1);
        let r2 = resolve_script_url(src, Some("https://open.bigmodel.cn/pricing"));
        eprintln!("proto-rel with https base: {:?}", r2);
        // 无 base URL（render-file 场景）
        let r3 = resolve_script_url(src, None);
        eprintln!("proto-rel without base: {:?}", r3);
        // 普通 https 绝对 URL
        let r4 = resolve_script_url("https://cdn.example.com/x.js", None);
        eprintln!("absolute https without base: {:?}", r4);
    }
}
