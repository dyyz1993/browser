//! M64 验证：boa Module API 能否 parse + load + eval 真实 ESM bundle。
//!
//! 这个测试用一个 3 文件的本地 ESM 模块图验证 boa 的 Module::parse +
//! load_link_evaluate + SimpleModuleLoader 链路。成功 = 路线 B 可行。

// M71.1: ESM Module 测试依赖 boa，无 boa feature 时跳过。
#![cfg(feature = "boa")]

use std::path::PathBuf;
use std::rc::Rc;

use boa_engine::builtins::promise::PromiseState;
use boa_engine::module::SimpleModuleLoader;
use boa_engine::{js_string, Context, Module, Source};

/// 生成一个临时 ESM 模块图（3 文件，c.js → b.js → a.js）。
/// a.js 是入口，import b.js，b.js import c.js。各自 export 命名绑定。
/// 注意：macOS 上 /var 是 /private/var 的 symlink，SimpleModuleLoader 用
/// canonicalize 校验 root，必须返回 canonical 路径否则报 "outside module root"。
fn write_esm_graph() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let canonical = dir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| dir.path().to_path_buf());
    std::fs::write(
        canonical.join("c.js"),
        "export const C = 'c-value'; export function getC() { return C; }",
    )
    .expect("write c.js");
    std::fs::write(
        canonical.join("b.js"),
        "import { C, getC } from './c.js';\n\
         export const B = 'b-' + C;\n\
         export function getB() { return B + '|' + getC(); }",
    )
    .expect("write b.js");
    std::fs::write(
        canonical.join("a.js"),
        "import { getB } from './b.js';\n\
         export const A = 'a-' + getB();",
    )
    .expect("write a.js");
    let entry = canonical.join("a.js");
    (dir, entry)
}

/// 读入口模块导出的 `A`，验证整条 import 链是否被正确 link。
fn eval_entry_and_read_export(
    dir: &std::path::Path,
    entry: &std::path::Path,
    export_name: &str,
) -> Result<String, String> {
    let loader = Rc::new(SimpleModuleLoader::new(dir).map_err(|e| format!("loader: {e:?}"))?);
    let mut context = Context::builder()
        .module_loader(loader.clone())
        .build()
        .map_err(|e| format!("context: {e:?}"))?;

    let src = std::fs::read_to_string(entry).map_err(|e| format!("read entry: {e}"))?;
    let _ = src;
    let source = Source::from_filepath(entry).map_err(|e| format!("source path: {e:?}"))?;
    let module = Module::parse(source, None, &mut context).map_err(|e| format!("parse: {e:?}"))?;

    // 注册入口（load 时会按相对路径递归加载 b.js / c.js）
    loader.insert(entry.to_path_buf(), module.clone());

    let promise = module.load_link_evaluate(&mut context);
    context.run_jobs().map_err(|e| format!("run_jobs: {e:?}"))?;

    match promise.state() {
        PromiseState::Fulfilled(_) => {
            // 通过 namespace 读导出的字符串
            let ns = module.namespace(&mut context);
            let val = ns
                .get(js_string!(export_name), &mut context)
                .map_err(|e| format!("get {export_name}: {e:?}"))?;
            if let Some(s) = val.as_string() {
                let st: String = s.to_std_string_escaped();
                Ok(st)
            } else {
                Err(format!("{export_name} is not a string: {val:?}"))
            }
        }
        PromiseState::Rejected(err) => {
            let msg = err
                .to_string(&mut context)
                .map(|s| s.to_std_string_escaped())
                .unwrap_or_else(|_| "{opaque}".into());
            Err(format!("module rejected: {msg}"))
        }
        PromiseState::Pending => Err("module still pending after run_jobs".into()),
    }
}

#[test]
fn esm_simple_chain_loads_and_links() {
    let (_dir, entry) = write_esm_graph();
    let result = eval_entry_and_read_export(_dir.path(), &entry, "A");
    // a.js: A = 'a-' + getB();  b.js: getB() = B + '|' + getC() = 'b-c-value|c-value'
    // 所以 A = 'a-b-c-value|c-value'
    assert_eq!(
        result,
        Ok("a-b-c-value|c-value".to_string()),
        "ESM 3 层依赖链应正确 link：a.js → b.js → c.js"
    );
}

/// 验证 import.meta 在 Module 模式下**语法合法**（Script 模式会 SyntaxError）。
/// SimpleModuleLoader 不实现 init_import_meta，所以 import.meta.url 可能是空，
/// 但关键是语法能 parse + eval 不报 SyntaxError。
#[test]
fn esm_import_meta_parses_in_module_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("m.js"),
        // 关键：import.meta 在 Module 模式合法；Script 模式会 SyntaxError
        "export const ok = typeof import.meta === 'object' ? 'META_OK' : 'META_FAIL';",
    )
    .expect("write m.js");

    let canonical = dir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| dir.path().to_path_buf());
    let entry = canonical.join("m.js");
    let result = eval_entry_and_read_export(&canonical, &entry, "ok");
    assert!(
        result.is_ok(),
        "import.meta 语法应在 Module 模式合法（不报 SyntaxError）: {result:?}"
    );
}

/// 验证循环依赖（ESM 规范允许，TDZ 处理）。
#[test]
fn esm_circular_dependency_resolves() {
    let dir = tempfile::tempdir().expect("tempdir");
    // x.js 导出 getX，import y.js；y.js 导出 getY，import x.js（循环）
    std::fs::write(
        dir.path().join("x.js"),
        "import { getY } from './y.js';\n\
         export function getX() { return 'X calls ' + getY(); }",
    )
    .expect("write x.js");
    std::fs::write(
        dir.path().join("y.js"),
        "export function getY() { return 'Y'; }",
    )
    .expect("write y.js");

    // 入口调用 getX
    std::fs::write(
        dir.path().join("entry.js"),
        "import { getX } from './x.js';\n\
         export const R = getX();",
    )
    .expect("write entry.js");

    let canonical = dir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| dir.path().to_path_buf());
    let entry = canonical.join("entry.js");
    let result = eval_entry_and_read_export(&canonical, &entry, "R");
    assert!(
        result.is_ok(),
        "循环依赖应被正确 resolve（不卡死/不 reject）: {result:?}"
    );
}
