// MSE 捕获：劫持 MediaSource.addSourceBuffer / endOfStream 与 SourceBuffer
// appendBuffer / appendStream，把分片实时 POST 到本地服务器（边下边存）。

import { rt } from "./runtime";
import { classify, sigOf, detectFamily, detectKind, toU8, preCopy, findBytes } from "./util";
import { post, postChunkWithRetry } from "./api";

export interface MSEntry {
  ms: MediaSource;
  kind: string;
  mime: string;
  family: string;
  ext: string;
  trackId: number | null;
  initSig: string | null;  // 首个 init 段指纹（判定后续纯 init 是重发还是新周期）
  hadInit: boolean;        // 是否已落首个 init 段（用于丢弃后续重发的纯 init）
  pending: Uint8Array[];   // trackId 未分配前的暂存
  pendingBytes: number;
  registered: boolean;
  ended: boolean;
  sbs: SourceBuffer[];
  sendQ: Promise<void>;    // 串行上传队列：保证分片按到达顺序写入，杜绝乱序空白
  session: number;         // 播放会话分组：同一 MediaSource 的所有 SB 共享（配对的可靠主键）
}

const sbMap = new WeakMap<SourceBuffer, MSEntry>(); // SourceBuffer -> entry（每条轨道独立）
// 用 WeakMap 持有「MediaSource -> 其下所有轨道 entry」，键对 MediaSource 弱引用。
// 站点放弃 MediaSource（SPA 跳转、直播未调 endOfStream、中途切流重建）后该对象可被 GC，
// 连带 entry.pending（最多 64MB 缓冲）一起回收，避免 msEntries 强数组无限增长导致 WebView2 内存泄漏。
const msEntriesMap = new WeakMap<MediaSource, MSEntry[]>(); // MediaSource -> 其下 entry 列表

// 会话 ID：同一 MediaSource 的 音/视频 SourceBuffer 打进同一播放会话。
// 混流配对的主键由此而来 —— 比 document.title 可靠（SPA/动态标题不会错配）。
let msSeq = 0;
const msSessionMap = new WeakMap<MediaSource, number>();

function sessionOf(ms: MediaSource): number {
  let s = msSessionMap.get(ms);
  if (s === undefined) { s = ++msSeq; msSessionMap.set(ms, s); }
  return s;
}

// 串行上传：每个分片等上一个 POST 完成（含重试）后才发下一个。
// 客户端按序发、且等响应再发下一个，服务器即可按到达顺序逐条写盘，
// 从而杜绝并发乱序导致的 fragmented 文件时间线空白（情形A根因）。
function upload(entry: MSEntry, u8: Uint8Array): void {
  if (!rt.cfg || !rt.cfg.enabled) return;
  const data = u8.slice(0);
  entry.sendQ = entry.sendQ.then(function () {
    return postChunkWithRetry(entry, data, 0);
  });
}

export function patchMSE(): void {
  if (typeof MediaSource === "undefined") return;
  const MS = MediaSource;

  const nativeAddSB = MS.prototype.addSourceBuffer;
  if (!nativeAddSB || (MS.prototype as any).__mdtPatched) return;
  (MS.prototype as any).__mdtPatched = true;

  MS.prototype.addSourceBuffer = function (mimeType: string): SourceBuffer {
    const sb = nativeAddSB.call(this, mimeType);
    // 每个 SourceBuffer 独立成轨：视频/音频分别保存为两个文件。
    // 关键修复：旧逻辑按 MediaSource 共享同一条 entry，导致站点在“同一
    // MediaSource 上”再 open 一个 audio/mp4 缓冲时被合并进视频轨，音频丢失。
    // 改为按 SourceBuffer 各自建 entry，音视频各落一个文件。
    const det = detectFamily(mimeType);
    const entry: MSEntry = {
      ms: this,
      kind: detectKind(mimeType),
      mime: mimeType || "",
      family: det.family,
      ext: det.ext,
      trackId: null,
      initSig: null,
      hadInit: false,
      pending: [],
      pendingBytes: 0,
      registered: false,
      ended: false,
      sbs: [sb],
      sendQ: Promise.resolve(),
      session: sessionOf(this),
    };
    sbMap.set(sb, entry);
    let arr = msEntriesMap.get(this);
    if (!arr) { arr = []; msEntriesMap.set(this, arr); }
    arr.push(entry);
    wrapSB(sb, entry);
    return sb;
  };

  const nativeEOS = MS.prototype.endOfStream;
  MS.prototype.endOfStream = function (): void {
    const r = nativeEOS.apply(this, arguments as any);
    if (rt.cfg) {
      // 通知该 MediaSource 下所有轨道结束（视频 + 音频各自一条）。
      // entry 列表从 WeakMap 取；站点若已放弃该 MediaSource，GC 后列表自动消失，无需手动删除（避免强引用泄漏）。
      const arr = msEntriesMap.get(this);
      if (arr) {
        for (let k = 0; k < arr.length; k++) {
          const entry = arr[k];
          if (!entry.ended) {
            entry.ended = true;
            if (entry.trackId != null) {
              post("/seg/" + rt.cfg.token + "/" + entry.trackId + "/end", new Uint8Array(0));
            }
          }
        }
        // 不清空数组：保留 ended 标记，便于 end-of-stream 后仍有 append 的补帧也能正确跳过；
        // 真正的回收交给 GC（WeakMap 键随 MediaSource 死亡而回收）。
      }
    }
    return r;
  };
}

function wrapSB(sb: SourceBuffer, entry: MSEntry): void {
  const nativeAppend = sb.appendBuffer ? sb.appendBuffer.bind(sb) : null;
  const nativeAppendStream = (sb as any).appendStream ? (sb as any).appendStream.bind(sb) : null;
  const nativeRemove = sb.remove ? sb.remove.bind(sb) : null;

  if (nativeAppend) {
    sb.appendBuffer = function (buf: BufferSource) {
      const copy = preCopy(buf);
      const r = nativeAppend.call(this, buf);
      try {
        if (copy) handleAppend(entry, copy);
        else handleAppend(entry, buf);
      } catch (e) {}
      return r;
    };
  }
  if (nativeAppendStream) {
    (sb as any).appendStream = function (stream: any) {
      const r = nativeAppendStream.apply(this, arguments);
      try {
        if (stream && typeof stream.getReader === "function") {
          const reader = stream.getReader();
          const pump = function () {
            return reader.read().then(function (res: { done: boolean; value?: Uint8Array }) {
              if (res.done) return;
              handleAppend(entry, res.value || new Uint8Array(0));
              return pump();
            });
          };
          pump().catch(function () {});
        }
      } catch (e) {}
      return r;
    };
  }
  // remove() 会清空缓冲区间 —— 保留原生实现即可，不干预
  if (nativeRemove) {
    sb.remove = function () {
      return nativeRemove.apply(this, arguments as any);
    };
  }
}

function handleAppend(entry: MSEntry, buf: any): void {
  if (!rt.cfg || !rt.cfg.enabled) return; // 服务器未发现 / 已禁用
  const u8 = toU8(buf);
  if (!u8) {
    // Blob
    if (typeof Blob !== "undefined" && buf instanceof Blob) {
      buf
        .arrayBuffer()
        .then(function (ab: ArrayBuffer) {
          handleAppend(entry, new Uint8Array(ab));
        })
        .catch(function () {});
    }
    return;
  }

  // 首个分片到达即注册轨道。
  // 关键：浏览器喂给 MSE 的 TS 分片未必以 0x47 同步字节对齐开头，
  // 按字节特征判断会漏抓；央视等 HLS 通过 addSourceBuffer("video/mp2t")
  // 暴露类型，MIME 比字节特征更可靠（沿用 media-sniffer-extension 思路）。
  if (!entry.registered) {
    const cls = classify(u8);
    if (cls.kind === "mp4-init") { entry.hadInit = true; entry.initSig = sigOf(u8); }
    // 仅当 MIME 为通用 mp4、但字节能识别出 TS/FLV 时，才升级 family/ext
    if (entry.family === "mp4" && cls.kind === "ts") {
      entry.family = "ts";
      entry.ext = "ts";
    } else if (entry.family === "mp4" && cls.kind === "flv") {
      entry.family = "flv";
      entry.ext = "flv";
    }
    registerEntry(entry, "first");
    if (entry.pendingBytes < 64 * 1024 * 1024) {
      entry.pending.push(u8.slice(0));
      entry.pendingBytes += u8.byteLength;
    }
    return;
  }

  // 等待注册返回；暂存（限制内存）
  if (entry.trackId == null) {
    if (entry.pendingBytes < 64 * 1024 * 1024) {
      entry.pending.push(u8.slice(0));
      entry.pendingBytes += u8.byteLength;
    }
    return;
  }
  // 丢弃站点周期重发的「纯 init 段」(ftyp+moov 且不含 moof)：直播/滑动窗口/
  // HLS-DISCONTINUITY 站会重发 init 来重置时间线，但已 concat 的文件无法再重置，
  // 混进去只会打断 fragmented 时间线，导致 mkvmerge 混流渐进/周期不同步。
  // 首个 init 已保留为文件头；init+moof 混合段不丢。
  if (entry.hadInit && classify(u8).kind === "mp4-init" && findBytes(u8, [0x6d, 0x6f, 0x6f, 0x66]) < 0) {
    // 纯 init（无 moof）：仅当与首个 init 指纹相同才是站点周期重发的 reset 才丢弃；
    // 指纹不同视为码率/周期切换的新 init，保留（避免漏帧；其混流对齐属情形B）。
    if (entry.initSig && sigOf(u8) === entry.initSig) return;
  }
  upload(entry, u8);
}

function registerEntry(entry: MSEntry, why: string, forceFamily?: string, forceExt?: string): void {
  entry.registered = true;
  if (!rt.cfg || !rt.cfg.enabled) return;
  if (forceFamily) entry.family = forceFamily;
  if (forceExt) entry.ext = forceExt;
  rt.registered++;
  const body = JSON.stringify({
    kind: entry.kind,
    mime: entry.mime,
    ext: entry.ext,
    family: entry.family,
    why: why,
    session: entry.session,
    pageUrl: location.href,
    title: document.title,
  });
  post("/seg/" + rt.cfg.token + "/register", body, function (err, text) {
    if (err) {
      rt.registered--;
      entry.registered = false; // 注册失败允许下一分片重试，避免分片无限暂存内存
      return;
    }
    try {
      const info = JSON.parse(text as string);
      entry.trackId = info.id;
    } catch (e) {
      // 回包解析失败：且当注册失败处理，允许下一分片重试；
      // 否则 trackId 永远为 null，分片会一直暂存到 64MB 封顶后被静默丢弃。
      try { console.warn("[mdt] track register parse failed:", String(e)); } catch (e2) {}
      rt.registered--;
      entry.registered = false;
      return;
    }
    // 若 endOfStream 早于注册返回（极端时序），补发 end，避免该轨一直“下载中”
    if (entry.ended && rt.cfg) {
      post("/seg/" + rt.cfg.token + "/" + entry.trackId + "/end", new Uint8Array(0));
    }
    const pend = entry.pending;
    entry.pending = [];
    entry.pendingBytes = 0;
    for (let i = 0; i < pend.length; i++) upload(entry, pend[i]);
  });
}