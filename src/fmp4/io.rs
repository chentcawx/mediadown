//! 主流程 finalize：扫描源文件 box，聚合样本，两阶段布局，流式写出标准 MP4。

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::box_util::be32;
use super::box_util::be64;
use super::box_util::for_each_child;
use super::layout::{assemble, Mdat};
use super::moov::trak_track_id;
use super::parser::{parse_tfhd, parse_trun, Tfhd, TrackFragments};

// 防御性上限：合法 moov（索引/元数据）正常只有 KB~MB 级，moof 同理。
// 超过上限视为异常输入，直接报错避免分配巨量内存或静默截断写坏文件。
const MAX_MOOV_LEN: usize = 512 * 1024 * 1024;
const MAX_MOOF_LEN: usize = 128 * 1024 * 1024;

/// 把 fragmented MP4（ftyp+moov+mvex + moof/mdat 序列）重建成标准 MP4
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
        f.read_exact(&mut hdr[..8]).map_err(|e| e.to_string())?;
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
                    if content_len > MAX_MOOV_LEN {
                        return Err(format!("moov 段过大: {content_len} > {MAX_MOOV_LEN}"));
                    }
                    let mut buf = vec![0u8; content_len];
                    f.seek(SeekFrom::Start(content_start))
                        .map_err(|e| e.to_string())?;
                    f.read_exact(&mut buf).map_err(|e| e.to_string())?;
                    moov_payload = Some(buf);
                }
            }
            b"moof" => {
                if content_len > MAX_MOOF_LEN {
                    return Err(format!("moof 段过大: {content_len} > {MAX_MOOF_LEN}"));
                }
                let mut buf = vec![0u8; content_len];
                f.seek(SeekFrom::Start(content_start))
                    .map_err(|e| e.to_string())?;
                f.read_exact(&mut buf).map_err(|e| e.to_string())?;
                // 记录 moof box 起点而非内容起点：default-base-is-moof 时
                // trun 的 data_offset 相对 moof box 首字节（ISO 14496-12 8.8.12.1），
                // 用内容起点会在所有 stco/co64 上偏 +8，样本错位、整片解码失败。
                moofs.push((pos, buf));
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
        let mut traf_entries: Vec<(Tfhd, Vec<super::parser::Trun>)> = Vec::new();
        for_each_child(payload, 0, payload.len(), |typ, cs, ce| {
            if typ == b"traf" {
                let mut tfhd = Tfhd::default();
                let mut truns: Vec<super::parser::Trun> = Vec::new();
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

    // 3) 两阶段布局（必要时升级 64 位偏移）
    let mut use_64 = false;
    let new_moov = loop {
        let (moov_bytes, _mdat_final_starts) =
            assemble(ftyp.len(), &moov, &traks, &per_track, &mdats, use_64);
        let mdat_total: u64 = mdats.iter().map(|m| m.header as u64 + m.data_len).sum();
        let total = ftyp.len() as u64 + moov_bytes.len() as u64 + mdat_total;
        if total > u32::MAX as u64 && !use_64 {
            use_64 = true;
            continue;
        }
        break moov_bytes;
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fmp4::box_util::{be32, box_bytes};

    /// 构造一个 trun（flags: data-offset + sample-duration + sample-size）
    fn trun_bytes(samples: &[(u32, u32)], data_offset: i32) -> Vec<u8> {
        let flags: u32 = 0x01 | 0x100 | 0x200;
        let mut content = Vec::new();
        content.extend_from_slice(&flags.to_be_bytes());
        content.extend_from_slice(&(samples.len() as u32).to_be_bytes());
        content.extend_from_slice(&data_offset.to_be_bytes());
        for (dur, size) in samples {
            content.extend_from_slice(&dur.to_be_bytes());
            content.extend_from_slice(&size.to_be_bytes());
        }
        box_bytes(b"trun", &content)
    }

    /// 构造一个 moof：含 traf(tfhd+trun)，tfhd.base_data_offset 指向对应 mdat 数据段
    fn moof_bytes(samples: &[(u32, u32)], base_data_offset: u64) -> Vec<u8> {
        let mut tfhd_content = Vec::new();
        tfhd_content.extend_from_slice(&0x01u32.to_be_bytes()); // base-data-offset
        tfhd_content.extend_from_slice(&1u32.to_be_bytes()); // track_id
        tfhd_content.extend_from_slice(&base_data_offset.to_be_bytes());
        let traf_content = {
            let mut v = box_bytes(b"tfhd", &tfhd_content);
            v.extend_from_slice(&trun_bytes(samples, 0));
            v
        };
        box_bytes(b"moof", &box_bytes(b"traf", &traf_content))
    }

    /// 构造 default-base-is-moof 形态的 moof：tfhd 设置 0x020000 且不写 base_data_offset，
    /// trun.data_offset 相对 moof box 起点。若 build_stbl 把 base 误当作 moof 内容起点，
    /// 最终 stco 会整体偏 +8，回归测试用于锁死该语义。moof 为容器 box（无 version/flags）。
    fn moof_bytes_moof(samples: &[(u32, u32)]) -> Vec<u8> {
        let n = samples.len();
        // moof_len = 8(box头) + 16(mfhd) + (8 + 16(tfhd) + 20+8n(trun)) = 68 + 8n；
        // trun.data_offset = moof_len + 8（紧随 moof 的 mdat 之数据段起点）
        let data_offset = (68 + 8 * n as i32) + 8;
        let mut tfhd_content = Vec::new();
        tfhd_content.extend_from_slice(&0x020000u32.to_be_bytes()); // default-base-is-moof
        tfhd_content.extend_from_slice(&1u32.to_be_bytes()); // track_id
        let mut traf_content = box_bytes(b"tfhd", &tfhd_content);
        traf_content.extend_from_slice(&trun_bytes(samples, data_offset));
        let mut moof_content = Vec::new();
        moof_content.extend_from_slice(&box_bytes(b"mfhd", &[0u8; 8]));
        moof_content.extend_from_slice(&box_bytes(b"traf", &traf_content));
        box_bytes(b"moof", &moof_content)
    }

    /// 构造最小可 finalize 的 fMP4 内存镜像（ftyp + moov + N 个 moof/mdat 片段）
    fn make_fragmented(samples: &[&[(u32, u32)]]) -> Vec<u8> {
        let ftyp = box_bytes(b"ftyp", b"isom\0\0\0\0isom");

        // 旧 trak：tkhd(track_id=1) + mdia(minf(stbl(stsd))) + mvex
        let mut tkhd_content = vec![0u8; 20];
        tkhd_content[12..16].copy_from_slice(&1u32.to_be_bytes());
        let stsd = box_bytes(b"stsd", &[0u8; 16]);
        let stbl = box_bytes(b"stbl", &stsd);
        let minf = box_bytes(b"minf", &stbl);
        let mdia = box_bytes(b"mdia", &minf);
        let trak = box_bytes(b"trak", &{
            let mut t = box_bytes(b"tkhd", &tkhd_content);
            t.extend_from_slice(&mdia);
            t
        });
        let moov = box_bytes(b"moov", &{
            let mut m = box_bytes(b"mvex", &[0u8; 4]);
            m.extend_from_slice(&trak);
            m
        });

        let mut file = Vec::new();
        file.extend_from_slice(&ftyp);
        file.extend_from_slice(&moov);
        for s in samples {
            let moof_start = file.len() as u64;
            let n = s.len() as u64;
            let moof_len = 60 + 8 * n; // 结构固定长度
            let base = moof_start + moof_len + 8; // 下一个 box 的数据段起点
            file.extend_from_slice(&moof_bytes(s, base));
            let data_len: usize = s.iter().map(|(_, sz)| *sz as usize).sum();
            let mut data = vec![0xAAu8; data_len];
            data.iter_mut().enumerate().for_each(|(i, b)| *b = i as u8);
            file.extend_from_slice(&box_bytes(b"mdat", &data));
        }
        file
    }

    /// 与 make_fragmented 相同，但分片用 default-base-is-moof 形态的 moof。
    fn make_fragmented_moof(samples: &[&[(u32, u32)]]) -> Vec<u8> {
        let ftyp = box_bytes(b"ftyp", b"isom\0\0\0\0isom");
        let mut tkhd_content = vec![0u8; 20];
        tkhd_content[12..16].copy_from_slice(&1u32.to_be_bytes());
        let stsd = box_bytes(b"stsd", &[0u8; 16]);
        let stbl = box_bytes(b"stbl", &stsd);
        let minf = box_bytes(b"minf", &stbl);
        let mdia = box_bytes(b"mdia", &minf);
        let trak = box_bytes(b"trak", &{
            let mut t = box_bytes(b"tkhd", &tkhd_content);
            t.extend_from_slice(&mdia);
            t
        });
        let moov = box_bytes(b"moov", &{
            let mut m = box_bytes(b"mvex", &[0u8; 4]);
            m.extend_from_slice(&trak);
            m
        });
        let mut file = Vec::new();
        file.extend_from_slice(&ftyp);
        file.extend_from_slice(&moov);
        for s in samples {
            file.extend_from_slice(&moof_bytes_moof(s));
            let data_len: usize = s.iter().map(|(_, sz)| *sz as usize).sum();
            let mut data = vec![0xAAu8; data_len];
            data.iter_mut().enumerate().for_each(|(i, b)| *b = i as u8);
            file.extend_from_slice(&box_bytes(b"mdat", &data));
        }
        file
    }

    #[test]
    fn finalize_rebuilds_standard_mp4() {
        let src_bytes = make_fragmented(&[
            &[(1000, 10), (1000, 20)], // 片段0：2 样本
            &[(500, 15)],              // 片段1：1 样本
        ]);
        // ftyp 头合法（size 字段 = 20）
        assert_eq!(be32(&src_bytes[..4]), 20);

        let dir = std::env::temp_dir();
        let src = dir.join(format!("md-test-{}-src.mp4", std::process::id()));
        let dst = dir.join(format!("md-test-{}-dst.mp4", std::process::id()));
        std::fs::write(&src, &src_bytes).unwrap();
        let r = finalize(&src, &dst);
        assert!(r.is_ok(), "{:?}", r.err());
        let out = std::fs::read(&dst).unwrap();
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);

        // 顶层 box：ftyp、moov、mdat、mdat
        let mut tops: Vec<(Vec<u8>, usize, usize)> = Vec::new();
        for_each_child(&out, 0, out.len(), |typ, cs, ce| {
            tops.push((typ.to_vec(), cs, ce));
        });
        let top_types: Vec<String> =
            tops.iter().map(|(t, _, _)| String::from_utf8_lossy(t).to_string()).collect();
        assert_eq!(top_types, vec!["ftyp", "moov", "mdat", "mdat"]);

        let moov_content = &out[tops[1].1..tops[1].2];
        // mvex / moof 不应残留，出现新样本表
        for want in &[b"mvex", b"moof"] {
            assert!(!moov_content.windows(4).any(|w| w == *want), "{want:?} 不应残留");
        }
        for want in &[b"stts", b"stsz", b"stsc", b"stss", b"stco", b"stsd"] {
            assert!(moov_content.windows(4).any(|w| w == *want), "moov 应含 {want:?}");
        }
        assert!(!moov_content.windows(4).any(|w| w == b"co64"));

        // 输出 mdat 内容与源文件中的 mdat 逐段一致（顺序、字节完全相同）
        let mut src_mdats: Vec<&[u8]> = Vec::new();
        for_each_child(&src_bytes, 0, src_bytes.len(), |typ, cs, ce| {
            if typ == b"mdat" {
                src_mdats.push(&src_bytes[cs..ce]);
            }
        });
        let out_mdats: Vec<&[u8]> =
            tops[2..].iter().map(|(_, cs, ce)| &out[*cs..*ce]).collect();
        assert_eq!(out_mdats.len(), src_mdats.len());
        for (o, s) in out_mdats.iter().zip(&src_mdats) {
            assert_eq!(*o, *s);
        }

        // 防回归：stco 偏移必须计入 ftyp 头（输出布局 ftyp||moov||mdat(s)），
        // 否则全部样本指针偏小、文件播放/混流必坏。
        // moov 完整 box 长度 = 头 8 字节 + content；mdat 数据段起点 = ftyp(20)+moov+8.
        let moov_content = &out[tops[1].1..tops[1].2];
        // stco 嵌套在 trak/mdia/minf/stbl 内，且为 stbl 最后一个 box；rposition
        // 命中真实 box 头，避开其它 box 数据中的巧合 "stco" 字节。
        let stco_off = moov_content
            .windows(4)
            .rposition(|w| w == b"stco")
            .expect("moov 应含 stco");
        // stco box：8 字节头（stco_off 指向类型字节）+ version/flags(4)+entry_count(4)+entries
        let count = be32(&moov_content[stco_off + 8..stco_off + 12]);
        assert_eq!(count as usize, 2);
        let first = be32(&moov_content[stco_off + 12..stco_off + 16]);
        let second = be32(&moov_content[stco_off + 16..stco_off + 20]);
        let moov_full_len = tops[1].2 - tops[1].1 + 8; // content start 换算回完整 box 长
        let expect_first = 20 + moov_full_len + 8; // ftyp + moov + mdat1 的 8 字节头
        // src_mdats 是 content 段（不含头），mdat1 全 box 长 = 8 字节头 + content
        let expect_second = expect_first + 8 + src_mdats[0].len();
        assert_eq!(first as usize, expect_first, "mdat1 数据起点应位于 ftyp+moov 之后");
        assert_eq!(second as usize, expect_second, "mdat2 数据起点应顺接 mdat1");
    }

    #[test]
    fn finalize_handles_default_base_is_moof() {
        // 回归：default-base-is-moof 分片的 data_offset 相对 moof box 起点。
        // 若解析误用 moof 内容起点（+8），最终 stco 整体偏 +8，样本错位不可解码。
        let src_bytes = make_fragmented_moof(&[&[(1000, 10), (1000, 20)], &[(500, 15)]]);

        let dir = std::env::temp_dir();
        let src = dir.join(format!("md-test-moof-{}-src.mp4", std::process::id()));
        let dst = dir.join(format!("md-test-moof-{}-dst.mp4", std::process::id()));
        std::fs::write(&src, &src_bytes).unwrap();
        let r = finalize(&src, &dst);
        assert!(r.is_ok(), "{:?}", r.err());
        let out = std::fs::read(&dst).unwrap();
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);

        let mut tops: Vec<(Vec<u8>, usize, usize)> = Vec::new();
        for_each_child(&out, 0, out.len(), |typ, cs, ce| {
            tops.push((typ.to_vec(), cs, ce));
        });
        let moov_content = &out[tops[1].1..tops[1].2];
        let stco_off = moov_content
            .windows(4)
            .rposition(|w| w == b"stco")
            .expect("moov 应含 stco");
        let count = be32(&moov_content[stco_off + 8..stco_off + 12]);
        assert_eq!(count as usize, 2);
        let first = be32(&moov_content[stco_off + 12..stco_off + 16]);
        let second = be32(&moov_content[stco_off + 16..stco_off + 20]);
        let moov_full_len = tops[1].2 - tops[1].1 + 8;
        let expect_first = 20 + moov_full_len + 8;
        let expect_second = expect_first + 8 + 30; // mdat1 content = 10+20
        assert_eq!(first as usize, expect_first, "default-base-is-moof 首 chunk 应落在 mdat1 数据起点");
        assert_eq!(second as usize, expect_second, "default-base-is-moof 次 chunk 应顺接 mdat1");
    }
}