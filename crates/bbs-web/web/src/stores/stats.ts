import { ref } from 'vue'
import { defineStore } from 'pinia'
import { api } from '../api/client'

interface Stats {
  pending_users: number
  active_users: number
  active_sessions: number
  discovered_contacts: number
  protected_contacts: number
  [key: string]: unknown
}

export const useStatsStore = defineStore('stats', () => {
  const pendingUsers = ref(0)
  const activeUsers = ref(0)
  const activeSessions = ref(0)
  const discoveredContacts = ref(0)
  const protectedContacts = ref(0)
  const errorAlerts = ref(0)
  const rssAlertActive = ref(false)
  const rssGrowthBytes = ref(0)
  let pollTimer: ReturnType<typeof setInterval> | null = null
  let eventSource: EventSource | null = null
  let errorSource: EventSource | null = null
  let rssSource: EventSource | null = null

  async function refresh() {
    try {
      const s = await api.get<Stats>('/api/v1/stats')
      pendingUsers.value = s.pending_users
      activeUsers.value = s.active_users
      activeSessions.value = s.active_sessions
      discoveredContacts.value = s.discovered_contacts
      protectedContacts.value = s.protected_contacts
    } catch {
      // non-fatal — badge just won't update
      return
    }
    // The session is good, so any feed that gave up can be opened again.
    if (pollTimer !== null) openFeeds()
  }

  function clearErrorAlerts() {
    errorAlerts.value = 0
  }

  // EventSource reconnects on its own after a dropped connection. It gives
  // up (readyState CLOSED) only when a reconnect is refused outright: a 401
  // once the server has ended the session behind it, or a 503 while the
  // server can't check it. Either way the feed is dropped and a refresh
  // asked for: a 401 there sends the user to sign in, success opens the
  // feeds again, and after a 503 the next poll or click will.
  function feedGaveUp(which: 'events' | 'errors' | 'rss') {
    const source = which === 'events' ? eventSource : which === 'errors' ? errorSource : rssSource
    if (source?.readyState !== EventSource.CLOSED) return
    source.close()
    if (which === 'events') eventSource = null
    else if (which === 'errors') errorSource = null
    else rssSource = null
    refresh()
  }

  function startPolling() {
    if (pollTimer !== null) return
    // Slow background poll as fallback in case SSE misses an event.
    pollTimer = setInterval(refresh, 120_000)
    openFeeds()
    refresh()
  }

  // Open whichever live feeds aren't open. Safe to call repeatedly.
  function openFeeds() {
    // Subscribe to domain events for immediate badge updates.
    if (eventSource === null) {
      eventSource = new EventSource('/api/v1/sse/events')
      eventSource.addEventListener('user_created', () => refresh())
      eventSource.addEventListener('user_validated', () => refresh())
      eventSource.onerror = () => feedGaveUp('events')
    }

    // Subscribe to error alerts for the errors-page badge.
    if (errorSource === null) {
      errorSource = new EventSource('/api/v1/sse/errors')
      errorSource.addEventListener('error_alert', () => { errorAlerts.value++ })
      errorSource.onerror = () => feedGaveUp('errors')
    }

    // Subscribe to RSS growth alerts for the metrics-page badge.
    if (rssSource === null) {
      rssSource = new EventSource('/api/v1/sse/rss-alert')
      rssSource.addEventListener('rss_alert', (e: MessageEvent) => {
        try {
          const data = JSON.parse(e.data)
          if (data.cleared) {
            rssAlertActive.value = false
            rssGrowthBytes.value = 0
          } else {
            rssAlertActive.value = true
            rssGrowthBytes.value = data.growth_bytes ?? 0
          }
        } catch { /* ignore malformed events */ }
      })
      rssSource.onerror = () => feedGaveUp('rss')
    }
  }

  function stopPolling() {
    if (pollTimer !== null) { clearInterval(pollTimer); pollTimer = null }
    if (eventSource !== null) { eventSource.close(); eventSource = null }
    if (errorSource !== null) { errorSource.close(); errorSource = null }
    if (rssSource !== null) { rssSource.close(); rssSource = null }
  }

  return {
    pendingUsers,
    activeUsers,
    activeSessions,
    discoveredContacts,
    protectedContacts,
    errorAlerts,
    rssAlertActive,
    rssGrowthBytes,
    refresh,
    clearErrorAlerts,
    startPolling,
    stopPolling,
  }
})
