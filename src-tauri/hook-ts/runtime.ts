// 共享可变运行时状态（模块化后不能直接 export let 变量再赋新值，
// 因此所有跨模块共享的可变状态收敛到 rt 单例对象上，语义与原 IIFE 的
// let 顶层变量完全一致）。

export interface Cfg {
  port: number;
  token: string;
  enabled: boolean;
  auto: boolean;
  copyUnlock: boolean;
  rate: number;
}

export const rt = {
  cfg: null as Cfg | null,       // {port, token, enabled, auto, copyUnlock, rate}
  registered: 0,                 // 已注册的轨道数（诊断用）
  discoverActive: false,
};

export const CONFIG_TTL = 3000;

// 所有上传都使用保存的原生 fetch 引用，避免递归劫持。
export const nativeFetch = (window as any).fetch.bind(window) as typeof fetch;