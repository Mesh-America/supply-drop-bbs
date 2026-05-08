// Format a Unix timestamp (seconds) as local date+time.
export function fmtLocal(secs: number): string {
  if (!secs) return '—'
  return new Date(secs * 1000).toLocaleString()
}
