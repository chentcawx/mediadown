// 网络 API：自发现端点 / 配置刷新 / 二进制 POST（含串行重试）/ 直链上报 / 诊断上报。

import { rt, nativeFetch } from "./runtime";
import { applyRateToAll } from "./rate";
import { applyCopyUnlock } from "./copy";

// 上传：使用保存的原生 fetch，POST 二进制 body 到本地服务器。
export function post(path: string, body: BodyInit | ArrayBufferView | null, cb?: (err: Error | null, text?: string) => void): void {
  if (!rt.cfg) {
    if (cb) cb(new Error("no cfg"));
    return;
  }
  nativeFetch("http://127.0.0.1:" + rt.cfg.port + path, {
    method: "POST",
    headers: { "Content-Type": "application/octet-stream" },
    body: body as unknown as BodyInit,
    cache: "no-store",
  })
    .then(function (r) {
      // 非 2xx（413 超限 / 403 无权限 / 500 服务器错）一律视为失败：
      // 否则分片被静默丢弃且不触发重试，整轨数据残缺。
      if (!r.ok) return Promise.reject(new Error("HTTP " + r.status));
      return r.text();
    })
    .then(function (t) {
      if (cb) cb(null, t);
    })
    .catch(function (e) {
      if (cb) cb(e instanceof Error ? e : new Error(String(e)));
    });
}

// 分片上传，失败自动重试（最多 5 次，指数退避）；仍失败则放弃该分片
// （极端网络错误），避免无限阻塞后续分片造成更大空缺。
export function postChunkWithRetry(entry: { trackId: number | null }, data: Uint8Array, attempt: number): Promise<void> {
  return new Promise<void>(function (resolve) {
    if (!rt.cfg || !rt.cfg.enabled) { resolve(); return; }
    post("/seg/" + rt.cfg.token + "/" + entry.trackId + "/chunk", data, function (err) {
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

export interface RemoteCfg {
  app?: string;
  port: number;
  token: string;
  enabled: boolean;
  auto: boolean;
  copyUnlock: boolean;
  rate: number;
}

// discover: scan 127.0.0.1:49321~49330 once; if no hit, retry every 3s until cfg is set.
// (startup race / transient server-not-ready would otherwise leave hook stuck "un-injected".)
export function discover(): void {
  if (rt.cfg || rt.discoverActive) return;
  rt.discoverActive = true;
  function scan(i: number) {
    if (rt.cfg) { rt.discoverActive = false; return; }
    if (i >= 10) {
      // whole round failed -> retry after 3s (loop stays active)
      setTimeout(scan, 3000, 0);
      return;
    }
    const port = 49321 + i;
    nativeFetch("http://127.0.0.1:" + port + "/cfg", { cache: "no-store" })
      .then(function (r) {
        return r.ok ? (r.json() as Promise<RemoteCfg>) : null;
      })
      .then(function (c: RemoteCfg | null) {
        if (c && c.app === "mediadown") {
          rt.cfg = c;
          rt.discoverActive = false;
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
export function refreshConfig(): void {
  if (!rt.cfg) return;
  const port = rt.cfg.port;
  nativeFetch("http://127.0.0.1:" + port + "/cfg", { cache: "no-store" })
    .then(function (r) {
      return r.ok ? (r.json() as Promise<RemoteCfg>) : null;
    })
    .then(function (c: RemoteCfg | null) {
      if (c && rt.cfg) {
        rt.cfg.port = c.port;
        rt.cfg.token = c.token;
        rt.cfg.enabled = c.enabled;
        rt.cfg.auto = c.auto;
        rt.cfg.copyUnlock = c.copyUnlock;
        rt.cfg.rate = c.rate;
        applyCopyUnlock();
      }
    })
    .catch(function () {
      rt.cfg = null; // 服务器重启后重新发现
      discover();
    });
}

// ---------------- 直链媒体上报 ----------------
// 只做短期去重（同 URL 拍平重复上报），长期去重交给服务器端（已按 url+type 去重）。
// 上限防止直播/轮播等带时间戳的新 URL 在页面生命周期内无限累积内存。
const REPORTED_LIMIT = 500;
const reported = new Set<string>();

export function reportMedia(url: string, type: string, extra: Record<string, unknown>): void {
  if (!rt.cfg || !rt.cfg.enabled || !url || url.indexOf("blob:") === 0 || url.indexOf("data:") === 0) return;
  if (url.indexOf("127.0.0.1") >= 0 || url.indexOf("localhost") >= 0) return;
  const key = type + "|" + url;
  if (reported.has(key)) return;
  if (reported.size >= REPORTED_LIMIT) {
    // 超限后从旧到新剔除一半，保持近端去重能力
    const evict = reported.values().next().value;
    if (evict !== undefined) reported.delete(evict);
  }
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
  post("/seg/" + rt.cfg.token + "/report", body);
}

export function reportDiag(): void {
  if (!rt.cfg) return;
  post(
    "/seg/" + rt.cfg.token + "/diag",
    JSON.stringify({
      installed: true,
      registered: rt.registered,
      inFrame: window !== window.top,
      pageUrl: location.href,
      title: document.title,
    })
  );
}