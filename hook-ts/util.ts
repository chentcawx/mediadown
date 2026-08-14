// 纯工具函数：字节识别 / 分类 / MIME 判定。无外部依赖。

export interface FamilyExt {
  family: string;
  ext: string;
}

export function detectFamily(mime: string | null | undefined): FamilyExt {
  if (!mime) return { family: "mp4", ext: "mp4" };
  const m = mime.toLowerCase();
  const isAudio = m.indexOf("audio/") === 0;
  if (m.indexOf("webm") >= 0) return { family: "webm", ext: "webm" };
  if (m.indexOf("mp2t") >= 0 || m.indexOf("mpeg-ts") >= 0 || m.indexOf("mpegts") >= 0) return { family: "ts", ext: "ts" };
  if (m.indexOf("x-flv") >= 0 || m.indexOf("flv") >= 0) return { family: "flv", ext: "flv" };
  if (m.indexOf("mp4") >= 0 || m.indexOf("aac") >= 0 || m.indexOf("h264") >= 0 || m.indexOf("h265") >= 0 || m.indexOf("avc1") >= 0 || m.indexOf("hev1") >= 0) {
    // audio/mp4 -> .m4a, video/mp4 -> .mp4（沿用 media-sniffer-extension 的 MIME->ext 映射）
    return isAudio ? { family: "mp4", ext: "m4a" } : { family: "mp4", ext: "mp4" };
  }
  if (m.indexOf("vtt") >= 0 || m.indexOf("subrip") >= 0) return { family: "text", ext: "vtt" };
  // 通用回退：音频用 m4a，视频用 mp4
  return isAudio ? { family: "mp4", ext: "m4a" } : { family: "mp4", ext: "mp4" };
}

export function detectKind(mime: string | null | undefined): string {
  if (!mime) return "video";
  if (mime.indexOf("video") === 0) return "video";
  if (mime.indexOf("audio") === 0) return "audio";
  if (mime.indexOf("text") === 0 || mime.indexOf("application/x-subrip") === 0) return "text";
  return "video";
}

export function toU8(buf: ArrayBuffer | ArrayBufferView | Blob): Uint8Array | null {
  if (buf instanceof ArrayBuffer) return new Uint8Array(buf);
  if (ArrayBuffer.isView(buf)) return new Uint8Array(buf.buffer as ArrayBuffer, buf.byteOffset, buf.byteLength);
  return null;
}

// 在调用原生 appendBuffer 之前拷贝一份数据，避免原生实现转移/清空底层
// ArrayBuffer 后被读到空数据（沿用 media-sniffer-extension 的拷贝策略）。
export function preCopy(buf: ArrayBuffer | ArrayBufferView): Uint8Array | null {
  try {
    if (buf instanceof ArrayBuffer) return new Uint8Array(buf.slice(0));
    if (ArrayBuffer.isView(buf)) {
      const ab = buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength);
      return new Uint8Array(ab);
    }
  } catch (e) {
    /* noop */
  }
  return null;
}

export function findBytes(hay: Uint8Array, needle: number[]): number {
  const n = needle.length;
  if (hay.length < n) return -1;
  outer: for (let i = 0; i <= hay.length - n; i++) {
    for (let j = 0; j < n; j++) {
      if (hay[i + j] !== needle[j]) continue outer;
    }
    return i;
  }
  return -1;
}

export function asciiAt(hay: Uint8Array, i: number, len: number): string {
  let s = "";
  for (let k = 0; k < len; k++) s += String.fromCharCode(hay[i + k] || 0);
  return s;
}

// 轻量 init 指纹：长度 + 头 16 字节，用于判定后续纯 init 是站点周期重发还是新周期/码率切换
export function sigOf(u8: Uint8Array): string {
  if (u8.length < 16) {
    let s = u8.length + ":";
    for (let i = 0; i < u8.length; i++) s += u8[i].toString(16) + ",";
    return s;
  }
  let h = u8.length + ":";
  for (let i = 0; i < 16; i++) h += u8[i].toString(16) + ",";
  return h;
}

export interface ClassifyResult {
  kind: string;
  hasMoov?: boolean;
}

export function classify(u8: Uint8Array): ClassifyResult {
  if (u8.length < 12) return { kind: "unknown" };
  const fourcc = asciiAt(u8, 4, 4);
  if (fourcc === "ftyp" || fourcc === "styp") {
    const hasMoov = findBytes(u8, [0x6d, 0x6f, 0x6f, 0x76]) >= 0; // 'moov'
    return { kind: "mp4-init", hasMoov: hasMoov };
  }
  if (fourcc === "moof") return { kind: "mp4-seg" };
  // WebM EBML
  if (u8[0] === 0x1a && u8[1] === 0x45 && u8[2] === 0xdf && u8[3] === 0xa3) {
    return { kind: "webm" };
  }
  // FLV: "FLV\x01"
  if (u8[0] === 0x46 && u8[1] === 0x4c && u8[2] === 0x56 && u8[3] === 0x01) {
    return { kind: "flv" };
  }
  // TS (MPEG-2 Transport Stream): 0x47 sync byte, 188-byte packets
  if (u8[0] === 0x47) {
    let looksTs = true;
    const step = 188;
    if (u8.length >= step * 2) {
      if (u8[step] !== 0x47 && u8[step * 2] !== 0x47) looksTs = false;
    } else if (u8.length >= step) {
      if (u8[step] !== 0x47) looksTs = false;
    }
    if (looksTs) return { kind: "ts" };
  }
  return { kind: "unknown" };
}

export const MEDIA_EXT =
  /\.(mp4|m4v|m4a|webm|mkv|mov|flv|ts|m3u8|mp3|aac|ogg|ogv|oga|wav|opus)(\?|#|$)/i;

export function looksMediaUrl(u: string): boolean {
  return MEDIA_EXT.test(u);
}

export function mediaTypeOf(ctype: string | null | undefined, url: string): string | null {
  if (!ctype) return looksMediaUrl(url) ? "other" : null;
  const c = ctype.toLowerCase();
  if (c.indexOf("video/") === 0) {
    if (c.indexOf("mpegurl") >= 0) return "hls";
    if (c.indexOf("mp4") >= 0 || c.indexOf("webm") >= 0 || c.indexOf("ogg") >= 0 || c.indexOf("quicktime") >= 0) return "video";
    return "video";
  }
  if (c.indexOf("audio/") === 0) return "audio";
  if (c.indexOf("application/vnd.apple.mpegurl") >= 0 || c.indexOf("application/x-mpegurl") >= 0) return "hls";
  if (c.indexOf("application/octet-stream") >= 0 && looksMediaUrl(url)) return "other";
  return null;
}