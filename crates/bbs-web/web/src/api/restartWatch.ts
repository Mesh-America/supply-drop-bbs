// Detects that the service has restarted, for the Backups page's restore flow.
//
// The public health endpoint reports a random per-process `boot_id`. Read it
// just before triggering the restore (`fetchBootId`), then poll until a
// different one answers (`watchForRestart`). That is immune to things that
// also change while waiting: an expired session or a logout in another tab
// (401s), a reverse proxy answering 502/503/504 while the service is down, or
// a restart so quick the old process never looks down.
//
// If the id could not be read beforehand, fall back to "the service was down
// for at least two polls in a row and then answered again".

const HEALTH_URL = '/api/v1/health'
const REQUEST_TIMEOUT_MS = 5000

interface Health {
  status?: string
  boot_id?: string
}

async function getHealth(
  fetchFn: typeof fetch,
  timeoutMs: number = REQUEST_TIMEOUT_MS
): Promise<Health | null> {
  // A hung connection must not freeze the poll or the elapsed counter.
  const ctl = new AbortController()
  const timer = setTimeout(() => ctl.abort(), timeoutMs)
  try {
    const res = await fetchFn(HEALTH_URL, {
      cache: 'no-store',
      signal: ctl.signal,
    })
    if (!res.ok) return null
    return (await res.json()) as Health
  } catch {
    return null
  } finally {
    clearTimeout(timer)
  }
}

/** The running process's boot id, or null if it can't be read. */
export async function fetchBootId(fetchFn: typeof fetch = fetch): Promise<string | null> {
  const health = await getHealth(fetchFn)
  return typeof health?.boot_id === 'string' && health.boot_id ? health.boot_id : null
}

export interface RestartWatchOptions {
  /** Boot id read before the restore was triggered, if it could be read. */
  baselineBootId: string | null
  /** Called once, when the restarted service answers. */
  onRestarted: () => void
  /**
   * Called before every poll with the seconds waited so far and whether the
   * previous poll was answered by the same process as `baselineBootId`.
   */
  onTick?: (elapsedSeconds: number, sameProcessAnswering: boolean) => void
  intervalMs?: number
  requestTimeoutMs?: number
  fetchFn?: typeof fetch
}

export function watchForRestart(opts: RestartWatchOptions): () => void {
  const interval = opts.intervalMs ?? 1500
  const doFetch = opts.fetchFn ?? fetch
  const started = Date.now()
  let stopped = false
  let inFlight = false
  // Consecutive failed polls, and the longest such run. Without a baseline id
  // a single failed poll can be a proxy blip; a real restart takes several.
  let downStreak = 0
  let longestDown = 0
  let sameProcess = false

  const stop = () => {
    stopped = true
    clearInterval(timer)
  }

  const timer = setInterval(async () => {
    // A slow answer must not stack up further polls.
    if (stopped || inFlight) return
    inFlight = true
    opts.onTick?.(Math.floor((Date.now() - started) / 1000), sameProcess)
    try {
      const health = await getHealth(doFetch, opts.requestTimeoutMs)
      if (stopped) return
      if (health === null) {
        downStreak++
        longestDown = Math.max(longestDown, downStreak)
        sameProcess = false
        return
      }
      downStreak = 0
      sameProcess =
        opts.baselineBootId !== null && health.boot_id === opts.baselineBootId
      const restarted =
        opts.baselineBootId !== null
          ? typeof health.boot_id === 'string' && health.boot_id !== opts.baselineBootId
          : longestDown >= 2
      if (restarted) {
        stop()
        opts.onRestarted()
      }
    } finally {
      inFlight = false
    }
  }, interval)

  return stop
}
