//! Movement, by Motion (motion.dev). The library is a script on the page
//! (assets/motion.min.js, global `Motion`); without it the page works, without movement.
//!
//! Two kinds of movement, as on iOS: things that travel (a view arriving, a card appearing,
//! a number changing) ease in and out exponentially; things that answer a touch (a pressed
//! button, a selection sliding to where it was tapped) follow a spring.

use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
const M = () => globalThis.Motion;
// Exponential in/out: slow to leave, fast through the middle, slow to arrive.
const EXPO = [0.87, 0, 0.13, 1];
// Exponential out: for things that arrive.
const EXPO_OUT = [0.16, 1, 0.3, 1];
const SPRING = { type: 'spring', stiffness: 520, damping: 34, mass: 0.9 };
const SOFT = { type: 'spring', stiffness: 260, damping: 26, mass: 1 };
const calm = () => globalThis.matchMedia && matchMedia('(prefers-reduced-motion: reduce)').matches;

export function enter(el, delay) {
  if (!M() || !el || calm()) return;
  M().animate(el, { opacity: [0, 1], y: [18, 0], scale: [0.985, 1] }, { duration: 0.6, delay, ease: EXPO_OUT });
}
export function enter_all(selector) {
  if (!M() || calm()) return;
  const els = document.querySelectorAll(selector);
  if (els.length) M().animate(els, { opacity: [0, 1], y: [20, 0], scale: [0.97, 1] }, { duration: 0.65, delay: M().stagger(0.045), ease: EXPO_OUT });
}
// A view arriving, as a pushed screen does: from the side, exponential in and out.
export function view_in(el, from) {
  if (!M() || !el || calm()) return;
  M().animate(el, { opacity: [0, 1], x: [from, 0] }, { duration: 0.55, ease: EXPO });
}
export function slide_in(el) {
  if (!M() || !el || calm()) return;
  M().animate(el, { opacity: [0, 1], x: [-22, 0] }, { duration: 0.6, ease: EXPO });
}
// A sheet or card that appears where the user is looking.
export function sheet(el) {
  if (!M() || !el || calm()) return;
  M().animate(el, { opacity: [0, 1], scale: [0.92, 1], y: [14, 0] }, SOFT);
}
export function attention(el) {
  if (!M() || !el || calm()) return;
  M().animate(el, { scale: [0.96, 1.015, 1], boxShadow: ['0 0 0 0 rgba(251,191,36,0)', '0 0 0 10px rgba(251,191,36,0.20)', '0 0 0 0 rgba(251,191,36,0)'] }, { duration: 0.9, ease: EXPO });
}
export function count_to(el, to, decimals, suffix) {
  if (!el) return;
  const from = parseFloat(el.dataset.v || '0') || 0;
  el.dataset.v = String(to);
  const show = v => { el.textContent = v.toFixed(decimals) + suffix; };
  if (!M() || from === to || calm()) { show(to); return; }
  M().animate(from, to, { duration: 0.9, ease: EXPO, onUpdate: show });
}
// The selection of a segmented control slides, on a spring, to the chosen item.
export function slide_selection(container_selector) {
  const box = document.querySelector(container_selector);
  if (!box) return;
  const mark = box.querySelector('.selection'), on = box.querySelector('.on');
  if (!mark || !on) return;
  const to = { x: on.offsetLeft, y: on.offsetTop, width: on.offsetWidth, height: on.offsetHeight, opacity: 1 };
  if (!M() || calm() || !mark.dataset.placed) { Object.assign(mark.style, { transform: `translate(${to.x}px, ${to.y}px)`, width: to.width + 'px', height: to.height + 'px', opacity: 1 }); mark.dataset.placed = '1'; return; }
  M().animate(mark, to, SPRING);
}
// A change of theme or layout: the page dips and returns, exponentially, as it changes.
export function retheme(apply) {
  const root = document.querySelector('.app') || document.body;
  if (!M() || calm()) { apply(); return; }
  M().animate(root, { opacity: [1, 0.25], scale: [1, 0.985] }, { duration: 0.22, ease: EXPO }).then(() => {
    apply();
    M().animate(root, { opacity: [0.25, 1], scale: [0.985, 1] }, { duration: 0.5, ease: EXPO });
  });
}
// What is pressed gives way and springs back, wherever it is on the page.
let installed = false;
export function install() {
  if (installed || !M()) return;
  installed = true;
  const pressable = e => e.target.closest && e.target.closest('button, .row, .chip, .pressable');
  let held = null;
  document.addEventListener('pointerdown', e => {
    const el = pressable(e);
    if (!el || el.disabled || calm()) return;
    held = el;
    M().animate(el, { scale: 0.955 }, { duration: 0.14, ease: EXPO_OUT });
  }, { passive: true });
  const release = () => { if (held) { M().animate(held, { scale: 1 }, SPRING); held = null; } };
  document.addEventListener('pointerup', release, { passive: true });
  document.addEventListener('pointercancel', release, { passive: true });
  // The lights behind the glass drift, slowly.
  if (!calm()) {
    M().animate('.glow.a', { x: [0, 90, -40, 0], y: [0, 50, 110, 0], scale: [1, 1.15, 0.95, 1] }, { duration: 34, repeat: Infinity, ease: 'easeInOut' });
    M().animate('.glow.b', { x: [0, -110, 30, 0], y: [0, -60, 40, 0], scale: [1, 0.9, 1.12, 1] }, { duration: 41, repeat: Infinity, ease: 'easeInOut' });
  }
}
export function has_motion() { return !!M(); }
// Put text on the clipboard, in the tap that asked for it. The page is served with a
// self-signed certificate, where the asynchronous clipboard may be withheld: a hidden field
// and the old command stand in.
export function copy_text(text) {
  try {
    const field = document.createElement('textarea');
    field.value = text;
    field.setAttribute('readonly', '');
    field.style.cssText = 'position:fixed;left:-9999px;top:0;opacity:0';
    document.body.appendChild(field);
    field.select();
    field.setSelectionRange(0, text.length);
    const done = document.execCommand('copy');
    field.remove();
    if (done) return true;
  } catch (_) {}
  try {
    if (navigator.clipboard && navigator.clipboard.writeText) { navigator.clipboard.writeText(text).catch(() => {}); return true; }
  } catch (_) {}
  return false;
}
// Bring an element of the conversation into view.
export function go_to(id) {
  const el = document.getElementById(id);
  if (el) el.scrollIntoView({ behavior: calm() ? 'auto' : 'smooth', block: 'start' });
}
"#)]
extern "C" {
    /// Fade and rise into place.
    pub fn enter(el: &web_sys::Element, delay: f64);
    /// The same for every element matching a selector, one after another.
    pub fn enter_all(selector: &str);
    /// A view arriving from `from` pixels to the side.
    pub fn view_in(el: &web_sys::Element, from: f64);
    pub fn slide_in(el: &web_sys::Element);
    pub fn sheet(el: &web_sys::Element);
    /// A pulse on something that waits for the user.
    pub fn attention(el: &web_sys::Element);
    /// Count a number up or down to its new value.
    pub fn count_to(el: &web_sys::Element, to: f64, decimals: u32, suffix: &str);
    /// Move the `.selection` mark of a segmented control to its `.on` item.
    pub fn slide_selection(container_selector: &str);
    /// Run `apply` (which changes theme or layout) inside a dip of the page.
    pub fn retheme(apply: &js_sys::Function);
    /// Press feedback and the drifting background, once.
    pub fn install();
    pub fn has_motion() -> bool;
    /// Put text on the clipboard; false where the browser will not.
    pub fn copy_text(text: &str) -> bool;
    /// Scroll to the element with this id.
    pub fn go_to(id: &str);
}
