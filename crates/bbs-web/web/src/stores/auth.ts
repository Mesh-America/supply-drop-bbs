import { defineStore } from 'pinia'
import { api } from '../api/client'

export interface User {
  username: string
  is_sysop: boolean
  permission_level: number
}

export const useAuthStore = defineStore('auth', {
  state: () => ({
    user: null as User | null,
    loading: false,
  }),
  getters: {
    isSysop: (state) => (state.user?.permission_level ?? 0) >= 100,
    isAide:  (state) => (state.user?.permission_level ?? 0) >= 50,
  },
  actions: {
    async whoami() {
      this.loading = true
      try {
        this.user = await api.get<User>('/api/v1/auth/whoami')
      } finally {
        this.loading = false
      }
    },
    async login(username: string, password: string) {
      const result = await api.post<{ ok: boolean; username: string; permission_level: number }>(
        '/api/v1/auth/login',
        { username, password },
      )
      this.user = {
        username: result.username,
        is_sysop: result.permission_level >= 100,
        permission_level: result.permission_level,
      }
      return this.user
    },
    async logout() {
      try {
        await api.post('/api/v1/auth/logout')
      } finally {
        this.user = null
      }
    },
  },
})
