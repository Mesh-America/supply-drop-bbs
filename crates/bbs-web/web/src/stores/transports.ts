import { ref } from 'vue'
import { defineStore } from 'pinia'
import { api } from '../api/client'

interface Transports {
  meshcore: boolean
  meshtastic: boolean
}

export const useTransportsStore = defineStore('transports', () => {
  const meshcore = ref(false)
  const meshtastic = ref(false)

  async function refresh() {
    try {
      const t = await api.get<Transports>('/api/v1/transports')
      meshcore.value = t.meshcore
      meshtastic.value = t.meshtastic
    } catch {
      // non-fatal — nav items just won't filter
    }
  }

  return { meshcore, meshtastic, refresh }
})
