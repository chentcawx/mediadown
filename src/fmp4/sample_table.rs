//! stbl 样本表构建：stts/ctts/stsz/stsc/stss/stco(co64) 的生成。

use super::parser::{Sample, TrackFragments};

/// 用已确定的 chunk（trun）偏移构建整张 stbl（stts+stsz+stsc+[ctts]+[stss]+stco/co64）
pub(crate) fn build_stbl_with_offsets(
    frags: &TrackFragments,
    chunk_offsets: &[u64],
    use_64: bool,
) -> Vec<u8> {
    let mut samples: Vec<Sample> = Vec::new();
    for tr in &frags.truns {
        samples.extend(tr.samples.iter().cloned());
    }

    let stts = build_stts(&samples);
    let ctts = build_ctts(&samples);
    let stsz = build_stsz(&samples);
    let stsc = build_stsc(frags);
    let stss = build_stss(&samples);

    let chunk_count = frags.truns.len();
    let body = if use_64 { chunk_count * 8 } else { chunk_count * 4 };
    let size = 8 + 8 + body;
    let mut stco = Vec::with_capacity(size);
    stco.extend_from_slice(&(size as u32).to_be_bytes());
    stco.extend_from_slice(if use_64 { b"co64" } else { b"stco" });
    stco.extend_from_slice(&(if use_64 { 1u32 } else { 0 }).to_be_bytes());
    stco.extend_from_slice(&(chunk_count as u32).to_be_bytes());
    for off in chunk_offsets {
        if use_64 {
            stco.extend_from_slice(&off.to_be_bytes());
        } else {
            stco.extend_from_slice(&(*off as u32).to_be_bytes());
        }
    }

    let mut stbl = Vec::new();
    stbl.extend_from_slice(&stts);
    stbl.extend_from_slice(&stsz);
    stbl.extend_from_slice(&stsc);
    if let Some(c) = ctts {
        stbl.extend_from_slice(&c);
    }
    if let Some(s) = stss {
        stbl.extend_from_slice(&s);
    }
    stbl.extend_from_slice(&stco);
    stbl
}

fn build_stts(samples: &[Sample]) -> Vec<u8> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for s in samples {
        if let Some(last) = runs.last_mut() {
            if last.1 == s.dur {
                last.0 += 1;
                continue;
            }
        }
        runs.push((1, s.dur));
    }
    let mut b = Vec::new();
    b.extend_from_slice(&((8 + 8 + runs.len() * 8) as u32).to_be_bytes());
    b.extend_from_slice(b"stts");
    b.extend_from_slice(&0u32.to_be_bytes());
    b.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (cnt, delta) in &runs {
        b.extend_from_slice(&cnt.to_be_bytes());
        b.extend_from_slice(&delta.to_be_bytes());
    }
    b
}

fn build_ctts(samples: &[Sample]) -> Option<Vec<u8>> {
    if !samples.iter().any(|s| s.cts != 0) {
        return None;
    }
    let any_neg = samples.iter().any(|s| s.cts < 0);
    let mut runs: Vec<(u32, i32)> = Vec::new();
    for s in samples {
        if let Some(last) = runs.last_mut() {
            if last.1 == s.cts {
                last.0 += 1;
                continue;
            }
        }
        runs.push((1, s.cts));
    }
    let mut b = Vec::new();
    b.extend_from_slice(&((8 + 8 + runs.len() * 8) as u32).to_be_bytes());
    b.extend_from_slice(b"ctts");
    b.extend_from_slice(&(if any_neg { 1u32 } else { 0 }).to_be_bytes());
    b.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (cnt, off) in &runs {
        b.extend_from_slice(&cnt.to_be_bytes());
        b.extend_from_slice(&(*off as u32).to_be_bytes());
    }
    Some(b)
}

fn build_stsz(samples: &[Sample]) -> Vec<u8> {
    let uniform = samples
        .first()
        .map(|f| samples.iter().all(|s| s.size == f.size))
        .unwrap_or(true);
    let mut b = Vec::new();
    if uniform && !samples.is_empty() {
        let sz = samples[0].size;
        b.extend_from_slice(&((8 + 12) as u32).to_be_bytes());
        b.extend_from_slice(b"stsz");
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&sz.to_be_bytes());
        b.extend_from_slice(&(samples.len() as u32).to_be_bytes());
    } else {
        b.extend_from_slice(&((8 + 12 + samples.len() * 4) as u32).to_be_bytes());
        b.extend_from_slice(b"stsz");
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&(samples.len() as u32).to_be_bytes());
        for s in samples {
            b.extend_from_slice(&s.size.to_be_bytes());
        }
    }
    b
}

fn build_stsc(frags: &TrackFragments) -> Vec<u8> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for (chunk_idx, tr) in (1u32..).zip(frags.truns.iter()) {
        let cnt = tr.samples.len() as u32;
        // stsc 条目 (first_chunk, samples_per_chunk) 从 first_chunk 起连续覆盖到下一条目，
        // 因此相同样本数的连续 chunk 应复用现有条目（first_chunk 保持为「起始块」）。
        // 不能用 last.0 += 1 上移至末尾块，否则 first_chunk 之前的 chunk 样本数失去定义，
        // 导致 stsc 与 stsz/stco 错位——这正是「视频跳帧 + 音频整轨无法解码」的根因。
        if runs.last().is_some_and(|last| last.1 == cnt) {
            continue;
        }
        runs.push((chunk_idx, cnt));
    }
    let mut b = Vec::new();
    b.extend_from_slice(&((8 + 8 + runs.len() * 12) as u32).to_be_bytes());
    b.extend_from_slice(b"stsc");
    b.extend_from_slice(&0u32.to_be_bytes());
    b.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (fc, spc) in &runs {
        b.extend_from_slice(&fc.to_be_bytes());
        b.extend_from_slice(&spc.to_be_bytes());
        b.extend_from_slice(&1u32.to_be_bytes());
    }
    b
}

fn build_stss(samples: &[Sample]) -> Option<Vec<u8>> {
    let indices: Vec<u32> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.sync)
        .map(|(i, _)| i as u32 + 1)
        .collect();
    if indices.is_empty() {
        return None;
    }
    let mut b = Vec::new();
    b.extend_from_slice(&((8 + 8 + indices.len() * 4) as u32).to_be_bytes());
    b.extend_from_slice(b"stss");
    b.extend_from_slice(&0u32.to_be_bytes());
    b.extend_from_slice(&(indices.len() as u32).to_be_bytes());
    for idx in &indices {
        b.extend_from_slice(&idx.to_be_bytes());
    }
    Some(b)
}

#[cfg(test)]
mod tests {
    use super::super::box_util::{be32, for_each_child};
    use super::super::parser::Trun;
    use super::*;

    fn frag(truns: Vec<Trun>) -> TrackFragments {
        TrackFragments { truns }
    }

    #[test]
    fn stbl_builds_all_tables() {
        // 2 truns：3 样本 + 2 样本；样本尺寸不一，cts 有正有负
        let samples = vec![
            Sample { size: 100, dur: 1000, cts: 0, sync: true },
            Sample { size: 200, dur: 1000, cts: -3, sync: false },
            Sample { size: 150, dur: 500, cts: 5, sync: false },
        ];
        let frags = frag(vec![
            Trun { data_abs: 1000, samples },
            Trun {
                data_abs: 1500,
                samples: vec![
                    Sample { size: 100, dur: 1000, cts: 0, sync: true },
                    Sample { size: 100, dur: 1000, cts: 0, sync: false },
                ],
            },
        ]);
        let stbl = build_stbl_with_offsets(&frags, &[1000, 1500], false);

        let mut types = Vec::new();
        for_each_child(&stbl, 0, stbl.len(), |typ, _, _| {
            types.push(String::from_utf8_lossy(typ).to_string());
        });
        assert_eq!(types, vec!["stts", "stsz", "stsc", "ctts", "stss", "stco"]);

        // stco：2 个 32 位偏移（位于 stbl 尾部）
        assert_eq!(&stbl[stbl.len() - 8..stbl.len() - 4], &1000u32.to_be_bytes());
        assert_eq!(&stbl[stbl.len() - 4..], &1500u32.to_be_bytes());
    }

    #[test]
    fn stbl_omits_ctts_stss_when_unneeded() {
        let frags = frag(vec![Trun {
            data_abs: 0,
            samples: vec![
                Sample { size: 10, dur: 100, cts: 0, sync: false },
                Sample { size: 10, dur: 100, cts: 0, sync: false },
            ],
        }]);
        let stbl = build_stbl_with_offsets(&frags, &[0], false);
        let mut types = Vec::new();
        for_each_child(&stbl, 0, stbl.len(), |typ, _, _| {
            types.push(String::from_utf8_lossy(typ).to_string());
        });
        assert_eq!(types, vec!["stts", "stsz", "stsc", "stco"]);

        // stsz：uniform 形式（sample_size=10，无逐样本表）
        let stsz_off = stbl
            .windows(4)
            .position(|w| w == b"stsz")
            .unwrap();
        assert_eq!(&stbl[stsz_off + 4..stsz_off + 8], &0u32.to_be_bytes());
        assert_eq!(&stbl[stsz_off + 8..stsz_off + 12], &10u32.to_be_bytes());
        assert_eq!(&stbl[stsz_off + 12..stsz_off + 16], &2u32.to_be_bytes());
    }

    #[test]
    fn stbl_co64_when_large() {
        let frags = frag(vec![Trun {
            data_abs: 0,
            samples: vec![Sample { size: 1, dur: 1, cts: 0, sync: true }],
        }]);
        let stbl = build_stbl_with_offsets(&frags, &[0x1_0000_0000], true);
        let mut types = Vec::new();
        for_each_child(&stbl, 0, stbl.len(), |typ, _, _| {
            types.push(String::from_utf8_lossy(typ).to_string());
        });
        assert_eq!(&types[types.len() - 1], "co64");
        assert_eq!(
            &stbl[stbl.len() - 8..],
            &0x1_0000_0000u64.to_be_bytes()
        );
    }

    #[test]
    fn stsc_keeps_first_chunk_on_merge() {
        // 回归：视频形态 110×250 + 1×73。正确 stsc = [(1,250),(111,73)]；
        // 旧实现 last.0 += 1 会把 first_chunk 误推到末尾块，产出 [(110,250),(111,73)]，
        // 使 chunk 1..109 的样本数失去定义，解码器只能猜测，导致跳帧/音频损坏。
        let mk = |truns: Vec<Trun>| -> TrackFragments { TrackFragments { truns } };
        fn trun_with(cnt: usize) -> Trun {
            Trun {
                data_abs: 0,
                samples: vec![
                    Sample { size: 10, dur: 100, cts: 0, sync: false };
                    cnt
                ],
            }
        }

        let truns: Vec<Trun> = (0..110).map(|_| trun_with(250)).chain([trun_with(73)]).collect();
        let offsets: Vec<u64> = (0..truns.len() as u64).collect();
        let stbl = build_stbl_with_offsets(&mk(truns), &offsets, false);

        // 解析 stsc 条目：size(4) type(4) version/flags(4) entry_count(4) entries(...)
        let stsc_off = stbl.windows(4).position(|w| w == b"stsc").unwrap();
        let entry_count = be32(&stbl[stsc_off + 8..stsc_off + 12]);
        assert_eq!(entry_count, 2);
        let mut entries = Vec::new();
        let mut off = stsc_off + 12;
        for _ in 0..entry_count {
            let first_chunk = be32(&stbl[off..off + 4]);
            let spc = be32(&stbl[off + 4..off + 8]);
            entries.push((first_chunk, spc));
            off += 12;
        }
        assert_eq!(entries, vec![(1, 250), (111, 73)]);
    }
}