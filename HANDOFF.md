# MediaDown 项目 HandOff

## 项目概述
Tauri v2 + Rust 的在线媒体嗅探与下载工具。左侧控制台管理轨道和设置，右侧 WebView 加载目标站点并注入 hook.js 捕获媒体分片。

## 当前状态
✅ 已完成双窗口修复（黑屏问题）
✅ 构建测试通过

**修复内容**：
- `tauri.conf.json`: 添加 `"visible": false`，窗口初始隐藏
- `src/main.rs`: setup() 完成后调用 `main_window.show()?` 避免 WebView2 初始化黑屏
- 修复注释编号（4→5→6）

## 关键决策
1. **窗口初始不可见**：`visible: false` + 手动 `show()`，比监听 Focused 事件更可靠
2. **子 webview 命名**：主窗口 `main`，子 webview `browser`，add_child 挂载在 Panel_W(360px) 右侧
3. **HTML 文件**：`index.html`=控制台（主窗口），`start.html`=浏览器占位（子 webview 初始页）
4. **package.json 移植**：从 `D:\WorkBuddy\mediadown\media-down` 复制到本项目，修改脚本路径

## 文件结构
```
D:\WorkBuddy\mediadown2\
├── src/
│   ├── main.rs          # 入口，窗口 setup，命令注册
│   ├── state.rs         # AppState 管理
│   ├── httpd.rs         # 本地分片接收服务器
│   ├── direct.rs        # 直链下载
│   ├── fmp4/            # MP4 分片解析
│   └── hook.js          # 注入目标站点的嗅探脚本（由 hook-ts 编译）
├── hook-ts/             # TypeScript 源（hook.js 的来源）
├── ui/
│   ├── index.html       # 主窗口（控制台界面）
│   └── start.html       # 子 webview 初始页
├── tauri.conf.json      # Tauri 配置
├── package.json         # Node 脚本（build:hook, check:hook, test）
└── target/              # 构建产物
    ├── release/
    │   ├── media-down.exe
    │   └── bundle\nsis\MediaDown_0.1.0_x64-setup.exe
```

## 待办事项
- [ ] 安装后实际测试验证黑屏问题已解决
- [ ] 考虑是否需要 `capabilities/` 权限细化（当前 default.json 较宽泛）

## 踩过的坑
1. **黑屏/双窗口问题**：WebView2 初始化时短暂显示空白窗口，加 `visible: false` 解决
2. **npm 缺失**：项目根目录无 package.json，从旧项目复制并修改路径
3. **tauri CLI**：项目无全局 tauri CLI，需通过 node_modules/.bin 调用
4. **注释编号重复**：add_child 后插入代码导致后续注释编号混乱（4重复）

## 构建命令
```bash
cd D:\WorkBuddy\mediadown2
$env:PATH = ".\node_modules\.bin;$env:PATH"
tauri build
# 产物: target\release\bundle\nsis\MediaDown_0.1.0_x64-setup.exe
```
