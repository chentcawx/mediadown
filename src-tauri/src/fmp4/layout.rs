//! 两阶段布局：先用占位偏移定 moov 长度，再算 mdat 数据段真实偏移。

use std::collections::HashMap;

use super::moov::rebuild_moov;
use super::parser::TrackFragments;
use super::sample_table::build_stbl_with_offsets;

/// 原始文件中的一个 mdat box（数据段在 [start+header, start+header+data_len)）
#[derive(Debug)]
pub(crate) struct Mdat {
    pub(crate) start: u64,
    pub(crate) header: usize,
    pub(crate) data_len: u64,
}

/// 组装新 moov + 计算 mdat 数据段最终偏移（assemble 决定 stco/co64）
///
/// 阶段 1：占位偏移（0）构建 stbl -> 得到 moov 长度 -> 推导每个 mdat 数据段
/// 在输出文件中的最终绝对偏移。
/// 阶段 2：用 map_offset 得到的真实偏移重建 stbl（大小与占位版一致），
/// 输出最终 moov（长度不变）。
pub(crate) fn assemble(
    ftyp_len: usize,
    moov: &[u8],
    traks: &[(u32, Vec<u8>)],
    per_track: &HashMap<u32, TrackFragments>,
    mdats: &[Mdat],
    use_64: bool,
) -> (Vec<u8>, Vec<u64>) {
    // 占位 stbl（偏移填 0）——确定 moov 长度
    let mut tables: Vec<(u32, Vec<u8>)> = Vec::new();
    for (track_id, _) in traks {
        let frags = per_track.get(track_id).unwrap();
        let offsets = vec![0u64; frags.truns.len()];
        tables.push((*track_id, build_stbl_with_offsets(frags, &offsets, use_64)));
    }
    let moov_placeholder = rebuild_moov(moov, &tables);

    // mdat 数据段最终起点：输出文件布局为 ftyp || moov || mdat序列，
    // 偏移基准必须从 ftyp 之后算起，否则 stco/co64 全盘偏小（每档播放错位）。
    let mut final_data_off = ftyp_len as u64 + moov_placeholder.len() as u64;
    let mut mdat_final_starts = Vec::with_capacity(mdats.len());
    for m in mdats {
        mdat_final_starts.push(final_data_off + m.header as u64);
        final_data_off += m.header as u64 + m.data_len;
    }

    // 真实偏移 stbl -> 最终 moov（大小与占位版一致）
    let mut tables2: Vec<(u32, Vec<u8>)> = Vec::new();
    for (track_id, _) in traks {
        let frags = per_track.get(track_id).unwrap();
        let offsets: Vec<u64> = frags
            .truns
            .iter()
            .map(|tr| map_offset(tr.data_abs, mdats, &mdat_final_starts))
            .collect();
        tables2.push((*track_id, build_stbl_with_offsets(frags, &offsets, use_64)));
    }
    let moov_final = rebuild_moov(moov, &tables2);
    (moov_final, mdat_final_starts)
}

/// 把原始文件中的绝对偏移映射到输出文件中的对应偏移。
/// 样本数据必然落在 mdat 的数据段内，故按数据段起点换算。
fn map_offset(abs: u64, mdats: &[Mdat], final_starts: &[u64]) -> u64 {
    for (i, m) in mdats.iter().enumerate() {
        let data_start = m.start + m.header as u64;
        let data_end = data_start + m.data_len;
        if abs >= data_start && abs < data_end {
            return final_starts[i] + (abs - data_start);
        }
    }
    abs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_offset_within_data_segments() {
        let mdats = vec![
            Mdat { start: 100, header: 8, data_len: 50 }, // 数据段 [108, 158)
            Mdat { start: 300, header: 16, data_len: 25 }, // 数据段 [316, 341)
        ];
        let final_starts = vec![500, 600];
        assert_eq!(map_offset(108, &mdats, &final_starts), 500); // 数据段起点
        assert_eq!(map_offset(138, &mdats, &final_starts), 530); // 数据段内偏移
        assert_eq!(map_offset(316, &mdats, &final_starts), 600);
        assert_eq!(map_offset(320, &mdats, &final_starts), 604);
        // 区间外（含 box 头区域）原样返回
        assert_eq!(map_offset(5, &mdats, &final_starts), 5);
        assert_eq!(map_offset(100, &mdats, &final_starts), 100);
        assert_eq!(map_offset(341, &mdats, &final_starts), 341);
    }

    #[test]
    fn map_offset_prefers_first_match() {
        let mdats = vec![Mdat { start: 0, header: 8, data_len: 8 }];
        let final_starts = vec![100];
        assert_eq!(map_offset(12, &mdats, &final_starts), 104);
    }
}