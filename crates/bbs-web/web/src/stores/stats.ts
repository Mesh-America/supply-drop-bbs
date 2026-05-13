import { ref } from 'vue'
import { defineStore } from 'pinia'
import { api } from '../api/client'

interface Stats {
  pending_users: number
  [key: string]: unknown
}

export const useStatsStore = defineStore('stats', () => {
  const pendingUsers = ref(0)
  const errorAlerts = ref(0)
  let pollTimer: ReturnType<typeof setInterval> | null = null
  let eventSource: EventSource | null = null
  let errorSource: EventSource | null = null

  async function refresh() {
    try {
      const s = await api.get<Stats>('/api/v1/stats')
      pendingUsers.value = s.pending_users
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
  }

  function stopPolling() {
    if (pollTimer !== null) { clearInterval(pollTimer); pollTimer = null }
    if (eventSource !== null) { eventSource.close(); eventSource = null }
    if (errorSource !== null) { errorSource.close(); errorSource = null }
  }

  return { pendingUsers, errorAlerts, refresh, clearErrorAlerts, startPolling, stopPolling }
})
