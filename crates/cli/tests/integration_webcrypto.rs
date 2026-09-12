//! M93.14 集成测试：纯 JS WebCrypto 子集（P-256 ECDH + AES-256-GCM + HMAC/HKDF）。
//!
//! 实现位置：`js-runtime/src/scripts.rs` 的 `QUICKJS_WEBCRYPTO_SHIM`（纯 JS，
//! G4 自研，零 Rust 依赖）。本文件用真实引擎（`render-script`，QuickJS）+
//! 权威测试向量做端到端验收：
//!
//! - ECDH P-256：RFC 5903 §8.1（双向）+ RFC 5114 A.6（dA×qB）。
//!   注：RFC 5114 A.6 印刷版 dB 与 qB 不自洽（dB·G ≠ qB，经 Python 独立参考
//!   实现核实为 RFC 排印错误），dB 方向断言不纳入；双向性由 RFC 5903 覆盖。
//! - AES-256-GCM：NIST GCM spec Appendix B TC13（96-bit IV + AAD）+ Go
//!   crypto/cipher 同源向量（pt=13 / 1-byte IV + AAD / pt=51 + AAD=100），
//!   全部经 Python `cryptography`（OpenSSL 后端）交叉核对。1-byte IV 用例
//!   覆盖 J0 = GHASH_H(IV‖pad‖len) 的非 96-bit 分支。
//! - HKDF：RFC 5869 A.1 / A.3（extract+expand，SHA-256）。
//! - generateKey/exportKey/importKey 往返 + 派生对称性 + 密钥用法门禁 + 性能。
//!
//! 测试模式：HTML 内嵌 script 跑 `crypto.subtle`，断言结果写 DOM（`OUT:OK <标记>`
//! / `OUT:FAIL <标记> ...` 行），`render-script` 渲染后断言 stdout。每步独立
//! `.then/.catch`——单步拒绝写 FAIL 标记且不断链，防一处失败静默吞掉整条链。

use assert_cmd::Command;
use predicates::prelude::*;

fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

/// 跑一段内嵌 JS 的 HTML（render-script），返回 stdout。
fn run_webcrypto_html(script: &str) -> String {
    // JS 含大量 `{}`，不能用 format!（转义地狱），统一用拼接。
    let html = r#"<!doctype html>
<html><body><div id="out">pending</div>
<script>"#
        .to_string()
        + HARNESS
        + script
        + TAIL
        + r#"</script>
</body></html>"#;
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("m93_webcrypto_{}_{n}.html", std::process::id()));
    std::fs::write(&path, html).expect("write temp html");
    let output = bin()
        .args([
            "render-script",
            path.to_str().expect("temp path"),
            "--width",
            "240",
        ])
        .output()
        .expect("run render-script");
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "render-script should succeed. stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn expect_ok(stdout: &str, marker: &str) {
    // ASCII 渲染会在空格处折行，先把换行压平再匹配（标记 token 本身无空格）。
    let flat = stdout.replace('\n', " ");
    assert!(
        predicate::str::contains(format!("OUT:OK {marker}").as_str()).eval(&flat),
        "expected OUT:OK {marker}. stdout={stdout:?}"
    );
    assert!(
        !predicate::str::contains("OUT:FAIL").eval(&flat),
        "unexpected OUT:FAIL. stdout={stdout:?}"
    );
}

/// 公共 JS 前置：hex 工具 + step 链（独立 .then/.catch，单步失败不断链）。
const HARNESS: &str = r#"
var out = [];
var hexToBytes = function(h) { var o = new Uint8Array(h.length/2); for (var i=0;i<o.length;i++) o[i]=parseInt(h.substr(2*i,2),16); return o; };
var bytesToHex = function(b) { b = new Uint8Array(b.buffer === undefined ? b : b.buffer, b.byteOffset || 0, b.byteLength); var s=''; for (var i=0;i<b.length;i++) s += (b[i]&255).toString(16).padStart(2,'0'); return s; };
var b64u = function(h) { var s=''; var b=hexToBytes(h); for (var i=0;i<b.length;i++) s+=String.fromCharCode(b[i]); return btoa(s).replace(/\+/g,'-').replace(/\//g,'_').replace(/=+$/,''); };
var subtle = crypto.subtle;
var chain = Promise.resolve();
function step(name, fn) {
  chain = chain.then(fn).then(
    function(r) { out.push('OUT:OK ' + name + (r ? ' ' + r : '')); },
    function(e) { out.push('OUT:FAIL ' + name + ' REJECT=' + ((e && (e.name + ':' + e.message)) || e)); }
  );
}
function expectEq(name, got, exp) {
  var g = bytesToHex(got);
  if (g !== exp) throw new Error(name + ' got=' + g + ' exp=' + exp);
}
function privJwk(hex) { return { kty:'EC', crv:'P-256', d: b64u(hex) }; }
"#;

const TAIL: &str = r#"
chain = chain.then(function() {
  document.getElementById('out').textContent = 'WCRB ' + out.join(' | ') + ' WCRE';
});
"#;

/// RFC 5903 §8.1 + RFC 5114 A.6：双方私钥/公钥/共享密钥全公开的 P-256 ECDH。
#[test]
fn ecdh_p256_rfc_vectors_shared_secrets() {
    let stdout = run_webcrypto_html(
        r#"
var R = {
  di: 'C88F01F510D9AC3F70A292DAA2316DE544E9AAB8AFE84049C62A9C57862D1433',
  gix: 'DAD0B65394221CF9B051E1FECA5787D098DFE637FC90B9EF945D0C3772581180',
  giy: '5271A0461CDB8252D61F1C456FA3E59AB1F45B33ACCF5F58389E0577B8990BB3',
  dr: 'C6EF9C5D78AE012A011164ACB397CE2088685D8F06BF9BE0B283AB46476BEE53',
  grx: 'D12DFB5289C8D4F81208B70270398C342296970A0BCCB74C736FC7554494BF63',
  gry: '56FBF3CA366CC23E8157854C13C58D6AAC23F046ADA30F8353E74F33039872AB',
  zz: 'd6840f6b42f6edafd13116e0e12565202fef8e9ece7dce03812464d04b9442de',
  dA: '814264145F2F56F2E96A8E337A1284993FAF432A5ABCE59E867B7291D507A3AF',
  qBraw: '04B120DE4AA36492795346E8DE6C2C8646AE06AAEA279FA775B3AB0715F6CE51B09F1B7EECE20D7B5ED8EC685FA3F071D83727027092A8411385C34DDE5708B2B6',
  xz: 'dd0f5396219d1ea393310412d19a08f1f5811e9dc8ec8eea7f80d21c820c2788'
};
step('ECDH_5903_dr_x_pubI', function() {
  return subtle.importKey('raw', hexToBytes('04' + R.gix + R.giy), {name:'ECDH',namedCurve:'P-256'}, false, [])
    .then(function(pubI) {
      return subtle.importKey('jwk', privJwk(R.dr), {name:'ECDH',namedCurve:'P-256'}, false, ['deriveBits'])
        .then(function(privR) { return subtle.deriveBits({name:'ECDH',public:pubI}, privR, 256); });
    })
    .then(function(zz) { expectEq('5903-dr', zz, R.zz); });
});
step('ECDH_5903_di_x_pubR', function() {
  return subtle.importKey('raw', hexToBytes('04' + R.grx + R.gry), {name:'ECDH',namedCurve:'P-256'}, false, [])
    .then(function(pubR) {
      return subtle.importKey('jwk', privJwk(R.di), {name:'ECDH',namedCurve:'P-256'}, false, ['deriveBits'])
        .then(function(privI) { return subtle.deriveBits({name:'ECDH',public:pubR}, privI, 256); });
    })
    .then(function(zz) { expectEq('5903-di', zz, R.zz); });
});
step('ECDH_5114_dA_x_pubB', function() {
  return subtle.importKey('raw', hexToBytes(R.qBraw), {name:'ECDH',namedCurve:'P-256'}, false, [])
    .then(function(pubB) {
      return subtle.importKey('jwk', privJwk(R.dA), {name:'ECDH',namedCurve:'P-256'}, false, ['deriveBits'])
        .then(function(privA) { return subtle.deriveBits({name:'ECDH',public:pubB}, privA, 256); });
    })
    .then(function(xz) { expectEq('5114', xz, R.xz); });
});
"#,
    );
    expect_ok(&stdout, "ECDH_5903_dr_x_pubI");
    expect_ok(&stdout, "ECDH_5903_di_x_pubR");
    expect_ok(&stdout, "ECDH_5114_dA_x_pubB");
}

/// AES-256-GCM：NIST GCM Appendix B TC13 + Go 同源向量（含非 96-bit IV 分支）。
#[test]
fn aes_gcm_nist_vectors_encrypt_and_decrypt() {
    let stdout = run_webcrypto_html(
        r#"
var K = 'feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308';
var V = {
  tc13iv: 'cafebabefacedbaddecaf888',
  tc13a: 'feedfacedeadbeeffeedfacedeadbeefabaddad2',
  tc13p: 'd9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b391aafd255',
  tc13ct: '522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662898015ad2df7cd675b4f09163b41ebf980a7f638',
  v1iv: '12823ab601c350ea4bc2488c',
  v1p: '793cd125b0b84a043e3ac67717',
  v1ct: 'e796c39074c7783a38193e3f8d46b355adacca7198d16d879fbfeac6e3',
  v2iv: '75',
  v2a: '4f6e2585c161f05a9ae1f2f894e9f0ab52b45d0f',
  v2p: 'ca6131faf0ff210e4e693d6c31c109fc5b6f54224eb120f37de31dc59ec669b6',
  v2ct: 'd73bebe722c5e312fe910ba71d5a6a063a4297203f819103dfa885a8076d095545a999affde3dbac2b5be6be39195ed0',
  v3iv: 'e1934f5db57cc983e6b180e7',
  v3a: '0a8a18a7150e940c3d87b38e73baee9a5c049ee21795663e264b694a949822b639092d0e67015e86363583fcf0ca645af9f43375f05fdb4ce84f411dcbca73c2220dea03a20115d2e51398344b16bee1ed7c499b353d6c597af8',
  v3p: '73ed042327f70fe9c572a61545eda8b2a0c6e1d6c291ef19248e973aee6c312012f490c2c6f6166f4a59431e182663fcaea05a',
  v3ct: 'fc1ae2b5dcd2c4176c3f538b4c3cc21197f79e608cc3730167936382e4b1e5a7b75ae1678bcebd876705477eb0e0fdbbcda92fb9a0dc58c8d8f84fb590e0422e6077ef'
};
step('GCM_TC13_encrypt', function() {
  return subtle.importKey('raw', hexToBytes(K), {name:'AES-GCM'}, false, ['encrypt'])
    .then(function(key) {
      return subtle.encrypt({name:'AES-GCM', iv:hexToBytes(V.tc13iv), additionalData:hexToBytes(V.tc13a)}, key, hexToBytes(V.tc13p))
        .then(function(ct) { expectEq('tc13-enc', ct, V.tc13ct); });
    });
});
step('GCM_TC13_decrypt', function() {
  return subtle.importKey('raw', hexToBytes(K), {name:'AES-GCM'}, false, ['decrypt'])
    .then(function(key) {
      return subtle.decrypt({name:'AES-GCM', iv:hexToBytes(V.tc13iv), additionalData:hexToBytes(V.tc13a)}, key, hexToBytes(V.tc13ct))
        .then(function(pt) { expectEq('tc13-dec', pt, V.tc13p); });
    });
});
step('GCM_pt13', function() {
  return subtle.importKey('raw', hexToBytes(K), {name:'AES-GCM'}, false, ['encrypt'])
    .then(function(key) {
      return subtle.encrypt({name:'AES-GCM', iv:hexToBytes(V.v1iv)}, key, hexToBytes(V.v1p))
        .then(function(ct) { expectEq('v1', ct, V.v1ct); });
    });
});
step('GCM_iv1B_non96', function() {
  return subtle.importKey('raw', hexToBytes(K), {name:'AES-GCM'}, false, ['encrypt','decrypt'])
    .then(function(key) {
      return subtle.encrypt({name:'AES-GCM', iv:hexToBytes(V.v2iv), additionalData:hexToBytes(V.v2a)}, key, hexToBytes(V.v2p))
        .then(function(ct) {
          expectEq('v2-enc', ct, V.v2ct);
          return subtle.decrypt({name:'AES-GCM', iv:hexToBytes(V.v2iv), additionalData:hexToBytes(V.v2a)}, key, ct)
            .then(function(pt) { expectEq('v2-dec', pt, V.v2p); });
        });
    });
});
step('GCM_pt51_aad100', function() {
  return subtle.importKey('raw', hexToBytes(K), {name:'AES-GCM'}, false, ['encrypt'])
    .then(function(key) {
      return subtle.encrypt({name:'AES-GCM', iv:hexToBytes(V.v3iv), additionalData:hexToBytes(V.v3a)}, key, hexToBytes(V.v3p))
        .then(function(ct) { expectEq('v3', ct, V.v3ct); });
    });
});
"#,
    );
    expect_ok(&stdout, "GCM_TC13_encrypt");
    expect_ok(&stdout, "GCM_TC13_decrypt");
    expect_ok(&stdout, "GCM_pt13");
    expect_ok(&stdout, "GCM_iv1B_non96");
    expect_ok(&stdout, "GCM_pt51_aad100");
}

/// GCM tag 校验：翻转密文尾部 tag 任一位 → decrypt 必须抛 OperationError。
#[test]
fn aes_gcm_tampered_tag_rejected_with_operation_error() {
    let stdout = run_webcrypto_html(
        r#"
step('GCM_TAG_MISMATCH', function() {
  return subtle.importKey('raw', hexToBytes('feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308'), {name:'AES-GCM'}, false, ['encrypt','decrypt'])
    .then(function(key) {
      return subtle.encrypt({name:'AES-GCM', iv:hexToBytes('12823ab601c350ea4bc2488c')}, key, hexToBytes('793cd125b0b84a043e3ac67717'))
        .then(function(ct) {
          var bad = new Uint8Array(ct);
          bad[bad.length - 1] ^= 1;
          return subtle.decrypt({name:'AES-GCM', iv:hexToBytes('12823ab601c350ea4bc2488c')}, key, bad)
            .then(function() { throw new Error('decrypt accepted tampered tag'); },
                  function(e) {
                    if (e && e.name === 'OperationError') return 'OperationError as expected';
                    throw new Error('expected OperationError, got ' + (e && e.name));
                  });
        });
    });
});
"#,
    );
    expect_ok(&stdout, "GCM_TAG_MISMATCH");
}

/// HKDF（RFC 5869）A.1/A.3 向量 + deriveKey(HKDF)→AES-GCM→encrypt/decrypt 闭环
///（xcancel 指纹上报管线的形状：salt 为 ASCII 串）。
#[test]
fn hkdf_rfc5869_vectors_and_derive_key_to_aes_gcm() {
    let stdout = run_webcrypto_html(
        r#"
var ikm22 = hexToBytes('0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b');
step('HKDF_5869_A1', function() {
  return subtle.importKey('raw', ikm22, {name:'HKDF'}, false, ['deriveBits'])
    .then(function(k) {
      return subtle.deriveBits({name:'HKDF',hash:'SHA-256',salt:hexToBytes('000102030405060708090a0b0c'),info:hexToBytes('f0f1f2f3f4f5f6f7f8f9')}, k, 336);
    })
    .then(function(okm) { expectEq('A1', okm, '3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865'); });
});
step('HKDF_5869_A3_empty_salt_info', function() {
  return subtle.importKey('raw', ikm22, {name:'HKDF'}, false, ['deriveBits'])
    .then(function(k) {
      return subtle.deriveBits({name:'HKDF',hash:'SHA-256'}, k, 336);
    })
    .then(function(okm) { expectEq('A3', okm, '8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8'); });
});
step('HKDF_deriveKey_to_AESGCM_roundtrip', function() {
  var ikm = hexToBytes('000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f');
  return subtle.importKey('raw', ikm, {name:'HKDF'}, false, ['deriveKey'])
    .then(function(base) {
      return subtle.deriveKey({name:'HKDF',hash:'SHA-256',salt:new TextEncoder().encode('antibot-fp-encryption-key')},
                              base, {name:'AES-GCM',length:256}, false, ['encrypt','decrypt']);
    })
    .then(function(aes) {
      var iv = hexToBytes('0102030405060708090a0b0c');
      var msg = new TextEncoder().encode('{"fp":"webcrypto-shim-test"}');
      return subtle.encrypt({name:'AES-GCM', iv:iv}, aes, msg)
        .then(function(ct) {
          return subtle.decrypt({name:'AES-GCM', iv:iv}, aes, ct).then(function(back) {
            // shim 的简化版 TextDecoder 不识别 ArrayBuffer，按 spec 形状先包 view
            var plain = new TextDecoder().decode(new Uint8Array(back));
            if (plain !== '{"fp":"webcrypto-shim-test"}') {
              throw new Error('roundtrip plaintext mismatch: ' + plain);
            }
            return 'enc/dec roundtrip OK';
          });
        });
    });
});
"#,
    );
    expect_ok(&stdout, "HKDF_5869_A1");
    expect_ok(&stdout, "HKDF_5869_A3_empty_salt_info");
    expect_ok(&stdout, "HKDF_deriveKey_to_AESGCM_roundtrip");
}

/// generateKey → exportKey('raw') 65 字节 0x04 往返 + JWK 往返 + 自洽派生 +
/// 用法门禁 + usages 校验。同时输出引擎内 ECDH 耗时（Date.now 粗测）。
#[test]
fn ecdh_generate_key_export_import_roundtrip_and_perf() {
    let stdout = run_webcrypto_html(
        r#"
step('ECDH_GEN_export_raw65', function() {
  return subtle.generateKey({name:'ECDH',namedCurve:'P-256'}, true, ['deriveBits'])
    .then(function(kp) {
      if (!kp.publicKey || !kp.privateKey || kp.publicKey.type !== 'public' || kp.privateKey.type !== 'private') {
        throw new Error('generateKey must yield {publicKey, privateKey}');
      }
      return subtle.exportKey('raw', kp.publicKey).then(function(raw) {
        var u = new Uint8Array(raw);
        if (u.length !== 65 || u[0] !== 4) throw new Error('raw export must be 65B starting 0x04, got len=' + u.length);
        return '65B 0x04 OK';
      });
    });
});
step('ECDH_GEN_jwk_roundtrip', function() {
  return subtle.generateKey({name:'ECDH',namedCurve:'P-256'}, true, ['deriveBits'])
    .then(function(kp) {
      return subtle.exportKey('raw', kp.publicKey).then(function(raw0) {
        return subtle.exportKey('jwk', kp.publicKey).then(function(jwk) {
          if (jwk.kty !== 'EC' || jwk.crv !== 'P-256' || !jwk.x || !jwk.y) throw new Error('bad public JWK shape');
          return subtle.importKey('jwk', jwk, {name:'ECDH',namedCurve:'P-256'}, false, [])
            .then(function(reimported) { return subtle.exportKey('raw', reimported); })
            .then(function(raw2) {
              if (bytesToHex(raw2) !== bytesToHex(raw0)) throw new Error('jwk roundtrip changed the key');
              return 'key identical';
            });
        });
      });
    });
});
step('ECDH_GEN_derive_symmetry_and_perf', function() {
  return subtle.generateKey({name:'ECDH',namedCurve:'P-256'}, true, ['deriveBits'])
    .then(function(kp) {
      var t0 = Date.now();
      return subtle.generateKey({name:'ECDH',namedCurve:'P-256'}, true, ['deriveBits'])
        .then(function(kp2) {
          var genMs = Date.now() - t0;
          return subtle.deriveBits({name:'ECDH',public:kp2.publicKey}, kp.privateKey, 256)
            .then(function(s1) {
              var t1 = Date.now();
              return subtle.deriveBits({name:'ECDH',public:kp.publicKey}, kp2.privateKey, 256)
                .then(function(s2) {
                  var deriveMs = Date.now() - t1;
                  if (bytesToHex(s1) !== bytesToHex(s2)) throw new Error('ECDH asymmetry: both sides disagree');
                  if (genMs > 20000) throw new Error('generateKey too slow: ' + genMs + 'ms');
                  if (deriveMs > 20000) throw new Error('deriveBits too slow: ' + deriveMs + 'ms');
                  return 'symmetric gen=' + genMs + 'ms derive=' + deriveMs + 'ms';
                });
            });
        });
    });
});
step('ECDH_GEN_usage_gate', function() {
  // 私钥未授予 deriveBits 时 deriveBits 必须拒绝（InvalidAccessError）
  return subtle.generateKey({name:'ECDH',namedCurve:'P-256'}, true, ['deriveKey'])
    .then(function(kp) {
      return subtle.deriveBits({name:'ECDH',public:kp.publicKey}, kp.privateKey, 256)
        .then(function() { throw new Error('usage gate did not reject'); },
              function(e) {
                if (e && e.name === 'InvalidAccessError') return 'InvalidAccessError as expected';
                throw new Error('expected InvalidAccessError, got ' + (e && e.name));
              });
    });
});
step('ECDH_GEN_curve_point_validation', function() {
  // 曲线下非法点必须拒绝导入（DataError）
  var bad = hexToBytes('0400000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002');
  return subtle.importKey('raw', bad, {name:'ECDH',namedCurve:'P-256'}, false, [])
    .then(function() { throw new Error('off-curve point accepted'); },
          function(e) {
            if (e && e.name === 'DataError') return 'DataError as expected';
            throw new Error('expected DataError, got ' + (e && e.name));
          });
});
step('SHA256_digest_regression', function() {
  return subtle.digest('SHA-256', new TextEncoder().encode('abc')).then(function(d) {
    expectEq('sha256-abc', d, 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
  });
});
"#,
    );
    expect_ok(&stdout, "ECDH_GEN_export_raw65");
    expect_ok(&stdout, "ECDH_GEN_jwk_roundtrip");
    expect_ok(&stdout, "ECDH_GEN_derive_symmetry_and_perf");
    expect_ok(&stdout, "ECDH_GEN_usage_gate");
    expect_ok(&stdout, "ECDH_GEN_curve_point_validation");
    expect_ok(&stdout, "SHA256_digest_regression");
    // 打印引擎内性能（粗测），便于 PROGRESS 记录
    for line in stdout.lines() {
        if line.contains("symmetric gen=") {
            eprintln!("[perf] {line}");
        }
    }
}
