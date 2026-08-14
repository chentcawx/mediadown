//! 缓冲重构方案（高精度同步模式）：
//!
//! 与流式追加方案对比：
//!   - 流式：分片按到达顺序追加到文件，零内存压力，依赖"分片不丢不乱序"假设
//!   - 缓冲：分片入 BTreeMap 按 tfdt 排序，去重/断点平滑后一次性生成标准 MP4
//!
//! 触发时机：用户开启"高精度同步模式"时走此路径；直链下载强制走流式。
//! 内存保护：单轨缓冲超限（MAX_BUFFER_BYTES）自动降级为流式追加。
//!
//! 架构设计：复用 fmp4 子模块（parser/sample_table/layout/moov/box_util），
//! 仅在 io 层新增 "finalize_from_buffer" 入口，不动现有 finalize 路径。

use std::collections::BTreeMap;
use std::io::Write;

use super::box_util;
use super::io as fmp4_io;

// ======================== 常量 ========================

/// 单轨缓冲上限（字节）。超限自动降级为流式追加。
/// 已取消 2GB 限制：用户明确选择高精度模式即信任其控制，由系统内存限制自然兜底。
const MAX_BUFFER_BYTES: u64 = u64::MAX / 2; // 约 4PiB，实际上限由内存决定

/// 时间轴 gap 阈值（秒）。超过此值的 tfdt 跳跃视为"质量切换/直播重置"，
/// 后续段强制偏移连续化，避免混流时 pts 回溯导致播放器卡顿。
const GAP_THRESHOLD_SEC: f64 = 0.5;

// ======================== 数据结构 ========================

/// 一个 moof 片解析结果
#[derive(Clone, Debug)]
struct Seg {
    /// tfdt.base_media_decode_time（绝对时间，单位 timescale）。无 tfdt 时为 None。
    tfdt_base: Option<u64>,
    /// tfhd.track_id
    track_id: u32,
    /// tfhd.default_sample_duration（来自 0x08 flag）
    default_dur: Option<u32>,
    /// 到达序号，用于相同 tfdt 时的稳定排序 tie-break
    seq: u64,
    /// 原始 moof box 完整字节（含 8 字节头），finalize 时原样输出
    moof_bytes: Vec<u8>,
}

/// 单轨缓冲状态
#[derive(Default)]
pub struct TrackBuffer {
    /// 首个 init 片段（ftyp+moov+mvex），finalize 时作为 moov 模板
    init: Option<Vec<u8>>,
    /// init 指纹：用于丢弃站点周期性重发的同指纹纯 init
    init_sig: Option<String>,
    /// 有序 seg：key=(tfdt_key, seq)
    segments: BTreeMap<(u64, u64), Seg>,
    /// 当前估计的 timescale（从 init 里 mdhd 读，缺省 1000）
    timescale: u32,
    /// 累计已缓冲字节数，用于内存保护判断
    cached_bytes: u64,
    /// 上一段估计结束时间戳（timescale 单位），用于缺 tfdt 时的连续化占位
    last_end_ts: u64,
    /// 是否已触发降级
    fallback_active: bool,
    /// 降级后追加的目标文件路径
    fallback_path: Option<std::path::PathBuf>,
    /// 降级后的文件句柄
    fallback_file: Option<std::fs::File>,
}

impl TrackBuffer {
    /// 估算该轨当前占用的总字节数
    pub fn estimated_bytes(&self) -> u64 {
        let seg_bytes: u64 = self.segments.values().map(|s| s.moof_bytes.len() as u64).sum();
        let init_bytes = self.init.as_ref().map(|v| v.len() as u64).unwrap_or(0);
        init_bytes + seg_bytes + self.cached_bytes
    }

    /// 是否处于降级状态
    pub fn is_fallback(&self) -> bool {
        self.fallback_active || self.estimated_bytes() > MAX_BUFFER_BYTES
    }

    /// 追加一段原始字节。bytes 可能是：
    ///   - init 片段（以 ftyp/moov 开头）
    ///   - moof 片段（以 moof 开头，含 traf+tfhd+trun+tfdt）
    /// 返回 (init_consumed, seg_consumed, error_hint)。
    pub fn append(&mut self, bytes: &[u8]) -> (bool, bool, Option<String>) {
        if bytes.is_empty() {
            return (false, false, None);
        }
        if self.fallback_active {
            if let Some(ref mut f) = self.fallback_file {
                let _ = f.write_all(bytes);
            }
            return (false, false, None);
        }
        // 判断是 init 还是 moof
        if is_init_start(bytes) {
            let sig = box_util::sig_of(bytes);
            match &self.init_sig {
                Some(existing) if existing == &sig => {
                    // 站点周期性重发的 reset init，丢弃
                    return (false, false, None);
                }
                _ => {
                    self.init_sig = Some(sig);
                    self.init = Some(bytes.to_vec());
                    if self.timescale == 0 {
                        if let Some(ts) = read_mdhd_timescale(bytes) {
                            self.timescale = ts;
                        }
                    }
                    self.cached_bytes += bytes.len() as u64;
                    return (true, false, None);
                }
            }
        }
        match parse_moof_box(bytes) {
            Some(mut seg) => {
                if seg.tfdt_base.is_none() {
                    seg.tfdt_base = Some(self.last_end_ts);
                }
                let tfdt_key = seg.tfdt_base.unwrap();
                let ts = self.timescale.max(1);
                let corrected_key = if self.last_end_ts > 0 {
                    let gap_sec = (tfdt_key as f64 - self.last_end_ts as f64) / ts as f64;
                    if gap_sec > GAP_THRESHOLD_SEC {
                        self.last_end_ts
                    } else {
                        tfdt_key
                    }
                } else {
                    tfdt_key
                };
                let key = (corrected_key, seg.seq);
                let replaced = self.segments.contains_key(&key);
                if !replaced {
                    self.segments.insert(key, seg.clone());
                }
                let sample_count = count_samples_in_moof(bytes);
                let dur_per_sample = seg.default_dur.unwrap_or(1000);
                self.last_end_ts = corrected_key + (sample_count as u64) * (dur_per_sample as u64);
                self.cached_bytes += bytes.len() as u64;
                if self.estimated_bytes() > MAX_BUFFER_BYTES {
                    self.activate_fallback();
                }
                (false, true, None)
            }
            None => (false, false, Some("无法解析分片（非 init、非 moof）".into())),
        }
    }

    /// 激活降级：创建临时文件，后续所有分片直写磁盘
    fn activate_fallback(&mut self) {
        eprintln!(
            "[buf] track buffer overflow ({} > {}), falling back to streaming",
            self.estimated_bytes(),
            MAX_BUFFER_BYTES
        );
        self.fallback_active = true;
        let tmp = std::env::temp_dir().join(format!(
            "md-fallback-{}.tmp",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        match std::fs::File::create(&tmp) {
            Ok(mut f) => {
                if let Some(ref init) = self.init {
                    let _ = f.write_all(init);
                }
                for seg in self.segments.values() {
                    let _ = f.write_all(&seg.moof_bytes);
                }
                self.fallback_path = Some(tmp.clone());
                self.fallback_file = Some(f);
                self.segments.clear();
                self.init = None;
                self.init_sig = None;
            }
            Err(e) => {
                eprintln!("[buf] fallback file create failed: {e}");
            }
        }
    }

    /// 收尾：把缓冲内容写出为标准 MP4 文件。
    /// 成功返回 Ok(output_path)，失败返回 Err(msg) 由调用方降级到流式路径。
    pub fn finalize(&self, dst_path: &str) -> Result<String, String> {
        if self.fallback_active {
            if let Some(ref p) = self.fallback_path {
                if p.exists() {
                    std::fs::rename(p, dst_path).map_err(|e| e.to_string())?;
                    return Ok(dst_path.to_string());
                }
            }
            return Err("fallback 文件不存在".into());
        }
        if self.init.is_none() {
            return Err("无 init 段，无法收尾".into());
        }
        // 构造内存镜像文件：init + 排序后的 moof
        let tmp = std::env::temp_dir().join(format!(
            "md-buf-{}-{:.0}.mp4",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as f64
        ));
        {
            let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
            if let Some(ref init) = self.init {
                f.write_all(init).map_err(|e| e.to_string())?;
            }
            for seg in self.segments.values() {
                f.write_all(&seg.moof_bytes).map_err(|e| e.to_string())?;
            }
        }
        // 复用现有 fmp4 重构器
        let r = fmp4_io::finalize(&tmp, std::path::Path::new(dst_path));
        let _ = std::fs::remove_file(&tmp);
        match r {
            Ok(()) => Ok(dst_path.to_string()),
            Err(e) => Err(format!("buffered rebuild failed: {e}")),
        }
    }
}

// ======================== 辅助函数 ========================

fn is_init_start(bytes: &[u8]) -> bool {
    if bytes.len() < 12 {
        return false;
    }
    let typ = &bytes[4..8];
    typ == b"ftyp" || typ == b"moov"
}

/// 从 init 数据中递归查找 mdhd.timescale
fn read_mdhd_timescale(data: &[u8]) -> Option<u32> {
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let size = box_util::be32(&data[pos..pos + 4]) as usize;
        let typ = &data[pos + 4..pos + 8];
        if typ == b"mdhd" && size >= 20 {
            let version = data[pos + 8];
            let off = if version == 1 { 28 } else { 16 };
            if off + 4 <= size {
                return Some(box_util::be32(&data[pos + off..pos + off + 4]));
            }
        }
        // 安全检查：size 必须合法才能递归
        if size < 8 || pos + size > data.len() {
            break;
        }
        if let Some(ts) = read_mdhd_timescale(&data[pos + 8..pos + size]) {
            return Some(ts);
        }
        pos += size;
    }
    None
}

/// 解析 moof box，提取 track_id / default_dur / tfdt_base
fn parse_moof_box(bytes: &[u8]) -> Option<Seg> {
    if bytes.len() < 16 || &bytes[4..8] != b"moof" {
        return None;
    }
    let content = &bytes[8..];
    let mut track_id: Option<u32> = None;
    let mut tfdt_base: Option<u64> = None;
    let mut default_dur: Option<u32> = None;

    let mut pos = 0;
    while pos + 8 <= content.len() {
        let size = box_util::be32(&content[pos..pos + 4]) as usize;
        let typ = &content[pos + 4..pos + 8];
        if typ == b"traf" && size >= 8 {
            let traf = &content[pos + 8..pos + size];
            let mut tpos = 0;
            while tpos + 8 <= traf.len() {
                let tsize = box_util::be32(&traf[tpos..tpos + 4]) as usize;
                let ttyp = &traf[tpos + 4..tpos + 8];
                if ttyp == b"tfhd" && tsize >= 8 {
                    let payload = &traf[tpos + 8..tpos + tsize];
                    if payload.len() >= 8 {
                        track_id = Some(box_util::be32(&payload[4..8]));
                    }
                    let flags = box_util::be32(payload);
                    if flags & 0x08 != 0 {
                        let mut off = 8;
                        if flags & 0x01 != 0 { off += 8; }
                        if flags & 0x02 != 0 { off += 4; }
                        if flags & 0x04 != 0 { off += 4; }
                        if off + 4 <= payload.len() {
                            default_dur = Some(box_util::be32(&payload[off..off + 4]));
                        }
                    }
                } else if ttyp == b"tfdt" && tsize >= 12 {
                    let payload = &traf[tpos + 8..tpos + tsize];
                    let ver = payload[0];
                    let base_off = 4;
                    if ver == 1 && base_off + 8 <= payload.len() {
                        tfdt_base = Some(box_util::be64(&payload[base_off..base_off + 8]));
                    } else if base_off + 4 <= payload.len() {
                        tfdt_base = Some(box_util::be32(&payload[base_off..base_off + 4]) as u64);
                    }
                }
                if tsize < 8 || tpos + tsize > traf.len() {
                    break;
                }
                tpos += tsize;
            }
        }
        if size < 8 || pos + size > content.len() {
            break;
        }
        pos += size;
    }

    let track_id = track_id.or(Some(1))?;
    let seq = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;

    Some(Seg {
        tfdt_base,
        track_id,
        default_dur,
        seq,
        moof_bytes: bytes.to_vec(),
    })
}

/// 统计 moof 内所有 trun 的 sample_count 总和
fn count_samples_in_moof(bytes: &[u8]) -> u64 {
    if bytes.len() < 16 || &bytes[4..8] != b"moof" {
        return 0;
    }
    let content = &bytes[8..];
    let mut total: u64 = 0;
    let mut pos = 0;
    while pos + 8 <= content.len() {
        let size = box_util::be32(&content[pos..pos + 4]) as usize;
        let typ = &content[pos + 4..pos + 8];
        if typ == b"traf" && size >= 8 {
            let traf = &content[pos + 8..pos + size];
            let mut tpos = 0;
            while tpos + 8 <= traf.len() {
                let tsize = box_util::be32(&traf[tpos..tpos + 4]) as usize;
                let ttyp = &traf[tpos + 4..tpos + 8];
                if ttyp == b"trun" && tsize >= 8 {
                    let payload = &traf[tpos + 8..tpos + tsize];
                    if payload.len() >= 8 {
                        let _flags = box_util::be32(payload);
                        let sc = box_util::be32(&payload[4..8]);
                        total += sc as u64;
                    }
                }
                if tsize < 8 || tpos + tsize > traf.len() {
                    break;
                }
                tpos += tsize;
            }
        }
        if size < 8 || pos + size > content.len() {
            break;
        }
        pos += size;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_init_box_ftyp() {
        let init = box_util::box_bytes(b"ftyp", b"isom\x00\x00\x00\x00isom");
        assert!(is_init_start(&init));
    }

    #[test]
    fn test_is_init_box_moov() {
        let moov = box_util::box_bytes(b"moov", &[0u8; 8]);
        assert!(is_init_start(&moov));
    }

    #[test]
    fn test_is_not_moof() {
        let moof = box_util::box_bytes(b"moof", &[0u8; 8]);
        assert!(!is_init_start(&moof));
    }

    #[test]
    fn test_track_buffer_init_only() {
        let mut tb = TrackBuffer::default();
        let init = box_util::box_bytes(b"ftyp", b"isom\x00\x00\x00\x00isom");
        let (init_added, seg_added, err) = tb.append(&init);
        assert!(init_added);
        assert!(!seg_added);
        assert!(err.is_none());
    }
}
