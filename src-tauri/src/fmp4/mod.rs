//! 纯 Rust 的 fMP4 -> 标准 MP4 重构。
//!
//! 捕获得到的是"init(ftyp+moov含mvex) + 大量(moof+mdat)"的 fragmented MP4。
//! 本模块把每个轨道的样本表（stts/ctts/stsc/stsz/stss/stco）从 moof 中重建，
//! 替换 moov 里的 stbl，并重排 mdat 偏移，输出可直接播放、可拖拽 seek 的
//! 标准 MP4。全程无外部依赖（不调 ffmpeg）。
//!
//! 布局两阶段：
//!   1) 用占位偏移构建 stbl -> 得到 moov 长度 -> 计算每个 mdat 数据段最终偏移
//!   2) 用真实偏移重建 stco/co64（大小不变）-> 写文件
//!
//! 子模块分工：
//!   - box_util：box 头 / 子 box 遍历 / 拼装
//!   - parser：tfhd / trun 解析出按轨道聚合的样本
//!   - sample_table：stts/ctts/stsc/stsz/stss/stco(co64) 生成
//!   - moov：丢弃 mvex、替换 trak 内 stbl 的重建
//!   - layout：两阶段布局与偏移映射
//!   - io：源文件扫描 + 流式写出（对外入口）

mod box_util;
mod io;
mod layout;
mod moov;
mod parser;
mod sample_table;

pub use io::finalize;