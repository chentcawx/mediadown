// 倍速控制：轮询强制 playbackRate + 监听 Rust 广播的 md-rate 事件即时生效。

import { rt } from "./runtime";

export function applyRateToAll(): void {
  if (!rt.cfg || !rt.cfg.enabled) return;
  const els = document.querySelectorAll("video");
  for (let i = 0; i < els.length; i++) {
    const el = els[i];
    if (Math.abs(el.playbackRate - rt.cfg.rate) > 0.01 && el.readyState > 0) {
      try {
        el.playbackRate = rt.cfg.rate;
      } catch (e) {}
    }
  }
}

// 控制台点击倍速后，Rust 会立即广播 md-rate 事件，使变速即时生效
// （3s 轮询作为兜底，本监听仅用于消除延迟、让效果立即可见）。
export function installRateEvent(): void {
  try {
    const T = (window as any).__TAURI__;
    if (T && T.event && typeof T.event.listen === "function") {
      T.event.listen("md-rate", function (e: { payload?: unknown }) {
        try {
          if (rt.cfg) {
            rt.cfg.rate = (typeof e.payload === "number") ? e.payload : rt.cfg.rate;
            applyRateToAll();
          }
        } catch (err) {}
      }).catch(function () {});
    }
  } catch (e) {}
}