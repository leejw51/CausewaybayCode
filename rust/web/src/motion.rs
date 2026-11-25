//! Motion (motion.dev) for the page's movement. The library is a script on the page
//! (assets/motion.min.js, global `Motion`); without it the page works, without movement.

use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
const M = () => globalThis.Motion;
const ease = [0.22, 1, 0.36, 1];
export function enter(el, delay) {
  if (!M() || !el) return;
  M().animate(el, { opacity: [0, 1], y: [12, 0], filter: ['blur(4px)', 'blur(0px)'] }, { duration: 0.42, delay, ease });
}
export function enter_all(selector) {
  if (!M()) return;
  const els = document.querySelectorAll(selector);
  if (els.length) M().animate(els, { opacity: [0, 1], y: [14, 0] }, { duration: 0.45, delay: M().stagger(0.05), ease });
}
export function slide_in(el) {
  if (!M() || !el) return;
  M().animate(el, { opacity: [0, 1], x: [-14, 0] }, { duration: 0.35, ease });
}
export function attention(el) {
  if (!M() || !el) return;
  M().animate(el, { scale: [0.97, 1.01, 1], boxShadow: ['0 0 0 0 rgba(251,191,36,0.0)', '0 0 0 8px rgba(251,191,36,0.18)', '0 0 0 0 rgba(251,191,36,0.0)'] }, { duration: 0.7, ease });
}
export function count_to(el, to, decimals, suffix) {
  if (!el) return;
  const from = parseFloat(el.dataset.v || '0') || 0;
  el.dataset.v = String(to);
  const show = v => { el.textContent = v.toFixed(decimals) + suffix; };
  if (!M() || from === to) { show(to); return; }
  M().animate(from, to, { duration: 0.6, ease, onUpdate: show });
}
export function has_motion() { return !!M(); }
"#)]
extern "C" {
    /// Fade and rise into place.
    pub fn enter(el: &web_sys::Element, delay: f64);
    /// The same for every element matching a selector, one after another.
    pub fn enter_all(selector: &str);
    pub fn slide_in(el: &web_sys::Element);
    /// A pulse on something that waits for the user.
    pub fn attention(el: &web_sys::Element);
    /// Count a number up or down to its new value.
    pub fn count_to(el: &web_sys::Element, to: f64, decimals: u32, suffix: &str);
    pub fn has_motion() -> bool;
}
