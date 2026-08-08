// 解除复制限制（参考 webacc copy-unlock）
// 1) 强制文本可选（覆盖 user-select:none）
// 2) 捕获阶段 stopImmediatePropagation 阻断站点的右键/复制/选择/拖拽拦截
// 3) 选中文本后自动复制到剪贴板（带轻提示）

import { rt } from "./runtime";

let __cuSelectEl: HTMLStyleElement | null = null;
let __cuInterceptOn = false;
let __cuAutoOn = false;
let __cuMouseup: (() => void) | null = null;
let __cuKeyup: ((e: KeyboardEvent) => void) | null = null;

const CU_SKIP = 'input, textarea, [contenteditable], [contenteditable=""], [contenteditable="true"]';
const CU_BLOCK_TYPES = ['contextmenu', 'copy', 'selectstart', 'dragstart', 'beforecopy'];

function cuInjectSelectStyle(): void {
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
function cuRemoveSelectStyle(): void {
  if (__cuSelectEl && __cuSelectEl.parentNode) __cuSelectEl.parentNode.removeChild(__cuSelectEl);
  __cuSelectEl = null;
}

function cuToast(msg: string): void {
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
    clearTimeout((t as any).__timer);
    (t as any).__timer = setTimeout(function () { t.style.opacity = '0'; }, 1400);
  } catch (e) { /* 忽略 */ }
}

function cuOnBlocked(e: Event): void {
  if (!rt.cfg || !rt.cfg.copyUnlock) return;
  const t = e.target as Node | null;
  const isEditable = t instanceof HTMLElement && typeof t.closest === 'function' && !!t.closest(CU_SKIP);
  if (isEditable) return; // 可编辑区不拦截（保留正常编辑/复制）
  // 阻断站点后续所有监听器（preventDefault / returnValue 均被隔断），不调用 preventDefault
  // -> 浏览器默认行为（右键菜单 / 复制 / 选择 / 拖拽）照常执行
  e.stopImmediatePropagation();
}

function cuInstallIntercept(): void {
  if (__cuInterceptOn) return;
  CU_BLOCK_TYPES.forEach(function (type) {
    document.addEventListener(type, cuOnBlocked, true);
  });
  __cuInterceptOn = true;
}

function cuUninstallIntercept(): void {
  CU_BLOCK_TYPES.forEach(function (type) {
    document.removeEventListener(type, cuOnBlocked, true);
  });
  __cuInterceptOn = false;
}

function cuCopyText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      return navigator.clipboard.writeText(text).then(function () { return true; })
        .catch(function () { return cuFallbackCopy(text); });
    }
  } catch (e) { /* 走回退 */ }
  return Promise.resolve(cuFallbackCopy(text));
}

function cuFallbackCopy(text: string): boolean {
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

function cuInstallAutoCopy(): void {
  if (__cuAutoOn) return;
  let lastText = '';
  let timer: number | null = null;
  function doCopy() {
    if (!rt.cfg || !rt.cfg.copyUnlock) return;
    const sel = window.getSelection();
    if (!sel || sel.isCollapsed) return;
    const text = sel.toString();
    if (!text || !text.trim()) return;
    const node = sel.anchorNode;
    if (node && node.nodeType === 3) {
      const parent = (node as Text).parentElement;
      if (parent && typeof parent.closest === 'function' && parent.closest(CU_SKIP)) return;
    }
    if (text === lastText) return;
    lastText = text;
    cuCopyText(text).then(function (ok) {
      cuToast(ok ? ('已复制 ' + text.length + ' 字') : '复制失败，请手动 Ctrl+C');
    });
  }
  function schedule() {
    clearTimeout(timer as number);
    timer = window.setTimeout(doCopy, 320);
  }
  __cuMouseup = schedule;
  __cuKeyup = function (e: KeyboardEvent) {
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

function cuUninstallAutoCopy(): void {
  if (__cuMouseup) document.removeEventListener('mouseup', __cuMouseup, true);
  if (__cuKeyup) document.removeEventListener('keyup', __cuKeyup, true);
  __cuMouseup = __cuKeyup = null;
  __cuAutoOn = false;
}

export function applyCopyUnlock(): void {
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