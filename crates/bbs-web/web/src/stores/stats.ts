import { ref } from 'vue'
import { defineStore } from 'pinia'
import { api } from '../api/client'

interface Stats {
  pending_users: number
  [key: string]: unknown
}

export const useStatsStore = defineStore('stats', () => {
  const pendingUsers = ref(0)
  let timer: ReturnType<typeof setInterval> | null = null

  async function refresh() {
    try {
      const s = await api.get<Stats>('/api/v1/stats')
      pendingUsers.value = s.pending_users
    } catch {
      // non-fatal — badge just won't update
    }
  }

  function startPolling() {
    if (timer !== null) return
    refresh()
    timer = setInterval(refresh, 60_000)
  }

  function stopPolling() {
    if (timer !== null) { clearInterval(timer); timer = null }
  }

  return { pendingUsers, refresh, startPolling, stopPolling }
})
