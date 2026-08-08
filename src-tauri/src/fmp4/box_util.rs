//! MP4 box 基础：大端整数、box 头解析、子 box 遍历、box 拼装。

pub(crate) fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

pub(crate) fn be64(b: &[u8]) -> u64 {
    u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// 读取 data[pos] 处的 box 头，返回 (typ, header_len, content_start, content_end)
pub(crate) fn box_header(data: &[u8], pos: usize) -> Option<([u8; 4], usize, usize, usize)> {
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

/// 对 [start, end) 内的每个子 box 调用 f(&typ, content_start, content_end)
pub(crate) fn for_each_child(
    data: &[u8],
    start: usize,
    end: usize,
    mut f: impl FnMut(&[u8; 4], usize, usize),
) {
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

/// 生成一个 8 字节普通大小的 box（size + typ + content）
pub(crate) fn box_bytes(typ: &[u8; 4], content: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + content.len());
    v.extend_from_slice(&((8 + content.len()) as u32).to_be_bytes());
    v.extend_from_slice(typ);
    v.extend_from_slice(content);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn be32_be64_roundtrip() {
        assert_eq!(be32(&[0x00, 0x00, 0x01, 0x00]), 256);
        assert_eq!(be32(&[0xDE, 0xAD, 0xBE, 0xEF]), 0xDEAD_BEEF);
        assert_eq!(be64(&[0, 0, 0, 0, 0, 0, 0, 1]), 1);
        assert_eq!(be64(&[0, 0, 0, 1, 0, 0, 0, 0]), 1 << 32);
    }

    #[test]
    fn box_header_normal() {
        let data = [0, 0, 0, 24, b's', b't', b'z', b'1', 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        let (typ, header, cs, ce) = box_header(&data, 0).unwrap();
        assert_eq!(&typ, b"stz1");
        assert_eq!(header, 8);
        assert_eq!(cs, 8);
        assert_eq!(ce, 24);
    }

    #[test]
    fn box_header_large_size64() {
        let mut data = Vec::new();
        data.extend_from_slice(&1u32.to_be_bytes()); // size32 == 1 -> 64-bit
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&(16u64 + 100).to_be_bytes());
        data.extend_from_slice(&[0u8; 100]);
        let (typ, header, cs, ce) = box_header(&data, 0).unwrap();
        assert_eq!(&typ, b"mdat");
        assert_eq!(header, 16);
        assert_eq!(cs, 16);
        assert_eq!(ce, 116);
    }

    #[test]
    fn box_header_truncated_returns_none() {
        assert!(box_header(&[0, 0, 0, 4], 0).is_none());
        assert!(box_header(&[0, 0, 0, 1, b'm'], 0).is_none());
    }

    #[test]
    fn for_each_child_traverses() {
        let mut data = Vec::new();
        data.extend_from_slice(&box_bytes(b"abcd", &[1, 2, 3]));
        data.extend_from_slice(&box_bytes(b"efgh", &[0u8; 20]));
        let mut found = Vec::new();
        for_each_child(&data, 0, data.len(), |typ, cs, ce| {
            found.push((typ.to_vec(), cs, ce));
        });
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, b"abcd");
        assert_eq!(found[0].1, 8);
        assert_eq!(found[0].2, 11);
        assert_eq!(found[1].0, b"efgh");
        assert_eq!(found[1].1, 19);
        assert_eq!(found[1].2, 39);
    }

    #[test]
    fn box_bytes_writes_size_and_type() {
        let b = box_bytes(b"test", &[9, 9]);
        assert_eq!(b.len(), 10);
        assert_eq!(&b[..4], &10u32.to_be_bytes());
        assert_eq!(&b[4..8], b"test");
        assert_eq!(&b[8..], &[9, 9]);
    }
}