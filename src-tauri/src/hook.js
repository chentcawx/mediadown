"use strict";
(() => {
  // src-tauri/hook-ts/runtime.ts
  var rt = {
    cfg: null,
    // {port, token, enabled, auto, copyUnlock, rate}
    registered: 0,
    // 已注册的轨道数（诊断用）
    discoverActive: false
  };
  var CONFIG_TTL = 3e3;
  var nativeFetch = window.fetch.bind(window);

  // src-tauri/hook-ts/rate.ts
  function applyRateToAll() {
    if (!rt.cfg || !rt.cfg.enabled) return;
    const els = document.querySelectorAll("video");
    for (let i = 0; i < els.length; i++) {
      const el = els[i];
      if (Math.abs(el.playbackRate - rt.cfg.rate) > 0.01 && el.readyState > 0) {
        try {
          el.playbackRate = rt.cfg.rate;
        } catch (e) {
        }
      }
    }
  }
  function installRateEvent() {
    try {
      const T = window.__TAURI__;
      if (T && T.event && typeof T.event.listen === "function") {
        T.event.listen("md-rate", function(e) {
          try {
            if (rt.cfg) {
              rt.cfg.rate = typeof e.payload === "number" ? e.payload : rt.cfg.rate;
              applyRateToAll();
            }
          } catch (err) {
          }
        }).catch(function() {
        });
      }
    } catch (e) {
    }
  }

  // src-tauri/hook-ts/copy.ts
  var __cuSelectEl = null;
  var __cuInterceptOn = false;
  var __cuAutoOn = false;
  var __cuMouseup = null;
  var __cuKeyup = null;
  var CU_SKIP = 'input, textarea, [contenteditable], [contenteditable=""], [contenteditable="true"]';
  var CU_BLOCK_TYPES = ["contextmenu", "copy", "selectstart", "dragstart", "beforecopy"];
  function cuInjectSelectStyle() {
    if (__cuSelectEl) return;
    const css = "*{ -webkit-user-select:text !important; -moz-user-select:text !important; -ms-user-select:text !important; user-select:text !important; -webkit-touch-callout:default !important; }";
    const el = document.createElement("style");
    el.setAttribute("data-mdt-copy", "");
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
      let t = document.getElementById("mdt-copy-toast");
      if (!t) {
        t = document.createElement("div");
        t.id = "mdt-copy-toast";
        t.style.cssText = 'position:fixed;left:50%;bottom:24px;transform:translateX(-50%);z-index:2147483647;background:rgba(17,24,39,.92);color:#fff;font:13px/1.4 -apple-system,"Segoe UI",Roboto,sans-serif;padding:8px 14px;border-radius:8px;pointer-events:none;box-shadow:0 4px 16px rgba(0,0,0,.3);opacity:0;transition:opacity .18s;max-width:80vw;';
        (document.body || document.documentElement).appendChild(t);
      }
      t.textContent = msg;
      t.style.opacity = "1";
      clearTimeout(t.__timer);
      t.__timer = setTimeout(function() {
        t.style.opacity = "0";
      }, 1400);
    } catch (e) {
    }
  }
  function cuOnBlocked(e) {
    if (!rt.cfg || !rt.cfg.copyUnlock) return;
    const t = e.target;
    const isEditable = t instanceof HTMLElement && typeof t.closest === "function" && !!t.closest(CU_SKIP);
    if (isEditable) return;
    e.stopImmediatePropagation();
  }
  function cuInstallIntercept() {
    if (__cuInterceptOn) return;
    CU_BLOCK_TYPES.forEach(function(type) {
      document.addEventListener(type, cuOnBlocked, true);
    });
    __cuInterceptOn = true;
  }
  function cuUninstallIntercept() {
    CU_BLOCK_TYPES.forEach(function(type) {
      document.removeEventListener(type, cuOnBlocked, true);
    });
    __cuInterceptOn = false;
  }
  function cuCopyText(text) {
    try {
      if (navigator.clipboard && navigator.clipboard.writeText) {
        return navigator.clipboard.writeText(text).then(function() {
          return true;
        }).catch(function() {
          return cuFallbackCopy(text);
        });
      }
    } catch (e) {
    }
    return Promise.resolve(cuFallbackCopy(text));
  }
  function cuFallbackCopy(text) {
    try {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.cssText = "position:fixed;left:-9999px;top:-9999px;opacity:0;";
      (document.body || document.documentElement).appendChild(ta);
      ta.focus();
      ta.select();
      const ok = document.execCommand("copy");
      if (ta.parentNode) ta.parentNode.removeChild(ta);
      return ok;
    } catch (e) {
      return false;
    }
  }
  function cuInstallAutoCopy() {
    if (__cuAutoOn) return;
    let lastText = "";
    let timer = null;
    function doCopy() {
      if (!rt.cfg || !rt.cfg.copyUnlock) return;
      const sel = window.getSelection();
      if (!sel || sel.isCollapsed) return;
      const text = sel.toString();
      if (!text || !text.trim()) return;
      const node = sel.anchorNode;
      if (node && node.nodeType === 3) {
        const parent = node.parentElement;
        if (parent && typeof parent.closest === "function" && parent.closest(CU_SKIP)) return;
      }
      if (text === lastText) return;
      lastText = text;
      cuCopyText(text).then(function(ok) {
        cuToast(ok ? "\u5DF2\u590D\u5236 " + text.length + " \u5B57" : "\u590D\u5236\u5931\u8D25\uFF0C\u8BF7\u624B\u52A8 Ctrl+C");
      });
    }
    function schedule() {
      clearTimeout(timer);
      timer = window.setTimeout(doCopy, 320);
    }
    __cuMouseup = schedule;
    __cuKeyup = function(e) {
      const k = e.key;
      if (e.shiftKey || k && (k.indexOf("Arrow") === 0 || k === "Home" || k === "End" || k === "PageUp" || k === "PageDown")) {
        schedule();
      }
    };
    document.addEventListener("mouseup", __cuMouseup, true);
    document.addEventListener("keyup", __cuKeyup, true);
    __cuAutoOn = true;
  }
  function cuUninstallAutoCopy() {
    if (__cuMouseup) document.removeEventListener("mouseup", __cuMouseup, true);
    if (__cuKeyup) document.removeEventListener("keyup", __cuKeyup, true);
    __cuMouseup = __cuKeyup = null;
    __cuAutoOn = false;
  }
  function applyCopyUnlock() {
    if (!rt.cfg) return;
    if (rt.cfg.copyUnlock) {
      cuInjectSelectStyle();
      cuInstallIntercept();
      cuInstallAutoCopy();
    } else {
      cuRemoveSelectStyle();
      cuUninstallIntercept();
      cuUninstallAutoCopy();
    }
  }

  // src-tauri/hook-ts/api.ts
  function post(path, body, cb) {
    if (!rt.cfg) {
      if (cb) cb(new Error("no cfg"));
      return;
    }
    nativeFetch("http://127.0.0.1:" + rt.cfg.port + path, {
      method: "POST",
      headers: { "Content-Type": "application/octet-stream" },
      body,
      cache: "no-store"
    }).then(function(r) {
      if (!r.ok) return Promise.reject(new Error("HTTP " + r.status));
      return r.text();
    }).then(function(t) {
      if (cb) cb(null, t);
    }).catch(function(e) {
      if (cb) cb(e instanceof Error ? e : new Error(String(e)));
    });
  }
  function postChunkWithRetry(entry, data, attempt) {
    return new Promise(function(resolve) {
      if (!rt.cfg || !rt.cfg.enabled) {
        resolve();
        return;
      }
      post("/seg/" + rt.cfg.token + "/" + entry.trackId + "/chunk", data, function(err) {
        if (!err) {
          resolve();
          return;
        }
        if (attempt >= 5) {
          try {
            console.warn("[mdt] chunk upload dropped after retries:", String(err));
          } catch (e) {
          }
          resolve();
          return;
        }
        setTimeout(function() {
          postChunkWithRetry(entry, data, attempt + 1).then(resolve, resolve);
        }, 200 * (attempt + 1));
      });
    });
  }
  function discover() {
    if (rt.cfg || rt.discoverActive) return;
    rt.discoverActive = true;
    function scan(i) {
      if (rt.cfg) {
        rt.discoverActive = false;
        return;
      }
      if (i >= 10) {
        setTimeout(scan, 3e3, 0);
        return;
      }
      const port = 49321 + i;
      nativeFetch("http://127.0.0.1:" + port + "/cfg", { cache: "no-store" }).then(function(r) {
        return r.ok ? r.json() : null;
      }).then(function(c) {
        if (c && c.app === "mediadown") {
          rt.cfg = c;
          rt.discoverActive = false;
          applyRateToAll();
          applyCopyUnlock();
        } else {
          scan(i + 1);
        }
      }).catch(function() {
        scan(i + 1);
      });
    }
    scan(0);
  }
  function refreshConfig() {
    if (!rt.cfg) return;
    const port = rt.cfg.port;
    nativeFetch("http://127.0.0.1:" + port + "/cfg", { cache: "no-store" }).then(function(r) {
      return r.ok ? r.json() : null;
    }).then(function(c) {
      if (c && rt.cfg) {
        rt.cfg.port = c.port;
        rt.cfg.token = c.token;
        rt.cfg.enabled = c.enabled;
        rt.cfg.auto = c.auto;
        rt.cfg.copyUnlock = c.copyUnlock;
        rt.cfg.rate = c.rate;
        applyCopyUnlock();
      }
    }).catch(function() {
      rt.cfg = null;
      discover();
    });
  }
  var REPORTED_LIMIT = 500;
  var reported = /* @__PURE__ */ new Set();
  function reportMedia(url, type, extra) {
    if (!rt.cfg || !rt.cfg.enabled || !url || url.indexOf("blob:") === 0 || url.indexOf("data:") === 0) return;
    if (url.indexOf("127.0.0.1") >= 0 || url.indexOf("localhost") >= 0) return;
    const key = type + "|" + url;
    if (reported.has(key)) return;
    if (reported.size >= REPORTED_LIMIT) {
      const evict = reported.values().next().value;
      if (evict !== void 0) reported.delete(evict);
    }
    reported.add(key);
    const body = JSON.stringify(
      Object.assign(
        {
          url,
          type,
          pageUrl: location.href,
          title: document.title
        },
        extra || {}
      )
    );
    post("/seg/" + rt.cfg.token + "/report", body);
  }
  function reportDiag() {
    if (!rt.cfg) return;
    post(
      "/seg/" + rt.cfg.token + "/diag",
      JSON.stringify({
        installed: true,
        registered: rt.registered,
        inFrame: window !== window.top,
        pageUrl: location.href,
        title: document.title
      })
    );
  }

  // src-tauri/hook-ts/util.ts
  function detectFamily(mime) {
    if (!mime) return { family: "mp4", ext: "mp4" };
    const m = mime.toLowerCase();
    const isAudio = m.indexOf("audio/") === 0;
    if (m.indexOf("webm") >= 0) return { family: "webm", ext: "webm" };
    if (m.indexOf("mp2t") >= 0 || m.indexOf("mpeg-ts") >= 0 || m.indexOf("mpegts") >= 0) return { family: "ts", ext: "ts" };
    if (m.indexOf("x-flv") >= 0 || m.indexOf("flv") >= 0) return { family: "flv", ext: "flv" };
    if (m.indexOf("mp4") >= 0 || m.indexOf("aac") >= 0 || m.indexOf("h264") >= 0 || m.indexOf("h265") >= 0 || m.indexOf("avc1") >= 0 || m.indexOf("hev1") >= 0) {
      return isAudio ? { family: "mp4", ext: "m4a" } : { family: "mp4", ext: "mp4" };
    }
    if (m.indexOf("vtt") >= 0 || m.indexOf("subrip") >= 0) return { family: "text", ext: "vtt" };
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
  function preCopy(buf) {
    try {
      if (buf instanceof ArrayBuffer) return new Uint8Array(buf.slice(0));
      if (ArrayBuffer.isView(buf)) {
        const ab = buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength);
        return new Uint8Array(ab);
      }
    } catch (e) {
    }
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
      const hasMoov = findBytes(u8, [109, 111, 111, 118]) >= 0;
      return { kind: "mp4-init", hasMoov };
    }
    if (fourcc === "moof") return { kind: "mp4-seg" };
    if (u8[0] === 26 && u8[1] === 69 && u8[2] === 223 && u8[3] === 163) {
      return { kind: "webm" };
    }
    if (u8[0] === 70 && u8[1] === 76 && u8[2] === 86 && u8[3] === 1) {
      return { kind: "flv" };
    }
    if (u8[0] === 71) {
      let looksTs = true;
      const step = 188;
      if (u8.length >= step * 2) {
        if (u8[step] !== 71 && u8[step * 2] !== 71) looksTs = false;
      } else if (u8.length >= step) {
        if (u8[step] !== 71) looksTs = false;
      }
      if (looksTs) return { kind: "ts" };
    }
    return { kind: "unknown" };
  }
  var MEDIA_EXT = /\.(mp4|m4v|m4a|webm|mkv|mov|flv|ts|m3u8|mp3|aac|ogg|ogv|oga|wav|opus)(\?|#|$)/i;
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

  // src-tauri/hook-ts/mse.ts
  var sbMap = /* @__PURE__ */ new WeakMap();
  var msEntries = [];
  var msSeq = 0;
  var msSessionMap = /* @__PURE__ */ new WeakMap();
  function sessionOf(ms) {
    let s = msSessionMap.get(ms);
    if (s === void 0) {
      s = ++msSeq;
      msSessionMap.set(ms, s);
    }
    return s;
  }
  function upload(entry, u8) {
    if (!rt.cfg || !rt.cfg.enabled) return;
    const data = u8.slice(0);
    entry.sendQ = entry.sendQ.then(function() {
      return postChunkWithRetry(entry, data, 0);
    });
  }
  function patchMSE() {
    if (typeof MediaSource === "undefined") return;
    const MS = MediaSource;
    const nativeAddSB = MS.prototype.addSourceBuffer;
    if (!nativeAddSB || MS.prototype.__mdtPatched) return;
    MS.prototype.__mdtPatched = true;
    MS.prototype.addSourceBuffer = function(mimeType) {
      const sb = nativeAddSB.call(this, mimeType);
      const det = detectFamily(mimeType);
      const entry = {
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
        session: sessionOf(this)
      };
      sbMap.set(sb, entry);
      msEntries.push(entry);
      wrapSB(sb, entry);
      return sb;
    };
    const nativeEOS = MS.prototype.endOfStream;
    MS.prototype.endOfStream = function() {
      const r = nativeEOS.apply(this, arguments);
      if (rt.cfg) {
        for (let k = 0; k < msEntries.length; k++) {
          const entry = msEntries[k];
          if (entry.ms === this && !entry.ended) {
            entry.ended = true;
            if (entry.trackId != null) {
              post("/seg/" + rt.cfg.token + "/" + entry.trackId + "/end", new Uint8Array(0));
            }
          }
        }
        const self = this;
        msEntries = msEntries.filter(function(e) {
          return e.ms !== self;
        });
      }
      return r;
    };
  }
  function wrapSB(sb, entry) {
    const nativeAppend = sb.appendBuffer ? sb.appendBuffer.bind(sb) : null;
    const nativeAppendStream = sb.appendStream ? sb.appendStream.bind(sb) : null;
    const nativeRemove = sb.remove ? sb.remove.bind(sb) : null;
    if (nativeAppend) {
      sb.appendBuffer = function(buf) {
        const copy = preCopy(buf);
        const r = nativeAppend.call(this, buf);
        try {
          if (copy) handleAppend(entry, copy);
          else handleAppend(entry, buf);
        } catch (e) {
        }
        return r;
      };
    }
    if (nativeAppendStream) {
      sb.appendStream = function(stream) {
        const r = nativeAppendStream.apply(this, arguments);
        try {
          if (stream && typeof stream.getReader === "function") {
            const reader = stream.getReader();
            const pump = function() {
              return reader.read().then(function(res) {
                if (res.done) return;
                handleAppend(entry, res.value || new Uint8Array(0));
                return pump();
              });
            };
            pump().catch(function() {
            });
          }
        } catch (e) {
        }
        return r;
      };
    }
    if (nativeRemove) {
      sb.remove = function() {
        return nativeRemove.apply(this, arguments);
      };
    }
  }
  function handleAppend(entry, buf) {
    if (!rt.cfg || !rt.cfg.enabled) return;
    const u8 = toU8(buf);
    if (!u8) {
      if (typeof Blob !== "undefined" && buf instanceof Blob) {
        buf.arrayBuffer().then(function(ab) {
          handleAppend(entry, new Uint8Array(ab));
        }).catch(function() {
        });
      }
      return;
    }
    if (!entry.registered) {
      const cls = classify(u8);
      if (cls.kind === "mp4-init") {
        entry.hadInit = true;
        entry.initSig = sigOf(u8);
      }
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
    if (entry.trackId == null) {
      if (entry.pendingBytes < 64 * 1024 * 1024) {
        entry.pending.push(u8.slice(0));
        entry.pendingBytes += u8.byteLength;
      }
      return;
    }
    if (entry.hadInit && classify(u8).kind === "mp4-init" && findBytes(u8, [109, 111, 111, 102]) < 0) {
      if (entry.initSig && sigOf(u8) === entry.initSig) return;
    }
    upload(entry, u8);
  }
  function registerEntry(entry, why, forceFamily, forceExt) {
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
      why,
      session: entry.session,
      pageUrl: location.href,
      title: document.title
    });
    post("/seg/" + rt.cfg.token + "/register", body, function(err, text) {
      if (err) {
        rt.registered--;
        entry.registered = false;
        return;
      }
      try {
        const info = JSON.parse(text);
        entry.trackId = info.id;
      } catch (e) {
        try {
          console.warn("[mdt] track register parse failed:", String(e));
        } catch (e2) {
        }
        rt.registered--;
        entry.registered = false;
        return;
      }
      if (entry.ended && rt.cfg) {
        post("/seg/" + rt.cfg.token + "/" + entry.trackId + "/end", new Uint8Array(0));
      }
      const pend = entry.pending;
      entry.pending = [];
      entry.pendingBytes = 0;
      for (let i = 0; i < pend.length; i++) upload(entry, pend[i]);
    });
  }

  // src-tauri/hook-ts/net.ts
  function patchFetch() {
    const orig = window.fetch;
    if (!orig || orig.__mdtPatched) return;
    const wrapper = function(input, init2) {
      const req = new Request(input, init2);
      if (req.method === "GET") {
        return orig.apply(this, arguments).then(function(resp) {
          try {
            const ctype = resp.headers ? resp.headers.get("content-type") : null;
            const mtype = mediaTypeOf(ctype, req.url);
            if (mtype && resp.ok && !req.url.includes("127.0.0.1")) {
              const len = resp.headers ? resp.headers.get("content-length") : null;
              if (len !== null && len !== "" && /^\d+$/.test(len)) {
                reportMedia(req.url, mtype, { size: parseInt(len, 10) });
              } else {
                reportMedia(req.url, mtype, { size: 0 });
              }
            }
          } catch (e) {
          }
          return resp;
        });
      }
      return orig.apply(this, arguments);
    };
    wrapper.__mdtPatched = true;
    try {
      Object.defineProperty(window, "fetch", { value: wrapper, writable: true, configurable: true });
    } catch (e) {
    }
  }
  function patchXHR() {
    const origOpen = XMLHttpRequest.prototype.open;
    const origSend = XMLHttpRequest.prototype.send;
    if (origOpen.__mdtPatched) return;
    XMLHttpRequest.prototype.open = function(method, url) {
      this.__mdtUrl = url;
      return origOpen.apply(this, arguments);
    };
    XMLHttpRequest.prototype.open.__mdtPatched = true;
    XMLHttpRequest.prototype.send = function() {
      const xhr = this;
      const origOnLoad = xhr.onload;
      xhr.onload = function(ev) {
        try {
          const mdtUrl = xhr.__mdtUrl;
          if (mdtUrl && xhr.status >= 200 && xhr.status < 300) {
            const ctype = xhr.getResponseHeader && xhr.getResponseHeader("content-type");
            const mtype = mediaTypeOf(ctype, String(mdtUrl));
            if (mtype && !String(mdtUrl).includes("127.0.0.1")) {
              let size = 0;
              if (typeof xhr.response === "string") size = xhr.response.length;
              else if (xhr.response) {
                try {
                  size = xhr.response.byteLength || xhr.response.size || 0;
                } catch (e) {
                }
              }
              reportMedia(String(mdtUrl), mtype, { size });
            }
          }
        } catch (e) {
        }
        if (typeof origOnLoad === "function") return origOnLoad.call(this, ev);
      };
      return origSend.apply(this, arguments);
    };
    XMLHttpRequest.prototype.send.__mdtPatched = true;
  }
  function pollMediaElements() {
    if (!rt.cfg || !rt.cfg.enabled) return;
    const els = document.querySelectorAll("video,audio");
    for (let i = 0; i < els.length; i++) {
      const el = els[i];
      const src = el.currentSrc || el.src;
      if (src && looksMediaUrl(src)) {
        reportMedia(src, el.tagName === "AUDIO" ? "audio" : "video", { size: 0 });
      }
    }
    for (let i = 0; i < els.length; i++) {
      const el = els[i];
      if (!el.__mdtSrcWatched) {
        el.__mdtSrcWatched = true;
        try {
          const desc = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, "src");
          if (desc && desc.set) {
            const origSet = desc.set;
            Object.defineProperty(el, "src", {
              get: function() {
                return origSet.get ? origSet.get.call(this) : this.getAttribute("src");
              },
              set: function(v) {
                origSet.call(this, v);
                if (typeof v === "string" && looksMediaUrl(v)) {
                  reportMedia(v, this.tagName === "AUDIO" ? "audio" : "video", { size: 0 });
                }
              }
            });
          }
        } catch (e) {
        }
      }
    }
  }

  // src-tauri/hook-ts/index.ts
  function init() {
    if (window.__mdtHookInstalled) return;
    window.__mdtHookInstalled = true;
    function boot() {
      discover();
      patchMSE();
      patchFetch();
      patchXHR();
      installRateEvent();
      setInterval(function() {
        refreshConfig();
        applyRateToAll();
      }, CONFIG_TTL);
      setInterval(pollMediaElements, 1500);
      setInterval(reportDiag, 3e3);
      reportDiag();
    }
    if (document.readyState === "loading") {
      document.addEventListener("DOMContentLoaded", boot);
    } else {
      boot();
    }
    window.__mdtHookInfo = function() {
      return {
        installed: true,
        hasCfg: !!rt.cfg,
        cfg: rt.cfg,
        registeredTracks: rt.registered
      };
    };
  }
  init();
})();
