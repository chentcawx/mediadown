//! moov 重建：丢弃 mvex，替换每轨 trak 内的 stbl，保持其它 box 原样。

use super::box_util::{be32, box_bytes, for_each_child};

/// 从 trak 的 tkhd 中读出 track_id
pub(crate) fn trak_track_id(trak: &[u8]) -> u32 {
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

/// 重建 moov：mvex 丢弃；trak 按 table_by_track 换新 stbl
pub(crate) fn rebuild_moov(
    moov: &[u8],
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_id_from_tkhd_v0() {
        // trak: [tkhd v0] + 其它；tkhd v0 中 track_id 位于 content 偏移 12
        let mut tkhd = Vec::new();
        tkhd.extend_from_slice(&0u32.to_be_bytes()); // version+flags
        tkhd.extend_from_slice(&[0u8; 16]); // creation/mod/track_id/reserved 占位
        tkhd[12..16].copy_from_slice(&7u32.to_be_bytes());
        let trak = box_bytes(b"trak", &box_bytes(b"tkhd", &tkhd));
        assert_eq!(trak_track_id(&trak), 7);
    }

    #[test]
    fn rebuild_keeps_stsd_replaces_stbl() {
        // 构造一个 tkhd v0（track_id = 7）
        let mut tkhd = Vec::new();
        tkhd.extend_from_slice(&0u32.to_be_bytes()); // version+flags
        tkhd.extend_from_slice(&[0u8; 20]); // creation/mod/track_id/... 占位
        tkhd[12..16].copy_from_slice(&7u32.to_be_bytes());

        let old_stsd = box_bytes(b"stsd", &[0u8; 16]);
        let mut old_stbl_content = box_bytes(b"stts", &[0u8; 8]);
        old_stbl_content.extend_from_slice(&old_stsd);
        let old_stbl = box_bytes(b"stbl", &old_stbl_content);

        let minf = box_bytes(b"minf", &old_stbl);
        let mdia = box_bytes(b"mdia", &minf);
        let trak = box_bytes(b"trak", &{
            let mut v = box_bytes(b"tkhd", &tkhd);
            v.extend_from_slice(&mdia);
            v
        });
        let moov = box_bytes(b"moov", &{
            let mut v = box_bytes(b"mvex", &[0u8; 4]); // 应被丢弃
            v.extend_from_slice(&trak);
            v
        });

        let new_stbl_content = {
            let mut v = box_bytes(b"stts", &[0u8; 8]);
            v.extend_from_slice(&old_stsd);
            v
        };
        // rebuild_moov 接收的是 moov 的 content（子 box 列表，不含 moov 头）
        let rebuilt = rebuild_moov(&moov[8..], &[(7, new_stbl_content)]);

        let mut types = Vec::new();
        for_each_child(&rebuilt, 8, rebuilt.len(), |typ, _, _| {
            types.push(String::from_utf8_lossy(typ).to_string());
        });
        assert_eq!(types, vec!["trak"]); // mvex 已丢弃

        // 找到重建后的 stbl：应包含旧 stsd + 新 stts + 新 stsd
        // （trak/mdia/minf/stbl 均以整 box 形式向下遍历，故从偏移 8 看子 box）
        let mut sub_tables: Vec<String> = Vec::new();
        for_each_child(&rebuilt, 8, rebuilt.len(), |typ, cs, ce| {
            if typ == b"trak" {
                let trak2 = &rebuilt[cs - 8..ce];
                for_each_child(trak2, 8, trak2.len(), |t2, cs2, ce2| {
                    if t2 == b"mdia" {
                        let mdia2 = &trak2[cs2 - 8..ce2];
                        for_each_child(mdia2, 8, mdia2.len(), |t3, cs3, ce3| {
                            if t3 == b"minf" {
                                let minf2 = &mdia2[cs3 - 8..ce3];
                                for_each_child(minf2, 8, minf2.len(), |t4, cs4, ce4| {
                                    if t4 == b"stbl" {
                                        let stbl2 = &minf2[cs4 - 8..ce4];
                                        for_each_child(stbl2, 8, stbl2.len(), |t5, _, _| {
                                            sub_tables.push(String::from_utf8_lossy(t5).to_string());
                                        });
                                    }
                                });
                            }
                        });
                    }
                });
            }
        });
        assert_eq!(sub_tables, vec!["stsd", "stts", "stsd"]);
        assert_eq!(&rebuilt[..4], &((rebuilt.len()) as u32).to_be_bytes());
    }
}