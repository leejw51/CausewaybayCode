// The Forge: while the model downloads, a robot is forged on an anvil (three.js, vendored
// beside this file; loaded only when the scene is shown). The page (src/forge.rs) mounts it
// in an element and sets `el.__forge = {stage, percent, speed}`; each frame reads that.
//
//   heat    the furnace glows up from cold; no robot yet
//   hammer  parts arrive with the percent (feet, legs, torso, arms, head, face plate): each
//           flies in red-hot, the hammer strikes with a burst of sparks, it cools to steel
//   temper  a cyan beam scans the robot top to bottom; the hammer rests
//   wake    the core pulses, the eyes flicker on and brighten
//   ready   eyes full cyan, it waves, one burst of sparks and confetti; the camera stops
//   failed  it sags, the embers dim          missing  a cold forge, no robot
//
// Timings and colours are the design's (design/forge/project/Tokens.dc.html). The GPU runs
// the model next: the pixel ratio is capped, nothing is drawn while the tab is hidden or the
// scene is off screen, and the loop stops once Ready has played. Reduced motion: one still
// render per change, nothing moves.
import * as THREE from './three.module.min.js';

const C = {
  night: 0x0a0d24, forge: 0xff8a3d, ember: 0xffb547, eye: 0x50d8f8, steel: 0x8a96b8,
  ready: 0x5ce08c, cold: 0xff8a7a, hot: 0xff6a1f, wall: 0x141a3a,
};
// When each part arrives, in percent of the bytes.
const AT = [1, 17, 34, 51, 68, 85];
const FLY = 0.5, GLOW = 1.2, COOL = 1.5;
const calm = () => matchMedia('(prefers-reduced-motion: reduce)').matches;
const easeOut = t => 1 - Math.pow(1 - Math.min(Math.max(t, 0), 1), 3);

export function supported() {
  try {
    const c = document.createElement('canvas');
    return !!(window.WebGLRenderingContext && (c.getContext('webgl2') || c.getContext('webgl')));
  } catch (_) { return false; }
}

function box(w, h, d, material) { return new THREE.Mesh(new THREE.BoxGeometry(w, h, d), material); }

function steel() {
  return new THREE.MeshStandardMaterial({ color: C.steel, metalness: 0.75, roughness: 0.42, emissive: C.hot, emissiveIntensity: 0, flatShading: true });
}

// The robot: six parts, each a group of chunky boxes, and what lights up in it.
function robot() {
  const root = new THREE.Group();
  const parts = [];
  const part = (build) => { const g = new THREE.Group(); const mats = []; build(g, () => { const m = steel(); mats.push(m); return m; }); root.add(g); parts.push({ group: g, mats, home: g.position.clone(), born: -1 }); return g; };
  part((g, m) => { for (const x of [-0.19, 0.19]) { const f = box(0.3, 0.13, 0.42, m()); f.position.set(x, 0.065, 0.04); g.add(f); } });
  part((g, m) => { for (const x of [-0.19, 0.19]) { const l = box(0.2, 0.5, 0.22, m()); l.position.set(x, 0.38, 0); g.add(l); const k = box(0.24, 0.1, 0.26, m()); k.position.set(x, 0.42, 0); g.add(k); } });
  let core;
  part((g, m) => {
    const t = box(0.72, 0.62, 0.44, m()); t.position.set(0, 0.94, 0); g.add(t);
    const belt = box(0.6, 0.1, 0.4, m()); belt.position.set(0, 0.6, 0); g.add(belt);
    core = new THREE.Mesh(new THREE.OctahedronGeometry(0.1, 0), new THREE.MeshStandardMaterial({ color: 0x1a2a40, emissive: C.eye, emissiveIntensity: 0, roughness: 0.3 }));
    core.position.set(0, 0.97, 0.24); g.add(core);
  });
  const arms = [];
  part((g, m) => {
    for (const side of [-1, 1]) {
      const shoulder = new THREE.Group(); shoulder.position.set(side * 0.47, 1.17, 0);
      const s = box(0.22, 0.2, 0.24, m()); shoulder.add(s);
      const a = box(0.16, 0.5, 0.18, m()); a.position.set(0, -0.3, 0); shoulder.add(a);
      const h = box(0.2, 0.14, 0.2, m()); h.position.set(0, -0.6, 0); shoulder.add(h);
      g.add(shoulder); arms.push(shoulder);
    }
  });
  part((g, m) => {
    const h = box(0.48, 0.4, 0.42, m()); h.position.set(0, 1.47, 0); g.add(h);
    const neck = box(0.16, 0.08, 0.16, m()); neck.position.set(0, 1.27, 0); g.add(neck);
    const ant = new THREE.Mesh(new THREE.CylinderGeometry(0.02, 0.02, 0.18, 5), m()); ant.position.set(0.12, 1.75, 0); g.add(ant);
    const tip = new THREE.Mesh(new THREE.IcosahedronGeometry(0.045, 0), m()); tip.position.set(0.12, 1.86, 0); g.add(tip);
  });
  const eyes = [];
  part((g, m) => {
    const plate = new THREE.Mesh(new THREE.BoxGeometry(0.4, 0.22, 0.05), m()); plate.position.set(0, 1.49, 0.22); g.add(plate);
    plate.material.color.set(0x2a3352);
    for (const x of [-0.09, 0.09]) {
      const e = box(0.08, 0.05, 0.03, new THREE.MeshStandardMaterial({ color: 0x0d1630, emissive: C.eye, emissiveIntensity: 0 }));
      e.position.set(x, 1.5, 0.25); g.add(e); eyes.push(e);
    }
  });
  return { root, parts, core, arms, eyes };
}

function anvil() {
  const g = new THREE.Group();
  const iron = new THREE.MeshStandardMaterial({ color: 0x2b3150, metalness: 0.8, roughness: 0.5, flatShading: true });
  const base = box(1.0, 0.3, 0.7, iron); base.position.y = 0.15; g.add(base);
  const waist = box(0.55, 0.32, 0.45, iron); waist.position.y = 0.46; g.add(waist);
  const top = box(1.5, 0.26, 0.72, iron); top.position.y = 0.75; g.add(top);
  const horn = new THREE.Mesh(new THREE.ConeGeometry(0.2, 0.6, 4), iron); horn.rotation.z = -Math.PI / 2; horn.rotation.x = Math.PI / 4; horn.position.set(1.05, 0.77, 0); g.add(horn);
  return g;
}

function furnace() {
  const g = new THREE.Group();
  const brick = new THREE.MeshStandardMaterial({ color: 0x3a2230, roughness: 0.95, flatShading: true });
  const body = box(1.3, 1.7, 1.1, brick); body.position.y = 0.85; g.add(body);
  const chimney = box(0.5, 1.4, 0.5, brick); chimney.position.set(0.1, 2.3, -0.2); g.add(chimney);
  const mouth = new THREE.Mesh(new THREE.PlaneGeometry(0.7, 0.55), new THREE.MeshBasicMaterial({ color: C.forge }));
  mouth.position.set(0, 0.7, 0.56); g.add(mouth);
  const light = new THREE.PointLight(C.forge, 0, 9, 1.3); light.position.set(0.2, 0.8, 1.0); g.add(light);
  return { group: g, mouth, light };
}

// A pool of glowing points: sparks, embers, confetti.
function pool(n, size, additive) {
  const geo = new THREE.BufferGeometry();
  const pos = new Float32Array(n * 3), col = new Float32Array(n * 3);
  geo.setAttribute('position', new THREE.BufferAttribute(pos, 3));
  geo.setAttribute('color', new THREE.BufferAttribute(col, 3));
  const mat = new THREE.PointsMaterial({ size, vertexColors: true, transparent: true, depthWrite: false, blending: additive ? THREE.AdditiveBlending : THREE.NormalBlending, sizeAttenuation: true });
  const points = new THREE.Points(geo, mat);
  points.frustumCulled = false;
  const p = { points, pos, col, n, vel: new Float32Array(n * 3), life: new Float32Array(n), max: new Float32Array(n), base: new Float32Array(n * 3), next: 0 };
  for (let i = 0; i < n; i++) pos[i * 3 + 1] = -99;
  return p;
}

function emit(p, at, spread, up, color, life) {
  const i = p.next; p.next = (p.next + 1) % p.n;
  p.pos[i * 3] = at.x; p.pos[i * 3 + 1] = at.y; p.pos[i * 3 + 2] = at.z;
  const a = Math.random() * Math.PI * 2, s = spread * (0.4 + Math.random());
  p.vel[i * 3] = Math.cos(a) * s; p.vel[i * 3 + 1] = up * (0.5 + Math.random()); p.vel[i * 3 + 2] = Math.sin(a) * s;
  const c = new THREE.Color(color);
  p.base[i * 3] = c.r; p.base[i * 3 + 1] = c.g; p.base[i * 3 + 2] = c.b;
  p.life[i] = life; p.max[i] = life;
}

function step(p, dt, gravity, drag) {
  for (let i = 0; i < p.n; i++) {
    if (p.life[i] <= 0) { p.pos[i * 3 + 1] = -99; continue; }
    p.life[i] -= dt;
    p.vel[i * 3 + 1] -= gravity * dt;
    for (let k = 0; k < 3; k++) { p.vel[i * 3 + k] *= 1 - drag * dt; p.pos[i * 3 + k] += p.vel[i * 3 + k] * dt; }
    const f = Math.max(p.life[i] / p.max[i], 0);
    for (let k = 0; k < 3; k++) p.col[i * 3 + k] = p.base[i * 3 + k] * f;
  }
  p.points.geometry.attributes.position.needsUpdate = true;
  p.points.geometry.attributes.color.needsUpdate = true;
}

export function mount(el) {
  if (!supported()) { el.dataset.webgl = 'no'; return null; }
  let renderer;
  try {
    renderer = new THREE.WebGLRenderer({ antialias: true, powerPreference: 'low-power' });
  } catch (_) { el.dataset.webgl = 'no'; return null; }
  el.dataset.webgl = 'yes';
  el.dataset.frames = '0';
  renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 1.5));
  renderer.setClearColor(C.night, 1);
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.domElement.className = 'forge-canvas';
  renderer.domElement.setAttribute('aria-hidden', 'true');
  el.appendChild(renderer.domElement);

  const scene = new THREE.Scene();
  scene.fog = new THREE.Fog(C.night, 7, 16);
  const camera = new THREE.PerspectiveCamera(34, 1, 0.1, 50);
  const target = new THREE.Vector3(-0.3, 1.45, 0);

  scene.add(new THREE.HemisphereLight(0x2a3a80, 0x0a0610, 0.55));
  const rim = new THREE.DirectionalLight(C.eye, 1.4); rim.position.set(3, 4, -3); scene.add(rim);
  const fill = new THREE.DirectionalLight(0xffd7a8, 0.35); fill.position.set(1, 3, 5); scene.add(fill);

  const floor = new THREE.Mesh(new THREE.CircleGeometry(9, 40), new THREE.MeshStandardMaterial({ color: C.wall, roughness: 0.9 }));
  floor.rotation.x = -Math.PI / 2; scene.add(floor);
  const wall = new THREE.Mesh(new THREE.PlaneGeometry(30, 12), new THREE.MeshStandardMaterial({ color: 0x0e1330, roughness: 1 }));
  wall.position.set(0, 5, -4); scene.add(wall);

  const a = anvil(); scene.add(a);
  const f = furnace(); f.group.position.set(-2.5, 0, -1.4); f.group.rotation.y = 0.5; scene.add(f.group);
  const bot = robot(); bot.root.position.set(0, 0.88, 0); scene.add(bot.root);
  for (const p of bot.parts) p.group.visible = false;

  // The hammer, hinged at its handle's end, to the robot's right.
  const hammer = new THREE.Group(); hammer.position.set(1.45, 1.1, 0.35);
  const wood = new THREE.MeshStandardMaterial({ color: 0x6b4a2e, roughness: 0.8 });
  const handle = new THREE.Mesh(new THREE.CylinderGeometry(0.035, 0.04, 1.1, 6), wood); handle.position.set(0, 0.55, 0); hammer.add(handle);
  const head = box(0.42, 0.2, 0.2, new THREE.MeshStandardMaterial({ color: 0x5a6280, metalness: 0.9, roughness: 0.35 })); head.position.set(-0.08, 1.1, 0); hammer.add(head);
  scene.add(hammer);

  // The scan beam: a thin cyan ring that sweeps the robot.
  const beam = new THREE.Mesh(new THREE.CylinderGeometry(0.7, 0.7, 0.03, 32, 1, true), new THREE.MeshBasicMaterial({ color: C.eye, transparent: true, opacity: 0, blending: THREE.AdditiveBlending, side: THREE.DoubleSide, depthWrite: false }));
  scene.add(beam);
  const beamGlow = new THREE.PointLight(C.eye, 0, 3, 1.5); scene.add(beamGlow);

  const sparks = pool(320, 0.07, true); scene.add(sparks.points);
  const embers = pool(90, 0.06, true); scene.add(embers.points);
  const confetti = pool(160, 0.09, false); scene.add(confetti.points);

  let width = 0, height = 0;
  const resize = () => {
    const w = el.clientWidth || 1, h = el.clientHeight || 1;
    if (w === width && h === height) return;
    width = w; height = h;
    renderer.setSize(w, h, false);
    camera.aspect = w / h;
    // Narrow screens step back, so that the robot and the anvil stay in the picture.
    camera.position.set(0, 2.3, w / h < 1 ? 9.5 : 7.2);
    // On a wide window the numbers' panel covers the lower part: the robot is framed higher.
    target.y = w / h < 1 ? 1.45 : 0.75;
    camera.updateProjectionMatrix();
    wake();
  };

  let visible = true, last = performance.now(), clock = 0, frames = 0, running = false, ended = false;
  let stage = '', since = 0, strikeAt = -10, strikeTarget = null, readyAt = -1, shown = 0, still = '';
  const state = () => el.__forge || { stage: 'heat', percent: 0, speed: 0 };

  function strike(partIndex) {
    strikeAt = clock;
    const p = bot.parts[partIndex];
    const box3 = new THREE.Box3().setFromObject(p.group);
    strikeTarget = box3.getCenter(new THREE.Vector3());
    strikeTarget.y = box3.max.y;
  }

  function update(dt) {
    const s = state();
    if (s.stage !== stage) { stage = s.stage; since = clock; if (stage === 'ready') readyAt = clock; }
    const t = clock - since;
    const moving = !calm();
    // How many parts the percent has reached.
    let want = 0;
    if (stage === 'hammer') want = AT.filter(at => s.percent >= at).length;
    else if (['temper', 'wake', 'ready'].includes(stage)) want = 6;
    else if (stage === 'failed') want = Math.max(shown, AT.filter(at => s.percent >= at).length);
    for (let i = 0; i < 6; i++) {
      const p = bot.parts[i];
      if (i < want && !p.group.visible) {
        p.group.visible = true;
        // Arriving now (a part reached while watching), or already there (a later visit).
        p.born = moving && frames > 0 && stage === 'hammer' ? clock : clock - 10;
        if (p.born === clock) setTimeout(() => {}, 0);
      }
      if (i >= want && p.group.visible && stage !== 'failed') { p.group.visible = false; p.born = -1; }
      if (!p.group.visible) continue;
      const age = clock - p.born;
      const fly = easeOut(age / FLY);
      p.group.position.set(p.home.x, p.home.y + (1 - fly) * 1.4, p.home.z);
      p.group.scale.setScalar(0.6 + 0.4 * fly);
      // Red-hot while it lands and glows, then it cools to steel.
      const heat = age < FLY + GLOW ? 1 : Math.max(0, 1 - (age - FLY - GLOW) / COOL);
      for (const m of p.mats) m.emissiveIntensity = heat * 1.6;
      if (age >= FLY && age - dt < FLY && moving) strike(i);
    }
    shown = bot.parts.filter(p => p.group.visible).length;
    el.dataset.parts = String(shown);

    // The furnace: up from cold while heating, roaring while hammering, low when cold.
    const roar = { heat: Math.min(1, 0.25 + t / 3), hammer: 1, temper: 0.7, wake: 0.6, ready: 0.75, failed: 0.12, missing: 0.08 }[stage] ?? 0.5;
    const flicker = moving ? 0.85 + 0.15 * Math.sin(clock * 13) * Math.sin(clock * 7.3) : 1;
    f.light.intensity = 14 * roar * flicker;
    f.mouth.material.color.setHex(C.forge).multiplyScalar(0.25 + 0.9 * roar * flicker);

    // The hammer: a strike when a part lands, and a beat now and then while hammering.
    if (moving && stage === 'hammer' && shown > 0 && clock - strikeAt > 1.6) strike(shown - 1);
    const since_strike = clock - strikeAt;
    let swing = -0.15;
    if (moving && since_strike < 0.45) {
      swing = since_strike < 0.18 ? -0.15 - 0.9 * easeOut(since_strike / 0.18) : -1.05 + 0.9 * easeOut((since_strike - 0.18) / 0.27);
      if (since_strike >= 0.18 && since_strike - dt < 0.18 && strikeTarget) {
        for (let i = 0; i < 40; i++) emit(sparks, strikeTarget, 1.6, 2.2, Math.random() < 0.5 ? C.ember : C.forge, 0.5 + Math.random() * 0.6);
      }
    }
    if (['temper', 'wake', 'ready', 'failed', 'missing'].includes(stage)) swing = 0.25;
    hammer.rotation.z = swing;
    hammer.visible = stage !== 'missing';

    // Embers rise from the furnace: more when it downloads faster.
    if (moving && stage !== 'missing') {
      const rate = stage === 'failed' ? 3 : 10 + Math.min(50, (s.speed || 0) / 1e6);
      const mouth = new THREE.Vector3(); f.mouth.getWorldPosition(mouth);
      for (let k = 0; k < rate * dt; k++) {
        const at = mouth.clone().add(new THREE.Vector3((Math.random() - 0.5) * 0.6, 0.1, 0.1));
        emit(embers, at, 0.15, 0.6, stage === 'failed' ? 0x664433 : (Math.random() < 0.7 ? C.ember : C.forge), 2 + Math.random() * 2);
      }
    }

    // Tempering: the beam sweeps top to bottom, again and again.
    const scanning = stage === 'temper';
    beam.material.opacity = scanning ? 0.75 : 0;
    beamGlow.intensity = scanning ? 3 : 0;
    if (scanning) {
      const y = 0.88 + 1.95 - ((moving ? t : 0.4) % 1.6) / 1.6 * 1.95;
      beam.position.set(0, y, 0); beamGlow.position.set(0, y, 0.7);
    }

    // Waking: the core pulses, the eyes flicker on; ready: full.
    let eyes = 0, core = 0;
    if (stage === 'wake') {
      core = moving ? 0.8 + 0.7 * Math.sin(t * 4) : 1.2;
      eyes = moving ? Math.min(1, t / 6) * (Math.random() < 0.12 ? 0.2 : 1) * 2.2 : 1.5;
    } else if (stage === 'ready') {
      core = 1.6; eyes = 3;
    }
    bot.core.material.emissiveIntensity = core;
    for (const e of bot.eyes) e.material.emissiveIntensity = eyes;

    // Ready: it waves, one burst of sparks and confetti. Failed: it sags.
    const wave = stage === 'ready' && moving ? Math.sin(t * 9) * 0.5 * Math.max(0, 1 - t / 2.5) : 0;
    bot.arms[1].rotation.z = stage === 'ready' ? 2.4 + wave : 0;
    bot.arms[0].rotation.z = 0;
    const sag = stage === 'failed' ? 1 : 0;
    bot.root.rotation.x = sag * 0.18;
    bot.root.position.y = 0.88 - sag * 0.06;
    for (const arm of bot.arms) arm.rotation.x = sag * 0.35;
    if (stage === 'ready' && readyAt === clock - t && t < dt * 1.5 && moving) {
      const top = new THREE.Vector3(0, 2.6, 0);
      for (let i = 0; i < 60; i++) emit(sparks, top, 2.2, 2.6, i % 2 ? C.eye : C.ember, 0.8 + Math.random() * 0.6);
      const colours = [C.ready, C.eye, C.ember, C.forge, 0xffffff, 0xe9c94b];
      for (let i = 0; i < 120; i++) emit(confetti, top, 1.8, 3.2, colours[i % colours.length], 2 + Math.random());
    }
    step(sparks, dt, 6, 0.6);
    step(embers, dt, -0.15, 0.3);
    step(confetti, dt, 2.2, 1.6);

    // The camera: a slow 20 s orbit, ±12°, that stops for Ready.
    const orbit = moving && stage !== 'ready' ? Math.sin(clock / 20 * Math.PI * 2) * (12 * Math.PI / 180) : 0;
    const dist = Math.hypot(camera.position.x, camera.position.z) || 7.2;
    camera.position.x = Math.sin(orbit) * dist; camera.position.z = Math.cos(orbit) * dist;
    camera.lookAt(target);
  }

  function frame(now) {
    running = false;
    if (ended || !visible || document.hidden) return;
    const dt = Math.min((now - last) / 1000, 0.05);
    last = now; clock += dt;
    resize();
    update(dt);
    renderer.render(scene, camera);
    frames++; el.dataset.frames = String(frames); el.dataset.stage = stage;
    // Ready has played (the page fades the scene out after it): nothing more to draw.
    if (stage === 'ready' && clock - readyAt > 3.5) return;
    if (calm()) return;
    running = true;
    requestAnimationFrame(frame);
  }

  // One still render (reduced motion: each time the stage or the parts change).
  function wake() {
    if (ended || running) return;
    last = performance.now();
    running = true;
    requestAnimationFrame(frame);
  }

  const seen = new IntersectionObserver(entries => { visible = entries.some(e => e.isIntersecting); if (visible) wake(); });
  seen.observe(el);
  const sized = new ResizeObserver(() => wake());
  sized.observe(el);
  const shownAgain = () => { if (!document.hidden) wake(); };
  document.addEventListener('visibilitychange', shownAgain);
  resize();
  wake();

  return {
    // Called by the page when the stage, the percent or the speed changes.
    update() {
      const s = state();
      const key = `${s.stage}:${AT.filter(at => s.percent >= at).length}`;
      if (key !== still || s.stage !== 'ready') { still = key; wake(); }
    },
    dispose() {
      ended = true;
      seen.disconnect(); sized.disconnect();
      document.removeEventListener('visibilitychange', shownAgain);
      scene.traverse(o => { if (o.geometry) o.geometry.dispose(); if (o.material) o.material.dispose(); });
      renderer.dispose();
      renderer.domElement.remove();
    },
  };
}
