# MediaDown 经验沉淀

## 本次修复的核心教训

### 问题：启动时出现黑色窗体
- **根因**：WebView2 初始化需要时间，主窗口 visible=true 时会短暂显示空白/黑屏
- **解决**：tauri.conf.json 设 `visible: false`，setup() 资源就绪后手动 `show()`
- **教训**：Tauri v2 多窗口场景，子 webview add_child 后需要显式控制主窗口可见性

### 问题：npm/tauri CLI 缺失
- **根因**：项目迁移后 package.json 未同步，node_modules 丢失
- **解决**：从原项目复制 package.json，修改脚本路径，复制 node_modules
- **教训**：Tauri 项目变更目录结构时需同步维护 package.json 中的相对路径

### 问题：注释编号混乱
- **根因**：在已有注释的步骤中间插入新步骤，未重编号
- **解决**：统一改为 4→5→6
- **教训**：插入代码块时同步更新后续注释编号

## 通用模式

| 场景 | 解决方案 |
|------|----------|
| Tauri 启动黑屏 | `visible: false` + 手动 `show()` |
| 多 webview 初始化 | 确保 add_child 完成后再 show 主窗口 |
| 项目复制迁移 | 检查 package.json 路径、node_modules |
