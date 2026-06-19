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

    /// M65: 预扫描入口模块的依赖图，并行 fetch 所有 chunk 到缓存。
    /// 解析入口源码找 `from"./xxx"` 引用，递归扫描（最多 3 层），收集所有 URL，
    /// 然后用线程池并行 fetch + 写临时文件。后续 Module::parse 的 load_imported_module
    /// 全部缓存命中（无需串行 fetch）。
    pub fn prefetch_dependencies(&self, entry_url: &str) {
        use std::sync::mpsc;
        let mut to_fetch = vec![entry_url.to_string()];
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        visited.insert(entry_url.to_string());
        let mut all_urls: Vec<String> = Vec::new();
        // BFS 扫描依赖图（fetch 入口 → 找 import → fetch 子 chunk → 找 import...）
        let max_depth = 4;
        for _depth in 0..max_depth {
            if to_fetch.is_empty() {
                break;
            }
            let batch: Vec<String> = std::mem::take(&mut to_fetch);
            // M65: 单 runtime + 单 client 并发 fetch（HTTP/2 多路复用）
            let (tx, rx) = mpsc::channel();
            let origin = self.origin.clone();
            let batch_clone = batch.clone();
            let _join = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("esm prefetch runtime");
                let client = browser_net::HttpClient::new();
                let tx = std::sync::Arc::new(std::sync::Mutex::new(tx));
                rt.block_on(async {
                    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
                    let mut tasks = Vec::new();
                    for url in &batch_clone {
                        let permit = sem.clone().acquire_owned().await.unwrap();
                        let client = client.clone();
                        let tx = tx.clone();
                        let url = url.clone();
                        tasks.push(tokio::spawn(async move {
                            let _permit = permit;
                            if let Ok(bytes) = client.get(&url).await {
                                if let Ok(body) = String::from_utf8(bytes) {
                                    if let Ok(t) = tx.lock() {
                                        let _ = t.send((url, Ok(body)));
                                    }
                                    return;
                                }
                            }
                            if let Ok(t) = tx.lock() {
                                let _ = t.send((url, Err("fetch failed".to_string())));
                            }
                        }));
                    }
                    for t in tasks {
                        let _ = t.await;
                    }
                });
            })
            .join();
            let _ = origin;
            // tx 被 thread move 走了，thread 结束时 drop → rx.iter() 退出
            for (url, result) in rx.iter() {
                if let Ok(source) = result {
                    all_urls.push(url.clone());
                    // 扫描子依赖
                    for spec in extract_import_specifiers(&source) {
                        let child_url = self.resolve_url(&spec, Some(&url));
                        if !visited.contains(&child_url) {
                            visited.insert(child_url.clone());
                            to_fetch.push(child_url);
                        }
                    }
                    // 预写临时文件（load_module 会缓存命中）
                    let temp_path = self.url_to_temp_path(&url);
                    if let Some(parent) = temp_path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(&temp_path, &source);
                }
            }
        }
    }

    /// 加载入口模块 + load_link_evaluate。
    /// M65: 先并行预取所有 chunk，再 Module::parse（避免串行 fetch）。
    pub fn load_and_eval(&self, entry_url: &str, context: &mut Context) -> JsResult<()> {
        // M65: 并行预取依赖图（BFS 扫描 import → 线程池并行 fetch）
        self.prefetch_dependencies(entry_url);
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
    /// M65: 如果 prefetch_dependencies 已预写临时文件，直接读文件（跳过 fetch）。
    fn load_module(&self, url: &str, context: &mut Context) -> JsResult<Module> {
        // 缓存命中
        if let Some(m) = self.modules.borrow().get(url).cloned() {
            return Ok(m);
        }

        // M65: 检查临时文件是否已由 prefetch 预写
        let temp_path = self.url_to_temp_path(url);
        let source_text = if temp_path.exists() {
            std::fs::read_to_string(&temp_path).unwrap_or_else(|_| {
                // 预写文件读取失败，退回 fetch
                fetch_sync(url).unwrap_or_default()
            })
        } else {
            fetch_sync(url)
                .map_err(|e| JsNativeError::typ().with_message(format!("esm fetch `{url}`: {e}")))?
        };

        // 写临时文件（如果 prefetch 没写过）
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

/// 同步 HTTP fetch（独立 spawn + 新 HttpClient，避免 net worker 的压缩解码问题）。
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

/// M65: 从 JS 源码提取静态 import 的 specifier（`from"./xxx"`）。
/// 用于 prefetch_dependencies 的依赖图扫描。只找相对路径的 import。
fn extract_import_specifiers(source: &str) -> Vec<String> {
    let mut specs = Vec::new();
    // 匹配 from"./xxx" 或 from'./xxx'（minified ESM 标准）
    let bytes = source.as_bytes();
    let mut i = 0;
    while i + 6 < bytes.len() {
        if &bytes[i..i + 4] == b"from" {
            // 跳过空格
            let mut j = i + 4;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
                let quote = bytes[j];
                let start = j + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end] != quote {
                    end += 1;
                }
                if end < bytes.len() {
                    let spec = &source[start..end];
                    if spec.starts_with("./") || spec.starts_with("../") {
                        specs.push(spec.to_string());
                    }
                }
            }
        }
        i += 1;
    }
    specs
}

// 避免未使用警告
#[allow(dead_code)]
fn _unused(_v: JsValue, _c: Component) {}
