//! The Forge (assets/forge.js, on three.js, both vendored): the 3D scene shown while the
//! model is on its way. Loaded only when it is shown; without WebGL nothing is loaded and the
//! page shows the bar and the numbers alone.

use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
export function forge_mount(el) {
  if (!el || el.__forgeHandle || el.__forgeLoading) return;
  el.__forgeLoading = true;
  import('/forge.js').then(m => {
    el.__forgeLoading = false;
    if (!el.isConnected) return;
    el.__forgeHandle = m.mount(el);
    if (!el.__forgeHandle) el.dataset.webgl = 'no';
  }, e => { el.__forgeLoading = false; el.dataset.webgl = 'no'; if (window.__tell) window.__tell('forge: ' + e); });
}
export function forge_update(el, stage, percent, speed) {
  if (!el) return;
  el.__forge = { stage, percent, speed };
  if (el.__forgeHandle) el.__forgeHandle.update();
}
export function forge_dispose(el) {
  if (el && el.__forgeHandle) { el.__forgeHandle.dispose(); el.__forgeHandle = null; }
}
// The big percent: counts up to each new value in 600 ms, never backwards.
export function count_up(el, to, decimals, suffix) {
  if (!el) return;
  const from = parseFloat(el.dataset.v || '0') || 0;
  if (to < from) to = from;
  el.dataset.v = String(to);
  const show = v => { el.textContent = v.toFixed(decimals) + suffix; };
  const M = globalThis.Motion;
  if (!M || from === to || matchMedia('(prefers-reduced-motion: reduce)').matches) { show(to); return; }
  M.animate(from, to, { duration: 0.6, ease: [0.16, 1, 0.3, 1], onUpdate: show });
}
"#)]
extern "C" {
    pub fn forge_mount(el: &web_sys::Element);
    pub fn forge_update(el: &web_sys::Element, stage: &str, percent: f64, speed: f64);
    pub fn forge_dispose(el: &web_sys::Element);
    pub fn count_up(el: &web_sys::Element, to: f64, decimals: u32, suffix: &str);
}
