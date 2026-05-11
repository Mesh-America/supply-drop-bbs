<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import { api } from '../api/client'

interface Session {
  session_id: number
  transport: string
  username: string | null
  permission_level: number
}

const sessions = ref<Session[]>([])
const loading = ref(false)
const error = ref<string | null>(null)

function levelLabel(l: number): string {
  if (l >= 100) return 'sysop'
  if (l >= 50) return 'aide'
  if (l >= 10) return 'user'
  return 'unvalidated'
}

async function load() {
  loading.value = true
  error.value = null
  try {
    sessions.value = await api.get<Session[]>('/api/v1/sessions')
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load sessions'
  } finally {
    loading.value = false
  }
}

let timer: ReturnType<typeof setInterval> | null = null
onMounted(() => { load(); timer = setInterval(load, 10_000) })
onUnmounted(() => { if (timer !== null) clearInterval(timer) })
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>sessions</h1>
        <p class="muted">Active BBS connections across all transports</p>
      </div>
    </header>

    <p v-if="error" class="error">{{ error }}</p>
    <p v-if="!loading && sessions.length === 0 && !error" class="muted">No active sessions.</p>

    <table v-if="sessions.length > 0">
      <thead>
        <tr>
          <th>session id</th>
          <th>transport</th>
          <th>username</th>
          <th>level</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="s in sessions" :key="s.session_id">
          <td><code>{{ s.session_id }}</code></td>
          <td>{{ s.transport }}</td>
          <td>{{ s.username ?? '—' }}</td>
          <td>{{ levelLabel(s.permission_level) }}</td>
        </tr>
      </tbody>
    </table>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 1rem; }
.page-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; flex-wrap: wrap; }
.page-header div { display: flex; flex-direction: column; gap: 0.2rem; }
h1 { margin: 0; }
p { margin: 0; }
</style>
