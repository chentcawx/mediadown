//! 纯 Rust 的 fMP4 -> 标准 MP4 重构
//!
//! 捕获得到的是"init(ftyp+moov含mvex) + 大量(moof+mdat)"的 fragmented MP4。
//! 本模块把每个轨道的样本表（stts/ctts/stsc/stsz/stss/stco）从 moof 中重建，
//! 替换 moov 里的 stbl，并重排 mdat 偏移，输出可直接播放、可拖拽 seek 的
//! 标准 MP4。全程无外部依赖（不调 ffmpeg）。
//!
//! 布局两阶段：
//!   1) 用占位偏移构建 stbl -> 得到 moov 长度 -> 计算每个 mdat 数据段最终偏移
//!   2) 用真实偏移重建 stco/co64（大小不变）-> 写文件

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

// ---------------- box 基础 ----------------

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn be64(b: &[u8]) -> u64 {
    u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// 读取 data[pos] 处的 box 头，返回 (typ, header_len, content_start, content_end)
fn box_header(data: &[u8], pos: usize) -> Option<([u8; 4], usize, usize, usize)> {
    if pos + 8 > data.len() {
        return None;
    }
    let size32 = be32(&data[pos..pos + 4]);
    let typ = [data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]];
    let (size, header) = if size32 == 1 {
        if pos + 16 > data.len() {
            return None;
        }
        (be64(&data[pos + 8..pos + 16]) as usize, 16)
    } else if size32 == 0 {
        (data.len() - pos, 8)
    } else {
        (size32 as usize, 8)
    };
    Some((typ, header, pos + header, pos + size))
}

fn for_each_child(data: &[u8], start: usize, end: usize, mut f: impl FnMut(&[u8; 4], usize, usize)) {
    let mut pos = start;
    while pos + 8 <= end {
        if let Some((typ, _, cs, ce)) = box_header(data, pos) {
            f(&typ, cs, ce);
            pos = ce;
        } else {
            break;
        }
    }
}

fn for_each_child_mut(_data: &mut [u8], _start: usize, _end: usize, _f: impl FnMut(&mut [u8; 4], usize, usize)) {
    // 占位保留签名；当前最终化流程只做只读解析，无需原地修改 box。
}

// ---------------- moof 解析 ----------------

#[derive(Debug, Clone)]
struct Sample {
    size: u32,
    dur: u32,
    cts: i32,
    sync: bool,
}

#[derive(Debug, Clone, Default)]
struct Tfhd {
    track_id: u32,
    base_data_offset: Option<u64>,
    default_sample_flags: Option<u32>,
}

#[derive(Debug, Clone)]
struct Trun {
    data_abs: u64, // 该 trun 首个样本在原始文件中的绝对偏移
    samples: Vec<Sample>,
}

#[derive(Debug, Clone, Default)]
struct TrackFragments {
    truns: Vec<Trun>,
}

fn parse_tfhd(payload: &[u8]) -> Tfhd {
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
        off += 4;
    }
    if flags & 0x10 != 0 {
        off += 4;
    }
    if flags & 0x20 != 0 {
        if off + 4 <= payload.len() {
            tfhd.default_sample_flags = Some(be32(&payload[off..off + 4]));
        }
    }
    tfhd
}

fn parse_trun(payload: &[u8], moof_abs: u64, tfhd: &Tfhd) -> Option<Trun> {
    if payload.len() < 8 {
        return None;
    }
    let ver = payload[0];
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
            0
        };
        let size = if has_size && off + 4 <= payload.len() {
            let v = be32(&payload[off..off + 4]);
            off += 4;
            v
        } else {
            0
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
            if ver == 1 { v } else { v }
        } else {
            0
        };

        let flags_val = sample_flags
            .or_else(|| if i == 0 { first_sample_flags } else { None })
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
        data_abs: if data_offset.is_some() {
            let base2 = tfhd.base_data_offset.unwrap_or(moof_abs);
            (base2 as i64 + data_offset.unwrap() as i64) as u64
        } else {
            moof_abs
        },
        samples,
    })
}

// ---------------- 主流程 ----------------

struct Mdat {
    start: u64,
    header: usize,
    data_len: u64,
}

pub fn finalize(src: &Path, dst: &Path) -> Result<(), String> {
    let mut f = File::open(src).map_err(|e| e.to_string())?;
    let file_len = f.metadata().map_err(|e| e.to_string())?.len();
    let mut pos: u64 = 0;

    let mut ftyp: Option<Vec<u8>> = None;
    let mut moov_payload: Option<Vec<u8>> = None;
    let mut mdats: Vec<Mdat> = Vec::new();
    let mut moofs: Vec<(u64, Vec<u8>)> = Vec::new();

    while pos + 8 <= file_len {
        f.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
        let mut hdr = [0u8; 16];
        let n = f.read(&mut hdr[..8]).map_err(|e| e.to_string())?;
        if n < 8 {
            break;
        }
        let size32 = be32(&hdr[0..4]);
        let typ = [hdr[4], hdr[5], hdr[6], hdr[7]];
        let (size, header) = if size32 == 1 {
            f.read_exact(&mut hdr[8..16]).map_err(|e| e.to_string())?;
            (be64(&hdr[8..16]), 16)
        } else if size32 == 0 {
            (file_len - pos, 8)
        } else {
            (size32 as u64, 8)
        };
        if size < header as u64 || pos + size > file_len {
            return Err(format!("bad box {:?} at {pos}", String::from_utf8_lossy(&typ)));
        }
        let content_len = (size - header as u64) as usize;
        let content_start = pos + header as u64;

        match &typ {
            b"ftyp" => {
                let mut buf = vec![0u8; content_len];
                f.seek(SeekFrom::Start(content_start))
                    .map_err(|e| e.to_string())?;
                f.read_exact(&mut buf).map_err(|e| e.to_string())?;
                let mut full = Vec::with_capacity(header + content_len);
                full.extend_from_slice(&hdr[..header]);
                full.extend_from_slice(&buf);
                ftyp = Some(full);
            }
            b"moov" => {
                if moov_payload.is_none() {
                    let mut buf = vec![0u8; content_len.min(512 * 1024 * 1024)];
                    f.seek(SeekFrom::Start(content_start))
                        .map_err(|e| e.to_string())?;
                    f.read_exact(&mut buf).map_err(|e| e.to_string())?;
                    moov_payload = Some(buf);
                }
            }
            b"moof" => {
                let mut buf = vec![0u8; content_len.min(128 * 1024 * 1024)];
                f.seek(SeekFrom::Start(content_start))
                    .map_err(|e| e.to_string())?;
                f.read_exact(&mut buf).map_err(|e| e.to_string())?;
                moofs.push((content_start, buf));
            }
            b"mdat" => {
                mdats.push(Mdat {
                    start: pos,
                    header,
                    data_len: content_len as u64,
                });
            }
            _ => {}
        }
        pos += size;
    }

    let ftyp = ftyp.ok_or_else(|| "no ftyp".to_string())?;
    let moov = moov_payload.ok_or_else(|| "no moov".to_string())?;
    if moofs.is_empty() {
        return Err("no fragments".to_string());
    }
    if mdats.is_empty() {
        return Err("no mdat".to_string());
    }

    // 1) 解析 moof -> 按轨道聚合样本
    let mut per_track: HashMap<u32, TrackFragments> = HashMap::new();
    for (moof_abs, payload) in &moofs {
        let mut traf_entries: Vec<(Tfhd, Vec<Trun>)> = Vec::new();
        for_each_child(payload, 0, payload.len(), |typ, cs, ce| {
            if typ == b"traf" {
                let mut tfhd = Tfhd::default();
                let mut truns = Vec::new();
                for_each_child(payload, cs, ce, |t2, cs2, ce2| match t2 {
                    b"tfhd" => tfhd = parse_tfhd(&payload[cs2..ce2]),
                    b"trun" => {
                        if let Some(tr) = parse_trun(&payload[cs2..ce2], *moof_abs, &tfhd) {
                            truns.push(tr);
                        }
                    }
                    _ => {}
                });
                traf_entries.push((tfhd, truns));
            }
        });
        for (tfhd, truns) in traf_entries {
            per_track.entry(tfhd.track_id).or_default().truns.extend(truns);
        }
    }
    if per_track.is_empty() {
        return Err("no sample tables".to_string());
    }

    // 2) moov 中保留有数据的 trak
    let mut traks: Vec<(u32, Vec<u8>)> = Vec::new();
    for_each_child(&moov, 0, moov.len(), |typ, cs, ce| {
        if typ == b"trak" {
            let trak_raw = moov[cs - 8..ce].to_vec();
            let track_id = trak_track_id(&trak_raw);
            if per_track.contains_key(&track_id) {
                traks.push((track_id, trak_raw));
            }
        }
    });
    if traks.is_empty() {
        return Err("no matching trak".to_string());
    }

    // 3) 两阶段布局
    let mut use_64 = false;
    let (new_moov, mdat_final_starts) = loop {
        let (moov_bytes, starts) = assemble(&moov, &traks, &per_track, &mdats, use_64);
        let mdat_total: u64 = mdats
            .iter()
            .map(|m| m.header as u64 + m.data_len)
            .sum();
        let total = ftyp.len() as u64 + moov_bytes.len() as u64 + mdat_total;
        if total > u32::MAX as u64 && !use_64 {
            use_64 = true;
            continue;
        }
        break (moov_bytes, starts);
    };

    // 4) 写出：ftyp + 新moov + 原始 mdat（流式拷贝）
    let mut out = File::create(dst).map_err(|e| e.to_string())?;
    out.write_all(&ftyp).map_err(|e| e.to_string())?;
    out.write_all(&new_moov).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1024 * 1024];
    for m in &mdats {
        let mut remaining = m.header as u64 + m.data_len;
        let mut rpos = m.start;
        while remaining > 0 {
            let want = (remaining as usize).min(buf.len());
            f.seek(SeekFrom::Start(rpos)).map_err(|e| e.to_string())?;
            let got = f.read(&mut buf[..want]).map_err(|e| e.to_string())?;
            if got == 0 {
                break;
            }
            out.write_all(&buf[..got]).map_err(|e| e.to_string())?;
            remaining -= got as u64;
            rpos += got as u64;
        }
    }
    out.flush().map_err(|e| e.to_string())?;
    let _ = mdat_final_starts;
    Ok(())
}

/// 组装新 moov + 计算 mdat 数据段最终偏移（use_64 决定 stco/co64）
fn assemble(
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
    let moov_placeholder = rebuild_moov(moov, traks, &tables);

    // mdat 数据段最终起点
    let mut final_data_off = moov_placeholder.len() as u64;
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
    let moov_final = rebuild_moov(moov, traks, &tables2);
    (moov_final, mdat_final_starts)
}

fn map_offset(abs: u64, mdats: &[Mdat], final_starts: &[u64]) -> u64 {
    for (i, m) in mdats.iter().enumerate() {
        if abs >= m.start && abs < m.start + m.header as u64 + m.data_len {
            return final_starts[i] + (abs - m.start);
        }
    }
    abs
}

// ---------------- trak 解析 ----------------

fn trak_track_id(trak: &[u8]) -> u32 {
    let mut id = 0;
    for_each_child(trak, 8, trak.len(), |typ, cs, ce| {
        if typ == b"tkhd" {
            let p = &trak[cs..ce];
            if p.len() >= 16 {
                let ver = p[0];
                let off = if ver == 1 { 20 } else { 12 };
                if p.len() >= off + 4 {
                    id = be32(&p[off..off + 4]);
                }
            }
        }
    });
    id
}

// ---------------- moov 重建 ----------------

fn rebuild_moov(
    moov: &[u8],
    _traks: &[(u32, Vec<u8>)],
    table_by_track: &[(u32, Vec<u8>)],
) -> Vec<u8> {
    let mut content: Vec<u8> = Vec::new();
    for_each_child(moov, 0, moov.len(), |typ, cs, ce| {
        if typ == b"mvex" {
            // 丢弃（不再是 fragmented）
        } else if typ == b"trak" {
            let trak_raw = moov[cs - 8..ce].to_vec();
            let track_id = trak_track_id(&trak_raw);
            if let Some((_, stbl)) = table_by_track.iter().find(|(id, _)| *id == track_id) {
                content.extend_from_slice(&rebuild_trak(&trak_raw, stbl));
            }
        } else {
            content.extend_from_slice(&moov[cs - 8..ce]);
        }
    });
    box_bytes(b"moov", &content)
}

fn rebuild_trak(trak: &[u8], new_stbl: &[u8]) -> Vec<u8> {
    let mut content: Vec<u8> = Vec::new();
    for_each_child(trak, 8, trak.len(), |typ, cs, ce| {
        if typ == b"mdia" {
            content.extend_from_slice(&rebuild_mdia(&trak[cs - 8..ce], new_stbl));
        } else {
            content.extend_from_slice(&trak[cs - 8..ce]);
        }
    });
    box_bytes(b"trak", &content)
}

fn rebuild_mdia(mdia: &[u8], new_stbl: &[u8]) -> Vec<u8> {
    let mut content: Vec<u8> = Vec::new();
    for_each_child(mdia, 8, mdia.len(), |typ, cs, ce| {
        if typ == b"minf" {
            content.extend_from_slice(&rebuild_minf(&mdia[cs - 8..ce], new_stbl));
        } else {
            content.extend_from_slice(&mdia[cs - 8..ce]);
        }
    });
    box_bytes(b"mdia", &content)
}

fn rebuild_minf(minf: &[u8], new_stbl: &[u8]) -> Vec<u8> {
    let mut content: Vec<u8> = Vec::new();
    for_each_child(minf, 8, minf.len(), |typ, cs, ce| {
        if typ == b"stbl" {
            let stbl_raw = &minf[cs - 8..ce];
            let mut stsd: Option<Vec<u8>> = None;
            for_each_child(stbl_raw, 8, stbl_raw.len(), |t2, cs2, ce2| {
                if t2 == b"stsd" {
                    stsd = Some(stbl_raw[cs2 - 8..ce2].to_vec());
                }
            });
            let mut new_content = Vec::new();
            if let Some(s) = stsd {
                new_content.extend_from_slice(&s);
            }
            new_content.extend_from_slice(new_stbl);
            content.extend_from_slice(&box_bytes(b"stbl", &new_content));
        } else {
            content.extend_from_slice(&minf[cs - 8..ce]);
        }
    });
    box_bytes(b"minf", &content)
}

// ---------------- stbl 构建 ----------------

fn build_stbl_with_offsets(frags: &TrackFragments, chunk_offsets: &[u64], use_64: bool) -> Vec<u8> {
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
    let body = if use_64 {
        chunk_count * 8
    } else {
        chunk_count * 4
    };
    let mut stco = Vec::with_capacity(16 + body);
    stco.extend_from_slice(&(8 + 8 + body).to_be_bytes());
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
    b.extend_from_slice(&(8 + 8 + runs.len() * 8).to_be_bytes());
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
    b.extend_from_slice(&(8 + 8 + runs.len() * 8).to_be_bytes());
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
    let mut chunk_idx = 1u32;
    for tr in &frags.truns {
        let cnt = tr.samples.len() as u32;
        if let Some(last) = runs.last_mut() {
            if last.1 == cnt {
                last.0 += 1;
            } else {
                runs.push((chunk_idx, cnt));
            }
        } else {
            runs.push((chunk_idx, cnt));
        }
        chunk_idx += 1;
    }
    let mut b = Vec::new();
    b.extend_from_slice(&(8 + 8 + runs.len() * 12).to_be_bytes());
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
    b.extend_from_slice(&(8 + 8 + indices.len() * 4).to_be_bytes());
    b.extend_from_slice(b"stss");
    b.extend_from_slice(&0u32.to_be_bytes());
    b.extend_from_slice(&(indices.len() as u32).to_be_bytes());
    for idx in &indices {
        b.extend_from_slice(&idx.to_be_bytes());
    }
    Some(b)
}

fn box_bytes(typ: &[u8; 4], content: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + content.len());
    v.extend_from_slice(&((8 + content.len()) as u32).to_be_bytes());
    v.extend_from_slice(typ);
    v.extend_from_slice(content);
    v
}
