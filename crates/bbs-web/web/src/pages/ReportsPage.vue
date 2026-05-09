<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { api } from '../api/client'

interface Stats {
  active_users: number
  pending_users: number
  banned_users: number
  total_messages: number
  total_rooms: number
  active_sessions: number
}

const stats = ref<Stats | null>(null)
const loading = ref(false)
const error = ref<string | null>(null)

async function load() {
  loading.value = true
  error.value = null
  try {
    stats.value = await api.get<Stats>('/api/v1/stats')
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load stats'
  } finally {
    loading.value = false
  }
}

onMounted(load)
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>reports</h1>
        <p class="muted">Aggregate BBS statistics</p>
      </div>
      <button class="secondary" @click="load" :disabled="loading">refresh</button>
    </header>

    <p v-if="error" class="error">{{ error }}</p>

    <section v-if="stats" class="stat-grid">
      <div class="stat-card">
        <div class="stat-label">active users</div>
        <div class="stat-value">{{ stats.active_users }}</div>
      </div>
      <div class="stat-card">
        <div class="stat-label">pending validation</div>
        <div class="stat-value warn">{{ stats.pending_users }}</div>
      </div>
      <div class="stat-card">
        <div class="stat-label">banned users</div>
        <div class="stat-value">{{ stats.banned_users }}</div>
      </div>
      <div class="stat-card">
        <div class="stat-label">total messages</div>
        <div class="stat-value">{{ stats.total_messages }}</div>
      </div>
      <div class="stat-card">
        <div class="stat-label">rooms</div>
        <div class="stat-value">{{ stats.total_rooms }}</div>
      </div>
      <div class="stat-card">
        <div class="stat-label">live sessions</div>
        <div class="stat-value">{{ stats.active_sessions }}</div>
      </div>
    </section>

    <p v-if="loading" class="muted">Loading…</p>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 1.5rem; }
.page-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; flex-wrap: wrap; }
.page-header div { display: flex; flex-direction: column; gap: 0.2rem; }
h1 { margin: 0; }
p { margin: 0; }

.stat-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(160px, 1fr)); gap: 1rem; }
.stat-card {
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 1rem 1.2rem;
  background: var(--bg);
}
.stat-label { font-size: 0.75em; color: var(--muted); text-transform: uppercase; letter-spacing: 0.06em; margin-bottom: 0.4rem; }
.stat-value { font-size: 2em; font-weight: 700; }
.stat-value.warn { color: var(--warning); }
</style>
