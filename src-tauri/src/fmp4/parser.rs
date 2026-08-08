//! moof 解析：把 tfhd/trun 解成按轨道聚合的样本序列。

use super::box_util::be32;
use super::box_util::be64;

/// 单个样本在 trun 中的记录
#[derive(Debug, Clone)]
pub(crate) struct Sample {
    pub(crate) size: u32,
    pub(crate) dur: u32,
    pub(crate) cts: i32,
    pub(crate) sync: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Tfhd {
    pub(crate) track_id: u32,
    pub(crate) base_data_offset: Option<u64>,
    pub(crate) default_sample_flags: Option<u32>,
    pub(crate) default_sample_duration: Option<u32>,
    pub(crate) default_sample_size: Option<u32>,
}

#[derive(Debug, Clone)]
pub(crate) struct Trun {
    /// 该 trun 首个样本在原始文件中的绝对偏移
    pub(crate) data_abs: u64,
    pub(crate) samples: Vec<Sample>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TrackFragments {
    pub(crate) truns: Vec<Trun>,
}

pub(crate) fn parse_tfhd(payload: &[u8]) -> Tfhd {
    let mut tfhd = Tfhd::default();
    if payload.len() < 8 {
        return tfhd;
    }
    let flags = be32(payload) & 0xFFFFFF;
    tfhd.track_id = be32(&payload[4..8]);
    let mut off = 8;
    if flags & 0x01 != 0 {
        if off + 8 <= payload.len() {
            tfhd.base_data_offset = Some(be64(&payload[off..off + 8]));
        }
        off += 8;
    }
    if flags & 0x02 != 0 {
        off += 4;
    }
    if flags & 0x08 != 0 {
        // 关键：Chrome MSE / 多数 HLS fMP4 用 tfhd 携带默认时长，trun 常不带逐样本 duration。
        // 不解析则所有样本 dur=0，stts 全 0，输出文件时长归零、播放器判定损坏。
        if off + 4 <= payload.len() {
            tfhd.default_sample_duration = Some(be32(&payload[off..off + 4]));
        }
        off += 4;
    }
    if flags & 0x10 != 0 {
        // 同名默认样本大小：trun 不带逐样本 size 时用于划分样本边界。
        if off + 4 <= payload.len() {
            tfhd.default_sample_size = Some(be32(&payload[off..off + 4]));
        }
        off += 4;
    }
    if flags & 0x20 != 0 && off + 4 <= payload.len() {
        tfhd.default_sample_flags = Some(be32(&payload[off..off + 4]));
    }
    tfhd
}

/// moof_abs 为 moof **box 起点**（非内容起点）：default-base-is-moof 或缺失
/// base 时，trun 的 data_offset 相对 moof box 首字节（ISO 14496-12 8.8.12.1）。
pub(crate) fn parse_trun(payload: &[u8], moof_abs: u64, tfhd: &Tfhd) -> Option<Trun> {
    if payload.len() < 8 {
        return None;
    }
    let flags = be32(payload) & 0xFFFFFF;
    let sample_count = be32(&payload[4..8]) as usize;
    let mut off = 8;
    let mut data_offset: Option<i32> = None;
    if flags & 0x01 != 0 {
        data_offset = Some(be32(&payload[off..off + 4]) as i32);
        off += 4;
    }
    let mut first_sample_flags: Option<u32> = None;
    if flags & 0x04 != 0 {
        first_sample_flags = Some(be32(&payload[off..off + 4]));
        off += 4;
    }
    let has_dur = flags & 0x100 != 0;
    let has_size = flags & 0x200 != 0;
    let has_flags = flags & 0x400 != 0;
    let has_cts = flags & 0x800 != 0;

    let mut samples = Vec::with_capacity(sample_count.min(1 << 20));

    for i in 0..sample_count {
        let dur = if has_dur && off + 4 <= payload.len() {
            let v = be32(&payload[off..off + 4]);
            off += 4;
            v
        } else {
            // trun 无逐样本 duration 时回退 tfhd 默认时长（0x08）。
            // 漏掉此项会让时间轴全 0（文件“零时长”），正是多数破损档的症状。
            tfhd.default_sample_duration.unwrap_or(0)
        };
        let size = if has_size && off + 4 <= payload.len() {
            let v = be32(&payload[off..off + 4]);
            off += 4;
            v
        } else {
            // trun 无逐样本 size 时回退 tfhd 默认大小（0x10）。
            tfhd.default_sample_size.unwrap_or(0)
        };
        let sample_flags = if has_flags && off + 4 <= payload.len() {
            let v = be32(&payload[off..off + 4]);
            off += 4;
            Some(v)
        } else {
            None
        };
        let cts = if has_cts && off + 4 <= payload.len() {
            let v = be32(&payload[off..off + 4]) as i32;
            off += 4;
            v
        } else {
            0
        };

        let flags_val = sample_flags
            .or(if i == 0 { first_sample_flags } else { None })
            .or(tfhd.default_sample_flags);
        let sync = match flags_val {
            Some(fv) => {
                let depends = (fv >> 24) & 0x3;
                let non_sync = (fv >> 16) & 0x1;
                !(depends == 1 || non_sync == 1)
            }
            None => i == 0, // 无标志时保守假设：每 trun 首个样本为关键帧
        };

        samples.push(Sample {
            size,
            dur,
            cts,
            sync,
        });
    }

    Some(Trun {
        data_abs: if let Some(d_off) = data_offset {
            let base2 = tfhd.base_data_offset.unwrap_or(moof_abs);
            (base2 as i64 + d_off as i64) as u64
        } else {
            moof_abs
        },
        samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tfhd_no_flags() {
        let payload = [0, 0, 0, 0, 0, 0, 0, 1];
        let tfhd = parse_tfhd(&payload);
        assert_eq!(tfhd.track_id, 1);
        assert!(tfhd.base_data_offset.is_none());
        assert!(tfhd.default_sample_flags.is_none());
        assert!(parse_tfhd(&[0, 0, 0, 0]).track_id == 0);
    }

#[test]
    fn parse_tfhd_full_flags() {
        // flags: base-data-offset(0x01) + default-sample-flags(0x20)
        let mut payload = Vec::new();
        payload.extend_from_slice(&(0x01u32 | 0x20).to_be_bytes());
        payload.extend_from_slice(&2u32.to_be_bytes()); // track_id
        payload.extend_from_slice(&0x1_0000_0000u64.to_be_bytes()); // base_data_offset
        payload.extend_from_slice(&0xA0_00_00_00u32.to_be_bytes()); // default-sample-count/shown
        let tfhd = parse_tfhd(&payload);
        assert_eq!(tfhd.track_id, 2);
        assert_eq!(tfhd.base_data_offset, Some(0x1_0000_0000));
        assert_eq!(tfhd.default_sample_flags, Some(0xA0_00_00_00));
    }

    #[test]
    fn parse_tfhd_default_dur_and_size() {
        // flags: default-sample-duration(0x08) + default-sample-size(0x10)
        let mut payload = Vec::new();
        payload.extend_from_slice(&(0x08u32 | 0x10).to_be_bytes());
        payload.extend_from_slice(&7u32.to_be_bytes()); // track_id
        payload.extend_from_slice(&1001u32.to_be_bytes()); // default_sample_duration
        payload.extend_from_slice(&2002u32.to_be_bytes()); // default_sample_size
        let tfhd = parse_tfhd(&payload);
        assert_eq!(tfhd.default_sample_duration, Some(1001));
        assert_eq!(tfhd.default_sample_size, Some(2002));
        assert_eq!(tfhd.default_sample_flags, None);
    }

    #[test]
    fn parse_trun_uses_tfhd_defaults_when_unset() {
        // 模拟 Chrome MSE 常见形态：tfhd 携带默认 dur/size，trun 只有 data_offset + first-flags，
        // 无逐样本 duration/size 字段 —— 此时每个样本必须回退到 tfhd 默认值。
        let flags: u32 = 0x01 | 0x04;
        let mut payload = Vec::new();
        payload.extend_from_slice(&flags.to_be_bytes());
        payload.extend_from_slice(&2u32.to_be_bytes()); // sample_count
        payload.extend_from_slice(&16i32.to_be_bytes()); // data_offset
        payload.extend_from_slice(&0x0A_00_00_00u32.to_be_bytes()); // first-sample-flags
        let mut tfhd_payload = Vec::new();
        tfhd_payload.extend_from_slice(&(0x08u32 | 0x10 | 0x20).to_be_bytes());
        tfhd_payload.extend_from_slice(&1u32.to_be_bytes()); // track_id
        tfhd_payload.extend_from_slice(&1000u32.to_be_bytes()); // default dur
        tfhd_payload.extend_from_slice(&64u32.to_be_bytes()); // default size
        tfhd_payload.extend_from_slice(&0x00_01_00_00u32.to_be_bytes()); // default flags: sample_is_non_sync=1
        let tfhd = parse_tfhd(&tfhd_payload);
        let tr = parse_trun(&payload, 1000, &tfhd).unwrap();
        assert_eq!(tr.samples.len(), 2);
        assert_eq!(tr.data_abs, 1016);
        for (i, s) in tr.samples.iter().enumerate() {
            assert_eq!(s.dur, 1000, "样本 {i} 应取 tfhd 默认时长");
            assert_eq!(s.size, 64, "样本 {i} 应取 tfhd 默认大小");
        }
        // 首样本用 first-sample-flags（同步），其余用 tfhd.default_sample_flags（非同步）
        assert!(tr.samples[0].sync);
        assert!(!tr.samples[1].sync);
    }

    #[test]
    fn parse_trun_basic() {
        // flags: data-offset + first-sample-flags + duration + size + cts
        let flags: u32 = 0x01 | 0x04 | 0x100 | 0x200 | 0x800;
        let mut payload = Vec::new();
        payload.extend_from_slice(&flags.to_be_bytes());
        payload.extend_from_slice(&3u32.to_be_bytes()); // sample_count
        payload.extend_from_slice(&8i32.to_be_bytes()); // data_offset
        payload.extend_from_slice(&0x0A_00_00_00u32.to_be_bytes()); // first-sample-flags: sync
        // sample 1: dur=1000 size=10 cts=0
        payload.extend_from_slice(&1000u32.to_be_bytes());
        payload.extend_from_slice(&10u32.to_be_bytes());
        payload.extend_from_slice(&0u32.to_be_bytes());
        // sample 2: dur=1000 size=20 cts=2
        payload.extend_from_slice(&1000u32.to_be_bytes());
        payload.extend_from_slice(&20u32.to_be_bytes());
        payload.extend_from_slice(&2u32.to_be_bytes());
        // sample 3: dur=1000 size=15 cts=2
        payload.extend_from_slice(&1000u32.to_be_bytes());
        payload.extend_from_slice(&15u32.to_be_bytes());
        payload.extend_from_slice(&2u32.to_be_bytes());

        let tfhd = parse_tfhd(&[0u8; 8]); // 默认 tfhd：无额外字段
        let tr = parse_trun(&payload, 1000, &tfhd).unwrap();
        assert_eq!(tr.data_abs, 1008);
        assert_eq!(tr.samples.len(), 3);
        assert!(tr.samples[0].sync);
        assert!(!tr.samples[1].sync);
        assert!(!tr.samples[2].sync);
        assert_eq!(tr.samples[0].dur, 1000);
        assert_eq!(tr.samples[0].size, 10);
        assert_eq!(tr.samples[2].cts, 2);
        assert!(parse_trun(&[0; 4], 0, &tfhd).is_none());
    }
}