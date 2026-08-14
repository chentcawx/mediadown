fn main() {
    // 删除 tauri-codegen 缓存的嵌入资源，强制每次都用最新的 ui/ 重新生成。
    // 否则 tauri-codegen 仅在缓存文件“不存在”时才写入，旧缓存会一直被复用，
    // 导致改了 index.html/start.html 后，绿色版 exe 仍打包过时前端
    // （表现为新增的 UI 功能——如倍速按钮——不出现）。
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        let cache = std::path::Path::new(&out_dir).join("tauri-codegen-assets");
        let _ = std::fs::remove_dir_all(&cache);
    }

    tauri_build::build();

    // 显式跟踪关键 UI 文件，确保它们变化时本 build 脚本重新运行并重新嵌入前端。
    println!("cargo:rerun-if-changed=ui/index.html");
    println!("cargo:rerun-if-changed=ui/start.html");
}
