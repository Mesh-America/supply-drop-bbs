import { ref } from 'vue'
import { defineStore } from 'pinia'
import { api } from '../api/client'

interface Stats {
  pending_users: number
  active_users: number
  active_sessions: number
  [key: string]: unknown
}

export const useStatsStore = defineStore('stats', () => {
  const pendingUsers = ref(0)
  const activeUsers = ref(0)
  const activeSessions = ref(0)
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
    } catch {
      // non-fatal — badge just won't update
    }
  }

  function clearErrorAlerts() {
    errorAlerts.value = 0
  }

  function startPolling() {
    if (pollTimer !== null) return
    refresh()
    // Slow background poll as fallback in case SSE misses an event.
    pollTimer = setInterval(refresh, 120_000)

    // Subscribe to domain events for immediate badge updates.
    if (eventSource === null) {
      eventSource = new EventSource('/api/v1/sse/events')
      eventSource.addEventListener('user_created', () => refresh())
      eventSource.addEventListener('user_validated', () => refresh())
      eventSource.onerror = () => {
        // EventSource reconnects automatically; nothing to do here.
      }
    }

    // Subscribe to error alerts for the errors-page badge.
    if (errorSource === null) {
      errorSource = new EventSource('/api/v1/sse/errors')
      errorSource.addEventListener('error_alert', () => { errorAlerts.value++ })
      errorSource.onerror = () => { /* auto-reconnects */ }
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
      rssSource.onerror = () => { /* auto-reconnects */ }
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
    errorAlerts,
    rssAlertActive,
    rssGrowthBytes,
    refresh,
    clearErrorAlerts,
    startPolling,
    stopPolling,
  }
})
