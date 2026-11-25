// The layer behind everything, drawn with three.js: a banded sky, two fields of stars and
// three bands of a pixel skyline that drift past at different speeds. It is optional: with
// no WebGL (or no three.js) the page keeps the plain ground the stylesheet gives it.
import * as THREE from '/three/three.module.min.js';

const MOODS = {
  dark: { top: [0.03, 0.04, 0.13], bottom: [0.09, 0.13, 0.36], stars: 1, bands: ['#0d1238', '#111a4a', '#18225c'], lit: ['#f8d030', '#50d8f8', '#f87828'], dim: 0.55 },
  light: { top: [0.36, 0.58, 0.99], bottom: [0.86, 0.93, 1.0], stars: 0, bands: ['#b9c9ee', '#9fb3e2', '#8198d2'], lit: ['#fcecc8', '#ffffff', '#f8d088'], dim: 0.35 },
};

// A small repeatable generator, so the skyline is the same street every time.
function seeded(seed) {
  let s = seed >>> 0;
  return () => ((s = (s * 1664525 + 1013904223) >>> 0) / 4294967296);
}

// One band of towers with lit windows, painted into a strip that repeats sideways.
function band(colour, lit, seed, tallest, dim) {
  const [w, h] = [512, 128];
  const c = document.createElement('canvas');
  c.width = w; c.height = h;
  const g = c.getContext('2d');
  const next = seeded(seed);
  for (let x = 0; x < w;) {
    const width = 14 + Math.floor(next() * 26);
    const height = Math.floor(h * (0.25 + next() * tallest));
    g.fillStyle = colour;
    g.fillRect(x, h - height, width - 2, height);
    if (next() > 0.6) g.fillRect(x + Math.floor(width / 2) - 1, h - height - 6, 2, 6); // a mast
    for (let wy = h - height + 4; wy < h - 4; wy += 6) {
      for (let wx = x + 3; wx < x + width - 5; wx += 5) {
        if (next() < dim) continue;
        g.fillStyle = lit[Math.floor(next() * lit.length)];
        g.globalAlpha = 0.35 + next() * 0.55;
        g.fillRect(wx, wy, 2, 3);
        g.globalAlpha = 1;
      }
    }
    x += width;
  }
  const texture = new THREE.CanvasTexture(c);
  texture.magFilter = texture.minFilter = THREE.NearestFilter;
  texture.generateMipmaps = false;
  texture.wrapS = THREE.RepeatWrapping;
  texture.colorSpace = THREE.SRGBColorSpace;
  return texture;
}

function start() {
  const canvas = document.createElement('canvas');
  canvas.id = 'backdrop';
  canvas.setAttribute('aria-hidden', 'true');
  let renderer;
  try {
    renderer = new THREE.WebGLRenderer({ canvas, antialias: false, alpha: false, powerPreference: 'low-power' });
  } catch (_) {
    return;
  }
  renderer.setPixelRatio(1);
  document.body.prepend(canvas);
  document.documentElement.dataset.backdrop = 'on';

  const scene = new THREE.Scene();
  const camera = new THREE.OrthographicCamera(-1, 1, 1, -1, 0, 10);
  camera.position.z = 5;

  // The sky: a ramp cut into steps, as a cartridge would have drawn it.
  const sky = new THREE.Mesh(new THREE.PlaneGeometry(2, 2), new THREE.ShaderMaterial({
    depthTest: false,
    uniforms: { uTop: { value: new THREE.Vector3() }, uBottom: { value: new THREE.Vector3() } },
    vertexShader: 'varying vec2 vUv; void main() { vUv = uv; gl_Position = vec4(position.xy, 0.0, 1.0); }',
    fragmentShader: 'precision mediump float; varying vec2 vUv; uniform vec3 uTop; uniform vec3 uBottom;' +
      'void main() { float t = floor(vUv.y * 14.0) / 14.0; gl_FragColor = vec4(mix(uBottom, uTop, t), 1.0); }',
  }));
  sky.renderOrder = 0;
  scene.add(sky);

  const fields = [0.9, 0.5].map((bright, layer) => {
    const next = seeded(7 + layer * 31);
    const points = new Float32Array(360);
    for (let i = 0; i < points.length; i += 3) {
      points[i] = next() * 2 - 1;
      points[i + 1] = next() * 1.5 - 0.5;
      points[i + 2] = 0;
    }
    const geometry = new THREE.BufferGeometry();
    geometry.setAttribute('position', new THREE.BufferAttribute(points, 3));
    const field = new THREE.Points(geometry, new THREE.PointsMaterial({ color: layer ? 0x50d8f8 : 0xfcecc8, size: layer ? 1 : 2, sizeAttenuation: false, transparent: true, depthTest: false }));
    field.userData.bright = bright;
    field.renderOrder = 1;
    scene.add(field);
    return field;
  });

  const strips = [0, 1, 2].map((i) => {
    const strip = new THREE.Mesh(new THREE.PlaneGeometry(2, 1), new THREE.MeshBasicMaterial({ transparent: true, depthTest: false }));
    strip.userData = { tall: [0.7, 0.55, 0.4][i], height: [0.62, 0.46, 0.3][i], speed: [0.004, 0.009, 0.018][i] };
    strip.renderOrder = 2 + i;
    scene.add(strip);
    return strip;
  });

  let mood = null;
  const theme = () => (document.documentElement.dataset.theme === 'light' ? 'light' : 'dark');
  function repaint() {
    mood = MOODS[theme()];
    sky.material.uniforms.uTop.value.set(...mood.top);
    sky.material.uniforms.uBottom.value.set(...mood.bottom);
    strips.forEach((strip, i) => {
      if (strip.material.map) strip.material.map.dispose();
      strip.material.map = band(mood.bands[i], mood.lit, 11 + i * 17, strip.userData.tall, mood.dim);
      strip.material.needsUpdate = true;
    });
    resize();
  }
  function resize() {
    // Half the pixels, shown enlarged: a pixel stays a pixel.
    const [w, h] = [Math.max(2, Math.ceil(innerWidth / 2)), Math.max(2, Math.ceil(innerHeight / 2))];
    renderer.setSize(w, h, false);
    strips.forEach((strip) => {
      const height = strip.userData.height;
      strip.scale.y = height * 2;
      strip.position.y = -1 + height;
      if (strip.material.map) strip.material.map.repeat.x = w / ((h * height) * 4);
    });
    draw(performance.now());
  }

  const calm = matchMedia('(prefers-reduced-motion: reduce)');
  let pointer = 0;
  function draw(now) {
    const t = now / 1000;
    fields.forEach((field, i) => {
      field.material.opacity = mood.stars * field.userData.bright * (0.7 + 0.3 * Math.sin(t * (0.6 + i * 0.5) + i));
      field.position.x = pointer * 0.01 * (i + 1);
    });
    strips.forEach((strip, i) => {
      if (strip.material.map) strip.material.map.offset.x = (calm.matches ? 0 : t * strip.userData.speed) + pointer * 0.01 * (i + 1);
    });
    renderer.render(scene, camera);
  }
  let last = 0;
  function frame(now) {
    requestAnimationFrame(frame);
    // Thirty a second is plenty for a street that drifts, and nothing while the tab is away.
    if (document.hidden || calm.matches || now - last < 33) return;
    last = now;
    draw(now);
  }

  addEventListener('resize', resize);
  addEventListener('pointermove', (e) => { pointer = e.clientX / innerWidth - 0.5; }, { passive: true });
  new MutationObserver(repaint).observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
  repaint();
  requestAnimationFrame(frame);
}

if (document.body) start(); else addEventListener('DOMContentLoaded', start);
