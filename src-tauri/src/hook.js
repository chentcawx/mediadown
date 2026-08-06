/*
 * MediaDown 捕获引擎（注入目标站点，document_start / MAIN world）
 *
 * 功能：
 *  1. 自发现本地分片服务器（探测 http://127.0.0.1:49321~49330/cfg）
 *  2. 劫持 MediaSource.addSourceBuffer / endOfStream，把 MSE 分片实时
 *     POST 到本地服务器（边下边存）
 *  3. 劫持 fetch / XHR + 轮询 <video>/<audio>.src，上报直链媒体
 *  4. 根据控制台配置强制 playbackRate
 *
 * 所有上传都使用保存的原生 fetch 引用，避免递归劫持。
 */
(function () {
  "use strict";
  if (window.__mdtHookInstalled) return;
  window.__mdtHookInstalled = true;

  const nativeFetch = window.fetch.bind(window);
  const CONFIG_TTL = 3000;
  let cfg = null; // {port, token, enabled, auto, copyUnlock, rate}
  let __mdtUnknownBytes = 0; // 无法分类的 MSE 分片累计字节（诊断用）
  let __mdtRegistered = 0;   // 已注册的轨道数（诊断用）

  // ---------------- 自发现 ----------------
  // discover: scan 127.0.0.1:49321~49330 once; if no hit, retry every 3s until cfg is set.
  // (startup race / transient server-not-ready would otherwise leave hook stuck "un-injected".)
  let discoverActive = false;
  function discover() {
    if (cfg || discoverActive) return;
    discoverActive = true;
    function scan(i) {
      if (cfg) { discoverActive = false; return; }
      if (i >= 10) {
        // whole round failed -> retry after 3s (loop stays active)
        setTimeout(scan, 3000, 0);
        return;
      }
      const port = 49321 + i;
      nativeFetch("http://127.0.0.1:" + port + "/cfg", { cache: "no-store" })
        .then(function (r) {
          return r.ok ? r.json() : null;
        })
        .then(function (c) {
          if (c && c.app === "mediadown") {
            cfg = c;
            discoverActive = false;
            applyRateToAll();
            applyCopyUnlock();
          } else {
            scan(i + 1);
          }
        })
        .catch(function () { scan(i + 1); });
    }
    scan(0);
  }
  // 配置刷新（enabled / rate 变化）
  function refreshConfig() {
    if (!cfg) return;
    const port = cfg.port;
    nativeFetch("http://127.0.0.1:" + port + "/cfg", { cache: "no-store" })
      .then(function (r) {
        return r.ok ? r.json() : null;
      })
      .then(function (c) {
        if (c) {
          cfg.port = c.port;
          cfg.token = c.token;
          cfg.enabled = c.enabled;
          cfg.auto = c.auto;
          cfg.copyUnlock = c.copyUnlock;
          cfg.rate = c.rate;
          applyCopyUnlock();
        }
      })
      .catch(function () {
        cfg = null; // 服务器重启后重新发现
        discover();
      });
  }

  // ---------------- 上传 ----------------
  function post(path, body, cb) {
    if (!cfg) {
      if (cb) cb(new Error("no cfg"));
      return;
    }
    nativeFetch("http://127.0.0.1:" + cfg.port + path, {
      method: "POST",
      headers: { "Content-Type": "application/octet-stream" },
      body: body,
      cache: "no-store",
    })
      .then(function (r) {
        return r.text();
      })
      .then(function (t) {
        if (cb) cb(null, t);
      })
      .catch(function (e) {
        if (cb) cb(e);
      });
  }

  // ---------------- MSE 捕获 ----------------
  const sbMap = new WeakMap(); // SourceBuffer -> entry（每条轨道独立）
  let msEntries = [];          // 所有已注册 entry（endOfStream 时按 MediaSource 逐个通知）

  function detectFamily(mime) {
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

  function detectKind(mime) {
    if (!mime) return "video";
    if (mime.indexOf("video") === 0) return "video";
    if (mime.indexOf("audio") === 0) return "audio";
    if (mime.indexOf("text") === 0 || mime.indexOf("application/x-subrip") === 0) return "text";
    return "video";
  }

  function toU8(buf) {
    if (buf instanceof ArrayBuffer) return new Uint8Array(buf);
    if (ArrayBuffer.isView(buf)) return new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength);
    return null;
  }

  // 在调用原生 appendBuffer 之前拷贝一份数据，避免原生实现转移/清空底层
  // ArrayBuffer 后被读到空数据（沿用 media-sniffer-extension 的拷贝策略）。
  function preCopy(buf) {
    try {
      if (buf instanceof ArrayBuffer) return new Uint8Array(buf.slice(0));
      if (ArrayBuffer.isView(buf)) {
        const ab = buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength);
        return new Uint8Array(ab);
      }
    } catch (e) {}
    return null;
  }

  function findBytes(hay, needle) {
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

  function asciiAt(hay, i, len) {
    let s = "";
    for (let k = 0; k < len; k++) s += String.fromCharCode(hay[i + k] || 0);
    return s;
  }

  // 轻量 init 指纹：长度 + 头 16 字节，用于判定后续纯 init 是站点周期重发还是新周期/码率切换
  function sigOf(u8) {
    if (u8.length < 16) {
      let s = u8.length + ":";
      for (let i = 0; i < u8.length; i++) s += u8[i].toString(16) + ",";
      return s;
    }
    let h = u8.length + ":";
    for (let i = 0; i < 16; i++) h += u8[i].toString(16) + ",";
    return h;
  }

  function classify(u8) {
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

  function patchMSE() {
    if (typeof MediaSource === "undefined") return;
    const MS = MediaSource;

    const nativeAddSB = MS.prototype.addSourceBuffer;
    if (!nativeAddSB || MS.prototype.__mdtPatched) return;
    MS.prototype.__mdtPatched = true;

    MS.prototype.addSourceBuffer = function (mimeType) {
      const sb = nativeAddSB.call(this, mimeType);
      // 每个 SourceBuffer 独立成轨：视频/音频分别保存为两个文件。
      // 关键修复：旧逻辑按 MediaSource 共享同一条 entry，导致站点在“同一
      // MediaSource 上”再 open 一个 audio/mp4 缓冲时被合并进视频轨，音频丢失。
      // 改为按 SourceBuffer 各自建 entry，音视频各落一个文件。
      const det = detectFamily(mimeType);
      const entry = {
        ms: this,
        kind: detectKind(mimeType),
        mime: mimeType || "",
        family: det.family,
        ext: det.ext,
        trackId: null,
        hadInit: false, // 是否已落首个 init 段（用于丢弃后续重发的纯 init）
        initSig: null,  // 首个 init 段指纹（判定后续纯 init 是重发还是新周期）
        sendQ: Promise.resolve(), // 串行上传队列：保证分片按到达顺序写入，杜绝乱序空白
        pending: [], // trackId 未分配前的暂存
        pendingBytes: 0,
        registered: false,
        ended: false,
        sbs: [sb],
      };
      sbMap.set(sb, entry);
      msEntries.push(entry);
      wrapSB(sb, entry);
      return sb;
    };

    const nativeEOS = MS.prototype.endOfStream;
    MS.prototype.endOfStream = function () {
      const r = nativeEOS.apply(this, arguments);
      if (cfg) {
        // 通知该 MediaSource 下所有轨道结束（视频 + 音频各自一条）
        for (let k = 0; k < msEntries.length; k++) {
          const entry = msEntries[k];
          if (entry.ms === this && !entry.ended) {
            entry.ended = true;
            if (entry.trackId != null) {
              post("/seg/" + cfg.token + "/" + entry.trackId + "/end", new Uint8Array(0));
            }
          }
        }
        const self = this;
        msEntries = msEntries.filter(function (e) { return e.ms !== self; });
      }
      return r;
    };
  }

  function wrapSB(sb, entry) {
    const nativeAppend = sb.appendBuffer ? sb.appendBuffer.bind(sb) : null;
    const nativeAppendStream = sb.appendStream ? sb.appendStream.bind(sb) : null;
    const nativeRemove = sb.remove ? sb.remove.bind(sb) : null;

    if (nativeAppend) {
      sb.appendBuffer = function (buf) {
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
      sb.appendStream = function (stream) {
        const r = nativeAppendStream.apply(this, arguments);
        try {
          if (stream && typeof stream.getReader === "function") {
            const reader = stream.getReader();
            const pump = () =>
              reader.read().then(({ done, value }) => {
                if (done) return;
                handleAppend(entry, value);
                return pump();
              });
            pump().catch(() => {});
          }
        } catch (e) {}
        return r;
      };
    }
    // remove() 会清空缓冲区间 —— 保留原生实现即可，不干预
    if (nativeRemove) {
      sb.remove = function () {
        return nativeRemove.apply(this, arguments);
      };
    }
  }

  function handleAppend(entry, buf) {
    if (!cfg || !cfg.enabled) return; // 服务器未发现 / 已禁用
    let u8 = toU8(buf);
    if (!u8) {
      // Blob
      if (typeof Blob !== "undefined" && buf instanceof Blob) {
        buf
          .arrayBuffer()
          .then(function (ab) {
            handleAppend(entry, new Uint8Array(ab));
          })
          .catch(function () {});
      }
      return;
    }

    // 首个分片到达即按 addSourceBuffer 的 MIME 注册轨道。
    // 关键：浏览器喂给 MSE 的 TS 分片未必以 0x47 同步字节对齐开头，
    // 按字节特征判断会漏抓；央视等 HLS 站通过 addSourceBuffer("video/mp2t")
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
      registerTrack(entry, "first");
      if (entry.pendingBytes < 64 * 1024 * 1024) {
        entry.pending.push(u8.slice(0));
        entry.pendingBytes += u8.byteLength;
      }
      return;
    }

    if (entry.trackId == null) {
      // 等待注册返回；暂存（限制内存）
      if (entry.pendingBytes < 64 * 1024 * 1024) {
        entry.pending.push(u8.slice(0));
        entry.pendingBytes += u8.byteLength;
      }
      return;
    }
    // 丢弃站点周期重发的「纯 init 段」(ftyp+moov 且不含 moof)：直播/滑动窗口/
    // HLS-DISCONTINUITY 站会重发 init 来重置时间线，但本已 concat 的文件无法再
    // reset，混进去只会打断 fragmented 时间线，导致 mkvmerge 混流渐进/周期不同步。
    // 首个 init 已由 hadInit 标记并保留为文件头；init+seg 混合段(含 moof)不丢。
    if (entry.hadInit && classify(u8).kind === "mp4-init" && findBytes(u8, [0x6d, 0x6f, 0x6f, 0x66]) < 0) {
      // 纯 init（无 moof）：仅当与首个 init 完全同指纹才是站点周期重发的 reset 才丢弃；
      // 指纹不同视为码率/周期切换的新 init，保留（避免漏帧；其混流对齐属情形B）。
      if (entry.initSig && sigOf(u8) === entry.initSig) return;
    }
    upload(entry, u8);
  }

  function registerTrack(entry, why, forceFamily, forceExt) {
    entry.registered = true;
    if (!cfg || !cfg.enabled) return;
    if (forceFamily) entry.family = forceFamily;
    if (forceExt) entry.ext = forceExt;
    __mdtRegistered++;
    const body = JSON.stringify({
      kind: entry.kind,
      mime: entry.mime,
      ext: entry.ext,
      family: entry.family,
      why: why,
      pageUrl: location.href,
      title: document.title,
    });
    post("/seg/" + cfg.token + "/register", body, function (err, text) {
      if (err) {
        __mdtRegistered--;
        entry.registered = false; // 注册失败允许下一片重试，避免分片无限暂存内存
        return;
      }
      try {
        const info = JSON.parse(text);
        entry.trackId = info.id;
        // 若 endOfStream 早于注册返回（极端时序），补发 end，避免该轨一直“下载中”
        if (entry.ended && cfg) {
          post("/seg/" + cfg.token + "/" + entry.trackId + "/end", new Uint8Array(0));
        }
        const pend = entry.pending;
        entry.pending = [];
        entry.pendingBytes = 0;
        for (let i = 0; i < pend.length; i++) upload(entry, pend[i]);
      } catch (e) {}
    });
  }

  function upload(entry, u8) {
    if (!cfg || !cfg.enabled) return;
    const data = u8.slice(0);
    // 串行上传：每个分片等上一个 POST 完成（含重试）后才发下一个。
    // 客户端按序发、且等响应再发下一个，服务器即可按到达顺序逐条写盘，
    // 彻底杜绝并发乱序导致的 fragmented mp4 时间线空白（情形A根因）。
    entry.sendQ = entry.sendQ.then(function () {
      return postChunkWithRetry(entry, data, 0);
    });
  }

  // 分片上传，失败自动重试（最多 5 次，指数退避）；仍失败则放弃该分片
  // （极端网络错误），避免无限阻塞后续分片造成更大空缺。
  function postChunkWithRetry(entry, data, attempt) {
    return new Promise(function (resolve) {
      if (!cfg || !cfg.enabled) { resolve(); return; }
      post("/seg/" + cfg.token + "/" + entry.trackId + "/chunk", data, function (err) {
        if (!err) { resolve(); return; }
        if (attempt >= 5) {
          try { console.warn("[mdt] chunk upload dropped after retries:", String(err)); } catch (e) {}
          resolve();
          return;
        }
        setTimeout(function () {
          postChunkWithRetry(entry, data, attempt + 1).then(resolve, resolve);
        }, 200 * (attempt + 1));
      });
    });
  }

  // ---------------- 直链媒体上报 ----------------
  const reported = new Set();

  function reportMedia(url, type, extra) {
    if (!cfg || !cfg.enabled || !url || url.indexOf("blob:") === 0 || url.indexOf("data:") === 0) return;
    if (url.indexOf("127.0.0.1") >= 0 || url.indexOf("localhost") >= 0) return;
    const key = type + "|" + url;
    if (reported.has(key)) return;
    reported.add(key);
    const body = JSON.stringify(
      Object.assign(
        {
          url: url,
          type: type,
          pageUrl: location.href,
          title: document.title,
        },
        extra || {}
      )
    );
    post("/seg/" + cfg.token + "/report", body);
  }

  const MEDIA_EXT =
    /\.(mp4|m4v|m4a|webm|mkv|mov|flv|ts|m3u8|mp3|aac|ogg|ogv|oga|wav|opus)(\?|#|$)/i;

  function looksMediaUrl(u) {
    return MEDIA_EXT.test(u);
  }

  function mediaTypeOf(ctype, url) {
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

  function patchFetch() {
    const orig = window.fetch;
    if (!orig || orig.__mdtPatched) return;
    const wrapper = function (input, init) {
      const req = new Request(input, init);
      if (req.method === "GET") {
        return orig.apply(this, arguments).then(function (resp) {
          try {
            const ctype = resp.headers ? resp.headers.get("content-type") : null;
            const mtype = mediaTypeOf(ctype, req.url);
            if (mtype && resp.ok && !req.url.includes("127.0.0.1")) {
              resp.clone().arrayBuffer().then(function (ab) {
                reportMedia(req.url, mtype, { size: ab.byteLength });
              }).catch(function () {});
            }
          } catch (e) {}
          return resp;
        });
      }
      return orig.apply(this, arguments);
    };
    wrapper.__mdtPatched = true;
    try {
      Object.defineProperty(window, "fetch", { value: wrapper, writable: true, configurable: true });
    } catch (e) {}
  }

  function patchXHR() {
    const origOpen = XMLHttpRequest.prototype.open;
    const origSend = XMLHttpRequest.prototype.send;
    if (origOpen.__mdtPatched) return;

    XMLHttpRequest.prototype.open = function (method, url) {
      this.__mdtUrl = url;
      return origOpen.apply(this, arguments);
    };
    XMLHttpRequest.prototype.open.__mdtPatched = true;

    XMLHttpRequest.prototype.send = function () {
      const xhr = this;
      const origOnLoad = xhr.onload;
      xhr.onload = function (ev) {
        try {
          if (xhr.__mdtUrl && xhr.status >= 200 && xhr.status < 300) {
            const ctype = xhr.getResponseHeader && xhr.getResponseHeader("content-type");
            const mtype = mediaTypeOf(ctype, xhr.__mdtUrl);
            if (mtype && !String(xhr.__mdtUrl).includes("127.0.0.1")) {
              let size = 0;
              if (typeof xhr.response === "string") size = xhr.response.length;
              else if (xhr.response) {
                try {
                  size = xhr.response.byteLength || xhr.response.size || 0;
                } catch (e) {}
              }
              reportMedia(String(xhr.__mdtUrl), mtype, { size: size });
            }
          }
        } catch (e) {}
        if (typeof origOnLoad === "function") return origOnLoad.call(this, ev);
      };
      return origSend.apply(this, arguments);
    };
    XMLHttpRequest.prototype.send.__mdtPatched = true;
  }

  // ---------------- 轮询 video/audio src ----------------
  function pollMediaElements() {
    if (!cfg || !cfg.enabled) return;
    const els = document.querySelectorAll("video,audio");
    for (let i = 0; i < els.length; i++) {
      const el = els[i];
      const src = el.currentSrc || el.src;
      if (src && looksMediaUrl(src)) {
        reportMedia(src, el.tagName === "AUDIO" ? "audio" : "video", { size: 0 });
      }
    }
    // 新元素监听
    for (let i = 0; i < els.length; i++) {
      if (!els[i].__mdtSrcWatched) {
        els[i].__mdtSrcWatched = true;
        const el = els[i];
        try {
          const desc = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, "src");
          if (desc && desc.set) {
            const origSet = desc.set;
            Object.defineProperty(el, "src", {
              get: function () {
                return origSet.get ? origSet.get.call(this) : this.getAttribute("src");
              },
              set: function (v) {
                origSet.call(this, v);
                if (typeof v === "string" && looksMediaUrl(v)) {
                  reportMedia(v, this.tagName === "AUDIO" ? "audio" : "video", { size: 0 });
                }
              },
            });
          }
        } catch (e) {}
      }
    }
  }

  // ---------------- 倍速 ----------------
  function applyRateToAll() {
    if (!cfg || !cfg.enabled) return;
    const els = document.querySelectorAll("video");
    for (let i = 0; i < els.length; i++) {
      const el = els[i];
      if (Math.abs(el.playbackRate - cfg.rate) > 0.01 && el.readyState > 0) {
        try {
          el.playbackRate = cfg.rate;
        } catch (e) {}
      }
    }
  }

  // 控制台点击倍速后，Rust 会立即广播 md-rate 事件，使变速即时生效
  // （3s 轮询作为兜底，本监听仅用于消除延迟、让效果立即可见）。
  function installRateEvent() {
    try {
      var T = window.__TAURI__;
      if (T && T.event && typeof T.event.listen === "function") {
        T.event.listen("md-rate", function (e) {
          try {
            if (cfg) {
              cfg.rate = (typeof e.payload === "number") ? e.payload : cfg.rate;
              applyRateToAll();
            }
          } catch (err) {}
        }).catch(function () {});
      }
    } catch (e) {}
  }

  // ---------------- 解除复制限制（参考 webacc copy-unlock） ----------------
  // 1) 强制文本可选（覆盖 user-select:none）
  // 2) 捕获阶段 stopImmediatePropagation 阻断站点的右键/复制/选择/拖拽拦截
  // 3) 选中文本后自动复制到剪贴板（带轻提示）
  let __cuSelectEl = null;
  let __cuInterceptOn = false;
  let __cuAutoOn = false;
  let __cuMouseup = null, __cuKeyup = null;

  const CU_SKIP = 'input, textarea, [contenteditable], [contenteditable=""], [contenteditable="true"]';
  const CU_BLOCK_TYPES = ['contextmenu', 'copy', 'selectstart', 'dragstart', 'beforecopy'];

  function cuInjectSelectStyle() {
    if (__cuSelectEl) return;
    const css =
      '*{ -webkit-user-select:text !important; -moz-user-select:text !important; ' +
      '-ms-user-select:text !important; user-select:text !important; ' +
      '-webkit-touch-callout:default !important; }';
    const el = document.createElement('style');
    el.setAttribute('data-mdt-copy', '');
    el.textContent = css;
    const root = document.head || document.documentElement;
    if (root) root.appendChild(el);
    __cuSelectEl = el;
  }
  function cuRemoveSelectStyle() {
    if (__cuSelectEl && __cuSelectEl.parentNode) __cuSelectEl.parentNode.removeChild(__cuSelectEl);
    __cuSelectEl = null;
  }

  function cuToast(msg) {
    try {
      let t = document.getElementById('mdt-copy-toast');
      if (!t) {
        t = document.createElement('div');
        t.id = 'mdt-copy-toast';
        t.style.cssText =
          'position:fixed;left:50%;bottom:24px;transform:translateX(-50%);z-index:2147483647;' +
          'background:rgba(17,24,39,.92);color:#fff;font:13px/1.4 -apple-system,"Segoe UI",Roboto,sans-serif;' +
          'padding:8px 14px;border-radius:8px;pointer-events:none;box-shadow:0 4px 16px rgba(0,0,0,.3);' +
          'opacity:0;transition:opacity .18s;max-width:80vw;';
        (document.body || document.documentElement).appendChild(t);
      }
      t.textContent = msg;
      t.style.opacity = '1';
      clearTimeout(t.__timer);
      t.__timer = setTimeout(function () { t.style.opacity = '0'; }, 1400);
    } catch (e) { /* 忽略 */ }
  }

  function cuOnBlocked(e) {
    if (!cfg || !cfg.copyUnlock) return;
    const t = e.target;
    const isEditable = t instanceof HTMLElement && typeof t.closest === 'function' && t.closest(CU_SKIP);
    if (isEditable) return; // 可编辑区不拦截（保留正常编辑/复制）
    // 阻断站点后续所有监听器（preventDefault / returnValue 均被隔断），不调用 preventDefault
    // -> 浏览器默认行为（右键菜单 / 复制 / 选择 / 拖拽）照常执行
    e.stopImmediatePropagation();
  }
  function cuInstallIntercept() {
    if (__cuInterceptOn) return;
    CU_BLOCK_TYPES.forEach(function (type) {
      document.addEventListener(type, cuOnBlocked, true);
    });
    __cuInterceptOn = true;
  }
  function cuUninstallIntercept() {
    CU_BLOCK_TYPES.forEach(function (type) {
      document.removeEventListener(type, cuOnBlocked, true);
    });
    __cuInterceptOn = false;
  }

  function cuCopyText(text) {
    try {
      if (navigator.clipboard && navigator.clipboard.writeText) {
        return navigator.clipboard.writeText(text).then(function () { return true; })
          .catch(function () { return cuFallbackCopy(text); });
      }
    } catch (e) { /* 走回退 */ }
    return Promise.resolve(cuFallbackCopy(text));
  }
  function cuFallbackCopy(text) {
    try {
      const ta = document.createElement('textarea');
      ta.value = text;
      ta.style.cssText = 'position:fixed;left:-9999px;top:-9999px;opacity:0;';
      (document.body || document.documentElement).appendChild(ta);
      ta.focus();
      ta.select();
      const ok = document.execCommand('copy');
      if (ta.parentNode) ta.parentNode.removeChild(ta);
      return ok;
    } catch (e) { return false; }
  }
  function cuInstallAutoCopy() {
    if (__cuAutoOn) return;
    let lastText = '';
    let timer = null;
    function doCopy() {
      if (!cfg || !cfg.copyUnlock) return;
      const sel = window.getSelection();
      if (!sel || sel.isCollapsed) return;
      const text = sel.toString();
      if (!text || !text.trim()) return;
      const node = sel.anchorNode;
      if (node && node.nodeType === 3) {
        const parent = node.parentElement;
        if (parent && typeof parent.closest === 'function' && parent.closest(CU_SKIP)) return;
      }
      if (text === lastText) return;
      lastText = text;
      cuCopyText(text).then(function (ok) {
        cuToast(ok ? ('已复制 ' + text.length + ' 字') : '复制失败，请手动 Ctrl+C');
      });
    }
    function schedule() {
      clearTimeout(timer);
      timer = setTimeout(doCopy, 320);
    }
    __cuMouseup = schedule;
    __cuKeyup = function (e) {
      const k = e.key;
      if (e.shiftKey || (k && (k.indexOf('Arrow') === 0 ||
        k === 'Home' || k === 'End' || k === 'PageUp' || k === 'PageDown'))) {
        schedule();
      }
    };
    document.addEventListener('mouseup', __cuMouseup, true);
    document.addEventListener('keyup', __cuKeyup, true);
    __cuAutoOn = true;
  }
  function cuUninstallAutoCopy() {
    if (__cuMouseup) document.removeEventListener('mouseup', __cuMouseup, true);
    if (__cuKeyup) document.removeEventListener('keyup', __cuKeyup, true);
    __cuMouseup = __cuKeyup = null;
    __cuAutoOn = false;
  }

  function applyCopyUnlock() {
    if (!cfg) return;
    if (cfg.copyUnlock) {
      cuInjectSelectStyle();
      cuInstallIntercept();
      cuInstallAutoCopy();
    } else {
      cuRemoveSelectStyle();
      cuUninstallIntercept();
      cuUninstallAutoCopy();
    }
  }

  // ---------------- 启动 ----------------
  function reportDiag() {
    if (!cfg) return;
    post(
      "/seg/" + cfg.token + "/diag",
      JSON.stringify({
        installed: true,
        unknownBytes: __mdtUnknownBytes,
        registered: __mdtRegistered,
        inFrame: window !== window.top,
        pageUrl: location.href,
        title: document.title,
      })
    );
  }

  function boot() {
    discover();
    patchMSE();
    patchFetch();
    patchXHR();
    installRateEvent();
    setInterval(function () {
      refreshConfig();
      applyRateToAll();
    }, CONFIG_TTL);
    setInterval(pollMediaElements, 1500);
    setInterval(reportDiag, 3000);
    reportDiag();
    // 捕获 document 上后续出现的 video（已由 src setter + 轮询覆盖）
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot);
  } else {
    boot();
  }

  window.__mdtHookInfo = function () {
    return {
      installed: true,
      hasCfg: !!cfg,
      cfg: cfg,
      registeredTracks: __mdtRegistered,
      unknownBytes: __mdtUnknownBytes,
    };
  };
})();
