//! M96.18/ADR-0007: Chrome 同源 TLS 通道（`--features chrome-tls`）。
//!
//! 背景（MITM 判别实验铁证）：xcancel antibot 对 verify 请求按传输层
//! TLS ClientHello 形状评分——Chrome 形（curl_cffi）→ 200，rustls → 403
//! （同 payload/同头组单变量对照）。本模块用 **boring**（BoringSSL =
//! Chromium 同源 TLS 库）+ h2 crate 提供原生 Chrome 形传输：
//!
//! - TLS：BoringSSL 的 ClientHello（密码套件/扩展与 Chrome 同源）
//! - ALPN：h2 + http/1.1
//! - H2：连接复用（同 host 多请求单连接多路复用），SETTINGS 对齐
//!   Chrome 值（initial window 6291456 / frame 16384）
//! - 代理：读 https_proxy/http_proxy 环境变量走 CONNECT 隧道（与
//!   reqwest 环境代理语义一致——CLI --proxy 即设此变量）
//! - 压缩：第一版强制 `accept-encoding: identity`（解压零依赖；
//!   Chrome 形压缩协商后续迭代）
//!
//! 失败语义：调用方（[`crate::HttpClient::request_full_raw_hdr`]）在
//! 本通道 Err 时回退 reqwest 通道——保「能打开」优先。

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

type H2Sender = h2::client::SendRequest<bytes::Bytes>;

static CONNS: Mutex<Option<HashMap<String, H2Sender>>> = Mutex::new(None);

/// M96.18: boring 通道常驻 runtime——连接驱动任务（h2 conn）的生命周期必须
/// 跨请求存活；调用方可能用一次性 current_thread runtime（脚本加载线程），
/// spawn 到临时 runtime 会在其 drop 时杀死连接（843KB 脚本超时根因）。
/// request() 经 spawn_blocking 在此 runtime 上执行（避免嵌套 block_on）。
static BORING_RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

fn boring_rt() -> &'static tokio::runtime::Runtime {
    BORING_RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("boring runtime")
    })
}

/// 取/建目标 host 的 H2 连接（缓存复用；GOAWAY/失效时重建）。
async fn sender_for(host: &str, port: u16) -> Result<H2Sender, String> {
    {
        let g = CONNS.lock().map_err(|e| e.to_string())?;
        if let Some(map) = g.as_ref() {
            if let Some(tx) = map.get(host) {
                // clone 出新句柄（h2 多路复用语义）；有效性由 send_request
                // 失败时的重建兜底（is_closed 不可用）。
                return Ok(tx.clone());
            }
        }
    }
    // 新建：TCP（直连或 CONNECT 隧道）→ boring TLS → h2 handshake
    let stream = connect_stream(host, port).await?;
    let mut builder = boring::ssl::SslConnector::builder(boring::ssl::SslMethod::tls())
        .map_err(|e| e.to_string())?;
    let _ = builder.set_alpn_protos(b"\x02h2\x08http/1.1");
    // M97: 加载系统 CA 证书——BoringSSL 不自动找系统证书（不同 Linux
    // 发行版路径不同，逐个尝试标准路径）
    for ca_path in [
        "/etc/ssl/certs/ca-certificates.crt",   // Debian/Ubuntu
        "/etc/pki/tls/certs/ca-bundle.crt",     // RHEL/CentOS
        "/etc/ssl/cert.pem",                     // macOS / Alpine
        "/etc/ssl/ca-bundle.pem",                // openSUSE
    ] {
        if std::path::Path::new(ca_path).exists() {
            let _ = builder.set_ca_file(ca_path);
            break;
        }
    }
    let connector = builder.build();
    let ssl = tokio_boring::connect(
        connector.configure().map_err(|e| e.to_string())?,
        host,
        stream,
    )
    .await
    .map_err(|e| format!("boring tls: {e}"))?;
    // Chrome SETTINGS 值（netlog 实测）
    let mut h2b = h2::client::Builder::new();
    h2b.initial_window_size(6291456);
    h2b.initial_connection_window_size(15728640);
    h2b.max_frame_size(16384);
    let (tx, conn) = h2b
        .handshake::<_, bytes::Bytes>(ssl)
        .await
        .map_err(|e| format!("h2 handshake: {e}"))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let mut g = CONNS.lock().map_err(|e| e.to_string())?;
    g.get_or_insert_with(HashMap::new)
        .insert(host.to_string(), tx.clone());
    Ok(tx)
}

/// TCP 连接：https_proxy/http_proxy 环境变量存在 → CONNECT 隧道；否则直连。
async fn connect_stream(host: &str, port: u16) -> Result<TcpStream, String> {
    let proxy = std::env::var("https_proxy")
        .or_else(|_| std::env::var("http_proxy"))
        .or_else(|_| std::env::var("HTTPS_PROXY"))
        .or_else(|_| std::env::var("HTTP_PROXY"))
        .ok();
    let Some(proxy) = proxy else {
        return TcpStream::connect((host, port))
            .await
            .map_err(|e| e.to_string());
    };
    // 解析 proxy URL → host:port
    let purl = url::Url::parse(&proxy).map_err(|e| e.to_string())?;
    let phost = purl.host_str().unwrap_or("127.0.0.1").to_string();
    let pport = purl.port_or_known_default().unwrap_or(8080);
    let mut stream = TcpStream::connect((phost.as_str(), pport))
        .await
        .map_err(|e| format!("proxy tcp: {e}"))?;
    stream
        .write_all(
            format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n").as_bytes(),
        )
        .await
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("proxy closed during CONNECT".into());
        }
        buf.extend_from_slice(&chunk[..n]);
        let s = String::from_utf8_lossy(&buf);
        if s.contains("\r\n\r\n") {
            if s.contains(" 200 ") {
                return Ok(stream);
            }
            return Err(format!(
                "CONNECT rejected: {}",
                s.lines().next().unwrap_or("")
            ));
        }
    }
}

fn rebuild_request(
    method: &str,
    host: &str,
    scheme: &str,
    path: &str,
    headers: &[(String, String)],
    has_body: bool,
    body_bytes: &bytes::Bytes,
) -> Result<http::Request<()>, String> {
    let full = format!("{scheme}://{host}{path}");
    let uri: http::Uri = full.parse().map_err(|e| format!("uri: {e}"))?;
    let mut rb = http::Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("host") || k.eq_ignore_ascii_case("content-length") {
            continue;
        }
        if k.eq_ignore_ascii_case("accept-encoding") {
            continue;
        }
        rb = rb.header(k.as_str(), v.as_str());
    }
    rb = rb.header("accept-encoding", "identity");
    if has_body {
        rb = rb.header("content-length", body_bytes.len().to_string());
    } else if method != "GET" {
        rb = rb.header("content-length", "0");
    }
    rb.body(()).map_err(|e| e.to_string())
}

/// 发一次 HTTP/2 请求（headers 顺序 = 传入顺序；identity 压缩）。
/// 返回 (status, 响应头列表, body)。
#[allow(clippy::type_complexity)]
pub async fn request(
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<&str>,
) -> Result<(u16, Vec<(String, String)>, Vec<u8>), String> {
    let url = url.to_string();
    let method = method.to_string();
    let headers = headers.to_vec();
    let body = body.map(str::to_string);
    tokio::task::spawn_blocking(move || {
        boring_rt().block_on(request_impl(&url, &method, &headers, body.as_deref()))
    })
    .await
    .map_err(|e| format!("boring spawn_blocking: {e}"))?
}

async fn request_impl(
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<&str>,
) -> Result<(u16, Vec<(String, String)>, Vec<u8>), String> {
    let parsed = url::Url::parse(url).map_err(|e| e.to_string())?;
    let host = parsed.host_str().ok_or("no host")?.to_string();
    let port = parsed.port_or_known_default().unwrap_or(443);
    let path = if parsed.query().is_some() {
        format!("{}?{}", parsed.path(), parsed.query().unwrap_or(""))
    } else {
        parsed.path().to_string()
    };
    let mut tx = sender_for(&host, port).await?;
    let full = format!("{}://{host}{path}", parsed.scheme());
    let uri: http::Uri = full.parse().map_err(|e| format!("uri: {e}"))?;
    let mut rb = http::Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("host") || k.eq_ignore_ascii_case("content-length") {
            continue;
        }
        if k.eq_ignore_ascii_case("accept-encoding") {
            rb = rb.header("accept-encoding", "identity");
            continue;
        }
        rb = rb.header(k.as_str(), v.as_str());
    }
    rb = rb.header("accept-encoding", "identity");
    let body_bytes: bytes::Bytes = body.unwrap_or("").as_bytes().to_vec().into();
    if body.is_some() {
        rb = rb.header("content-length", body_bytes.len().to_string());
    } else if method != "GET" {
        rb = rb.header("content-length", "0");
    }
    let req = rb.body(()).map_err(|e| e.to_string())?;
    // 缓存句柄可能已 GOAWAY——send 失败时清缓存重建一次
    let (resp_rx, mut send) = match tx.send_request(req, false) {
        Ok(v) => v,
        Err(first_err) => {
            {
                let mut g = CONNS.lock().map_err(|e| e.to_string())?;
                if let Some(map) = g.as_mut() {
                    map.remove(&host);
                }
            }
            let mut tx2 = sender_for(&host, port).await?;
            let req2 = rebuild_request(
                method,
                &host,
                parsed.scheme(),
                &path,
                headers,
                body.is_some(),
                &body_bytes,
            )?;
            tx2.send_request(req2, false)
                .map_err(|e| format!("h2 send (retried): {e} / first: {first_err}"))?
        }
    };
    send.send_data(body_bytes, true)
        .map_err(|e| format!("h2 data: {e}"))?;
    let fut = async {
        let resp = resp_rx.await.map_err(|e| format!("h2 resp: {e}"))?;
        let status = resp.status().as_u16();
        let hdrs: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let mut rb = resp.into_body();
        let mut out = Vec::new();
        let t0 = std::time::Instant::now();
        let mut chunks = 0u32;
        while let Some(chunk) = rb.data().await {
            let chunk = chunk.map_err(|e| format!("h2 body: {e}"))?;
            let _ = rb.flow_control().release_capacity(chunk.len());
            out.extend_from_slice(&chunk);
            chunks += 1;
            if std::env::var("BROWSER_TRACE_FETCH").is_ok() && (chunks % 50 == 0) {
                eprintln!(
                    "[boring-diag] body {}B in {} chunks {:.0}ms",
                    out.len(),
                    chunks,
                    t0.elapsed().as_millis()
                );
            }
        }
        if std::env::var("BROWSER_TRACE_FETCH").is_ok() {
            eprintln!(
                "[boring-diag] body DONE {}B {} chunks {:.0}ms",
                out.len(),
                chunks,
                t0.elapsed().as_millis()
            );
        }
        Ok::<(u16, Vec<(String, String)>, Vec<u8>), String>((status, hdrs, out))
    };
    let (status, hdrs, out) = tokio::time::timeout(std::time::Duration::from_secs(40), fut)
        .await
        .map_err(|_| "boring h2 timeout (40s)".to_string())??;

    // M96.21: Reddit 式 PoW 挑战页自动解决——检测到挑战页（小 body + solution/jsc_token）
    // 时，解析参数、重发带 solution 的请求，返回真页面。
    if let Some(result) = solve_js_challenge(url, method, headers, status, &out).await {
        return result;
    }

    Ok((status, hdrs, out))
}

/// M96.21: Reddit 式 JS 拼接 PoW 挑战自动解决。
///
/// 挑战页结构：`await(async e=>e+e)("<hex>")` 把 hex 字符串重复一次作为
/// solution，连同 `jsc_token` 通过 GET 参数回传源站即获得真页面。
/// 在 boring 通道内检测到挑战页（<2000B + 含 solution/token 字段）时，
/// 解析参数并自动重发一次 GET。
async fn solve_js_challenge(
    url: &str,
    method: &str,
    headers: &[(String, String)],
    status: u16,
    body: &[u8],
) -> Option<Result<(u16, Vec<(String, String)>, Vec<u8>), String>> {
    // 只处理 200 且 body 很小（挑战页 <4KB）
    if status != 200 && status != 403 || body.len() > 16384 || body.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(body);
    // 必须同时含 solution input 和 jsc_token（Reddit 挑战页指纹）
    if !text.contains("solution") || !text.contains("jsc_token") {
        return None;
    }
    // 提取 PoW 参数：await(async e=>e+e)("<hex>")
    let re_challenge = text.contains("e+e");
    let token_re = text.contains("jsc_token");
    if !re_challenge || !token_re {
        return None;
    }
    // 提取 hex 种子：找 await(async e=>e+e)("XXXX") 模式
    let seed = {
        // 手动解析（无 regex crate 依赖——找到 await(async e=>e+e)( 后的引号内内容）
        let marker = "e+e)(\"";
        if let Some(pos) = text.find(marker) {
            let start = pos + marker.len();
            if let Some(end_rel) = text[start..].find('"') {
                text[start..start + end_rel].to_string()
            } else {
                return None;
            }
        } else {
            return None;
        }
    };
    let solution = format!("{}{}", seed, seed); // e+e = 字符串重复
    let jsc_token = {
        let marker = "jsc_token\" value=\"";
        if let Some(pos) = text.find(marker) {
            let start = pos + marker.len();
            let end = text[start..].find('"')?;
            text[start..start + end].to_string()
        } else {
            return None;
        }
    };
    eprintln!(
        "[boring] PoW challenge detected: seed={seed} solution={}B token={}B — auto-solving",
        solution.len(),
        jsc_token.len()
    );
    // 构造带 solution 的 GET 请求（form action 是原始 URL path）
    let parsed = url::Url::parse(url).ok()?;
    let path = parsed.path();
    let query = format!("solution={solution}&js_challenge=1&jsc_token={jsc_token}");
    let full = format!(
        "{}://{}{}?{}",
        parsed.scheme(),
        parsed.host_str().unwrap_or(""),
        path,
        query
    );
    // 重发 GET（会话 cookie 由 h2 连接层保持）
    // Box::pin 避免异步递归
    let result = Box::pin(request_impl(&full, "GET", headers, None)).await;
    eprintln!(
        "[boring] PoW solve result: {}",
        result.as_ref().map(|(s, _, b)| format!("{} {}B", s, b.len())).unwrap_or_default()
    );
    Some(result)
}
