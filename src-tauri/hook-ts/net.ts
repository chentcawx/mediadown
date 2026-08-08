// 直链媒体上报面：劫持 fetch / XHR 响应头识别媒体，轮询并监听
// <video>/<audio> 的 src，把直链 URL 上报给本地服务器。

import { rt } from "./runtime";
import { mediaTypeOf, looksMediaUrl } from "./util";
import { reportMedia } from "./api";

export function patchFetch(): void {
  const orig = window.fetch;
  if (!orig || (orig as any).__mdtPatched) return;
  const wrapper = function (this: unknown, input: RequestInfo | URL, init?: RequestInit) {
    const req = new Request(input, init);
    if (req.method === "GET") {
      return orig.apply(this, arguments as any).then(function (resp: Response) {
        try {
          const ctype = resp.headers ? resp.headers.get("content-type") : null;
          const mtype = mediaTypeOf(ctype, req.url);
          if (mtype && resp.ok && !req.url.includes("127.0.0.1")) {
            // 优先读 Content-Length，避免为报 size 而全量读取响应体
            // （大视频会把数 GB 读进内存导致页面卡死/OOM）。
            const len = resp.headers ? resp.headers.get("content-length") : null;
            if (len !== null && len !== "" && /^\d+$/.test(len)) {
              reportMedia(req.url, mtype, { size: parseInt(len, 10) });
            } else {
              // 无长度头（如 chunked 流）无法安全获知大小：shape 0 的元数据
              reportMedia(req.url, mtype, { size: 0 });
            }
          }
        } catch (e) {}
        return resp;
      });
    }
    return orig.apply(this, arguments as any);
  };
  (wrapper as any).__mdtPatched = true;
  try {
    Object.defineProperty(window, "fetch", { value: wrapper, writable: true, configurable: true });
  } catch (e) {}
}

export function patchXHR(): void {
  const origOpen = XMLHttpRequest.prototype.open;
  const origSend = XMLHttpRequest.prototype.send;
  if ((origOpen as any).__mdtPatched) return;

  XMLHttpRequest.prototype.open = function (method: string, url: string | URL) {
    (this as any).__mdtUrl = url;
    return origOpen.apply(this, arguments as any);
  };
  (XMLHttpRequest.prototype.open as any).__mdtPatched = true;

  XMLHttpRequest.prototype.send = function () {
    const xhr = this;
    const origOnLoad = xhr.onload;
    xhr.onload = function (ev: ProgressEvent) {
      try {
        const mdtUrl = (xhr as any).__mdtUrl;
        if (mdtUrl && xhr.status >= 200 && xhr.status < 300) {
          const ctype = xhr.getResponseHeader && xhr.getResponseHeader("content-type");
          const mtype = mediaTypeOf(ctype, String(mdtUrl));
          if (mtype && !String(mdtUrl).includes("127.0.0.1")) {
            let size = 0;
            if (typeof xhr.response === "string") size = xhr.response.length;
            else if (xhr.response) {
              try {
                size = (xhr.response as any).byteLength || (xhr.response as any).size || 0;
              } catch (e) {}
            }
            reportMedia(String(mdtUrl), mtype, { size: size });
          }
        }
      } catch (e) {}
      if (typeof origOnLoad === "function") return origOnLoad.call(this, ev);
    };
    return origSend.apply(this, arguments as any);
  };
  (XMLHttpRequest.prototype.send as any).__mdtPatched = true;
}

// ---------------- 轮询 video/audio src ----------------
export function pollMediaElements(): void {
  if (!rt.cfg || !rt.cfg.enabled) return;
  const els = document.querySelectorAll("video,audio");
  for (let i = 0; i < els.length; i++) {
    const el = els[i] as HTMLMediaElement;
    const src = el.currentSrc || el.src;
    if (src && looksMediaUrl(src)) {
      reportMedia(src, el.tagName === "AUDIO" ? "audio" : "video", { size: 0 });
    }
  }
  // 新元素监听
  for (let i = 0; i < els.length; i++) {
    const el = els[i] as HTMLMediaElement;
    if (!(el as any).__mdtSrcWatched) {
      (el as any).__mdtSrcWatched = true;
      try {
        const desc = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, "src");
        if (desc && desc.set) {
          const origSet = desc.set as any;
          Object.defineProperty(el, "src", {
            get: function () {
              return origSet.get ? origSet.get.call(this) : this.getAttribute("src");
            },
            set: function (v: string | null) {
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