/**
 * Liquid glass: real refraction, not a blur.
 *
 * A blurred translucent panel reads as frosted plastic. What makes glass look
 * like glass is that it *bends* what is behind it, and it bends it most at the
 * edges where the surface curves away. So the bezel is modelled as a curved
 * surface, refraction through it is solved with Snell's law, and the resulting
 * per-pixel offsets are baked into a displacement map that `feDisplacementMap`
 * applies to the backdrop.
 *
 * Channel convention: red carries the X offset, green the Y, 128 is neutral.
 *
 * Only Chromium runs an SVG filter inside `backdrop-filter` — every other engine
 * accepts the property and silently drops the SVG part. This window is WebView2,
 * so it works here; elsewhere it degrades to the blur, which is why the blur is
 * declared alongside rather than instead.
 */

const REFRACTIVE_INDEX = 1.46;

/** Signed distance to a rounded rectangle. Negative inside. */
function sdRoundedRect(px, py, w, h, r) {
  const qx = Math.abs(px - w / 2) - (w / 2 - r);
  const qy = Math.abs(py - h / 2) - (h / 2 - r);
  const ax = Math.max(qx, 0);
  const ay = Math.max(qy, 0);
  return Math.hypot(ax, ay) + Math.min(Math.max(qx, qy), 0) - r;
}

/**
 * Convex squircle profile: height of the glass at a normalised depth into the
 * bezel. Flat in the middle, falling away sharply at the rim — which is where
 * the refraction, and therefore the whole effect, lives.
 */
function surface(x) {
  const c = Math.min(Math.max(x, 0), 1);
  return Math.pow(1 - Math.pow(1 - c, 4), 0.25);
}

/** Refraction offset through a surface whose slope is `dy/dx`. */
function refract(slope) {
  // Angle between the surface normal and the viewing ray.
  const incidence = Math.atan(slope);
  const sinT = Math.sin(incidence) / REFRACTIVE_INDEX;
  if (Math.abs(sinT) >= 1) return 0; // total internal reflection
  return Math.tan(incidence) - Math.tan(Math.asin(sinT));
}

/**
 * Build the displacement map for one panel.
 * @returns {{url: string, scale: number}}
 */
export function displacementMap(w, h, radius, bezel) {
  const width = Math.max(1, Math.round(w));
  const height = Math.max(1, Math.round(h));

  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  const img = ctx.createImageData(width, height);

  const eps = 0.75;
  let peak = 0;
  const dx = new Float32Array(width * height);
  const dy = new Float32Array(width * height);

  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const i = y * width + x;
      const d = sdRoundedRect(x + 0.5, y + 0.5, width, height, radius);
      const depth = -d; // distance inward from the edge

      if (d > 0 || depth > bezel) continue; // outside, or on the flat centre

      // Gradient of the distance field — the direction the surface faces.
      const gx =
        sdRoundedRect(x + 0.5 + eps, y + 0.5, width, height, radius) -
        sdRoundedRect(x + 0.5 - eps, y + 0.5, width, height, radius);
      const gy =
        sdRoundedRect(x + 0.5, y + 0.5 + eps, width, height, radius) -
        sdRoundedRect(x + 0.5, y + 0.5 - eps, width, height, radius);
      const len = Math.hypot(gx, gy) || 1;

      // Slope of the glass at this depth, from the profile above.
      const t = depth / bezel;
      const slope =
        (surface(Math.min(t + 0.01, 1)) - surface(Math.max(t - 0.01, 0))) / 0.02;

      const magnitude = refract(slope) * bezel;
      dx[i] = (gx / len) * magnitude;
      dy[i] = (gy / len) * magnitude;
      peak = Math.max(peak, Math.abs(dx[i]), Math.abs(dy[i]));
    }
  }

  const scale = peak || 1;
  for (let i = 0; i < width * height; i++) {
    const p = i * 4;
    img.data[p] = 128 + (dx[i] / scale) * 127;
    img.data[p + 1] = 128 + (dy[i] / scale) * 127;
    img.data[p + 2] = 128;
    img.data[p + 3] = 255;
  }

  ctx.putImageData(img, 0, 0);
  return { url: canvas.toDataURL(), scale };
}

let counter = 0;

/**
 * Svelte action. Gives an element a live refraction filter that follows its size,
 * plus a specular sheen that tracks the pointer.
 *
 * `use:glass={{ radius, bezel, blur, sheen }}`
 */
export function glass(node, options = {}) {
  const id = `glass-${counter++}`;
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("aria-hidden", "true");
  svg.style.cssText = "position:absolute;width:0;height:0;pointer-events:none";
  // Three displacement passes at staggered scales, one per colour channel,
  // recombined with screen blending.
  //
  // A single pass bends the backdrop but keeps it grey-true; real glass disperses
  // — blue refracts harder than red, so the channels separate and the rim picks up
  // colour fringing. That dispersion is most of what distinguishes glass from a
  // blurred panel, and it is the part the previous version was missing entirely.
  const chan = {
    r: "1 0 0 0 0  0 0 0 0 0  0 0 0 0 0  0 0 0 1 0",
    g: "0 0 0 0 0  0 1 0 0 0  0 0 0 0 0  0 0 0 1 0",
    b: "0 0 0 0 0  0 0 0 0 0  0 0 1 0 0  0 0 0 1 0",
  };
  svg.innerHTML = `
    <filter id="${id}" color-interpolation-filters="sRGB">
      <feImage result="map" preserveAspectRatio="none"></feImage>

      <feDisplacementMap in="SourceGraphic" in2="map" result="dr"
        xChannelSelector="R" yChannelSelector="G"></feDisplacementMap>
      <feColorMatrix in="dr" type="matrix" values="${chan.r}" result="cr"></feColorMatrix>

      <feDisplacementMap in="SourceGraphic" in2="map" result="dg"
        xChannelSelector="R" yChannelSelector="G"></feDisplacementMap>
      <feColorMatrix in="dg" type="matrix" values="${chan.g}" result="cg"></feColorMatrix>

      <feDisplacementMap in="SourceGraphic" in2="map" result="db"
        xChannelSelector="R" yChannelSelector="G"></feDisplacementMap>
      <feColorMatrix in="db" type="matrix" values="${chan.b}" result="cb"></feColorMatrix>

      <feBlend in="cr" in2="cg" mode="screen" result="crg"></feBlend>
      <feBlend in="crg" in2="cb" mode="screen" result="crgb"></feBlend>
      <feGaussianBlur in="crgb" stdDeviation="0.35"></feGaussianBlur>
    </filter>`;
  document.body.appendChild(svg);

  const feImage = svg.querySelector("feImage");
  const passes = [...svg.querySelectorAll("feDisplacementMap")];

  let opts = {
    radius: 18, bezel: 14, blur: 14,
    sheen: true, aberration: true,
    // Channel separation, as a fraction of the bezel.
    dispersion: 0.16,
    // Which edge the panel is welded to. The blur is feathered away from it, so
    // the filter has no visible cut-off line.
    feather: null,
    ...options,
  };

  /**
   * The backdrop filter lives on its own layer rather than on the element.
   *
   * `backdrop-filter` is clipped hard to the element's box: even with no border
   * drawn, the blur simply stops on a straight line, and that edge is what reads
   * as a crude division. A filter on a masked child can be faded out instead, so
   * the glass dissolves into the surface behind it.
   */
  const surfaceLayer = document.createElement("div");
  surfaceLayer.className = "glass-surface";
  node.prepend(surfaceLayer);

  /**
   * Spherical aberration. Real glass does not blur its backdrop evenly: the
   * further from the centre of the lens, the more out of focus it goes. A single
   * uniform `blur()` is exactly what makes a panel read as frosted plastic — so a
   * second, heavier blur is layered in and masked to the rim.
   */
  let aberration = null;
  if (opts.aberration) {
    aberration = document.createElement("div");
    aberration.className = "glass-aberration";
    node.prepend(aberration);
  }

  function rebuild() {
    const { width, height } = node.getBoundingClientRect();
    if (width < 4 || height < 4) return;

    // The filter region does not follow the element, so it is sized by hand.
    const { url, scale } = displacementMap(width, height, opts.radius, opts.bezel);
    feImage.setAttribute("href", url);
    feImage.setAttribute("width", String(width));
    feImage.setAttribute("height", String(height));
    // Blue bends hardest, red least — the ordering that produces a warm fringe on
    // one side of an edge and a cool one on the other, as a real lens does.
    //
    // Dispersion is a SUB-PIXEL effect. Scaling it with the bezel put the three
    // channel passes ~6px apart, which stops reading as a colour fringe and
    // starts reading as three offset copies of the panel — the "stacked bars"
    // this looked like. Kept absolute and small.
    const spread = Math.min(opts.dispersion ?? 1, 1.6);
    passes[0].setAttribute("scale", String(scale));
    passes[1].setAttribute("scale", String(scale + spread));
    passes[2].setAttribute("scale", String(scale + spread * 2));

    // Saturation is kept near 1: pushing it is what gives cheap glass its lurid
    // rainbow edge. Real glass tints its backdrop barely at all.
    surfaceLayer.style.backdropFilter =
      `blur(${opts.blur}px) saturate(1.12) brightness(1.04) url(#${id})`;
    surfaceLayer.style.webkitBackdropFilter =
      `blur(${opts.blur}px) saturate(1.12) brightness(1.04)`;

    // `feather` names the edge where the glass should DISSOLVE — the one facing
    // the content. The mask must therefore reach zero at that edge; having it
    // opaque there puts the filter at full strength exactly on the boundary,
    // which is the hard line it was meant to remove.
    const fades = {
      bottom: "linear-gradient(to bottom, #000 0%, #000 40%, rgba(0,0,0,0.5) 76%, transparent 100%)",
      top: "linear-gradient(to top, #000 0%, #000 44%, rgba(0,0,0,0.5) 78%, transparent 100%)",
      right: "linear-gradient(to right, #000 0%, #000 52%, rgba(0,0,0,0.45) 84%, transparent 100%)",
    };
    const fade = fades[opts.feather] ?? "";
    surfaceLayer.style.mask = fade;
    surfaceLayer.style.webkitMaskImage = fade;

    if (aberration) {
      const soft = Math.round(opts.blur * 1.9);
      aberration.style.backdropFilter = `blur(${soft}px)`;
      // Sharp through the middle, softening only as the surface curves away.
      const fade = `radial-gradient(ellipse 76% 76% at 50% 46%, transparent 34%, #000 100%)`;
      aberration.style.mask = fade;
      aberration.style.webkitMaskImage = fade;
    }
  }

  const ro = new ResizeObserver(rebuild);
  ro.observe(node);
  rebuild();

  /**
   * The specular highlight, as a droplet rather than a spotlight.
   *
   * A gradient painted at the pointer's exact position can only ever read as a
   * circle being dragged around. Real liquid does two things instead: it *lags*
   * behind whatever is pulling it, and it *stretches* along the direction of
   * travel, rounding out again as it settles. Both need a per-frame simulation,
   * so the highlight is a real element driven by requestAnimationFrame:
   *
   *   position — eased towards the pointer, so it trails and catches up
   *   shape    — elongated along its own velocity, proportional to speed
   *
   * The loop only runs while the droplet is still moving, so a still pointer
   * costs nothing.
   */
  let drop = null;
  let raf = 0;
  const p = { x: 0, y: 0, tx: 0, ty: 0, vx: 0, vy: 0, seeded: false };

  if (opts.sheen) {
    drop = document.createElement("div");
    drop.className = "glass-drop";
    node.appendChild(drop);
  }

  function tick() {
    // Attraction: a fraction of the remaining distance each frame gives the
    // trailing, elastic feel. Higher = snappier, lower = more syrupy.
    const pull = 0.13;
    const nx = p.x + (p.tx - p.x) * pull;
    const ny = p.y + (p.ty - p.y) * pull;

    p.vx = nx - p.x;
    p.vy = ny - p.y;
    p.x = nx;
    p.y = ny;

    const speed = Math.hypot(p.vx, p.vy);
    // Surface tension: stretch along travel, thin across it, conserving area so
    // the droplet does not appear to grow as it moves.
    const stretch = Math.min(speed * 0.026, 0.62);
    const angle = speed > 0.12 ? Math.atan2(p.vy, p.vx) : 0;

    drop.style.transform =
      `translate(${p.x}px, ${p.y}px) rotate(${angle}rad) ` +
      `scale(${1 + stretch}, ${1 / (1 + stretch * 0.72)})`;

    if (speed > 0.05 || Math.hypot(p.tx - p.x, p.ty - p.y) > 0.5) {
      raf = requestAnimationFrame(tick);
    } else {
      raf = 0;
    }
  }

  function onMove(e) {
    const r = node.getBoundingClientRect();
    p.tx = e.clientX - r.left;
    p.ty = e.clientY - r.top;
    if (!p.seeded) {
      // Appear where the pointer entered instead of sliding in from the corner.
      p.x = p.tx;
      p.y = p.ty;
      p.seeded = true;
    }
    if (!raf) raf = requestAnimationFrame(tick);
  }

  function onLeave() {
    p.seeded = false;
  }

  if (opts.sheen) {
    node.addEventListener("pointermove", onMove);
    node.addEventListener("pointerleave", onLeave);
  }

  return {
    update(next) {
      opts = { ...opts, ...next };
      rebuild();
    },
    destroy() {
      ro.disconnect();
      if (raf) cancelAnimationFrame(raf);
      node.removeEventListener("pointermove", onMove);
      node.removeEventListener("pointerleave", onLeave);
      drop?.remove();
      aberration?.remove();
      surfaceLayer.remove();
      svg.remove();
    },
  };
}

/**
 * One light for the whole window.
 *
 * The per-panel highlight only exists where there is glass, which left the middle
 * of the app dead. This one lives *behind* everything instead: it follows the
 * pointer across the entire window, and because it is painted below the panels,
 * the glass refracts and disperses it as it passes underneath. The chrome is lit
 * by the same source as the content rather than each surface carrying its own.
 *
 * Same physics as the panel droplet — trails, stretches along its velocity,
 * settles round — driven from window-level pointer events.
 */
export function ambient(node) {
  const p = { x: innerWidth / 2, y: innerHeight / 3, tx: 0, ty: 0, vx: 0, vy: 0, seeded: false };
  let raf = 0;

  function tick() {
    const pull = 0.085; // slower than the panel droplet: a big mass moves lazily
    const nx = p.x + (p.tx - p.x) * pull;
    const ny = p.y + (p.ty - p.y) * pull;

    p.vx = nx - p.x;
    p.vy = ny - p.y;
    p.x = nx;
    p.y = ny;

    const speed = Math.hypot(p.vx, p.vy);
    const stretch = Math.min(speed * 0.02, 0.7);
    const angle = speed > 0.15 ? Math.atan2(p.vy, p.vx) : 0;

    node.style.transform =
      `translate3d(${p.x}px, ${p.y}px, 0) rotate(${angle}rad) ` +
      `scale(${1 + stretch}, ${1 / (1 + stretch * 0.66)})`;

    if (speed > 0.06 || Math.hypot(p.tx - p.x, p.ty - p.y) > 0.6) {
      raf = requestAnimationFrame(tick);
    } else {
      raf = 0;
    }
  }

  function onMove(e) {
    p.tx = e.clientX;
    p.ty = e.clientY;
    if (!p.seeded) {
      p.seeded = true;
      node.style.opacity = "1";
    }
    if (!raf) raf = requestAnimationFrame(tick);
  }

  window.addEventListener("pointermove", onMove, { passive: true });
  tick();

  return {
    destroy() {
      window.removeEventListener("pointermove", onMove);
      if (raf) cancelAnimationFrame(raf);
    },
  };
}
