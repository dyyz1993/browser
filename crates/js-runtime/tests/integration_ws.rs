//! M23.5 e2e: JS `new WebSocket()` 连接本地 echo server。
//!
//! 验证完整链路：JS new WebSocket → __wsCreate → 后台线程握手 →
//! onopen 触发 → ws.send → 后台线程 echo → onmessage 触发 → 渲染 DOM
//!
//! **关键设计**：echo server 用 `std::thread` + `std::net` 同步 I/O，
//! 不用 tokio::spawn——因为 `run_scripts_with_base` 同步阻塞调用线程，
//! 若 server 在同一 tokio runtime 上 spawn，握手 task 无法调度（死锁）。
//!
//! **标记动态拼接**：assert 的标记用 `'OPEN' + '_MK'` 形式，使执行结果
//! （`OPEN_MK`）与源码字面量（`'OPEN' + '_MK'`）不同，避免 body_text_content
//! 提取 `<script>` 源码导致的假阳性。

use std::io::{Read, Write};

use browser_html_parser::parse as parse_html;
use browser_js_runtime::{bridge::body_text_content, run_scripts_with_base};

/// Read HTTP request until \r\n\r\n, extract Sec-WebSocket-Key header value.
fn extract_ws_key(req: &str) -> Option<String> {
    for line in req.split("\r\n") {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("sec-websocket-key:") {
            let colon = line.find(':')?;
            return Some(line[colon + 1..].trim().to_string());
        }
    }
    None
}

/// Read a single WS frame (server-side: client frames are masked). Synchronous.
fn read_frame_sync(stream: &mut std::net::TcpStream) -> (Vec<u8>, u8) {
    let mut header = [0u8; 2];
    stream.read_exact(&mut header).expect("read hdr");
    let opcode = header[0] & 0x0F;
    let masked = header[1] & 0x80 != 0;
    let len7 = (header[1] & 0x7F) as usize;
    let payload_len = match len7 {
        0..=125 => len7,
        126 => {
            let mut ext = [0u8; 2];
            stream.read_exact(&mut ext).expect("read ext2");
            u16::from_be_bytes(ext) as usize
        }
        _ => {
            let mut ext = [0u8; 8];
            stream.read_exact(&mut ext).expect("read ext8");
            u64::from_be_bytes(ext) as usize
        }
    };
    let mask = if masked {
        let mut m = [0u8; 4];
        stream.read_exact(&mut m).expect("read mask");
        Some(m)
    } else {
        None
    };
    let mut payload = vec![0u8; payload_len];
    stream.read_exact(&mut payload).expect("read payload");
    if let Some(m) = mask {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= m[i % 4];
        }
    }
    (payload, opcode)
}

/// Send an unmasked server→client frame. Synchronous.
fn send_frame_sync(stream: &mut std::net::TcpStream, opcode: u8, payload: &[u8]) {
    let mut buf = vec![0x80 | opcode];
    let len = payload.len();
    if len < 126 {
        buf.push(len as u8);
    } else if len <= u16::MAX as usize {
        buf.push(126);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(127);
        buf.extend_from_slice(&(len as u64).to_be_bytes());
    }
    buf.extend_from_slice(payload);
    stream.write_all(&buf).expect("write frame");
}

/// Spawn a minimal synchronous echo server on its own OS thread.
/// Returns (port, join_handle). The thread owns the listener.
fn spawn_echo_server() -> (u16, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local_addr").port();
    let handle = std::thread::spawn(move || {
        let (mut sock, _) = match listener.accept() {
            Ok(x) => x,
            Err(_) => return,
        };
        let mut buf = [0u8; 8192];
        let mut total = 0usize;
        loop {
            let n = match sock.read(&mut buf[total..]) {
                Ok(0) => return,
                Ok(n) => n,
                Err(_) => return,
            };
            total += n;
            if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let req = String::from_utf8_lossy(&buf[..total]);
        let key = match extract_ws_key(&req) {
            Some(k) => k,
            None => return,
        };
        let accept = browser_ws::handshake::compute_accept(&key);
        let resp = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        if sock.write_all(resp.as_bytes()).is_err() {
            return;
        }
        loop {
            let (payload, opcode) = read_frame_sync(&mut sock);
            match opcode {
                0x8 => {
                    send_frame_sync(&mut sock, 0x8, &payload);
                    return;
                }
                0x9 => {
                    send_frame_sync(&mut sock, 0xA, &payload);
                }
                0x1 | 0x2 => {
                    send_frame_sync(&mut sock, opcode, &payload);
                }
                _ => {}
            }
        }
    });
    (port, handle)
}

/// Spawn a "reject" server: accepts the TCP connection then immediately drops
/// it (no HTTP response). Client's handshake read returns 0 bytes → error.
/// Reliable way to trigger onerror without relying on port-firewall behavior.
fn spawn_reject_server() -> (u16, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local_addr").port();
    let handle = std::thread::spawn(move || {
        // Accept once then exit (drop the socket without responding).
        let _ = listener.accept();
    });
    (port, handle)
}

#[test]
fn js_websocket_onopen_and_echo_to_dom() {
    let (port, server) = spawn_echo_server();
    let ws_url = format!("ws://127.0.0.1:{port}/echo");

    // 标记用 'X' + '_MK' 拼接：执行结果 OPEN_MK 与源码字面量不同，
    // 避免 body_text_content 提取 <script> 源码导致的假阳性。
    let html = format!(
        r#"<html><body><p>init</p>
<script>
var ws = new WebSocket('{ws_url}');
ws.onopen = function() {{
    __appendBody('OPEN' + '_MK');
    ws.send('hello-ws');
}};
ws.onmessage = function(e) {{
    __appendBody('MSG' + '_MK' + e.data);
    ws.close();
}};
ws.onclose = function() {{
    __appendBody('CLOSED' + '_MK');
}};
</script>
</body></html>"#,
        ws_url = ws_url
    );

    let tree = parse_html(&html);
    let (shared, _) = run_scripts_with_base(tree, Some(format!("http://127.0.0.1:{port}")));
    let body = body_text_content(&shared.borrow());

    assert!(
        body.contains("OPEN_MK"),
        "onopen should fire. body={body:?}"
    );
    assert!(
        body.contains("MSG_MKhello-ws"),
        "echo should come back. body={body:?}"
    );
    assert!(
        body.contains("CLOSED_MK"),
        "onclose should fire. body={body:?}"
    );

    let _ = server.join();
}

#[test]
fn js_websocket_multiple_messages() {
    let (port, server) = spawn_echo_server();
    let ws_url = format!("ws://127.0.0.1:{port}/echo");

    let html = format!(
        r#"<html><body><p>init</p>
<script>
var ws = new WebSocket('{ws_url}');
var received = [];
ws.onopen = function() {{
    __appendBody('OPEN' + '_MK');
    ws.send('one');
    ws.send('two');
    ws.send('three');
}};
ws.onmessage = function(e) {{
    received.push(e.data);
    if (received.length === 3) {{
        __appendBody('ALL' + '_MK' + received.join(','));
        ws.close();
    }}
}};
</script>
</body></html>"#,
        ws_url = ws_url
    );

    let tree = parse_html(&html);
    let (shared, _) = run_scripts_with_base(tree, Some(format!("http://127.0.0.1:{port}")));
    let body = body_text_content(&shared.borrow());

    assert!(
        body.contains("OPEN_MK"),
        "onopen should fire. body={body:?}"
    );
    assert!(
        body.contains("ALL_MKone,two,three"),
        "all 3 echoes should arrive in order. body={body:?}"
    );

    let _ = server.join();
}

#[test]
fn js_websocket_connect_error_fires_onerror() {
    // reject server：accept 后立即断开 → 握手读到 0 字节 → error
    let (port, server) = spawn_reject_server();
    let ws_url = format!("ws://127.0.0.1:{port}/nope");

    let html = format!(
        r#"<html><body><p>init</p>
<script>
var ws = new WebSocket('{ws_url}');
ws.onerror = function() {{
    __appendBody('ERROR' + '_MK');
}};
ws.onopen = function() {{
    __appendBody('BADOPEN' + '_MK');
}};
</script>
</body></html>"#,
        ws_url = ws_url
    );

    let tree = parse_html(&html);
    let (shared, _) = run_scripts_with_base(tree, Some(format!("http://127.0.0.1:{port}")));
    let body = body_text_content(&shared.borrow());

    assert!(
        body.contains("ERROR_MK"),
        "onerror should fire on handshake failure. body={body:?}"
    );
    assert!(
        !body.contains("BADOPEN_MK"),
        "onopen must NOT fire on failure. body={body:?}"
    );

    let _ = server.join();
}
