export function fmtTime(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) return '0:00';
  const total = Math.floor(seconds);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n) => String(n).padStart(2, '0');
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

export function plural(n, one, many) {
  return `${n} ${n === 1 ? one : many}`;
}

// Compact popularity, e.g. "7.6M plays". The precise figure is noise at the size
// this renders, and the useful fact is the order of magnitude that ranks two
// near-identical audiobook uploads against each other.
export function fmtPlays(n) {
  if (!n || n < 0) return '';
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)}B plays`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M plays`;
  if (n >= 1e3) return `${Math.round(n / 1e3)}K plays`;
  return `${n} plays`;
}
