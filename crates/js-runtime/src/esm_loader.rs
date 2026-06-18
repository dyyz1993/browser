//! M64: HTTP ModuleLoader —— 让 boa 能执行 `<script type="module">` 的 ESM bundle。
//!
//! 实现 boa 的 `ModuleLoader` trait。在 `load_imported_module` 时同步 HTTP fetch
//! 拉取 chunk → 写临时文件（保留 path 供 referrer 解析）→ `Module::parse`。
//! boa 自动处理依赖图解析、实例化、链接、循环依赖。
//!
//! 关键设计：
//! - `Module::parse` 用 `Source::from_filepath`（带 path），boa 用 path 作 referrer
//!   解析相对 import（`./chunks/x.js`）。所以 chunk 要写临时文件保留目录结构。
//! - `load_imported_module` 是 async fn，内部同步执行（spawn 线程 + block_on），
//!   返回 ready future（与 boa `SimpleModuleLoader` 同模式）。
//! - `origin` 是站点根 URL，import 的绝对 URL 由 origin + 相对 path 拼成。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use boa_engine::module::{Module, ModuleLoader, ModuleRequest, Referrer};
use boa_engine::{js_string, Context, JsNativeError, JsObject, JsResult, JsValue, Source};

/// M64: HTTP ModuleLoader。
///
/// 用法：
/// ```ignore
/// let loader = Rc::new(HttpModuleLoader::new("https://vite.dev"));
/// let mut ctx = Context::builder().module_loader(loader.clone()).build()?;
/// loader.load_and_eval("https://vite.dev/assets/app.js", &mut ctx)?;
/// ctx.run_jobs()?;
/// ```
pub struct HttpModuleLoader {
    /// 站点根 origin（如 `https://vite.dev`），用于 chunk URL 拼接。
    origin: String,
    /// 临时文件根目录（chunk 写这里，保留目录结构供 referrer 解析）。
    temp_root: PathBuf,
    /// 已 parse 的模块缓存：绝对 URL → Module。
    modules: RefCell<HashMap<String, Module>>,
}

impl HttpModuleLoader {
    #[must_use]
    pub fn new(origin: &str) -> Self {
        let origin = origin.trim_end_matches('/').to_string();
        let temp_root = std::env::temp_dir().join("browser_esm_modules");
        let _ = std::fs::create_dir_all(&temp_root);
        Self {
            origin,
            temp_root,
            modules: RefCell::new(HashMap::new()),
        }
    }

    /// 加载入口模块 + load_link_evaluate。
    pub fn load_and_eval(&self, entry_url: &str, context: &mut Context) -> JsResult<()> {
        let module = self.load_module(entry_url, context)?;
        let promise = module.load_link_evaluate(context);
        context.run_jobs()?;
        match promise.state() {
            boa_engine::builtins::promise::PromiseState::Fulfilled(_) => Ok(()),
            boa_engine::builtins::promise::PromiseState::Rejected(err) => {
                let msg = err
                    .to_string(context)
                    .map(|s| s.to_std_string_escaped())
                    .unwrap_or_else(|_| "{opaque}".into());
                // 尝试提取 stack
                let stack = if let Some(obj) = err.as_object() {
                    let stack_val = obj
                        .get(js_string!("stack"), context)
                        .unwrap_or(JsValue::undefined());
                    if let Some(s) = stack_val.as_string() {
                        s.to_std_string_escaped()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };
                eprintln!(
                    "[esm] eval rejected: {msg}{}",
                    if stack.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "\n  stack: {}",
                            stack.lines().take(5).collect::<Vec<_>>().join("\n  ")
                        )
                    }
                );
                Err(boa_engine::JsError::from_native(
                    JsNativeError::typ().with_message(format!("esm eval failed: {msg}")),
                ))
            }
            boa_engine::builtins::promise::PromiseState::Pending => {
                Err(boa_engine::JsError::from_native(
                    JsNativeError::typ().with_message("esm eval still pending"),
                ))
            }
        }
    }

    /// 加载单个模块：fetch → 写临时文件 → parse → 缓存。返回 Module。
    fn load_module(&self, url: &str, context: &mut Context) -> JsResult<Module> {
        // 缓存命中
        if let Some(m) = self.modules.borrow().get(url).cloned() {
            return Ok(m);
        }

        let source_text = fetch_sync(url)
            .map_err(|e| JsNativeError::typ().with_message(format!("esm fetch `{url}`: {e}")))?;

        // 写临时文件，保留 path 供 referrer 解析。
        // URL → 相对 path → 临时文件路径。
        let temp_path = self.url_to_temp_path(url);
        if let Some(parent) = temp_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&temp_path, &source_text).map_err(|e| {
            JsNativeError::typ().with_message(format!("esm write temp `{url}`: {e}"))
        })?;

        let source = Source::from_filepath(&temp_path).map_err(|e| {
            JsNativeError::typ().with_message(format!("esm open temp `{url}`: {e:?}"))
        })?;

        let module = Module::parse(source, None, context)
            .map_err(|e| JsNativeError::syntax().with_message(format!("esm parse `{url}`: {e}")))?;

        self.modules
            .borrow_mut()
            .insert(url.to_string(), module.clone());
        Ok(module)
    }

    /// URL → 临时文件路径（保留目录结构）。
    /// `https://vite.dev/assets/chunks/x.js` → `/tmp/browser_esm_modules/assets/chunks/x.js`
    fn url_to_temp_path(&self, url: &str) -> PathBuf {
        let relative = url
            .strip_prefix(&self.origin)
            .unwrap_or(url)
            .trim_start_matches('/');
        self.temp_root.join(relative)
    }

    /// 相对 specifier + referrer URL → 绝对 URL（HTTP URL 解析，非 PathBuf）。
    /// `./chunks/x.js` + referrer `https://vite.dev/assets/app.js`
    /// → `https://vite.dev/assets/chunks/x.js`
    fn resolve_url(&self, specifier: &str, referrer_url: Option<&str>) -> String {
        if specifier.starts_with("http://") || specifier.starts_with("https://") {
            return specifier.to_string();
        }
        if let Some(ref_url) = referrer_url {
            // 简单 URL 解析：取 referrer 的目录 + specifier
            let base_dir = ref_url.rfind('/').map(|i| &ref_url[..i]).unwrap_or(ref_url);
            if let Some(stripped) = specifier.strip_prefix("./") {
                return format!("{base_dir}/{stripped}");
            }
            if let Some(stripped) = specifier.strip_prefix("../") {
                // 回退一层
                let parent = base_dir
                    .rfind('/')
                    .map(|i| &base_dir[..i])
                    .unwrap_or(base_dir);
                return format!("{parent}/{stripped}");
            }
            return format!("{base_dir}/{specifier}");
        }
        format!("{}/{}", self.origin, specifier.trim_start_matches("./"))
    }

    /// 从 referrer path（临时文件路径）反推回绝对 URL。
    /// `/tmp/browser_esm_modules/assets/app.js` → `https://vite.dev/assets/app.js`
    fn temp_path_to_url(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.temp_root).ok()?;
        Some(format!("{}/{}", self.origin, rel.to_string_lossy()))
    }
}

impl ModuleLoader for HttpModuleLoader {
    fn load_imported_module(
        self: Rc<Self>,
        referrer: Referrer,
        request: ModuleRequest,
        context: &RefCell<&mut Context>,
    ) -> impl std::future::Future<Output = JsResult<Module>> {
        let result = (|| {
            let specifier = request.specifier().to_std_string_escaped();
            // referrer 的临时文件 path → 绝对 URL
            let referrer_url = referrer.path().and_then(|p| self.temp_path_to_url(p));
            let resolved_url = self.resolve_url(&specifier, referrer_url.as_deref());

            // 缓存命中
            if let Some(m) = self.modules.borrow().get(&resolved_url).cloned() {
                return Ok(m);
            }

            self.load_module(&resolved_url, &mut context.borrow_mut())
        })();
        async move { result }
    }

    /// M64: 注入 import.meta.url。
    fn init_import_meta(
        self: Rc<Self>,
        import_meta: &JsObject,
        module: &Module,
        context: &mut Context,
    ) {
        if let Some(path) = module.path() {
            if let Some(url) = self.temp_path_to_url(path) {
                let _ = import_meta.create_data_property_or_throw(
                    js_string!("url"),
                    js_string!(url.as_str()),
                    context,
                );
            }
        }
    }
}

/// 同步 HTTP fetch（spawn 线程 + 独立 tokio runtime，与 fetch_external_script 同模式）。
fn fetch_sync(url: &str) -> Result<String, String> {
    let url = url.to_string();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("tokio build: {e}"))?;
        let client = browser_net::HttpClient::new();
        let bytes = rt
            .block_on(client.get(&url))
            .map_err(|e| format!("{e:?}"))?;
        String::from_utf8(bytes).map_err(|e| format!("non-utf8: {e}"))
    });
    handle
        .join()
        .map_err(|_| "esm fetch thread panicked".to_string())?
}

// 避免未使用警告
#[allow(dead_code)]
fn _unused(_v: JsValue, _c: Component) {}
