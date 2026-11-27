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
// The outline beside the conversation, kept to scale: each bar sits where its item is in the
// whole conversation and is as tall as the item, and a frame marks the part now on screen.
// It follows scrolling, new text and a change of size; pressing or dragging on the outline
// scrolls the conversation there.
let outlined = false;
export function outline_install() {
  if (outlined) return;
  outlined = true;
  let due = false;
  const sync = () => {
    due = false;
    const scroller = document.getElementById('transcript');
    const outline = document.getElementById('outline');
    const frame = document.getElementById('outline-view');
    if (!scroller || !outline || !frame) return;
    const whole = Math.max(scroller.scrollHeight, 1);
    const from = scroller.getBoundingClientRect().top - scroller.scrollTop;
    const bars = outline.querySelectorAll('.o');
    const items = scroller.querySelectorAll('.it');
    bars.forEach((bar, i) => {
      const item = items[i];
      if (!item) return;
      const box = item.getBoundingClientRect();
      bar.style.top = (((box.top - from) / whole) * 100).toFixed(2) + '%';
      bar.style.height = 'max(2px, calc(' + ((box.height / whole) * 100).toFixed(2) + '% - 2px))';
    });
    outline.classList.add('scaled');
    // The part of the conversation on screen now.
    frame.style.top = ((scroller.scrollTop / whole) * 100).toFixed(2) + '%';
    frame.style.height = ((Math.min(scroller.clientHeight, whole) / whole) * 100).toFixed(2) + '%';
    frame.classList.toggle('all', scroller.scrollHeight <= scroller.clientHeight + 1);
  };
  const soon = () => { if (!due) { due = true; requestAnimationFrame(sync); } };
  document.addEventListener('scroll', (e) => { if (e.target && e.target.id === 'transcript') soon(); }, true);
  addEventListener('resize', soon);
  new MutationObserver(soon).observe(document.body, { childList: true, subtree: true, characterData: true });
  // Pressing on the outline brings that part of the conversation to the middle: the page
  // travels there (exponential in and out) and the frame with it. Dragging is followed as
  // the finger goes. The frame swells while it is held and springs back when let go.
  let dragging = false, pressed = null;
  const target = (e) => {
    const scroller = document.getElementById('transcript');
    const outline = document.getElementById('outline');
    if (!scroller || !outline) return null;
    const box = outline.getBoundingClientRect();
    const share = Math.min(1, Math.max(0, (e.clientY - box.top) / Math.max(box.height, 1)));
    return [scroller, share * scroller.scrollHeight - scroller.clientHeight / 2];
  };
  const grab = (held) => {
    const frame = document.getElementById('outline-view');
    if (!frame || !M() || calm()) return;
    M().animate(frame, { scaleX: held ? 1.12 : 1, opacity: held ? 1 : 0.9 }, held ? { duration: 0.18, ease: EXPO_OUT } : BOUNCE);
  };
  document.addEventListener('pointerdown', (e) => {
    if (!(e.target.closest && e.target.closest('#outline'))) return;
    const to = target(e);
    if (!to) return;
    dragging = true; pressed = e.clientY;
    glide(to[0], to[1], 0.62);
    grab(true);
    e.preventDefault();
  });
  document.addEventListener('pointermove', (e) => {
    if (!dragging) return;
    // (A press that has not moved is a press: the page keeps travelling.)
    if (pressed !== null && Math.abs(e.clientY - pressed) < 4) return;
    pressed = null;
    const to = target(e);
    if (to) { stop_glide(); to[0].scrollTop = to[1]; }
  });
  const let_go = () => { if (dragging) { dragging = false; pressed = null; grab(false); } };
  document.addEventListener('pointerup', let_go);
  document.addEventListener('pointercancel', let_go);
  soon();
}
// Scroll a box to its end (the console, when something was printed).
export function to_end(id) {
  const el = document.getElementById(id);
  if (el) el.scrollTop = el.scrollHeight;
}
// The "Remove?" capsule on a conversation's row, as a sheet on a phone arrives: it fades in
// and grows from where the × was on a spring with a little bounce (low damping), and its
// buttons follow one after the other.
const EXPO_IN = [0.7, 0, 0.84, 0];
const BOUNCE = { type: 'spring', stiffness: 460, damping: 20, mass: 0.8 };
export function confirm_in(el) {
  if (!M() || !el || calm()) return;
  M().animate(el, { opacity: [0, 1], scale: [0.6, 1], x: [14, 0] }, BOUNCE);
  const parts = el.children;
  if (parts.length) M().animate(parts, { opacity: [0, 1], y: [6, 0] }, { duration: 0.42, delay: M().stagger(0.05, { startDelay: 0.04 }), ease: EXPO_OUT });
}
// It fades and shrinks away (exponential in), and then `done` takes it off the row.
export function confirm_out(el, done) {
  if (!M() || !el || calm()) { done(); return; }
  M().animate(el, { opacity: [1, 0], scale: [1, 0.82], x: [0, 10] }, { duration: 0.2, ease: EXPO_IN }).then(done, done);
}
// A removed conversation's row fades, slides away and closes up (exponential in and out);
// then `done` takes it off the list.
export function row_out(el, done) {
  if (!M() || !el || calm()) { done(); return; }
  const h = el.getBoundingClientRect().height;
  el.style.overflow = 'hidden';
  M().animate(el, { opacity: [1, 0], x: [0, -28], scale: [1, 0.96] }, { duration: 0.28, ease: EXPO_IN })
    .then(() => M().animate(el, { height: [h + 'px', '0px'], paddingTop: 0, paddingBottom: 0, marginTop: 0, marginBottom: 0, minHeight: 0 }, { duration: 0.32, ease: EXPO }))
    .then(done, done);
}
// Bring an element of the conversation into view: the page travels to it.
export function go_to(id) {
  const el = document.getElementById(id);
  if (!el) return;
  const scroller = document.getElementById('transcript');
  if (scroller && scroller.contains(el)) {
    glide(scroller, scroller.scrollTop + el.getBoundingClientRect().top - scroller.getBoundingClientRect().top - 8, 0.62);
  } else {
    el.scrollIntoView({ behavior: calm() ? 'auto' : 'smooth', block: 'start' });
  }
}
// Scroll a box to `top`, travelling: exponential in and out. One journey at a time; the
// wheel or a finger on the box ends it.
let journey = null;
function stop_glide() { if (journey) { journey.stop(); journey = null; } }
function glide(box, top, seconds) {
  stop_glide();
  top = Math.max(0, Math.min(top, box.scrollHeight - box.clientHeight));
  if (!M() || calm() || Math.abs(top - box.scrollTop) < 2) { box.scrollTop = top; return; }
  if (!box.dataset.glides) {
    box.dataset.glides = '1';
    for (const kind of ['wheel', 'touchstart']) box.addEventListener(kind, stop_glide, { passive: true });
  }
  journey = M().animate(box.scrollTop, top, { duration: seconds, ease: EXPO, onUpdate: v => { box.scrollTop = v; } });
}
// A panel that opens under what the user pressed (the console): it grows from nothing and
// fades in, exponential in and out; what is in it follows, a moment later.
export function reveal(el) {
  if (!M() || !el || calm()) return;
  const h = el.getBoundingClientRect().height;
  el.style.minHeight = '0px';
  M().animate(el, { height: ['0px', h + 'px'], opacity: [0, 1], y: [12, 0] }, { duration: 0.52, ease: EXPO })
    .then(() => { el.style.height = ''; el.style.minHeight = ''; }, () => {});
  if (el.children.length) M().animate(el.children, { opacity: [0, 1], y: [8, 0] }, { duration: 0.45, delay: M().stagger(0.06, { startDelay: 0.16 }), ease: EXPO_OUT });
}
// It closes the same way, faster; then `done` takes it off the page.
export function conceal(el, done) {
  if (!M() || !el || calm()) { done(); return; }
  const h = el.getBoundingClientRect().height;
  el.style.minHeight = '0px';
  M().animate(el, { height: [h + 'px', '0px'], opacity: [1, 0], y: [0, 10] }, { duration: 0.36, ease: EXPO }).then(done, done);
}
// What was pressed and has started something (Run): it gives, and comes back on a spring
// with a little bounce, with a ring of light.
export function pop(el) {
  if (!M() || !el || calm()) return;
  M().animate(el, { scale: [0.9, 1] }, BOUNCE);
  M().animate(el, { boxShadow: ['0 0 0 0 rgba(251,191,36,0.45)', '0 0 0 14px rgba(251,191,36,0)'] }, { duration: 0.7, ease: EXPO_OUT });
}
// New text in a box (what a run printed): it fades in.
export function fade_in(el) {
  if (!M() || !el || calm()) return;
  M().animate(el, { opacity: [0.2, 1], y: [6, 0] }, { duration: 0.5, ease: EXPO_OUT });
}
// The items of a list arriving, one after the other (a conversation's messages: the last
// ones only, so that a long one costs no more). Nothing of the movement is left on them.
export function rows_in(selector, last) {
  if (!M() || calm()) return;
  let els = [...document.querySelectorAll(selector)];
  if (last > 0) els = els.slice(-last);
  if (!els.length) return;
  const clean = () => els.forEach(el => { el.style.transform = ''; el.style.opacity = ''; });
  M().animate(els, { opacity: [0, 1], y: [12, 0] }, { duration: 0.5, delay: M().stagger(0.035), ease: EXPO_OUT }).then(clean, clean);
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
    /// The "Remove?" capsule arriving on a row, on a spring with a little bounce.
    pub fn confirm_in(el: &web_sys::Element);
    /// It fades away; then `done` is called.
    pub fn confirm_out(el: &web_sys::Element, done: &js_sys::Function);
    /// A removed row fades, slides away and closes up; then `done` is called.
    pub fn row_out(el: &web_sys::Element, done: &js_sys::Function);
    /// Run `apply` (which changes theme or layout) inside a dip of the page.
    pub fn retheme(apply: &js_sys::Function);
    /// Press feedback and the drifting background, once.
    pub fn install();
    pub fn has_motion() -> bool;
    /// Put text on the clipboard; false where the browser will not.
    pub fn copy_text(text: &str) -> bool;
    /// Keep the conversation's outline to scale, with a frame for the part on screen; once.
    pub fn outline_install();
    /// Scroll the element with this id to its end.
    pub fn to_end(id: &str);
    /// Scroll to the element with this id.
    pub fn go_to(id: &str);
    /// A panel growing open and fading in.
    pub fn reveal(el: &web_sys::Element);
    /// It closes; then `done` is called.
    pub fn conceal(el: &web_sys::Element, done: &js_sys::Function);
    /// A pressed button that started something springs back, with a ring of light.
    pub fn pop(el: &web_sys::Element);
    /// New text fading in.
    pub fn fade_in(el: &web_sys::Element);
    /// The (last `last`, or all when 0) elements matching a selector arrive one after another.
    pub fn rows_in(selector: &str, last: u32);
}
