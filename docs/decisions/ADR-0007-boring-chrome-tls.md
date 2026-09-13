# ADR-0007: boring（BoringSSL）作为可选 Chrome 同源 TLS 后端

日期：2026-09-13 · 状态：已实施（M96.18）

## 背景

xcancel antibot 对 verify 请求按**传输层 TLS ClientHello 形状**评分。
MITM 判别实验铁证（M96.16）：同 payload、同头组、同 IP，单变量对照——
Chrome 形传输（curl_cffi/BoringSSL 系）→ verify 200；rustls → 403。
服务端挑战暂停窗口的「rustls 也能过」是环境使然（裸 curl 同样直过），
不作数；挑战恢复后 rustls 复现 403、boring 复现 200（M96.18 实弹）。

用户裁决（2026-09-13）：「跟浏览器一样」是标准——把 Chromium 开源
代码里的行为（含 TLS 形状）实现正确是浏览器实现的保真度，不是伪装。
Chrome 的 TLS 库就是 BoringSSL（开源）——用同源库是最高保真路径。

## 决策

1. 引入 `boring`/`tokio-boring`（BoringSSL 绑定）+ `h2`/`http`/`bytes`
   作为 **optional 依赖**（feature `chrome-tls`，默认不编译——与 ADR-0006
   的 V8 可选后端同纪律：默认构建零影响）。
2. `net/src/boring_h2.rs`：原生 Chrome 同源通道——BoringSSL ClientHello
   + ALPN h2 + per-host H2 连接复用（SETTINGS 对齐 Chrome 值：
   initial window 6291456 / conn window 15728640 / frame 16384）+
   `https_proxy` 环境变量 CONNECT 隧道（与 reqwest 语义一致）+
   **常驻专用 runtime**（连接驱动任务跨请求存活——脚本加载线程的
   一次性 runtime 会在 drop 时杀死连接，843KB 脚本超时的根因）。
3. `HttpClient::request_full_raw_hdr`：`chrome-tls` feature 下 https
   请求优先走 boring 通道，**失败回退 reqwest**（保「能打开」优先）。
4. 响应头转换用 **append**（非 insert）——多条 Set-Cookie（__antibot
   会话票 + __antibot_ref）不得互相覆盖（覆盖导致 reload 又拿挑战页）。

## 实测

- 二进制净增量 **≈0.9MB**（基线 0.41MB → +boring 1.29MB 独立实测；
  合入主二进制后 default 10.3MB 不变，chrome-tls 构建约 11MB 级）。
- 实弹（挑战开启状态，经 Clash 出口）：challenge 200 → **verify 200
  （纯 boring，无 MITM/无 curl_cffi）** → __antibot 票 Set-Cookie →
  reload 带票 → 真身页 16.6KB 推文 markdown，端到端复验通过。
- 压缩：第一版强制 `accept-encoding: identity`（解压零依赖）；Chrome
  形压缩协商（gzip/br/zstd + 解压）后续迭代。

## 后果

- 默认构建零变化（feature 门控）；`--features chrome-tls` 启用。
- 构建依赖：cmake + C/C++ 编译器（BoringSSL 源码编译，首构建 ~3min）。
- H2 帧序/PRIORITY 未逐帧对齐（h2 crate 形状）；当前实测足够过
  xcancel——如遇更强 H2 指纹站点再迭代。
