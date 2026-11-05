/**
 * Svelte action: horizontal swipe on a list row, without stealing the scroll.
 *
 *   <li use:swipeable on:swipe={…} on:swipeend={…}>
 *
 * Direction is locked on the first real movement: past the deadzone, whichever
 * axis is dominant wins for the rest of the gesture. A list where a slightly
 * diagonal flick either scrolls *and* swipes, or does neither, is the usual way
 * this feature is got wrong - and the row must stay scrollable, because it is
 * the whole library.
 *
 * The node needs `touch-action: pan-y`: the browser keeps vertical panning, we
 * take horizontal. Without it the gesture fights the scroller instead of
 * cooperating with it.
 *
 * Events:
 *   swipe     {dx}                 live offset, for drawing the row
 *   swipeend  {dx, dir, commit}    released; commit means past the threshold
 */
const DEADZONE = 6; // px before a gesture has a direction at all
const THRESHOLD = 72; // px to count as a committed swipe
const MAX = 112; // px the row can travel, so it always springs back

export function swipeable(node, options = {}) {
  let { threshold = THRESHOLD, max = MAX, enabled = true } = options;

  let id = null;
  let startX = 0;
  let startY = 0;
  let axis = null; // null until locked, then 'x' or 'y'
  let dx = 0;

  const emit = (name, detail) =>
    node.dispatchEvent(new CustomEvent(name, { detail }));

  const reset = () => {
    id = null;
    axis = null;
    dx = 0;
  };

  const onDown = (e) => {
    if (!enabled || (e.pointerType === "mouse" && e.button !== 0)) return;
    id = e.pointerId;
    startX = e.clientX;
    startY = e.clientY;
    axis = null;
    dx = 0;
  };

  const onMove = (e) => {
    if (id !== e.pointerId) return;
    const ax = e.clientX - startX;
    const ay = e.clientY - startY;

    if (axis === null) {
      if (Math.abs(ax) < DEADZONE && Math.abs(ay) < DEADZONE) return;
      axis = Math.abs(ax) > Math.abs(ay) ? "x" : "y";
      if (axis === "x") {
        // Take the gesture only once it is definitely ours, so a scroll that
        // began with a wobble is still a scroll.
        node.setPointerCapture?.(e.pointerId);
      }
    }
    if (axis !== "x") return;

    // Resistance past the maximum: the row still answers the finger, but it is
    // obvious it has run out of travel rather than feeling stuck.
    const raw = ax;
    dx =
      Math.abs(raw) <= max
        ? raw
        : Math.sign(raw) * (max + (Math.abs(raw) - max) * 0.15);
    emit("swipe", { dx });
  };

  const onUp = (e) => {
    if (id !== e.pointerId) return;
    if (axis === "x") {
      emit("swipeend", {
        dx,
        dir: dx > 0 ? "right" : "left",
        commit: Math.abs(dx) >= threshold,
      });
    }
    node.releasePointerCapture?.(e.pointerId);
    reset();
  };

  const onCancel = (e) => {
    if (id !== e.pointerId) return;
    if (axis === "x") emit("swipeend", { dx: 0, dir: null, commit: false });
    reset();
  };

  // A swipe must never also count as a tap: releasing a dragged row would
  // otherwise start playing it.
  const onClickCapture = (e) => {
    if (axis === "x" || Math.abs(dx) > DEADZONE) {
      e.preventDefault();
      e.stopPropagation();
    }
  };

  node.addEventListener("pointerdown", onDown);
  node.addEventListener("pointermove", onMove);
  node.addEventListener("pointerup", onUp);
  node.addEventListener("pointercancel", onCancel);
  node.addEventListener("click", onClickCapture, true);

  return {
    update(next = {}) {
      ({ threshold = THRESHOLD, max = MAX, enabled = true } = next);
    },
    destroy() {
      node.removeEventListener("pointerdown", onDown);
      node.removeEventListener("pointermove", onMove);
      node.removeEventListener("pointerup", onUp);
      node.removeEventListener("pointercancel", onCancel);
      node.removeEventListener("click", onClickCapture, true);
    },
  };
}
