/**
 * Svelte action: fire on a press-and-hold, and suppress the click that follows.
 *
 * Touch and mouse are handled together rather than via pointer events so a
 * long-press cannot also count as a tap. The guard matters here: holding a
 * track row must open its info, not start playing it, and without cancelling
 * the click the row would do both.
 *
 * Movement cancels, so a hold that turns into a scroll is a scroll. Android's
 * own list behaviour does the same and anything else feels broken on a phone.
 *
 *   <button use:longpress on:longpress={...}>
 */
const HOLD_MS = 450;
const MOVE_TOLERANCE = 10; // px before it counts as a scroll rather than a hold

export function longpress(node, { duration = HOLD_MS } = {}) {
  let timer = null;
  let fired = false;
  let startX = 0;
  let startY = 0;

  const clear = () => {
    clearTimeout(timer);
    timer = null;
  };

  const start = (x, y) => {
    fired = false;
    startX = x;
    startY = y;
    clear();
    timer = setTimeout(() => {
      fired = true;
      timer = null;
      // Haptics where available; a hold with no feedback feels like a dropped tap.
      if (navigator.vibrate) navigator.vibrate(12);
      node.dispatchEvent(new CustomEvent('longpress'));
    }, duration);
  };

  const moved = (x, y) =>
    Math.abs(x - startX) > MOVE_TOLERANCE || Math.abs(y - startY) > MOVE_TOLERANCE;

  const onTouchStart = (e) => start(e.touches[0].clientX, e.touches[0].clientY);
  const onTouchMove = (e) => {
    if (moved(e.touches[0].clientX, e.touches[0].clientY)) clear();
  };
  const onMouseDown = (e) => {
    if (e.button === 0) start(e.clientX, e.clientY);
  };

  // Runs before the click handler, so a completed hold can swallow it.
  const onClickCapture = (e) => {
    if (!fired) return;
    fired = false;
    e.preventDefault();
    e.stopPropagation();
  };

  node.addEventListener('touchstart', onTouchStart, { passive: true });
  node.addEventListener('touchmove', onTouchMove, { passive: true });
  node.addEventListener('touchend', clear);
  node.addEventListener('touchcancel', clear);
  node.addEventListener('mousedown', onMouseDown);
  node.addEventListener('mouseup', clear);
  node.addEventListener('mouseleave', clear);
  node.addEventListener('click', onClickCapture, true);
  // A hold on Android also raises the browser context menu; that is the OS
  // offering "copy text" over a row that has just opened its own sheet.
  node.addEventListener('contextmenu', (e) => e.preventDefault());

  return {
    destroy() {
      clear();
      node.removeEventListener('touchstart', onTouchStart);
      node.removeEventListener('touchmove', onTouchMove);
      node.removeEventListener('touchend', clear);
      node.removeEventListener('touchcancel', clear);
      node.removeEventListener('mousedown', onMouseDown);
      node.removeEventListener('mouseup', clear);
      node.removeEventListener('mouseleave', clear);
      node.removeEventListener('click', onClickCapture, true);
    },
  };
}
