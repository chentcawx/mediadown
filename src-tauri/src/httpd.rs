//! 本地分片接收服务器（127.0.0.1 固定端口区间）
//!
//! hook.js 捕获到的每个 MSE 分片通过 fetch 以二进制 body POST 到这里，
//! Rust 立即写入磁盘（边下边存，内存零堆积）。带 token 鉴权 + CORS 头
//! （含 Private-Network-Access 预检支持，保证 https 页面可访问）。
//!
//! 端口在 [49321..49331] 中取第一个空闲端口 —— hook 通过同样区间探测
//! GET /cfg 完成自发现（避免跨域 localStorage 不可见的问题）。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use crate::state::AppState;

const MAX_BODY: usize = 128 * 1024 * 1024; // 单个分片上限 128MB
const PORT_RANGE: std::ops::Range<u16> = 49321..49331;

pub fn spawn_server(
    state: Arc<AppState>,
    token: String,
    preferred_port: Option<u16>,
) -> Result<u16, String> {
    // 优先使用命令行指定的端口
    let preferred = preferred_port.and_then(|p| {
        if p == 0 {
            None
        } else {
            TcpListener::bind(("127.0.0.1", p)).ok().map(|l| (l, p))
        }
    });
    let mut listener = preferred;
    // 否则尝试固定区间端口
    if listener.is_none() {
        for port in PORT_RANGE {
            match TcpListener::bind(("127.0.0.1", port)) {
                Ok(l) => {
                    listener = Some((l, port));
                    break;
                }
                Err(_) => continue,
            }
        }
    }
    let (listener, port) = match listener {
        Some(x) => x,
        None => {
            let l = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
            let p = l.local_addr().map_err(|e| e.to_string())?.port();
            (l, p)
        }
    };

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(s) => {
                    let st = state.clone();
                    let tk = token.clone();
                    std::thread::spawn(move || {
                        let _ = handle(s, st, &tk);
                    });
                }
                Err(_) => break,
            }
        }
    });
    Ok(port)
}

fn handle(mut stream: std::net::TcpStream, state: Arc<AppState>, token: &str) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(60)));
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(60)));

    let mut lines = [0usize; 16];
    let mut n = 0usize;
    let mut buf = [0u8; 4096];
    let mut end = 0usize;
    loop {
        match stream.read(&mut buf[end..]) {
            Ok(0) => break,
            Ok(nread) => end += nread,
            Err(e) => {
                eprintln!("[httpd] read err: {}", e);
                return Ok(());
            }
        }
        if let Some(poss) = std::str::from_utf8(&buf[..end])
            .ok()
            .and_then(|s| s.find("\r\n\r\n"))
        {
            n = poss;
            break;
        }
        if end >= buf.len() {
            eprintln!("[httpd] header too large");
            return Ok(());
        }
    }

    let header = std::str::from_utf8(&buf[..n]).map_err(|_| {
        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
        std::io::Error::new(std::io::ErrorKind::InvalidData, "bad header")
    })?;

    for (i, line) in header.lines().enumerate() {
        if i >= lines.len() {
            break;
        }
        lines[i] = line.len();
    }

    if lines.is_empty() || lines[0] == 0 {
        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
        return Ok(());
    }

    let first = &buf[..lines[0]];
    let mut parts = first.splitn(3, |b| *b == b' ');
    let method = String::from_utf8_lossy(parts.next().unwrap_or(b"")).to_string();
    let target = match parts.next() {
        Some(t) => String::from_utf8_lossy(t).to_string(),
        None => {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
            return Ok(());
        }
    };
    let _version = parts.next();

    let cors = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET,POST,OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nAccess-Control-Expose-Headers: X-Download-Token\r\nAccess-Control-Allow-Private-Network: true\r\n";

    // CORS 预检（跨域 + https->127.0.0.1 私有网络访问都会触发 OPTIONS）。
    // 必须在路由前统一放行，否则浏览器会拦截真实 GET/POST（导致 /cfg 发现、
    // /seg 分片上传全部失败，表现为"能播不下载"）。
    if method.eq_ignore_ascii_case("OPTIONS") {
        respond(&mut stream, 204, cors, &[])?;
        return Ok(());
    }

    if target == "/cfg" {
        let cfg = serde_json::json!({
            "app": "mediadown",
            "port": state.port(),
            "token": state.server_token(),
            "enabled": state.enabled.load(std::sync::atomic::Ordering::Relaxed),
            "auto": state.auto.load(std::sync::atomic::Ordering::Relaxed),
            "copyUnlock": state.copy_unlock(),
            "rate": state.rate(),
        });
        respond(&mut stream, 200, cors, cfg.to_string().as_bytes())?;
    } else if target == "/options" {
        respond(&mut stream, 204, cors, &[])?;
    } else if target.starts_with("/seg/") {
        // 验证 token（token 在 /seg/{token}/... 位置）
        let seg = &target[5..]; // 去掉 "/seg/"
        let mut seg_parts = seg.split('/');
        let check_token = seg_parts.next().unwrap_or("");
        let a = seg_parts.next().unwrap_or("").to_string();
        let b = seg_parts.next().unwrap_or("").to_string();

        if check_token != token {
            respond(&mut stream, 403, cors, b"forbidden")?;
            return Ok(());
        }

        // 读取 body
        let mut content_length: usize = 0;
        let mut is_chunked = false;
        for line in header.lines().skip(1) {
            let ll = line.to_lowercase();
            if ll.starts_with("content-length:") {
                if let Ok(v) = line["content-length:".len()..].trim().parse::<usize>() {
                    content_length = v;
                }
            } else if ll.starts_with("transfer-encoding: trailers") || ll.contains("chunked") {
                is_chunked = true;
            }
        }

        let body_start = n + 4;
        let body = if is_chunked {
            // 解析 chunked 传输编码（fetch 偶发分块上传），避免旧实现直接丢弃
            // body 造成分片缺失、进而在播放时产生空白段。
            read_chunked_body(&mut stream, &buf[body_start..end])?
        } else {
            if body_start + content_length <= end {
                buf[body_start..body_start + content_length].to_vec()
            } else {
                // 需要继续读取
                let mut body = Vec::with_capacity(content_length);
                if body_start < end {
                    body.extend_from_slice(&buf[body_start..end]);
                }
                while body.len() < content_length {
                    let mut tmp = [0u8; 8192];
                    match stream.read(&mut tmp) {
                        Ok(0) => break,
                        Ok(nread) => body.extend_from_slice(&tmp[..nread]),
                        Err(_) => break,
                    }
                }
                body
            }
        };

        // 处理请求
        if a.is_empty() {
            respond(&mut stream, 400, cors, b"bad path")?;
        } else {
            let resp = handle_seg(&a, &b, &body, Arc::clone(&state));
            match resp {
                Ok(text) => {
                    respond(&mut stream, 200, cors, text.as_bytes())?;
                }
                Err(e) => {
                    respond(&mut stream, 500, cors, e.as_bytes())?;
                }
            }
        }
    } else {
        respond(&mut stream, 404, cors, b"not found")?;
    }

    Ok(())
}

fn handle_seg(a: &str, b: &str, body: &[u8], state: Arc<AppState>) -> Result<String, String> {
    match a {
        "register" => {
            let info: serde_json::Value =
                serde_json::from_slice(body).map_err(|e| e.to_string())?;
            let kind = info["kind"].as_str().unwrap_or("video");
            let mime = info["mime"].as_str().unwrap_or("");
            let ext = info["ext"].as_str().unwrap_or("mp4");
            let family = info["family"].as_str().unwrap_or("mp4");
            let title = info["title"].as_str().unwrap_or("");
            let auto = state.auto.load(std::sync::atomic::Ordering::Relaxed);
            let id = state.register_track(kind, mime, ext, family, title, auto);
            Ok(serde_json::json!({"id": id}).to_string())
        }
        "report" => {
            let rep: serde_json::Value =
                serde_json::from_slice(body).map_err(|e| e.to_string())?;
            state.add_media_report(&rep)?;
            Ok("ok".into())
        }
        "diag" => {
            let v: serde_json::Value =
                serde_json::from_slice(body).map_err(|e| e.to_string())?;
            state.set_hook_diag(&v);
            Ok("ok".into())
        }
        _ => {
            if b == "end" {
                let id: u32 = a.parse::<u32>().map_err(|e: std::num::ParseIntError| e.to_string())?;
                state.track_ended(id)?;
                // 自动下载开启且仍在下载时，流结束后自动收尾为可播放文件
                // （参照 media-sniffer-extension 的 endOfStream 自动保存行为）
                if state.auto() && state.is_downloading(id) {
                    let arc = Arc::clone(&state);
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(800));
                        arc.auto_finalize(id);
                    });
                }
                Ok("ok".into())
            } else {
                // /seg/{token}/{trackId}/chunk
                let track_id: u32 = a.parse::<u32>().map_err(|e: std::num::ParseIntError| e.to_string())?;
                state.append_chunk(track_id, body)?;
                Ok("ok".into())
            }
        }
    }
}

fn respond(
    stream: &mut std::net::TcpStream,
    status: u16,
    extra_headers: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let status_text = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "Error",
    };
    let content_length = body.len();
    let resp = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n{}\r\n",
        status, status_text, content_length, extra_headers
    );
    stream.write_all(resp.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

/// 解析 chunked Transfer-Encoding 的 body（用于 fetch 偶发的分块上传）。
/// `initial` 是已读入缓冲区、属于 body 起始部分的字节（首个 chunk-size 行可能已在其中）。
/// 避免旧实现直接 `vec![]` 丢弃 body 导致分片缺失、播放出现空白段。
fn read_chunked_body(stream: &mut std::net::TcpStream, initial: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut body: Vec<u8> = Vec::new();
    let mut buf: Vec<u8> = Vec::from(initial);
    loop {
        // 读取直到拿到一行 chunk-size（以 '\n' 结尾）
        let line_end = loop {
            if let Some(p) = buf.iter().position(|&b| b == b'\n') {
                break p;
            }
            let mut tmp = [0u8; 8192];
            let n = stream.read(&mut tmp)?;
            if n == 0 {
                return Ok(body); // 连接断开，返回已收内容
            }
            buf.extend_from_slice(&tmp[..n]);
        };
        let mut line = &buf[..line_end];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        let hex = line.split(|&b| b == b';').next().unwrap_or(&[]);
        let text = std::str::from_utf8(hex).map(|s| s.trim()).unwrap_or("0");
        let size = usize::from_str_radix(text, 16).unwrap_or(0);
        // 消费掉 chunk-size 行
        buf.drain(0..line_end + 1);
        if size == 0 {
            break; // 终止块
        }
        // 读取 size 字节的 chunk 数据 + 紧跟的 CRLF
        while buf.len() < size + 2 {
            let mut tmp = [0u8; 8192];
            let n = stream.read(&mut tmp)?;
            if n == 0 {
                if buf.len() >= size {
                    break;
                }
                return Ok(body); // 数据不足，尽力而为
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        if buf.len() >= size {
            body.extend_from_slice(&buf[..size]);
            buf.drain(0..size);
            if buf.len() >= 2 {
                buf.drain(0..2); // 丢弃 CRLF
            }
        } else {
            break;
        }
    }
    Ok(body)
}
