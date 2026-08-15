# MediaDown — 媒体下载器（边下边存）

基于 **Tauri v2 (Rust) + 注入式前端 hook** 的"边下边存"媒体下载器。劫持网页 `MediaSource` / `SSE` / `fetch`，把分片实时 POST 到本地 Rust 服务落盘，支持两种模式：

- **高精度同步**：分片在内存缓冲后由 `fmp4` 重建为标准 MP4（用于对齐音视频零点、校正漂移）。
- **边下边存**：分片流式直写 `.tmp-part`，收尾时再重建封装。

下载完成后自动用 `ffmpeg` / `mkvmerge` 把视频轨与音频轨混流为单个 `.mkv`。

## 构建（32 位绿色版，单 exe，无安装包）

```powershell
# 1. 类型检查注入脚本（改 hook-ts 后必跑）
npm run check:hook
npm run build:hook          # esbuild -> src/hook.js

# 2. 32 位发布版
cargo build --release --target i686-pc-windows-msvc
Copy-Item target\i686-pc-windows-msvc\release\media-down.exe MediaDown-x86\MediaDown-x86.exe -Force

# 3. 冒烟：标题应为 "MediaDown - 媒体下载器"
```

> `src/hook.js` 经 `main.rs` 的 `include_str!` 编译期嵌入，**改 `hook-ts/*.ts` 必须重编 Rust 才生效**。

## 混流工具（ffmpeg / mkvmerge）

混流依赖外部工具。本仓库不收录二进制，用脚本从本地源同步进绿色版：

```powershell
# 把 tools/ 下的 ffmpeg.exe / mkvmerge.exe 同步到 MediaDown-x86/tools/
pwsh -File scripts\bundle_tools.ps1
```

可用环境变量覆盖来源：`$env:MD_FFMPEG`、`$env:MD_MKVMERGE`。

## 测试

```powershell
cargo test --lib           # Rust 单元/集成测试（fmp4 重建、流式 gating 等）
```

> 注：本仓库 `npm run test` 为空操作（Rust 项目），验证请用 `cargo test --lib`。

## 目录结构

| 路径 | 作用 |
| --- | --- |
| `hook-ts/*.ts` | MSE 劫持核心（addSourceBuffer / endOfStream / appendBuffer）与分片上传 |
| `src/hook.js` | 由 `hook-ts` 生成的注入脚本（勿手工改） |
| `src/main.rs` | Tauri 命令接线 + `include_str!("src/hook.js")` 注入 |
| `src/state.rs` | AppState / append_chunk / finalize_impl / 高精度与流式 gating / 混流 guard |
| `src/fmp4/*.rs` | fmp4 缓冲重建 / 最终化 |
| `src/httpd.rs` | 本地 HTTP 分片接收端点 |
| `src/direct.rs` | 直接下载流式路径 |
| `ui/index.html` | 前端 UI（高精度开关等） |
| `scripts/bundle_tools.ps1` | 混流工具同步脚本 |
| `MediaDown-x86/` | 32 位绿色分发版输出目录（不进版本库） |

## 已知约束

- 高精度模式在内存中重组 MP4，必须保留 `moof` + `mdat` 配对（合并/分离两种发送方式均覆盖）。
- 混流前的"时长一致性校验"对读不到时长的轨道（fMP4 重建后常见某轨 `mdhd.duration=0`）自动跳过，仅双轨均可信且确实漂移才拦截。

## License

MIT
