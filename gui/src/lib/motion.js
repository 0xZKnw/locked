/**
 * The app's motion vocabulary.
 *
 * Two rules hold everything below together. Motion explains *where a thing came
 * from* or *what changed* — nothing moves to decorate. And an element that
 * arrives and one that leaves are not the same event: arrivals get a little
 * travel, departures get out of the way quickly, because waiting on something
 * you have already dismissed is the one kind of animation people notice.
 *
 * `prefers-reduced-motion` is honoured here rather than in CSS. Svelte
 * transitions write inline styles from JavaScript, so the blanket media query in
 * app.css cannot reach them — without this, turning motion off in the OS would
 * silence the CSS half of the app and leave the rest moving.
 */

const reduced = () =>
  typeof window !== "undefined" &&
  window.matchMedia?.("(prefers-reduced-motion: reduce)")?.matches === true;

/** A duration, or none at all if the reader has asked for stillness. */
export const ms = (n) => (reduced() ? 0 : n);

export const QUICK = 150;
export const BASE = 240;
export const CALM = 380;

/** Decelerating: fast out of the gate, gentle at rest. Matches --ease in CSS. */
export const ease = (t) => 1 - Math.pow(1 - t, 3);

/**
 * A damped spring — overshoots by about eight percent, then settles.
 *
 * This is a decaying cosine rather than one of the stock elastic curves on
 * purpose. `elasticOut` oscillates several times, which reads as a toy; a single
 * overshoot and a small counter-swing reads as something with mass arriving. The
 * damping is deliberately steep so the whole thing is over before it becomes a
 * performance.
 *
 * The endpoint is pinned: the curve alone lands at ~0.993, and a chip that stops
 * a hair short of its size is worse than one that never bounced.
 */
export const springOut = (t) => (t === 0 || t === 1 ? t : 1 - Math.exp(-5 * t) * Math.cos(6.2 * t));

/**
 * A staggered delay that cannot run away.
 *
 * A hundred receipts at 25ms each would make the last one arrive two and a half
 * seconds late, which is not a stagger, it is a queue. The ramp stops paying out
 * after a dozen items.
 */
export const stagger = (i, step = 22, cap = 12) => ms(Math.min(i, cap) * step);
