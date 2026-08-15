# 项目 HandOff — MediaDown 媒体下载器

> 阶段收尾：2026-08-15。本轮聚焦"WebView2 内存持续增长"排查与修复，以及"高精度模式混流失败"bug 修复。

## 项目概述
基于 Tauri v2 (Rust) + 注入式前端 hook 的"边下边存"媒体下载器。劫持网页 MediaSource / SSE / fetch，把分片实时 POST 到本地 Rust 服务落盘，支持两种模式：高精度（内存缓冲后 fmp4 重建）与边下边存（流式直写 `.tmp-part`）。

## 当前状态（2026-08-15）
- ✅ **WebView2 内存泄漏已定位并修复**：根因是注入脚本 `hook-ts/mse.ts` 中的强引用全局数组 `msEntries`，钉死每个 `MediaSource`（含每条轨道最多 64 MB 的 `pending` 缓冲），仅 `endOfStream` 才清理。已改为 `WeakMap<MediaSource, entry[]>`。
- ✅ 已重编 32 位 release 绿色版 `MediaDown-x86/MediaDown-x86.exe`，冒烟通过。
- ✅ **高精度模式混流失败 bug 已修复**：根因 `src/fmp4/buffered_recon.rs` 的 `TrackBuffer::append` 只识别 init/moof，**独立发送的 mdat 分片被丢弃** → HP 纯缓冲路径重建的 MP4 缺样本数据 → 混流失败（非 HP 走 `.tmp-part` 原始字节故正常）。已让 `append` 按顶层 box 拆分捕获 moof+mdat 并配对（合并/分离两种发送方式均覆盖），`tb.finalize`/`flush_raw`/`activate_fallback` 输出 `moof+mdat` 交替。`cargo test --lib` 全绿（含 2 个新增 mdat 回归测试）。
- ✅ **`mp4_duration_sec` 死代码修复**：该函数 stts 分支曾硬编码 `ts = 0u32` 导致时长一致性校验形同虚设，已改用 `find_mdhd_timescale` 真实分母。
- ⏳ **待真实环境验证**：内存与混流修复均经单测/引用图验证，运行时需用户实跑确认（开 HP 下完整下载一集并观察是否产出 mkv）。

## 关键决策
1. **WeakMap 替换强数组**：避免注入脚本在 WebView2 跨 SPA 跳转 / 直播未 `endOfStream` / 中途重建 `MediaSource` 时累积对象 → 64 MB×N 泄漏。
2. **HP 混流修复核心**：MSE appendBuffer 的 chunk 可能是 `moof+mdat` 合并段，也可能分两次各发 `moof`/`mdat`。缓冲重建必须把 mdat 与 moof 配对（按出现顺序，`fmp4::finalize` 以索引配对 moof[i]↔mdat[i]）。`last_moof` key 持久化在 TrackBuffer 上以跨 append 调用保持配对。
3. **构建链路**：`hook-ts/*.ts` → `npm run build:hook`(esbuild) → `src/hook.js`（include_str! 嵌入）→ 必须重编 Rust。
4. **32 位绿色版**：`cargo build --release --target i686-pc-windows-msvc`，产物拷到 `MediaDown-x86/`。

## 文件结构
- `src/fmp4/buffered_recon.rs` —— **本轮主要改动**：`TrackBuffer::append` 捕获 mdat、`Seg.mdat` 字段、`last_moof` 持久 key、finalize/flush_raw/activate_fallback 输出 mdat；含 mdat 回归测试。
- `src/state.rs` —— AppState / append_chunk / finalize_impl / `mp4_duration_sec` 修复 / 高精度与流式 gating（含流式回归测试模块）。
- `hook-ts/mse.ts` —— MSE 劫持（上轮 WeakMap 修复）。
- `src/hook.js` —— 由 hook-ts 生成（勿手工改）。
- `src/fmp4/io.rs` —— fmp4 `finalize`（按 moof/mdat 索引配对，重建标准 MP4）。
- `src/main.rs` `src/httpd.rs` `src/direct.rs` `ui/index.html` `MediaDown-x86/MediaDown-x86.exe` —— 接线 / 端点 / 流式 / UI / 绿色版。

## 阶段待办
- [ ] 真实环境开 HP 下载一集验证自动混流产出 mkv。
- [ ] 真实环境跑数小时确认 `msedgewebview2.exe` 内存不再单调增长。
- [ ] 考虑为 `hook-ts` 加轻量单测。

## 踩过的坑
- **HP 混流根因定位**：不能只看 finalize 路径，要先确认 HP 纯缓冲分支的输入与非 HP 的差异——HP 在内存里重组 moof，非 HP 直接落盘原始字节；mdat 在 HP 下被 append 丢弃是关键。
- **跨调用状态**：mdat 可能在独立 append 调用到达，`last_key` 局部变量会丢失 → 必须持久化到 TrackBuffer 字段。
- **测试目标**：fmp4 模块的测试在 **lib crate**（`cargo test --lib`），不在 `--bin media-down`（bin 只有 state::streaming_tests 等少量）。
- esbuild 不在 PATH、TS lint WSL 路径怪癖、ad-hoc GC 验证不稳定（见上轮）。
