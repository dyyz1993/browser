//! Input domain for CDP (M59.2 → M80.17).
//!
//! M59.2: no-op ack（文本渲染器无鼠标事件）。
//! M80.17: `Input.dispatchMouseEvent` 接入真实点击——
//!
//! 1. `mousePressed` + `button:left` + x/y → `browser_render::pixel::hit_test`
//!    （CSS px → 最深元素 NodeId，批 47 基建）
//! 2. 在该 Page 的共享树引擎会话里执行合成点击 JS
//!    （`run_scripts_with_post_exprs`：重跑页面脚本注册监听器 → 泵事件循环 →
//!    同会话 eval `__makeElement(nodeId).dispatchEvent(new MouseEvent('click'))`
//!    → 点击后再泵一轮事件循环，drain 回调里的 setTimeout/fetch）。
//!    对齐 CLI `--click`（M81 click_post_exprs）模式。
//! 3. `mouseReleased` / `mouseMoved` → ack（点击语义在 pressed 已完整；
//!    Playwright/Puppeteer 的 moved→pressed→released 序列只取 pressed）。
//! 4. 无布局树（未 navigate）/ 坐标未命中任何元素 → ack 不报错
//!    （CDP 惯例：input 事件无需可视反馈）。

use std::sync::{Arc, Mutex};

use browser_dom::{NodeData, NodeId, Tree};
use browser_js_runtime::EngineKind;

use crate::jsonrpc::{CdpError, CdpMessage, Json};
use crate::page::PageState;

/// hit_test 命中的可能是**文本节点**：布局引擎把 Text 盒也标注节点 id，且
/// 按钮/链接等 inline 元素的实际尺寸常落在文本子盒上（元素自身盒是零尺寸，
/// hit_test 跳过零尺寸盒 → 命中文本）。DOM 事件语义里 target 必须是元素
/// ——沿父链爬到最近的 Element（含自身）再合成点击。
fn nearest_element(tree: &Tree, mut id: NodeId) -> NodeId {
    loop {
        if matches!(tree.data(id), NodeData::Element { .. }) {
            return id;
        }
        match tree.get(id).parent {
            Some(p) => id = p,
            None => return id,
        }
    }
}

/// Dispatch Input domain commands.
///
/// Supported methods:
/// - `Input.dispatchMouseEvent`: mousePressed(left) → 真实合成点击；其余 ack
/// - `Input.dispatchKeyEvent` / `Input.insertText` / `Input.setInterceptDrags`: ack
pub async fn dispatch(
    id: i64,
    method: &str,
    params: Option<&Json>,
    state: Arc<Mutex<PageState>>,
    engine_kind: EngineKind,
) -> Result<String, CdpError> {
    match method {
        "Input.dispatchMouseEvent" => dispatch_mouse_event(id, params, state, engine_kind).await,
        "Input.dispatchKeyEvent" | "Input.insertText" | "Input.setInterceptDrags" => {
            Ok(CdpMessage::ok_empty(id))
        }
        _ => Err(CdpError::MethodNotFound(method.to_string())),
    }
}

/// `Input.dispatchMouseEvent`：仅 `mousePressed` + `button:"left"` 触发真实点击。
async fn dispatch_mouse_event(
    id: i64,
    params: Option<&Json>,
    state: Arc<Mutex<PageState>>,
    engine_kind: EngineKind,
) -> Result<String, CdpError> {
    let ack = Ok(CdpMessage::ok_empty(id));
    let Some(p) = params else {
        return ack; // 无参数（早期客户端）→ ack
    };
    let ev_type = p.get_str("type").unwrap_or("");
    // 点击语义在 pressed 已完整（released/moved 只是序列补齐）；非左键无操作。
    if ev_type != "mousePressed" || p.get_str("button").unwrap_or("none") != "left" {
        return ack;
    }
    // 坐标可能带小数（Playwright 传元素中心）——hit_test 前取整。
    let (Some(Json::Number(xf)), Some(Json::Number(yf))) = (p.get("x"), p.get("y")) else {
        return ack; // 无坐标无从命中
    };
    let x_px = xf.round() as f32;
    let y_px = yf.round() as f32;

    // 锁内：hit_test + 取 tree/url 克隆（锁不可跨 await——点击走 spawn_blocking）。
    let (node_id, tree, url) = {
        let st = state
            .lock()
            .map_err(|e| CdpError::Io(format!("lock: {e}")))?;
        let Some(layout) = st.layout.as_ref() else {
            return ack; // 未 navigate（无布局树）→ ack
        };
        match browser_render::pixel::hit_test(layout, x_px, y_px) {
            // 文本盒命中 → 爬到最近元素（DOM 事件 target 语义，见 nearest_element）。
            Some(node_id) => {
                let el = nearest_element(&st.tree, node_id);
                (el, st.tree.clone(), st.url.clone())
            }
            None => return ack, // 命中空白（无元素盒）→ ack
        }
    };
    eprintln!("[cdp] input: click node {node_id} at ({x_px},{y_px})");
    if !matches!(engine_kind, EngineKind::QuickJs) {
        eprintln!("[cdp] input: synthesized click not supported on boa engine (ignored)");
    }

    // 合成点击表达式：__makeElement(nodeId) → MouseEvent('click') → dispatchEvent。
    // 返回约定（js-runtime eval_i32 读回打日志）：-1 = 包装失败；否则 nodeId。
    // 三阶段 dispatch（capture→target→bubble）+ on* 属性 handler 均由 shim 完成；
    // onclick 属性从 DOM 属性表现读（跨会话可用），addEventListener 监听器靠
    // run_scripts_with_post_exprs 重跑页面脚本在本会话重新注册。
    let expr = format!(
        "(function(){{var el=window.__makeElement({node_id});\
         if(!el||typeof el.dispatchEvent!=='function'){{return -1;}}\
         var ev=new MouseEvent('click',{{bubbles:true,cancelable:true,view:window,\
         clientX:{x_px},clientY:{y_px}}});\
         el.dispatchEvent(ev);return {node_id};}})()"
    );

    // M68 同款隔离：SharedTree !Send + 引擎阻塞（含点击后事件泵 ≤500ms），
    // 必须 spawn_blocking；catch_unwind 防 JS panic 杀 server——panic 时回退
    // 原 tree（点击失败但不丢页面状态）。
    let url_opt = if url.is_empty() { None } else { Some(url) };
    let fallback = tree.clone();
    let js_tree = tokio::task::spawn_blocking(move || {
        use std::panic::AssertUnwindSafe;
        match std::panic::catch_unwind(AssertUnwindSafe(|| {
            browser_js_runtime::run_scripts_with_post_exprs(tree, url_opt, &engine_kind, &[expr])
        })) {
            Ok((shared, _n)) => shared.borrow().clone(),
            Err(_) => {
                eprintln!("[cdp] input: click js panicked — keeping previous DOM");
                fallback
            }
        }
    })
    .await
    .map_err(|e| CdpError::Io(format!("click task join: {e}")))?;

    // JS 改过的 DOM 回写 + 重新 layout+render（刷新布局树供下次 hit_test）。
    let mut st = state
        .lock()
        .map_err(|e| CdpError::Io(format!("lock: {e}")))?;
    st.tree = js_tree;
    st.render_from_tree();
    Ok(CdpMessage::ok_empty(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 递归找第一个 tag 匹配的元素节点（测试辅助）。
    fn find_tag(
        tree: &browser_dom::Tree,
        id: browser_dom::NodeId,
        tag: &str,
    ) -> Option<browser_dom::NodeId> {
        if let browser_dom::NodeData::Element { tag: t, .. } = tree.data(id) {
            if t.eq_ignore_ascii_case(tag) {
                return Some(id);
            }
        }
        for &child in tree.children_of(id) {
            if let Some(found) = find_tag(tree, child, tag) {
                return Some(found);
            }
        }
        None
    }

    /// 目标节点的 DOM 子树 id 集（inline 元素的真实尺寸盒常挂在文本子节点上）。
    fn subtree_ids(
        tree: &browser_dom::Tree,
        id: browser_dom::NodeId,
        out: &mut Vec<browser_dom::NodeId>,
    ) {
        out.push(id);
        for &child in tree.children_of(id) {
            subtree_ids(tree, child, out);
        }
    }

    /// 在布局树里找子树内节点的格单位盒中心（取**最深**非零尺寸盒）。
    fn box_center_subtree(
        bx: &browser_layout::LayoutBox,
        ids: &[browser_dom::NodeId],
        best: &mut Option<(f32, f32)>,
    ) {
        let d = &bx.dimensions;
        if bx.element_id.is_some_and(|e| ids.contains(&e)) && d.width > 0.0 && d.height > 0.0 {
            *best = Some((d.x + d.width / 2.0, d.y + d.height / 2.0));
        }
        for child in &bx.children {
            box_center_subtree(child, ids, best);
        }
    }

    /// 元素子树的布局中心（格单位 → CSS px），并断言 hit_test 命中后经
    /// nearest_element 爬升解析回目标元素（测试坐标与生产链路契约一致）。
    fn center_and_verify(st: &PageState, target: browser_dom::NodeId) -> (f32, f32) {
        let layout = st.layout.as_ref().expect("layout after render");
        let mut ids = Vec::new();
        subtree_ids(&st.tree, target, &mut ids);
        let mut best = None;
        box_center_subtree(&layout.root, &ids, &mut best);
        let (gx, gy) = best.expect("subtree box with size");
        let (cw, lh) = browser_render::pixel::cell_metrics();
        let (cx, cy) = (gx * cw, gy * lh);
        let hit =
            browser_render::pixel::hit_test(layout, cx, cy).map(|n| nearest_element(&st.tree, n));
        assert_eq!(
            hit,
            Some(target),
            "hit_test(center) must resolve to target element {target}"
        );
        (cx, cy)
    }

    fn params_json(s: &str) -> Json {
        crate::jsonrpc::parse_message(&format!(
            r#"{{"id":1,"method":"Input.dispatchMouseEvent","params":{s}}}"#
        ))
        .ok()
        .and_then(|m| m.params)
        .expect("params json")
    }

    fn page_with(html: &str) -> Arc<Mutex<PageState>> {
        let mut st = PageState::default();
        st.parse_only(html, "http://example.com/", 80);
        st.render_from_tree();
        Arc::new(Mutex::new(st))
    }

    #[tokio::test]
    async fn test_dispatch_key_event_ack() {
        let page = Arc::new(Mutex::new(PageState::default()));
        let resp = dispatch(2, "Input.dispatchKeyEvent", None, page, EngineKind::QuickJs)
            .await
            .unwrap();
        assert_eq!(resp, r#"{"id":2,"result":{}}"#);
    }

    #[tokio::test]
    async fn test_set_intercept_drags_ack() {
        let page = Arc::new(Mutex::new(PageState::default()));
        let resp = dispatch(
            3,
            "Input.setInterceptDrags",
            None,
            page,
            EngineKind::QuickJs,
        )
        .await
        .unwrap();
        assert_eq!(resp, r#"{"id":3,"result":{}}"#);
    }

    #[tokio::test]
    async fn test_unknown_method_returns_error() {
        let page = Arc::new(Mutex::new(PageState::default()));
        let resp = dispatch(4, "Input.unknown", None, page, EngineKind::QuickJs).await;
        assert!(matches!(resp, Err(CdpError::MethodNotFound(_))));
    }

    #[tokio::test]
    async fn test_mouse_moved_and_released_ack() {
        let page = page_with("<html><body><button id='b'>Go</button></body></html>");
        let moved = params_json(r#"{"type":"mouseMoved","x":10,"y":10}"#);
        let resp = dispatch(
            5,
            "Input.dispatchMouseEvent",
            Some(&moved),
            page.clone(),
            EngineKind::QuickJs,
        )
        .await
        .unwrap();
        assert_eq!(resp, r#"{"id":5,"result":{}}"#);
        let released =
            params_json(r#"{"type":"mouseReleased","x":10,"y":10,"button":"left","clickCount":1}"#);
        let resp = dispatch(
            6,
            "Input.dispatchMouseEvent",
            Some(&released),
            page,
            EngineKind::QuickJs,
        )
        .await
        .unwrap();
        assert_eq!(resp, r#"{"id":6,"result":{}}"#);
    }

    #[tokio::test]
    async fn test_mouse_pressed_without_layout_acks() {
        // 未 navigate（无布局树）→ ack 不报错。
        let page = Arc::new(Mutex::new(PageState::default()));
        let p =
            params_json(r#"{"type":"mousePressed","x":5,"y":5,"button":"left","clickCount":1}"#);
        let resp = dispatch(
            7,
            "Input.dispatchMouseEvent",
            Some(&p),
            page,
            EngineKind::QuickJs,
        )
        .await
        .unwrap();
        assert_eq!(resp, r#"{"id":7,"result":{}}"#);
    }

    #[tokio::test]
    async fn test_mouse_pressed_non_left_or_no_coords_acks() {
        let page = page_with("<html><body><button id='b'>Go</button></body></html>");
        // 右键 → ack
        let right =
            params_json(r#"{"type":"mousePressed","x":5,"y":5,"button":"right","clickCount":1}"#);
        let resp = dispatch(
            8,
            "Input.dispatchMouseEvent",
            Some(&right),
            page.clone(),
            EngineKind::QuickJs,
        )
        .await
        .unwrap();
        assert_eq!(resp, r#"{"id":8,"result":{}}"#);
        // 无坐标 → ack
        let noc = params_json(r#"{"type":"mousePressed","button":"left"}"#);
        let resp = dispatch(
            9,
            "Input.dispatchMouseEvent",
            Some(&noc),
            page,
            EngineKind::QuickJs,
        )
        .await
        .unwrap();
        assert_eq!(resp, r#"{"id":9,"result":{}}"#);
    }

    /// M80.17 核心链路：mousePressed 命中按钮 → onclick 改 DOM →
    /// 重新渲染的 rendered_text 反映改动。
    #[tokio::test]
    async fn test_mouse_pressed_clicks_button_updates_dom() {
        let html = r#"<html><body><button id="b" onclick="document.getElementById('out').textContent='CLICKED-OK'">Go</button><div id="out">INIT</div></body></html>"#;
        let page = page_with(html);

        // 定位按钮盒子中心（CSS px），证明点击走的是 hit_test 路径。
        let (btn, (cx, cy)) = {
            let st = page.lock().unwrap();
            let btn = find_tag(&st.tree, st.tree.root(), "button").expect("button node");
            let c = center_and_verify(&st, btn);
            (btn, c)
        };

        let pressed = params_json(&format!(
            r#"{{"type":"mousePressed","x":{cx},"y":{cy},"button":"left","clickCount":1}}"#
        ));
        let resp = dispatch(
            10,
            "Input.dispatchMouseEvent",
            Some(&pressed),
            page.clone(),
            EngineKind::QuickJs,
        )
        .await
        .expect("pressed dispatch ok");
        assert_eq!(resp, r#"{"id":10,"result":{}}"#);

        // Playwright 序列补齐：released → ack。
        let released = params_json(&format!(
            r#"{{"type":"mouseReleased","x":{cx},"y":{cy},"button":"left","clickCount":1}}"#
        ));
        dispatch(
            11,
            "Input.dispatchMouseEvent",
            Some(&released),
            page.clone(),
            EngineKind::QuickJs,
        )
        .await
        .expect("released dispatch ok");

        let st = page.lock().unwrap();
        assert!(
            st.rendered_text.contains("CLICKED-OK"),
            "onclick must fire via synthesized click on node {btn}, got: {}",
            st.rendered_text
        );
        assert!(!st.rendered_text.contains("INIT"), "old text replaced");
    }

    /// 点击回调排 setTimeout(0) 改 DOM —— 点击后事件泵应 drain 到。
    #[tokio::test]
    async fn test_click_handler_settimeout_drained() {
        let html = r#"<html><body><button id="b" onclick="setTimeout(function(){document.getElementById('out').textContent='ASYNC-CLICKED';},0)">Go</button><div id="out">WAIT</div></body></html>"#;
        let page = page_with(html);
        let (cx, cy) = {
            let st = page.lock().unwrap();
            let btn = find_tag(&st.tree, st.tree.root(), "button").unwrap();
            center_and_verify(&st, btn)
        };
        let pressed = params_json(&format!(
            r#"{{"type":"mousePressed","x":{cx},"y":{cy},"button":"left","clickCount":1}}"#
        ));
        dispatch(
            12,
            "Input.dispatchMouseEvent",
            Some(&pressed),
            page.clone(),
            EngineKind::QuickJs,
        )
        .await
        .unwrap();
        let st = page.lock().unwrap();
        assert!(
            st.rendered_text.contains("ASYNC-CLICKED"),
            "setTimeout(0) in click handler must be drained by post-click pump, got: {}",
            st.rendered_text
        );
    }

    /// addEventListener 注册的监听器（页面脚本注册）同样被触发。
    #[tokio::test]
    async fn test_click_triggers_add_event_listener_listener() {
        let html = r#"<html><body><button id="b">Go</button><div id="out">BEFORE</div>
<script>document.getElementById('b').addEventListener('click',function(){document.getElementById('out').textContent='LISTENER-FIRED';});</script>
</body></html>"#;
        let page = page_with(html);
        let (cx, cy) = {
            let st = page.lock().unwrap();
            let btn = find_tag(&st.tree, st.tree.root(), "button").unwrap();
            center_and_verify(&st, btn)
        };
        let pressed = params_json(&format!(
            r#"{{"type":"mousePressed","x":{cx},"y":{cy},"button":"left","clickCount":1}}"#
        ));
        dispatch(
            13,
            "Input.dispatchMouseEvent",
            Some(&pressed),
            page.clone(),
            EngineKind::QuickJs,
        )
        .await
        .unwrap();
        let st = page.lock().unwrap();
        assert!(
            st.rendered_text.contains("LISTENER-FIRED"),
            "page-script addEventListener listener must fire, got: {}",
            st.rendered_text
        );
    }

    /// 坐标带小数（Playwright 传元素中心）→ 取整后命中。
    #[tokio::test]
    async fn test_fractional_coords_hit_after_rounding() {
        let html = r#"<html><body><button id="b" onclick="document.getElementById('out').textContent='FRAC-OK'">Go</button><div id="out">NO</div></body></html>"#;
        let page = page_with(html);
        let (cx, cy) = {
            let st = page.lock().unwrap();
            let btn = find_tag(&st.tree, st.tree.root(), "button").unwrap();
            center_and_verify(&st, btn)
        };
        // 中心坐标加 0.6 偏移（仍在盒子内，但带小数）。
        let pressed = params_json(&format!(
            r#"{{"type":"mousePressed","x":{},"y":{},"button":"left","clickCount":1}}"#,
            cx + 0.6,
            cy + 0.6
        ));
        dispatch(
            14,
            "Input.dispatchMouseEvent",
            Some(&pressed),
            page.clone(),
            EngineKind::QuickJs,
        )
        .await
        .unwrap();
        let st = page.lock().unwrap();
        assert!(
            st.rendered_text.contains("FRAC-OK"),
            "fractional coords must hit after rounding, got: {}",
            st.rendered_text
        );
    }
}
