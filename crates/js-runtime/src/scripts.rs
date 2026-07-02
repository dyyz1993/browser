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
    // 替换 import.meta.env → 简单对象（Vite 环境变量桩）
    if patched.contains("import.meta.env") {
        patched = patched.replace(
            "import.meta.env",
            "({MODE:'production',DEV:false,PROD:true,SSR:false,BASE_URL:'/'})",
        );
    }
    // 其他裸 import.meta → 替换为带 url 属性的对象（防 import.meta["url"] 等）
    if patched.contains("import.meta") {
        let url = if base_url.is_empty() {
            "about:blank".to_string()
        } else {
            base_url.to_string()
        };
        patched = patched.replace("import.meta", &format!("({{url:\"{url}\",env:{{MODE:'production',DEV:false,PROD:true,SSR:false,BASE_URL:'/'}}}})"));
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
        // 用 tokio::time::timeout 给 fetch 加 8s 上限
        let result = rt.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(8), client.get(&url_owned)).await
        });
        match result {
            Ok(Ok(bytes)) => {
                String::from_utf8(bytes).map_err(|e| format!("non-utf8 response: {e}"))
            }
            Ok(Err(e)) => Err(format!("{e:?}")),
            Err(_) => Err(format!("fetch timeout (8s): {url_owned}")),
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

/// M16.3: Drain due timer callbacks until the wheel is idle or the
/// M66: QuickJS TypeScript 检测——QuickJS 不支持 TS 语法。
/// M66: QuickJS TypeScript 检测——QuickJS 不支持 TS 语法。
/// M69: 移出 quickjs feature 门控——boa pump 的动态 script drain 也用它跳过 TS chunk。
fn has_ts_syntax(code: &str) -> bool {
    code.contains(": string")
        || code.contains(": number")
        || code.contains(": boolean")
        || code.contains(": void")
        || code.contains(": any")
        || code.contains(" as const")
        || code.contains(": ReturnType<")
        || (code.contains(": \"") && code.contains(" | "))
        || code.contains("interface ")
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
/// 的 JS shim 检测到 script 标签后，把代码（inline textContent 或 __fetchSync
/// 拿到的外链源码）入队；这里取出用 `eval_safe` 执行（GC 安全，CaughtError
/// 在 ctx.with 闭包内 drop）。
///
/// 跳过 TypeScript 代码（`: string` / `as Type` 等，QuickJS 不支持 TS）。
/// eval 出的代码可能又 appendChild 新 script 入队，下一轮 pump 处理（多层链式加载）。
///
/// 返回本轮执行的 script 数（用于 pump 判断是否还有进展）。
#[cfg(feature = "quickjs")]
fn drain_and_eval_dynamic_scripts(engine: &mut crate::engine_quickjs::QuickJsEngine) -> usize {
    let codes = crate::bridge::drain_dynamic_scripts();
    let n = codes.len();
    for code in codes {
        if has_ts_syntax(&code) {
            continue;
        }
        // M71.4: 用户 script 用 sloppy mode（eval_user_script），
        // 兼容 SvelteKit 等框架的裸全局赋值（`__sveltekit_x = {}`）。
        match engine.eval_user_script(&code) {
            Ok(()) => {}
            Err(e) => eprintln!("[js] [quickjs dynamic] {e}"),
        }
    }
    n
}

/// M66-B: QuickJS 专用执行路径。
/// 安装 bridge（已在 engine 内部完成）+ JS shim + eval 脚本 + event loop。
#[cfg(feature = "quickjs")]
fn run_scripts_quickjs(
    shared: crate::bridge::SharedTree,
    base_url: Option<String>,
    mut engine_box: Box<dyn crate::engine::JsEngine>,
) -> (crate::bridge::SharedTree, usize) {
    // downcast 到 QuickJsEngineWrapper（需要 &mut）
    let wrapper: &mut crate::engine_quickjs::QuickJsEngineWrapper = (*engine_box)
        .as_any_mut()
        .downcast_mut::<crate::engine_quickjs::QuickJsEngineWrapper>()
        .expect("engine_name was quickjs but type mismatch");
    let engine = wrapper.engine();

    // 安装 thread_local DOM 后端
    let _guard = crate::bridge::install_shared_with_base(shared.clone(), base_url.clone());
    let storage = browser_storage::new_storage();
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
    if let Err(e) = engine.eval(&combined_shim) {
        eprintln!("[js-runtime] QuickJS combined shim install failed: {e}");
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
        "#,
    );

    // M66: QuickJS —— 逐个 eval script（shim 已经通过初始 eval 设置了 globalThis 属性）
    let scripts = {
        let borrowed = shared.borrow();
        extract_script_entries(&borrowed)
    };
    let __t_scripts_start = std::time::Instant::now();

    for script in &scripts {
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
            ScriptEntry::ExternalModule(src) => {
                match resolve_script_url(src, base_url.as_deref()) {
                    Some(url) => {
                        if should_skip_script(&url) {
                            continue;
                        }
                        match fetch_external_script(&url) {
                            Ok(code) => {
                                // M66: 有静态 import/export 的 module → Module::declare+eval
                                // 有 import.meta 但无 import/export → try_strip 后普通 eval
                                let has_imports = code.contains("from\"./")
                                    || code.contains("from './")
                                    || code.contains("import\"./")
                                    || code.contains("import './");
                                let has_export = code.contains("export{")
                                    || code.contains("export {")
                                    || code.contains("export*");
                                if has_imports || has_export {
                                    // Module 模式执行（支持 import/export/import.meta）
                                    match engine.eval_module_with_imports(&url, &code) {
                                        Ok(_) => executed += 1,
                                        Err(e) => eprintln!(
                                            "[js-runtime] QuickJS module eval failed: {url}: {e}"
                                        ),
                                    }
                                    continue;
                                }
                                let base = base_url.as_deref().unwrap_or("");
                                match try_strip_esm_for_eval(&code, base) {
                                    Some(p) => Some(p),
                                    None => Some(code),
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
                let has_imports = code.contains("from\"./") || code.contains("from './");
                if has_imports {
                    continue;
                }
                let base = base_url.as_deref().unwrap_or("");
                match try_strip_esm_for_eval(code, base) {
                    Some(p) => Some(p),
                    None => Some(code.clone()),
                }
            }
        };
        if let Some(code) = code {
            // M71.4: 用户 script 用 sloppy mode（eval_user_script），
            // 兼容 SvelteKit 等框架的裸全局赋值（`__sveltekit_x = {}`）。
            match engine.eval_user_script(&code) {
                Ok(_) => executed += 1,
                Err(e) => eprintln!("[js] [quickjs] {e}"),
            }
        }
    }

    // dispatch DOMContentLoaded/load
    let __t_dcl_start = std::time::Instant::now();
    let _ = engine.eval(
        r#"try {
            if (typeof document !== 'undefined' && typeof document.dispatchEvent === 'function') {
                var ev1 = new Event('DOMContentLoaded');
                document.dispatchEvent(ev1);
                var ev2 = new Event('load');
                document.dispatchEvent(ev2);
                if (typeof window !== 'undefined' && typeof window.dispatchEvent === 'function') {
                    window.dispatchEvent(ev1);
                    window.dispatchEvent(ev2);
                }
            }
        } catch(e) {}"#,
    );
    eprintln!(
        "[serve] scripts: {}ms, dcl: {}ms",
        __t_scripts_start.elapsed().as_millis(),
        __t_dcl_start.elapsed().as_millis()
    );

    // M66: DCL 后可能 schedule 了新 timer（框架初始化），drain 一轮
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

    // 所有脚本执行完后，循环触发 setTimeout/setInterval 回调，
    // 直到 pending timer 清空或动态 script 队列清空，或超时（3s 上限）。
    // M70.13: 事件循环参数——缩短超时 + 更快 idle 退出。
    const EL_MAX_TOTAL: std::time::Duration = std::time::Duration::from_secs(2);
    const EL_TICK_MS: u64 = 5;
    const EL_IDLE_ROUNDS: u32 = 3;
    const EL_IDLE_GRACE: std::time::Duration = std::time::Duration::from_millis(300);
    let el_start = std::time::Instant::now();
    let mut idle_rounds: u32 = 0;
    let mut idle_start: Option<std::time::Duration> = None;
    loop {
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
            // M72.4: 但有活动 WebSocket 连接时不 idle 退出（等 onopen/onmessage）
            let has_ws = crate::bridge::ws_connection_count() > 0;
            if idle_start.is_none() {
                idle_start = Some(el_start.elapsed());
            }
            if !has_ws {
                if idle_start.unwrap() >= EL_IDLE_GRACE {
                    idle_rounds += 1;
                    if idle_rounds >= EL_IDLE_ROUNDS {
                        break;
                    }
                } else {
                    idle_rounds = 0;
                }
                // M70.13: DOM 稳定检测——body 有子节点则内容已就绪，提前退出。
                if idle_start.unwrap() >= EL_IDLE_GRACE {
                    let dom_ready = engine
                        .eval_js_bool("__findTag('body')>0&&__children(__findTag('body')).length>0")
                        .unwrap_or(false);
                    if dom_ready {
                        break;
                    }
                }
            }
        }
        if fired == 0 && dyn_executed == 0 && trans_fired == 0 && ws_events.is_empty() {
            let has = engine.eval_js_bool("__hasPendingTimers()").unwrap_or(false);
            let has_trans = engine
                .eval_js_bool("__hasPendingTransitions()")
                .unwrap_or(false);
            // M72.4: 有活动 WS 连接时继续轮询（等 onopen/onmessage）
            let has_ws = crate::bridge::ws_connection_count() > 0;
            if !has && !has_trans && !has_ws {
                break;
            }
        }
        if el_start.elapsed() >= EL_MAX_TOTAL {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(EL_TICK_MS));
    }
    // M66-fix: 最后再 drain 一轮（最后一波 timer 回调可能 schedule 了 microtask）。
    engine.run_jobs();
    engine.gc();
    eprintln!("[serve] event_loop: {}ms", el_start.elapsed().as_millis());

    (shared, executed)
}

/// M66-B: 获取所有 JS shim 的 JS 字符串（引擎无关）。
/// 最小版本——只包含 QuickJS 验证所需的核心 shim。
/// 后续需要从 boa 的 shim 模块提取完整 JS 字符串。
#[cfg(feature = "quickjs")]
fn get_all_shim_js(_base_url: &Option<String>) -> Vec<(&'static str, String)> {
    vec![
        ("globals", QUICKJS_GLOBAL_SHIM.to_string()),
        ("element", QUICKJS_ELEMENT_SHIM.to_string()),
        ("document", QUICKJS_DOCUMENT_SHIM.to_string()),
        ("xhr", QUICKJS_XHR_SHIM.to_string()),
    ]
}

/// M66-B: QuickJS 最小全局 shim（window/document/navigator/setTimeout 桩）。
/// 这是验证用的最小集——后续替换为 boa 的完整 5400 行 shim。
#[cfg(feature = "quickjs")]
const QUICKJS_GLOBAL_SHIM: &str = r#"
// window 全局对象
var window = globalThis;
var self = globalThis;
	var top = globalThis;
	var parent = globalThis;
	
	// M70.6: 全局 onload/onerror 桩（防止 `onload is not defined` 报错）。
	// 某些框架（如 bing.com）直接引用 onload 全局变量而非 window.onload。
	var onload = null;
	var onerror = null;
	window.onload = null;
	window.onerror = null;
	
	// navigator
window.navigator = { userAgent: 'Mozilla/5.0', platform: 'MacIntel', language: 'en-US', languages: ['en-US','en'] };
window.scrollTo = window.scroll = function() {};
window.scrollX = window.scrollY = window.pageXOffset = window.pageYOffset = 0;
window.innerWidth = 1024;
window.innerHeight = 768;
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
    var now = Date.now();
    var fired = 0;
    // 正序遍历（FIFO：先注册的先触发）。
    // splice 会导致后续元素前移，所以用 while + 手动 i 控制。
    var i = 0;
    while (i < __pendingTimers.length) {
        var t = __pendingTimers[i];
        if (!t || now < t.fireAt) { i++; continue; }
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

// crypto.getRandomValues（uuid 库需要）
window.crypto = {
    getRandomValues: function(arr) {
        for (var i = 0; i < arr.length; i++) arr[i] = Math.floor(Math.random() * 256);
        return arr;
    }
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
    var host = afterProto.split('/')[0] || '';
    var hostNoPort = host.split(':')[0];
    var pathname;
    if (proto === 'about:') {
        pathname = afterProto; // 'blank'
    } else if (host) {
        pathname = '/' + afterProto.split('/').slice(1).join('/');
    } else {
        pathname = '/' + afterProto;
    }
    return {
        href: href,
        protocol: proto,
        host: host,
        hostname: hostNoPort,
        pathname: pathname,
        search: '', hash: '',
        origin: proto + (host ? '//' + host : ''),
        reload: function() {},
        replace: function(u) { __setLocHref(u); },
        assign: function(u) { __setLocHref(u); },
        toString: function() { return __locHref; }
    };
}
function __setLocHref(u) {
    __locHref = u;
    window.location = __parseLoc(u);
}
window.location = __parseLoc(__locHref);

// history API（docsify 路由需要 pushState/replaceState）
// length 是函数（对齐 boa navigation_shim：history.length() 返回栈深度）
window.history = (function() {
    var stack = [__locHref];
    var state = null;
    return {
        length: function() { return stack.length; },
        get state() { return state; },
        pushState: function(s, title, url) {
            state = s;
            if (url) { stack.push(url); __setLocHref(url); }
            else { stack.push(stack[stack.length-1]); }
        },
        replaceState: function(s, title, url) {
            state = s;
            if (url) { stack[stack.length-1] = url; __setLocHref(url); }
        },
        back: function() { if (stack.length > 1) { stack.pop(); __setLocHref(stack[stack.length-1]); } },
        forward: function() {},
        go: function(n) {},
        scrollRestoration: 'auto'
    };
})();

// document 占位（完整 document 在 document shim 里填充）
window.document = { createElement: function(tag) { return new Element(0); }, getElementById: function(id) { return null; } };

// __makeElement 工厂
window.__makeElement = function(nodeId) {
    if (typeof nodeId === 'number' && nodeId >= 0) return new Element(nodeId);
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
window.addEventListener = function(type, cb) {
    if (cb === null || cb === undefined) return;
    if (!__winListeners[type]) __winListeners[type] = [];
    __winListeners[type].push(cb);
};
window.removeEventListener = function(type, cb) {};
window.dispatchEvent = function(ev) {
    if (ev && __winListeners[ev.type]) {
        var cbs = __winListeners[ev.type];
        for (var i = 0; i < cbs.length; i++) {
            try { cbs[i].call(window, ev); } catch(e) {}
        }
    }
};

// M66: customElements + HTMLElement（no-op，不存引用避免 GC 泄漏）
window.customElements = { define: function(){}, get: function(){return undefined;}, upgrade: function(){}, whenDefined: function(){return Promise.resolve();} };
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
        // iframe 的 contentDocument：返回一个简易 document（含 body），
        // 这样页面可往里写内容。非 iframe 返回 null。
        if (this.tagName !== 'IFRAME') return null;
        if (!this.__contentDoc) {
            this.__contentDoc = { body: null, documentElement: null };
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
            this.__contentWin = {
                document: self.contentDocument,
                postMessage: function(msg) {
                    if (typeof window.onmessage === 'function') {
                        try { window.onmessage({ data: msg, origin: '*', source: self.__contentWin }); } catch(e) {}
                    }
                }
            };
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

// IntersectionObserver / ResizeObserver（no-op）
window.IntersectionObserver = function() { this.observe = function(){}; this.unobserve = function(){}; this.disconnect = function(){}; this.takeRecords = function(){return [];}; };
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

// localStorage / sessionStorage（存键值对，爬虫场景空存储够用）
var __localStorage = {};
window.localStorage = {
    getItem: function(k) { return (k in __localStorage) ? __localStorage[k] : null; },
    setItem: function(k, v) { __localStorage[k] = String(v); },
    removeItem: function(k) { delete __localStorage[k]; },
    clear: function() { __localStorage = {}; },
    key: function(i) { var keys = Object.keys(__localStorage); return keys[i] || null; },
    get length() { return Object.keys(__localStorage).length; }
};
window.sessionStorage = {
    getItem: function(k) { return null; },
    setItem: function(k, v) {},
    removeItem: function(k) {},
    clear: function() {},
    key: function(i) { return null; },
    get length() { return 0; }
};

// URL 构造器（简化版——避免 QuickJS 不支持的复杂正则）
window.URL = function(input, base) {
    input = String(input);
    if (base && input.indexOf('://') < 0) {
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
    this.search = q ? '?' + q.split('#')[0] : '';
    this.searchParams = new URLSearchParams(q || '');
    this.hash = input.indexOf('#') >= 0 ? '#' + input.split('#')[1] : '';
    this.origin = this.protocol + '//' + this.host;
    this.toString = function() { return this.href; };
    this.toJSON = function() { return this.href; };
};

// URLSearchParams（简化）
window.URLSearchParams = function(init) {
    var params = {};
    if (typeof init === 'string') {
        init.replace(/^\?/, '').split('&').forEach(function(p) {
            var kv = p.split('=');
            params[decodeURIComponent(kv[0])] = decodeURIComponent(kv[1] || '');
        });
    }
    this.get = function(k) { return (k in params) ? params[k] : null; };
    this.set = function(k, v) { params[k] = v; };
    this.has = function(k) { return k in params; };
    this.toString = function() {
        return Object.keys(params).map(function(k) { return k + '=' + params[k]; }).join('&');
    };
};

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
window.MutationObserver = function(cb) {
    this.observe = function(target, opts) {};
    this.disconnect = function() {};
    this.takeRecords = function() { return []; };
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
        // 返回伪 Uint8Array（length 正确，内容为 charCode）
        var arr = [];
        for (var i = 0; i < str.length; i++) { arr.push(str.charCodeAt(i) & 0xff); }
        arr.encoding = 'utf-8';
        return arr;
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
        var size = 0;
        if (parts) { for (var i = 0; i < parts.length; i++) { size += (parts[i] && parts[i].length) ? parts[i].length : String(parts[i]).length; } }
        this.size = size;
        this.type = (opts && opts.type) || '';
    };
    window.Blob.prototype.text = function() { return Promise.resolve(''); };
    window.Blob.prototype.arrayBuffer = function() { return Promise.resolve(new ArrayBuffer(0)); };
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
    if (el && typeof ns === 'string') { el.namespaceURI = ns; }
    return el;
};

// Node 常量（框架常用 nodeType 判断）
window.Node = window.Node || {};
window.Node.ELEMENT_NODE = 1;
window.Node.TEXT_NODE = 3;
window.Node.COMMENT_NODE = 8;
window.Node.DOCUMENT_NODE = 9;
window.Node.DOCUMENT_FRAGMENT_NODE = 11;
window.Node.DOCUMENT_POSITION_CONTAINED_BY = 16;

// AbortSignal（React/Next.js 在 GitHub 检查）
if (typeof AbortSignal === 'undefined') {
    window.AbortSignal = function() {};
    window.AbortSignal.prototype = Object.create(Object.prototype);
    AbortSignal.prototype.aborted = false;
    AbortSignal.prototype.reason = undefined;
    AbortSignal.prototype.throwIfAborted = function() {};
    AbortSignal.prototype.addEventListener = function() {};
    AbortSignal.prototype.removeEventListener = function() {};
    AbortSignal.prototype.dispatchEvent = function() { return true; };
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
if (typeof Document === 'undefined') {
    window.Document = function() {};
    Document.prototype = Object.create(Object.prototype);
    Document.prototype.body = null;
    Document.prototype.documentElement = null;
    Document.prototype.readyState = 'complete';
    Document.prototype.addEventListener = function() {};
    Document.prototype.removeEventListener = function() {};
    Document.prototype.dispatchEvent = function() { return true; };
    Document.prototype.createElement = function(tag) { return { tagName: tag.toUpperCase() }; };
    Document.prototype.createTextNode = function(t) { return { nodeType: 3, textContent: t, data: t }; };
}

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

/// M66-B: QuickJS Element shim（和 boa element_shim 的核心逻辑相同）。
#[cfg(feature = "quickjs")]
const QUICKJS_ELEMENT_SHIM: &str = r#"
function Element(nodeId) { this.__nodeId = nodeId; }
Element.prototype.getAttribute = function(key) {
    var v = __getAttr(this.__nodeId, key);
    return (v === null || v === undefined) ? null : String(v);
};
Element.prototype.setAttribute = function(key, val) { __setAttr(this.__nodeId, key, String(val)); };
Element.prototype.appendChild = function(child) {
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
            var code = null;
            if (src) {
                // 外链 script：同步 fetch（复用 __fetchSync，相对 URL 自动解析）。
                // 同步阻塞是期望行为——保证 webpack chunk loader 的 Promise.resolve 顺序。
                code = (typeof __fetchSync === 'function') ? __fetchSync(src) : null;
                if (!code) {
                    // fetch 失败（404/网络错误）→ 异步触发 onerror
                    var _err = child;
                    setTimeout(function() {
                        if (typeof _err.onerror === 'function') {
                            try { _err.onerror.call(_err, { type: 'error', target: _err }); } catch(e) {}
                        }
                    }, 0);
                    return child;
                }
            } else {
                // inline script：读 textContent（同样双 fallback：DOM + JS 属性）。
                code = __getText(child.__nodeId);
                if (!code && typeof child.textContent === 'string') code = child.textContent;
            }
            if (code) {
                // 入队，pump 循环（run_scripts_quickjs 里的 loop）取出 eval_safe。
                __enqueueDynamicScript(code);
                // 异步触发 onload（推迟到 event loop 下一轮，符合 HTML5 语义）。
                var _s = child;
                setTimeout(function() {
                    if (typeof _s.onload === 'function') {
                        try { _s.onload.call(_s, { type: 'load', target: _s }); } catch(e) {}
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
    if (child && typeof child.__nodeId === 'number') {
        __removeChild(this.__nodeId, child.__nodeId);
    }
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
Element.prototype.remove = function() {};
Element.prototype.addEventListener = function(type, cb) {
    if (cb === null || cb === undefined) return;
    if (!this.__listeners) this.__listeners = {};
    if (!this.__listeners[type]) this.__listeners[type] = [];
    this.__listeners[type].push(cb);
};
Element.prototype.cloneNode = function(deep) {
    var tag = String(__getTag(this.__nodeId) || 'div');
    var newId = __createEl(tag);
    if (newId < 0) return null;
    var copy = __makeElement(newId);
    // 复制文本内容（仅叶子元素）。
    // GAP-J: 不能对父元素无条件 __setText——__getText(父) 返回子节点文本拼接，
    // __setText 会给克隆的父元素加一个不该有的文本子节点（导致 cloneNode 多复制）。
    // 只在没有元素子节点（纯文本叶子，如 <li>text</li>）时才复制文本。
    try {
        var hasElementChild = false;
        var rawChildren = __children(this.__nodeId);
        if (rawChildren) {
            var childIds = rawChildren.split(',').filter(function(s) { return s; });
            for (var ci = 0; ci < childIds.length; ci++) {
                var ctag = __getTag(parseInt(childIds[ci], 10));
                if (ctag && ctag !== '__text__') { hasElementChild = true; break; }
            }
        }
        if (!hasElementChild) {
            var txt = __getText(this.__nodeId);
            if (txt) __setText(newId, String(txt));
        }
    } catch(e) {}
    // 深拷贝：递归克隆子元素（重建子树）
    if (deep !== false) {
        try {
            var cs = __children(this.__nodeId);
            if (cs) {
                var ids = cs.split(',').filter(function(s) { return s; });
                for (var i = 0; i < ids.length; i++) {
                    var cid = parseInt(ids[i], 10);
                    var ctag = __getTag(cid);
                    if (ctag && ctag !== '__text__') {
                        var childCopy = __makeElement(cid) ? __makeElement(cid).cloneNode(true) : null;
                        if (childCopy) {
                            try { __appendChild(newId, childCopy.__nodeId); } catch(e2) {}
                        }
                    }
                }
            }
        } catch(e3) {}
    }
    return copy;
};
Object.defineProperty(Element.prototype, 'tagName', {
    get: function() { return String(__getTag(this.__nodeId)).toUpperCase(); },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'textContent', {
    get: function() { return __getText(this.__nodeId); },
    set: function(v) { __setText(this.__nodeId, String(v)); },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'innerHTML', {
    get: function() {
        // 从 Rust Tree 读子节点的文本拼接（近似 innerHTML）
        var cs = __children(this.__nodeId);
        if (!cs) return '';
        var ids = cs.split(',').filter(function(s) { return s; });
        var out = '';
        for (var i = 0; i < ids.length; i++) {
            var id = parseInt(ids[i], 10);
            var tag = __getTag(id);
            var text = __getText(id);
            if (tag === '__text__') {
                out += text;
            } else {
                out += '<' + tag + '>' + text + '</' + tag + '>';
            }
        }
        return out;
    },
    set: function(v) {
        var s = String(v);
        if (typeof __parseHtml === 'function' && s.length > 0) {
            __parseHtml(this.__nodeId, s);
        } else {
            __setText(this.__nodeId, s);
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
Object.defineProperty(Element.prototype, 'classList', {
    get: function() {
        var self = this;
        var cls = (__getAttr(self.__nodeId, 'class') || '').split(/\s+/).filter(function(s) { return s; });
        return {
            add: function() {
                for (var i = 0; i < arguments.length; i++) {
                    var c = String(arguments[i]);
                    if (cls.indexOf(c) < 0) cls.push(c);
                }
                __setAttr(self.__nodeId, 'class', cls.join(' '));
            },
            remove: function() {
                for (var i = 0; i < arguments.length; i++) {
                    var idx = cls.indexOf(String(arguments[i]));
                    if (idx >= 0) cls.splice(idx, 1);
                }
                __setAttr(self.__nodeId, 'class', cls.join(' '));
            },
            toggle: function(c, force) {
                c = String(c);
                var has = cls.indexOf(c) >= 0;
                if (force === true || (!has && force !== false)) {
                    if (!has) cls.push(c);
                } else if (has) {
                    cls.splice(cls.indexOf(c), 1);
                }
                __setAttr(self.__nodeId, 'class', cls.join(' '));
                return cls.indexOf(c) >= 0;
            },
            contains: function(c) { return cls.indexOf(String(c)) >= 0; },
            item: function(i) { return cls[i] || null; },
            get length() { return cls.length; }
        };
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
Object.defineProperty(Element.prototype, 'nextSibling', {
    get: function() { return null; },
    enumerable: true, configurable: true
});
Object.defineProperty(Element.prototype, 'previousSibling', {
    get: function() { return null; },
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
Object.defineProperty(Element.prototype, 'nodeType', {
    get: function() { return this.__isFragment ? 11 : 1; },
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
Element.prototype.querySelector = function(sel) {
    // 简化：全局 qs（爬虫够用）
    var id = __qs(String(sel));
    return (id >= 0) ? __makeElement(id) : null;
};
Element.prototype.querySelectorAll = function(sel) {
    var ids = __qsAll(String(sel));
    if (!ids) return [];
    return ids.split(',').filter(function(s) { return s; }).map(function(s) { return __makeElement(parseInt(s, 10)); });
};
Element.prototype.contains = function(node) { return false; };
// matches/closest：CSS 选择器匹配。依赖 __qsMatch/__qsClosest bridge。
Element.prototype.matches = function(sel) {
    if (typeof __qsMatch === 'function') {
        try { return !!__qsMatch(this.__nodeId, String(sel)); } catch(e) { return false; }
    }
    return false;
};
Element.prototype.closest = function(sel) {
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
            var arr = ids.map(function(s) { return parseInt(s, 10); });
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
            return document.createDocumentFragment();
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
Element.prototype.dispatchEvent = function(ev) {
    // GAP-M: 事件冒泡。dispatchEvent 应沿 parent 链向上触发祖先监听器
    // （事件委托场景：ul 监听 click，点击 li 应冒泡到 ul）。
    if (!ev) return true;
    var cur = this;
    ev.target = this;
    while (cur) {
        ev.currentTarget = cur;
        if (cur.__listeners && cur.__listeners[ev.type]) {
            var cbs = cur.__listeners[ev.type];
            for (var i = 0; i < cbs.length; i++) {
                try { cbs[i].call(cur, ev); } catch(e) {}
            }
        }
        // bubbles=false 或已 stopPropagation 则停止冒泡
        if (!ev.bubbles || ev.__stopPropagation) break;
        // 沿 parent 链向上（用 __getParent bridge）
        try {
            var pid = __getParent(cur.__nodeId);
            if (pid >= 0) {
                cur = __makeElement(pid);
            } else {
                break;
            }
        } catch(pe) { break; }
    }
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
    // 简化：只支持 beforeend（最常用）
    if (pos === 'beforeend' && html) {
        __setAttr(this.__nodeId, 'innerHTML', (__getAttr(this.__nodeId, 'innerHTML') || '') + html);
    }
};
Element.prototype.getBoundingClientRect = function() {
    return { x:0, y:0, top:0, left:0, right:0, bottom:0, width:0, height:0 };
};
Element.prototype.focus = function() {};
Element.prototype.blur = function() {};
Element.prototype.scrollIntoView = function() {};
// dataset（框架常用 data-* 属性）——Proxy 动态反射到 data-* attribute。
// dataset.fooBar → __getAttr(nodeId, 'data-foo-bar')，写同步 __setAttr。
// 不依赖枚举所有属性（QuickJS bridge 无列属性 API），惰性按 key 反射。
Object.defineProperty(Element.prototype, 'dataset', {
    get: function() {
        var self = this;
        var cache = {};
        // 驼峰 ↔ kebab：fooBar ↔ data-foo-bar
        function toKebab(k) { return 'data-' + String(k).replace(/([A-Z])/g, function(_, c) { return '-' + c.toLowerCase(); }); }
        function toCamel(k) { return k.slice(5).replace(/-([a-z])/g, function(_, c) { return c.toUpperCase(); }); }
        try {
            return new Proxy(cache, {
                get: function(t, k) {
                    if (k in t) return t[k];
                    if (typeof k !== 'string') return undefined;
                    var v = __getAttr(self.__nodeId, toKebab(k));
                    return (v === null || v === undefined) ? undefined : v;
                },
                set: function(t, k, v) {
                    if (typeof k === 'string') {
                        __setAttr(self.__nodeId, toKebab(k), String(v));
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
Object.defineProperty(Element.prototype, 'outerHTML', {
    get: function() { return this.innerHTML || ''; },
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
undefined;
"#;

/// M66-B: QuickJS document shim。
#[cfg(feature = "quickjs")]
const QUICKJS_DOCUMENT_SHIM: &str = r#"
document.createElement = function(tag) {
    var id = __createEl(String(tag || 'div'));
    return __makeElement(id);
};
document.createElementNS = function(ns, tag) { return document.createElement(tag); };
document.createTextNode = function(text) {
    var id = __createEl('__text__');
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
document.createComment = function(text) { return document.createElement('div'); };
document.getElementById = function(id) {
    var nodeId = __getElById(String(id));
    return (nodeId >= 0) ? __makeElement(nodeId) : null;
};
document.querySelector = function(sel) {
    var nodeId = __qs(String(sel));
    return (nodeId >= 0) ? __makeElement(nodeId) : null;
};
document.querySelectorAll = function(sel) {
    var ids = __qsAll(String(sel));
    if (!ids) return [];
    return ids.split(',').filter(function(s) { return s; }).map(function(s) { return __makeElement(parseInt(s, 10)); });
};
document.getElementsByTagName = function(tag) {
    return document.querySelectorAll(tag);
};
document.getElementsByClassName = function(cls) {
    return document.querySelectorAll('.' + cls);
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
    get: function() { return document.body; },
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
Object.defineProperty(document, 'readyState', {
    get: function() { return 'complete'; },
    enumerable: true, configurable: true
});
document.addEventListener = function(type, cb) {
    if (!this.__listeners) this.__listeners = {};
    if (!this.__listeners[type]) this.__listeners[type] = [];
    this.__listeners[type].push(cb);
};
document.removeEventListener = function(type, cb) {};
document.dispatchEvent = function(ev) {
    if (this.__listeners && this.__listeners[ev && ev.type]) {
        var cbs = this.__listeners[ev.type];
        for (var i = 0; i < cbs.length; i++) {
            try { cbs[i](ev); } catch(e) {}
        }
    }
};
undefined;
"#;

/// M66-B: QuickJS XHR + Event + fetch shim（docsify 核心依赖）。
#[cfg(feature = "quickjs")]
const QUICKJS_XHR_SHIM: &str = r#"
// Event 构造器
function Event(type, opts) { this.type = type; this.target = null; this.currentTarget = null; }
Event.prototype.preventDefault = function() {};
Event.prototype.stopPropagation = function() {};
function CustomEvent(type, opts) {
    Event.call(this, type);
    this.detail = (opts && opts.detail) || null;
}
CustomEvent.prototype = Object.create(Event.prototype);

// XMLHttpRequest（同步 fetch 版——docsify 用它加载 markdown）
var __xhrSeq = 0;
function XMLHttpRequest() {
    __xhrSeq++;
    this.__id = __xhrSeq;
    this.__async = true;
    this.readyState = 0;
    this.status = 0;
    this.responseText = '';
    this.response = '';
    this.__listeners = {};
}
XMLHttpRequest.prototype.open = function(method, url, async) {
    this.__url = url;
    this.__method = method || 'GET';
    this.__async = (async !== false);
    this.readyState = 1;
};
XMLHttpRequest.prototype.setRequestHeader = function(key, val) {};
XMLHttpRequest.prototype.send = function(body) {
    var self = this;
    var url = this.__url;
    var method = this.__method || 'GET';
    // 同步（open 传 false）立即完成；异步（true 或省略）setTimeout 触发
    var sync = (this.__async === false);
    function doSend() {
        var raw = (typeof __fetchSync === 'function') ? __fetchSync(url) : null;
        if (typeof __log === 'function') __log('[xhr] send ' + url + ' → ' + (raw ? raw.length + ' bytes' : 'null'));
        if (raw) {
            self.responseText = raw;
            self.response = raw;
            self.status = 200;
        } else {
            self.status = 0;
        }
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
window.fetch = function(input, options) {
    var url = (typeof input === 'string') ? input : (input && input.url) || String(input);
    options = options || {};
    var method = options.method || 'GET';
    var body = options.body || null;
    var ct = options.headers ? (options.headers['Content-Type'] || options.headers['content-type'] || null) : null;
    return new Promise(function(resolve, reject) {
        var raw = null;
        var statusCode = 200;
        // POST/PUT/DELETE → __fetchSyncMethod（支持 method/body，返回 "{status}\n{body}"）
        // GET → __fetchSync（现有同步 fetch）
        if (method !== 'GET' && typeof __fetchSyncMethod === 'function') {
            raw = __fetchSyncMethod(url, method, body, ct);
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
            resolve({
                ok: statusCode >= 200 && statusCode < 300,
                status: statusCode,
                statusText: statusCode === 200 ? 'OK' : String(statusCode),
                url: url,
                redirected: false,
                text: function() { return Promise.resolve(raw); },
                json: function() { return Promise.resolve(JSON.parse(raw)); },
                headers: {
                    get: function(k) { return null; },
                    forEach: function() {}
                },
                clone: function() { return this; }
            });
        }
    });
};

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
    use std::cell::RefCell;
    use std::rc::Rc;
    let shared: crate::bridge::SharedTree = Rc::new(RefCell::new(tree));
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
    // M66: 通过 EngineKind 创建引擎（trait 抽象层）。
    let esm_origin = if has_module {
        Some(origin.as_str())
    } else {
        None
    };
    #[allow(unused_mut)]
    let mut engine = engine_kind.create(esm_origin);
    let engine_name = engine.name();

    // M66-B: QuickJS 走独立执行路径（不经过 boa Context）。
    #[cfg(feature = "quickjs")]
    if engine_name == "quickjs" {
        return run_scripts_quickjs(shared, base_url, engine);
    }
    #[cfg(not(feature = "quickjs"))]
    if engine_name == "quickjs" {
        eprintln!("[js-runtime] QuickJS requested but feature not enabled, using boa");
    }

    // M71.1: boa 路径仅在 --features boa 时编译。无 boa 时不可能走到这里
    //（QuickJS 分支已 return，或 EngineKind 只有 QuickJs）。
    #[cfg(feature = "boa")]
    {
        #[allow(clippy::needless_return)]
        return run_scripts_with_base_boa(shared, base_url, engine, engine_name);
    }
    #[cfg(not(feature = "boa"))]
    {
        let _ = engine;
        let _ = engine_name;
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

    if trace_scripts {
        let diag = r#"(function() {
            if (globalThis.__objectDiagInstalled) return;
            globalThis.__objectDiagInstalled = true;
            if (typeof globalThis.__log !== 'function') return;

            function __diag(name, args) {
                var first = args && args.length > 0 ? args[0] : undefined;
                if (first === null) {
                    globalThis.__log('[diag] ' + name + ' first arg = null');
                } else if (first === undefined) {
                    globalThis.__log('[diag] ' + name + ' first arg = undefined');
                }
            }

            var _keys = Object.keys;
            Object.keys = function(obj) {
                __diag('Object.keys', arguments);
                return _keys(obj);
            };

            var _entries = Object.entries;
            Object.entries = function(obj) {
                __diag('Object.entries', arguments);
                return _entries(obj);
            };

            var _fromEntries = Object.fromEntries;
            if (typeof _fromEntries === 'function') {
                Object.fromEntries = function() {
                    __diag('Object.fromEntries', arguments);
                    return _fromEntries.apply(Object, arguments);
                };
            }

            var _values = Object.values;
            Object.values = function(obj) {
                __diag('Object.values', arguments);
                return _values(obj);
            };

            var _assign = Object.assign;
            Object.assign = function(target) {
                __diag('Object.assign', arguments);
                return _assign.apply(Object, arguments);
            };

            var _hasOwn = Object.prototype.hasOwnProperty;
            if (typeof _hasOwn === 'function') {
                Object.prototype.hasOwnProperty = function(prop) {
                    __diag('Object.prototype.hasOwnProperty', arguments);
                    return _hasOwn.call(this, prop);
                };
            }

            var _defineProperty = Object.defineProperty;
            Object.defineProperty = function(obj, prop, desc) {
                __diag('Object.defineProperty', arguments);
                return _defineProperty(obj, prop, desc);
            };

            var _defineProperties = Object.defineProperties;
            Object.defineProperties = function(obj, props) {
                __diag('Object.defineProperties', arguments);
                return _defineProperties(obj, props);
            };

            var _create = Object.create;
            Object.create = function(obj) {
                __diag('Object.create', arguments);
                return _create.apply(Object, arguments);
            };

            var _setPrototypeOf = Object.setPrototypeOf;
            Object.setPrototypeOf = function(obj, proto) {
                __diag('Object.setPrototypeOf', arguments);
                return _setPrototypeOf(obj, proto);
            };

            var _getPrototypeOf = Object.getPrototypeOf;
            Object.getPrototypeOf = function(obj) {
                __diag('Object.getPrototypeOf', arguments);
                return _getPrototypeOf(obj);
            };

            var _getOwnPropertyNames = Object.getOwnPropertyNames;
            Object.getOwnPropertyNames = function(obj) {
                __diag('Object.getOwnPropertyNames', arguments);
                return _getOwnPropertyNames(obj);
            };

            var _fromEntries = Object.fromEntries;
            if (typeof _fromEntries === 'function') {
                Object.fromEntries = function() {
                    __diag('Object.fromEntries', arguments);
                    return _fromEntries.apply(Object, arguments);
                };
            }

            var _reflectApply = Reflect.apply;
            if (typeof _reflectApply === 'function') {
                Reflect.apply = function() {
                    __diag('Reflect.apply', arguments);
                    return _reflectApply.apply(Reflect, arguments);
                };
            }

            var _reflectConstruct = Reflect.construct;
            if (typeof _reflectConstruct === 'function') {
                Reflect.construct = function() {
                    __diag('Reflect.construct', arguments);
                    return _reflectConstruct.apply(Reflect, arguments);
                };
            }

            if (typeof Array.from === 'function') {
                var _arrayFrom = Array.from;
                Array.from = function() {
                    __diag('Array.from', arguments);
                    return _arrayFrom.apply(Array, arguments);
                };
            }

            if (typeof Array.prototype.slice === 'function') {
                var _arraySlice = Array.prototype.slice;
                Array.prototype.slice = function() {
                    __diag('Array.prototype.slice', arguments);
                    return _arraySlice.apply(this, arguments);
                };
            }

            var _deleteProperty = Reflect.deleteProperty;
            if (typeof _deleteProperty === 'function') {
                Reflect.deleteProperty = function(obj, key) {
                    __diag('Reflect.deleteProperty', arguments);
                    return _deleteProperty(obj, key);
                };
            }
        })();"#;
        if let Err(e) = ctx.eval(boa_engine::Source::from_bytes(diag)) {
            eprintln!("[js-runtime] object-diag install failed: {e}");
        }
    }
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
    use std::cell::RefCell;
    use std::rc::Rc;

    let shared: crate::bridge::SharedTree = Rc::new(RefCell::new(tree));

    // ── QuickJS 分支 ──
    #[cfg(feature = "quickjs")]
    if matches!(engine_kind, crate::engine::EngineKind::QuickJs) {
        return eval_in_tree_quickjs(shared, base_url, expr);
    }
    // 非 QuickJS（boa）或 quickjs feature 未启用时的回退。
    let _ = engine_kind;

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
    if let Err(e) = engine.eval(&combined_shim) {
        eprintln!("[js-runtime] QuickJS combined shim install failed: {e}");
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
    let result = engine.eval_display_string(expr);
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
