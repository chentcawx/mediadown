// MediaDown 捕获引擎（注入目标站点，document_start / MAIN world）
//
// 功能：
//  1. 自发现本地分片服务器（探测 http://127.0.0.1:49321~49330/cfg）
//  2. 劫持 MediaSource.addSourceBuffer / endOfStream，把 MSE 分片实时
//     POST 到本地服务器（边下边存）
//  3. 劫持 fetch / XHR + 轮询 <video>/<audio>.src，上报直链媒体
//  4. 根据控制台配置强制 playbackRate
//
// 所有上传都使用保存的原生 fetch 引用，避免递归劫持。
//
// 注意：本文件由 esbuild 从 hook-ts/ 模块打包生成，不要手工编辑。
// 源码位于 src-tauri/hook-ts/，构建命令见 package.json 的 build:hook。

import { rt, CONFIG_TTL } from "./runtime";
import { discover, refreshConfig, reportDiag } from "./api";
import { patchMSE } from "./mse";
import { patchFetch, patchXHR, pollMediaElements } from "./net";
import { applyRateToAll, installRateEvent } from "./rate";

function init(): void {
  // 防重入：脚本重复注入时直接返回（与注入脚本的整体 guard 语义一致）。
  if ((window as any).__mdtHookInstalled) return;
  (window as any).__mdtHookInstalled = true;

  function boot(): void {
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

  (window as any).__mdtHookInfo = function () {
    return {
      installed: true,
      hasCfg: !!rt.cfg,
      cfg: rt.cfg,
      registeredTracks: rt.registered,
    };
  };
}

init();