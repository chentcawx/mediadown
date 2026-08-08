//! 直链下载：普通 HTTP 视频（video src 直接指向 .mp4/.webm 等）
//!
//! reqwest 流式下载，支持断点续传（Range）、可中断、边下边存。

use std::io::Write;

use crate::state::AppState;

pub fn run_direct(state: &AppState, id: u64, url: &str, name: &str) -> Result<(), String> {
    let dir = state.save_dir();
    let _ = std::fs::create_dir_all(&dir);
    let base = sanitize(name);
    let ext = guess_ext(url);
    let tmp = format!("{}\\{}-{}.part", dir, base, id); // 临时文件带任务 id，避免同名任务并发共写同一 .part
    let out = format!("{}\\{}.{}", dir, base, ext);

    let client = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120 Safari/537.36")
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| e.to_string())?;

    // 已下载多少（续传）
    let mut existing = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);

    // 单次请求流程：断点续传只需一个 Range 请求，服务器忽略 Range 时
    // 自然退化为整体下载，无需外层重试循环。
    if is_aborted(state, id) {
        finish(state, id, &out, &tmp, true);
        return Ok(());
    }
    let mut req = client.get(url);
    if existing > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={}-", existing));
    }
    let resp = req.send().map_err(|e| e.to_string())?;
    if resp.status().is_redirection() {
        return Err("重定向过多".into());
    }
    if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT
        || resp.status() == reqwest::StatusCode::OK
    {
        // 记录总量
        let total = resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        if existing == 0 {
            if let Some(t) = total {
                set_total(state, id, existing + t);
            }
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        let mut buf = [0u8; 256 * 1024];
        let mut bytes = std::io::BufReader::new(resp);
        loop {
            if is_aborted(state, id) {
                finish(state, id, &out, &tmp, true);
                return Ok(());
            }
            let n = std::io::Read::read(&mut bytes, &mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            existing += n as u64;
            set_done(state, id, existing);
        }
        file.flush().map_err(|e| e.to_string())?;
    } else {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }

    // 完成：改名（跨盘/目标已存在可能会失败，兜底为流式复制，避免"显示完成、实际还在 .part"）
    if std::fs::rename(&tmp, &out).is_err() {
        let mut sf = std::fs::File::open(&tmp).map_err(|e| e.to_string())?;
        let mut df = std::fs::File::create(&out).map_err(|e| e.to_string())?;
        std::io::copy(&mut sf, &mut df).map_err(|e| e.to_string())?;
    }
    finish(state, id, &out, &tmp, false);
    Ok(())
}

fn is_aborted(state: &AppState, id: u64) -> bool {
    let ds = state.directs.lock().unwrap();
    ds.iter()
        .find(|d| d.id == id)
        .map(|d| d.aborted)
        .unwrap_or(false)
}

fn set_total(state: &AppState, id: u64, total: u64) {
    let mut ds = state.directs.lock().unwrap();
    if let Some(d) = ds.iter_mut().find(|d| d.id == id) {
        d.total = Some(total);
    }
}

fn set_done(state: &AppState, id: u64, done: u64) {
    let mut ds = state.directs.lock().unwrap();
    if let Some(d) = ds.iter_mut().find(|d| d.id == id) {
        d.done = done;
    }
}

fn finish(state: &AppState, id: u64, out: &str, tmp: &str, aborted: bool) {
    let mut ds = state.directs.lock().unwrap();
    if let Some(d) = ds.iter_mut().find(|d| d.id == id) {
        d.downloading = false;
        d.finished = true;
        d.aborted = aborted;
        if !aborted {
            d.out_path = Some(out.to_string());
            let _ = std::fs::remove_file(tmp);
        }
    }
}

fn sanitize(name: &str) -> String {
    // 防长标题撑爆 Windows MAX_PATH(260)：stem 截断到 120 字符
    let s: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if (c as u32) < 32 => '_',
            c => c,
        })
        .take(120)
        .collect();
    let s = s.trim();
    if s.is_empty() {
        "download".to_string()
    } else {
        s.to_string()
    }
}

fn guess_ext(url: &str) -> String {
    let lower = url.to_lowercase();
    for (pat, ext) in [
        (".mp4", "mp4"),
        (".m4v", "m4v"),
        (".webm", "webm"),
        (".mkv", "mkv"),
        (".mov", "mov"),
        (".flv", "flv"),
        (".mp3", "mp3"),
        (".m4a", "m4a"),
        (".aac", "aac"),
        (".ts", "ts"),
        (".m3u8", "m3u8"),
        (".jpg", "jpg"),
        (".jpeg", "jpeg"),
        (".png", "png"),
        (".gif", "gif"),
        (".webp", "webp"),
    ] {
        if lower.contains(pat) {
            return ext.to_string();
        }
    }
    "mp4".to_string()
}
