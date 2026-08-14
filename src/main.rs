#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

mod error;
use crate::error::{AppError, AppResult};
use tauri::{Emitter, Manager, WindowEvent};

mod direct;
mod httpd;
mod state;

use state::AppState;

/// 原生 Windows 文件夹选择对话框（用于“选择保存目录”，无需额外依赖）
#[cfg(target_os = "windows")]
#[allow(clippy::upper_case_acronyms)] // FFI 类型名沿用 Windows SDK 命名
mod filedlg {
    use std::os::windows::ffi::OsStringExt;
    use std::ptr;

    type PVOID = *mut std::ffi::c_void;

    #[repr(C)]
    struct BROWSEINFO {
        hwnd_owner: PVOID,
        pidl_root: PVOID,
        psz_display_name: *mut u16,
        lpsz_title: *const u16,
        ul_flags: u32,
        lpfn: PVOID,
        lparam: PVOID,
        i_image: i32,
    }

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(pv: PVOID, coinit: u32) -> i32;
        fn CoUninitialize();
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHBrowseForFolderW(bi: *mut BROWSEINFO) -> PVOID;
        fn SHGetPathFromIDListW(pidl: PVOID, psz_path: *mut u16) -> i32;
        fn CoTaskMemFree(pv: PVOID);
    }

    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    const BIF_RETURNONLYFSDIRS: u32 = 0x0000_0001;
    const BIF_NEWDIALOGSTYLE: u32 = 0x0000_0040;
    const MAX_PATH: usize = 260;

    /// 弹出系统文件夹选择框，返回所选绝对路径；取消则返回 None
    /// owner 为主窗口句柄（以 usize 传递，便于跨线程；内部转回 HWND），
    /// 传入后对话框以主窗口为父、置顶显示，避免被主窗口遮挡而“点了没反应”。
    pub fn pick_folder(owner: usize) -> Option<String> {
        // SHBrowseForFolderW 必须在 STA 线程运行；主线程已被 WebView2 初始化为
        // MTA，直接调用会弹不出对话框。这里放到独立的 STA 线程执行。
        let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(ptr::null_mut(), COINIT_APARTMENTTHREADED);
                let path = inner_pick(owner);
                CoUninitialize();
                let _ = tx.send(path);
            }
        });
        rx.recv().ok().flatten()
    }

    /// 实际弹窗逻辑（调用方保证在 STA 线程）
    unsafe fn inner_pick(owner: usize) -> Option<String> {
        let title: Vec<u16> = "选择下载保存目录"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut disp: [u16; MAX_PATH] = [0; MAX_PATH];
        let mut bi = BROWSEINFO {
            hwnd_owner: owner as PVOID,
            pidl_root: ptr::null_mut(),
            psz_display_name: disp.as_mut_ptr(),
            lpsz_title: title.as_ptr(),
            ul_flags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
            lpfn: ptr::null_mut(),
            lparam: ptr::null_mut(),
            i_image: 0,
        };
        let pidl = SHBrowseForFolderW(&mut bi);
        let result = if !pidl.is_null() {
            let mut path: [u16; MAX_PATH] = [0; MAX_PATH];
            if SHGetPathFromIDListW(pidl, path.as_mut_ptr()) != 0 {
                let len = path.iter().position(|&c| c == 0).unwrap_or(0);
                let s = std::ffi::OsString::from_wide(&path[..len]);
                Some(s.to_string_lossy().into_owned())
            } else {
                None
            }
        } else {
            None
        };
        if !pidl.is_null() {
            CoTaskMemFree(pidl);
        }
        result
    }
}

/// 控制台面板宽度（px），子 webview 从该位置开始
const PANEL_W: f64 = 360.0;

/// 在目标 webview 中注入嗅探脚本（document_start，MAIN world）
const HOOK_JS: &str = include_str!("hook.js");

/// 命令行参数（Tauri v2 核心不含 CLI 解析，故手动解析 std::env::args）
struct CliOpts {
    url: Option<String>,
    save_dir: Option<String>,
    rate: Option<f64>,
    port: Option<u16>,
    no_sniff: bool,
    no_auto: bool,
    no_copy_unlock: bool,
    no_mux: bool,
}

fn parse_cli() -> CliOpts {
    let mut opts = CliOpts {
        url: None,
        save_dir: None,
        rate: None,
        port: None,
        no_sniff: false,
        no_auto: false,
        no_copy_unlock: false,
        no_mux: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--save-dir" => opts.save_dir = it.next(),
            "--rate" => opts.rate = it.next().and_then(|s| s.parse::<f64>().ok()),
            "--port" => opts.port = it.next().and_then(|s| s.parse::<u16>().ok()),
            "--no-sniff" => opts.no_sniff = true,
            "--no-auto" => opts.no_auto = true,
            "--no-copy-unlock" => opts.no_copy_unlock = true,
            "--no-mux" => opts.no_mux = true,
            "--url" => opts.url = it.next(),
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--version" | "-v" => {
                println!("MediaDown {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            s if s.starts_with("--") => { /* 忽略未知长选项 */ }
            s => {
                if opts.url.is_none() {
                    opts.url = Some(s.to_string());
                }
            }
        }
    }
    opts
}

fn print_help() {
    println!(r#"MediaDown - 在线媒体嗅探与下载工具

用法:
  MediaDown-x86.exe [URL] [选项]

参数:
  URL              启动后直接打开并嗅探的网址
                 例: MediaDown-x86.exe "https://tv.cctv.com/..."

选项:
  --save-dir <路径>  下载保存目录（默认 <程序目录>\downloads，可经配置文件保存）
  --rate <倍速>      播放/下载倍速 0.5~16，例如 2
  --port <端口>      本地分片接收服务器端口（默认随机）
  --no-sniff         禁用嗅探/下载（仅浏览）
  --no-auto          禁用自动下载（捕获轨道后不自动开始）
  --no-copy-unlock   禁用解除复制限制
  -h, --help         显示本帮助
  -v, --version      显示版本号
"#);
}

/// 导航到目标站点（点击地址栏"打开"或状态栏地址）
#[tauri::command]
fn navigate_to(app: tauri::AppHandle, url: String) -> AppResult<()> {
    let trimmed = url.trim().to_string();
    if trimmed.is_empty() {
        return Err(AppError::InvalidArg("地址不能为空".into()));
    }
    let full = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed
    } else {
        format!("https://{}", trimmed)
    };
    let webview = app
        .get_webview("browser")
        .ok_or_else(|| AppError::NotFound("浏览器 webview 未初始化".into()))?;
    webview
        .navigate(full.parse::<url::Url>()?)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(())
}

/// 打开本地保存目录（explorer）
#[tauri::command]
fn open_save_dir(app: tauri::AppHandle) -> AppResult<()> {
    let state = app.state::<Arc<AppState>>();
    let dir = state.save_dir();
    std::fs::create_dir_all(&dir)?;
    std::process::Command::new("explorer")
        .arg(&dir)
        .spawn()?;
    Ok(())
}

/// 选择保存目录（弹出系统文件夹选择框），返回新目录
///
/// 关键：必须为 async 并用 spawn_blocking 包裹阻塞的对话框调用。
/// 同步命令会阻塞承载模态对话框 owner 的主线程消息循环，而 SHBrowseForFolderW
/// 需要通过主线程泵消息来完成跨线程 COM 封送，于是死锁表现为“点修改目录卡死程序”。
/// 改为异步后，主线程（异步运行时）在 await 期间不被阻塞，对话框 owner 可正常响应。
#[tauri::command]
async fn pick_save_dir(app: tauri::AppHandle) -> AppResult<String> {
    #[cfg(target_os = "windows")]
    {
        // 取主窗口 HWND 作为对话框父窗口，确保置顶可见
        let owner: usize = app
            .get_window("main")
            .and_then(|w| w.hwnd().ok())
            .map(|h| h.0 as usize)
            .unwrap_or(0);
        let picked = tauri::async_runtime::spawn_blocking(move || filedlg::pick_folder(owner))
            .await
            .unwrap_or(None);
        match picked {
            Some(p) => {
                let state = app.state::<Arc<AppState>>();
                state.set_save_dir(&p)?;
                Ok(state.save_dir())
            }
            None => Err(AppError::InvalidArg("未选择目录".into())),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        Err(AppError::InvalidArg("当前平台不支持目录选择".into()))
    }
}

/// 界面轮询当前状态
#[tauri::command]
fn get_state(app: tauri::AppHandle) -> serde_json::Value {
    let state = app.state::<Arc<AppState>>();
    state.snapshot()
}

/// 设置自定义文件名（trackId -> name）
#[tauri::command]
fn set_name(app: tauri::AppHandle, track_id: String, name: String) -> AppResult<()> {
    let state = app.state::<Arc<AppState>>();
    state.set_name(&track_id, name)
}

/// 按项目整体更名（project -> name），同一项目的 video/audio 一并改并重命名磁盘文件
#[tauri::command]
fn set_project_name(app: tauri::AppHandle, project: String, name: String) -> AppResult<()> {
    let state = app.state::<Arc<AppState>>();
    state.set_name_by_project(&project, name)
}

/// 设置倍速（影响 video.playbackRate）—— 同时广播 md-rate 事件，
/// 让注入目标站的 hook.js 立即把新倍速应用到正在播放的 <video>。
#[tauri::command]
fn set_rate(app: tauri::AppHandle, rate: f64) -> AppResult<()> {
    let state = app.state::<Arc<AppState>>();
    state.set_rate(rate)?;
    let _ = app.emit("md-rate", rate);
    Ok(())
}

/// 高精度同步（缓冲重构）开关：开启后 mp4 轨道先内存缓冲再重建，修正音画漂移。
#[tauri::command]
fn set_high_precision(app: tauri::AppHandle, enabled: bool) -> AppResult<()> {
    app.state::<Arc<AppState>>().set_high_precision(enabled)
}

/// 设置轨道空闲超时秒数（超过后自动收尾）
#[tauri::command]
fn set_idle_timeout(app: tauri::AppHandle, secs: u32) -> AppResult<()> {
    app.state::<Arc<AppState>>().set_idle_timeout(secs)
}

/// 手动结束轨道下载并立即收尾（同时结束同 session 所有轨道）
#[tauri::command]
fn manual_end_track(app: tauri::AppHandle, track_id: String) -> AppResult<()> {
    let id: u32 = track_id
        .parse()
        .map_err(|_| AppError::InvalidArg("bad track id".into()))?;
    app.state::<Arc<AppState>>().manual_end_all_session(id)
}

/// 插件总开关
#[tauri::command]
fn set_enabled(app: tauri::AppHandle, enabled: bool) -> AppResult<()> {
    let state = app.state::<Arc<AppState>>();
    state.set_enabled(enabled)
}

/// 自动下载开关（捕获到轨道后自动开始）
#[tauri::command]
fn set_auto(app: tauri::AppHandle, auto: bool) -> AppResult<()> {
    let state = app.state::<Arc<AppState>>();
    state.set_auto(auto)
}

/// 解除复制限制开关（user-select / 右键 / 复制 / 选择 拦截）
#[tauri::command]
fn set_copy_unlock(app: tauri::AppHandle, enabled: bool) -> AppResult<()> {
    let state = app.state::<Arc<AppState>>();
    state.set_copy_unlock(enabled)
}

/// 下载后自动调用 tools/mkvmerge.exe 混流（video+audio -> mkv）开关
#[tauri::command]
fn set_mux(app: tauri::AppHandle, mux: bool) -> AppResult<()> {
    app.state::<Arc<AppState>>().set_mux(mux)
}

/// 启动 / 停止下载某条轨道（边下边存，文件持续增长）
#[tauri::command]
fn download_track(app: tauri::AppHandle, track_id: String, start: bool) -> AppResult<()> {
    let state = app.state::<Arc<AppState>>();
    if start {
        state.download_start(&track_id)
    } else {
        state.download_stop(&track_id)
    }
}

/// 将已结束的轨道收尾为标准 MP4（重建索引表）
#[tauri::command]
fn finalize_track(app: tauri::AppHandle, track_id: String) -> AppResult<()> {
    let state = app.state::<Arc<AppState>>();
    state.finalize(&track_id)
}

/// 重置全部（清空会话）
#[tauri::command]
fn clear_all(app: tauri::AppHandle) -> AppResult<()> {
    let state = app.state::<Arc<AppState>>();
    state.clear_all()
}

/// 下载直链（普通 http 视频），支持断点续传
#[tauri::command]
fn direct_download(
    app: tauri::AppHandle,
    id: String,
    url: String,
    name: String,
    start: bool,
) -> AppResult<()> {
    let state = app.state::<Arc<AppState>>();
    if start {
        let id_u64: u64 = id.parse().map_err(|_| AppError::InvalidArg("bad direct id".into()))?;
        state.direct_register(&id, &url, &name)?;
        let arc = Arc::clone(&state);
        std::thread::spawn(move || {
            if let Err(e) = direct::run_direct(&arc, id_u64, &url, &name) {
                let mut ds = arc.directs.lock().unwrap();
                if let Some(d) = ds.iter_mut().find(|d| d.id == id_u64) {
                    d.error = Some(e.to_string());
                    d.downloading = false;
                }
            }
        });
        Ok(())
    } else {
        state.direct_stop(&id)
    }
}

fn main() {
    tauri::Builder::default()
        .manage(Arc::new(AppState::new()))
        .invoke_handler(tauri::generate_handler![
            navigate_to,
            open_save_dir,
            pick_save_dir,
            get_state,
            set_name,
            set_project_name,
            set_rate,
            set_enabled,
            set_auto,
            set_copy_unlock,
            set_mux,
            set_high_precision,
            set_idle_timeout,
            manual_end_track,
            download_track,
            finalize_track,
            clear_all,
            direct_download,
        ])
        .setup(|app| {
            let cli = parse_cli();
            let state = app.state::<Arc<AppState>>();

            // 记录 AppHandle，供混流完成后向 UI 推送 md-mux 事件
            state.set_app(app.handle().clone());

            // 1) 应用命令行参数（主要功能 / 关键信息均可经 CLI 控制）
            if let Some(d) = &cli.save_dir {
                let _ = state.set_save_dir(d);
            }
            if cli.no_sniff {
                let _ = state.set_enabled(false);
            }
            if cli.no_auto {
                let _ = state.set_auto(false);
            }
            if cli.no_copy_unlock {
                let _ = state.set_copy_unlock(false);
            }
            if cli.no_mux {
                let _ = state.set_mux(false);
            }
            if let Some(r) = cli.rate {
                let _ = state.set_rate(r);
            }
            let cli_port = cli.port;
            let start_url = cli.url.clone();

            // 2) 启动本地分片接收服务器（优先用 --port）
            let token = state.server_token();
            let port = httpd::spawn_server(state.inner().clone(), token, cli_port)?;
            state.set_port(port as u32);

            // 3) 子 webview：目标站点浏览器，注入 hook.js
            // 注意：add_child 是 Window（多 webview 承载者）的方法，不是 WebviewWindow
            let main_window = app.get_window("main").unwrap();
            let _child = main_window.add_child(
                tauri::webview::WebviewBuilder::new("browser", tauri::WebviewUrl::App("start.html".into()))
                    .initialization_script(HOOK_JS),
                tauri::LogicalPosition::new(PANEL_W, 0.0),
                tauri::LogicalSize::new(1040.0, 900.0),
            )?;

            // 4) 确保主窗口在资源就绪后显示（避免启动时短暂黑屏）
            main_window.show()?;

            // 5) 若指定了 URL，立即导航并开始嗅探
            if let Some(u) = start_url {
                let full = if u.starts_with("http://") || u.starts_with("https://") {
                    u
                } else {
                    format!("https://{}", u)
                };
                if let Some(wv) = app.get_webview("browser") {
                    if let Ok(parsed) = full.parse::<url::Url>() {
                        let _ = wv.navigate(parsed);
                    }
                }
            }

            // 6) 启动空闲超时检查线程：轨道已 ended 但长时间未活动未收尾时，自动触发收尾
            //    避免站点不调用 endOfStream（如视频播放完但页面未卸载）导致轨道永远“下载中”。
            {
                let state = Arc::clone(state.inner());
                use std::collections::HashMap;
                use std::time::Instant;
                let mut last_activity: HashMap<u32, Instant> = HashMap::new();

                std::thread::spawn(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(5));
                        let timeout = state.idle_timeout_secs();
                        if timeout == 0 {
                            continue;
                        }
                        let timeout_dur = std::time::Duration::from_secs(timeout as u64);

                        // 更新最后活动时间（正在下载或有数据则重置计时）
                        {
                            let ts = state.tracks.lock().unwrap();
                            for t in ts.iter() {
                                if t.downloading || t.segments > 0 {
                                    last_activity.insert(t.id, Instant::now());
                                }
                            }
                        }

                        // 收集超时未活动且已结束但未收尾的轨道
                        let now = Instant::now();
                        let needs_finalize: Vec<u32> = {
                            let ts = state.tracks.lock().unwrap();
                            ts.iter()
                                .filter(|t| t.ended && !t.finalized && !t.finalizing)
                                .map(|t| t.id)
                                .collect()
                        };

                        for id in needs_finalize {
                            let should = match last_activity.get(&id) {
                                Some(last) => now.duration_since(*last) > timeout_dur,
                                None => true, // 从未记录过活动，直接收尾
                            };
                            if should {
                                eprintln!(
                                    "[main] idle checker: track {} idle for {}s, auto-finalizing",
                                    id, timeout
                                );
                                state.auto_finalize(id);
                            }
                        }
                    }
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // 子 webview 跟随主窗口缩放（保持左侧控制台宽度）
            if let Some(webview) = window.get_webview("browser") {
                if let WindowEvent::Resized(size) = event {
                    let w = (size.width as f64 - PANEL_W).max(400.0);
                    let h = size.height as f64;
                    let _ = webview.set_position(tauri::LogicalPosition::new(PANEL_W, 0.0));
                    let _ = webview.set_size(tauri::LogicalSize::new(w, h));
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
