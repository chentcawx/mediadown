# MediaDown

> 能播就能下

Rust + Tauri v2（双 webview）+ 原生 JS 的 MSE 媒体实时捕获下载器。注入目标站点的 hook 劫持 `MediaSource` 分片，边下边存，收尾后重建标准 MP4 并自动混流为单个 MKV。

## 核心特性

- **MSE 实时捕获**：劫持 `MediaSource.addSourceBuffer` / `endOfStream`，按轨分流（video / audio / text 各自落盘），边播边存
- **缓冲重构（高精度同步）**：mp4 轨道默认开启内存缓冲，分片按 `tfdt` 排序、去重、断点平滑，生成标准 MP4，修正音视频漂移与乱序导致的音画不同步
- **自动混流**：video + audio 成对收尾时，优先调用 `tools/ffmpeg.exe`，缺失则回退 `tools/mkvmerge.exe`，合并为单个 `.mkv`
- **无级变速**：0.1x–16x 实时控制右侧预览视频的播放速率，并一键恢复 1x
- **空闲超时自动收尾**：轨道已 ended 但长时间无活动（默认 60s，可配置 10–3600s）时自动触发收尾，避免站点不调用 `endOfStream` 导致轨道永远“下载中”
- **同 session 一键结束**：手动结束某条轨道时，同时结束同一播放会话的所有轨道
- **项目级批量更名**：捕获到媒体轨道后，可按项目统一重命名（一次改 video.mp4 + audio.m4a）
- **解除复制限制**：强制放开 `user-select` / 右键 / 复制 / 选择 拦截
- **直链嗅探**：同时劫持 `fetch` / `XHR` 并轮询 `<video>`/`<audio>.src`，上报直链媒体（mp4/webm/ts/flv/…）
- **命令行控制**：`--url` `--port` `--rate` `--no-sniff` `--no-auto` `--no-copy-unlock` `--no-mux` 等启动参数

## 界面布局

双 webview 结构：

- **主窗口（控制台）** `ui/index.html` —— 管理轨道列表、设置、日志
- **子 webview（浏览器）** `ui/start.html` —— 加载目标站点，注入 `src/hook.js` 嗅探脚本；从该位置右侧开始铺满

## 快速开始

### 环境

- Rust 工具链（MSVC 目标，推荐 `rustup` 安装）
- Node.js（用于将 `hook-ts/` 编译为注入脚本 `src/hook.js`）
- 可选：`tools/ffmpeg.exe`（优先）或 `tools/mkvmerge.exe`（混流回退）

### 本地构建

```bash
npm install
npm run build:hook   # 用 esbuild 把 hook-ts/ 编译为 src/hook.js
cargo build          # 调试构建
# cargo build --release   # 发布构建（产物：target/release/media-down.exe）
```

> `npm run check:hook` 可用 `tsc` 做类型检查。
> `src/hook.js` 由构建产物生成，**不要手工编辑**；源码在 `hook-ts/`。

### 使用

1. 把 `tools/ffmpeg.exe`（推荐）或 `tools/mkvmerge.exe` 放到程序同目录 `tools/` 下，混流才能生效
2. 启动程序，输入站点 URL，点击“打开”
3. 播放页面中的视频，轨道自动被捕获并保存到“下载目录”
4. 捕获完成后，程序自动收尾（高精度重建 MP4）并混流为 `.mkv`

### CLI 参数

| 参数 | 作用 |
|---|---|
| `--url <url>` | 启动时自动导航到该 URL |
| `--port <n>` | 指定本地分片接收服务器端口（默认随机） |
| `--rate <f>` | 默认倍速（0.1–16） |
| `--save-dir <路径>` | 下载保存目录（默认 `<程序目录>/downloads`） |
| `--no-sniff` | 关闭 MSE 嗅探（仅直链） |
| `--no-auto` | 关闭自动下载（需手动点“下载”） |
| `--no-copy-unlock` | 不解除复制限制 |
| `--no-mux` | 下载后不自动混流 |
| `-h, --help` | 显示帮助 |
| `-v, --version` | 显示版本号 |

## 技术架构

```
┌─────────────────────────────────────────────┐
│                 主窗口（Tauri）               │
│  ┌──────────┐   ┌────────────────────────┐  │
│  │ 控制台 UI │   │ 子 webview（目标站点）  │  │
│  │(index)   │   │ 注入 src/hook.js       │  │
│  └────┬─────┘   └───────────┬────────────┘  │
│       │                     │               │
│       │   Tauri invoke      │ MSE / fetch 劫持
│       ▼                     ▼               │
│  ┌──────────────────────────────────────┐   │
│  │         本地 HTTP 服务器 (httpd)       │   │
│  │  /seg/<token>/register               │   │
│  │  /seg/<token>/<id>/end               │   │
│  │  /seg/<token>/<id>   (分片上传)       │   │
│  └──────────────────────────────────────┘   │
│                     │                       │
│                     ▼                       │
│         AppState (in-memory)               │
│   · TrackBuffer（高精度缓冲重构）            │
│   · 自动收尾 + 混流线程                     │
└─────────────────────────────────────────────┘
```

- **Hook（注入脚本）**：基于 TypeScript（`hook-ts/`）开发，由 esbuild 打包为单 IIFE 脚本 `src/hook.js`，在 `document_start` / MAIN world 注入
- **后端**：纯 Rust（Tokio 异步运行时 + Tauri 命令），HTTP 服务器处理分片接收与轨道状态管理
- **fMP4 重建**：纯 Rust 实现 `stts/ctts/stsc/stsz/stss/stco` 表重建，输出可拖拽 seek 的标准 MP4（不依赖 ffmpeg 做重建）
- **缓冲重构**（`high_precision`）：开启后 mp4 轨道分片走 `TrackBuffer`，按 `tfdt` 排序去重，修正乱序 / 质量切换导致的漂移；单轨超限自动降级为流式追加

## 目录结构

```
.
├── Cargo.toml            # Rust 包（package: media-down）
├── tauri.conf.json       # Tauri v2 配置（窗口 / 打包 / 注入）
├── package.json          # Node 脚本（build:hook / check:hook / test）
├── build.rs              # tauri-build 代码生成
├── src/
│   ├── main.rs           # 入口、窗口 setup、命令注册、CLI 解析
│   ├── lib.rs
│   ├── state.rs          # AppState（轨道 / 直链 / 设置）
│   ├── httpd.rs          # 本地分片接收服务器
│   ├── direct.rs         # 直链下载（断点续传）
│   ├── error.rs
│   ├── hook.js           # 注入目标站点的嗅探脚本（由 hook-ts 编译）
│   └── fmp4/             # MP4 分片解析与重建
├── hook-ts/              # TypeScript 源（hook.js 的来源）
├── ui/
│   ├── index.html        # 主窗口（控制台界面）
│   └── start.html        # 子 webview 初始页
├── capabilities/         # Tauri 权限配置
├── gen/schemas/          # 生成的 schema
├── icons/                # 应用图标
└── docs/                 # 经验沉淀 / HandOff
```

## 构建产物

- 调试：`target/debug/media-down.exe`
- 发布：`target/release/media-down.exe`
- 绿色版：将 `media-down.exe` 与 `tools/*.exe` 放在同一目录即可直接运行，无需安装

## 许可

ISC
