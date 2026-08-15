# 经验沉淀 — MediaDown 调试复盘（2026-08-15）

> 来源：长任务收尾（memory leak 修复 + 高精度混流 bug 修复 + 时长校验误杀回归）。
> 目的：把可复用的模式沉淀下来，避免下次重复踩坑。

## 一、被纠正 / 踩坑过的点

### 1. 高精度混流失败的根因定位方法
- **现象**：开 HP 混流失败、关 HP 正常。
- **第一性原理**：两条 finalize 路径都走同一个 `fmp4::finalize` 重建标准 MP4，差异只在**输入**。
  - 非 HP：原始分片字节（含 mdat）直接落盘 `.tmp-part`。
  - HP 纯缓冲：`TrackBuffer::append` 在内存里重组，**只识别 init/moof，把独立 mdat 丢弃** → 重建 MP4 缺样本数据 → 混流失败。
- **教训**：排查"某模式才失败"时，先对比两条路径的**输入差异**，而不是只看共同的下游函数。MDAT 在 fMP4 里是样本数据载体，丢了就等于没媒体。

### 2. MSE 分片发送模式有两种
- 站点可能把 `moof+mdat` **合并**成一个 appendBuffer 段，也可能**分离**成两段（先 moof 后 mdat）。
- 缓冲重建必须按**顶层 box 拆分**捕获 moof 与 mdat，并按出现顺序配对（`fmp4::finalize` 以索引配对 moof[i]↔mdat[i]）。
- **跨调用状态**：mdat 可能在**独立的 append 调用**到达。若 `last_moof` key 是函数局部变量，第二次调用就丢失配对 → mdat 变孤儿被丢。必须持久化到 `TrackBuffer` 字段（`last_moof: Option<(u64,u64)>`）。

### 3. "修一个 bug 引入另一个"的幽灵回归
- 把 `mp4_duration_sec` 里写死的 `ts = 0u32`（死代码，时长校验永远不触发）改成真实 timescale 后，**原本休眠的"时长差异 >1.5% 拒绝混流"guard 被激活**。
- fMP4 重建后的单轨文件常见某轨 `mdhd.duration = 0`（音频轨尤甚）→ 测得时长≈0 → diff/avg≈2.0 > 1.5% → **两条路径都被硬拒绝**（表现为"最新版开关都失败"）。
- **教训**：复活一个被禁用的校验/逻辑时，必须同时考虑它在真实数据上的触发后果。对"时长读不出来"的情况应**跳过校验**（`<0.5s` 视为不可靠），只在双轨都可信且确实漂移时才拦截。
- 回归测试价值：这类"guard 误杀"无法靠单元测一眼看出，靠**用户真实对比**（上一版正常 / 这一版两条都挂）才暴露。

### 4. 测试目标在 lib crate，不在 bin
- `fmp4` 模块的测试属于 **lib crate**，必须用 `cargo test --lib` 跑；`cargo test --bin media-down` 只含 `state::streaming_tests` 等少量，会让人误以为测试没跑/没编译。
- 本项目 `npm run test` 是 `echo 'No tests specified' && exit 0` 的空操作——Rust 改动别指望它验证。

### 5. git / 推送约束（本机）
- 本机 Hermes 安全网关**拦截** `git push --force / --force-with-lease / -f`，返回 BLOCKED；不要重试或改写命令，直接问用户要不要"强制推送"。
- 远程 `chentcawx/mediadown` 已有 `main` + 多个 tag；本地无 `.git` → 普通 push 因历史无关被拒，需 force（受上述约束）。
- 推送需写权限凭证（HTTPS + Windows schannel / 凭据管理器，或 PAT）；`git ls-remote` 走 schannel 可读，但 push 需认证。

## 二、通用可复用模式

1. **fMP4 缓冲重建清单**：任何"在内存里重组 MP4"的代码，必须保留 `ftyp+moov(mvex) + (moof + mdat)*` 的完整交替序列；mdat 与 moof 配对，配对 key 跨调用持久化。
2. **复活死代码 guard 前**：枚举真实数据会让该分支返回什么，对"读不到/≈0"加跳过分支。
3. **Rust 验证命令**：`cargo test --lib`（不是 `npm run test`）。
4. **长任务收尾三件套**：HANDOFF.md + docs/lessons-learned.md + 可复用 skill，每次大改动后更新。

## 三、下次避免的检查清单
- [ ] HP/非 HP 两条路径的输入差异是否真的都保留了样本数据？
- [ ] 复活/修复任何被禁用的校验前，是否考虑了真实畸形数据的触发后果？
- [ ] 时长/尺寸读取失败时，guard 是"跳过"还是"拒绝"？
- [ ] 状态要不要跨调用保持（last_moof 之类）？
- [ ] 改完先 `cargo test --lib` 全绿，再重编 32 位绿色版并冒烟。
