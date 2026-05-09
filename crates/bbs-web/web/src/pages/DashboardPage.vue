<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { api } from '../api/client'

interface Status {
  version: string
  uptime_secs: number
}

interface Stats {
  active_users: number
  pending_users: number
  banned_users: number
  total_messages: number
  total_rooms: number
  active_sessions: number
}

const status = ref<Status | null>(null)
const stats = ref<Stats | null>(null)
const error = ref<string | null>(null)

function fmtUptime(secs: number): string {
  const h = Math.floor(secs / 3600)
  const m = Math.floor((secs % 3600) / 60)
  const s = secs % 60
  if (h > 0) return `${h}h ${m}m`
  if (m > 0) return `${m}m ${s}s`
  return `${s}s`
}

onMounted(async () => {
  try { status.value = await api.get<Status>('/api/v1/status') } catch (e: any) { error.value = e?.message ?? 'failed to load status' }
  try { stats.value = await api.get<Stats>('/api/v1/stats') } catch (e: any) { if (!error.value) error.value = e?.message ?? 'failed to load stats' }
})
</script>

<template>
  <div class="page">
    <header class="page-header">
      <h1>dashboard</h1>
      <p class="muted">supply drop bbs — admin panel</p>
    </header>

    <p v-if="error" class="error">{{ error }}</p>

    <section class="stat-row">
      <div class="stat-card" v-if="status">
        <div class="stat-label">version</div>
        <div class="stat-value">{{ status.version }}</div>
      </div>
      <div class="stat-card" v-if="status">
        <div class="stat-label">uptime</div>
        <div class="stat-value">{{ fmtUptime(status.uptime_secs) }}</div>
      </div>
      <div class="stat-card" v-if="stats">
        <div class="stat-label">active users</div>
        <div class="stat-value">{{ stats.active_users }}</div>
      </div>
      <div class="stat-card" v-if="stats">
        <div class="stat-label">pending</div>
        <div class="stat-value">{{ stats.pending_users }}</div>
      </div>
      <div class="stat-card" v-if="stats">
        <div class="stat-label">messages</div>
        <div class="stat-value">{{ stats.total_messages }}</div>
      </div>
      <div class="stat-card" v-if="stats">
        <div class="stat-label">rooms</div>
        <div class="stat-value">{{ stats.total_rooms }}</div>
      </div>
      <div class="stat-card" v-if="stats">
        <div class="stat-label">live sessions</div>
        <div class="stat-value">{{ stats.active_sessions }}</div>
      </div>
    </section>

    <section class="quick-links">
      <h2>quick actions</h2>
      <div class="link-grid">
        <router-link class="quick-link" to="/adverts">
          <span class="ql-title">adverts</span>
          <span class="ql-hint muted">nodes heard over the mesh</span>
        </router-link>
        <router-link class="quick-link" to="/sessions">
          <span class="ql-title">sessions</span>
          <span class="ql-hint muted">active BBS connections</span>
        </router-link>
        <router-link class="quick-link" to="/users">
          <span class="ql-title">users</span>
          <span class="ql-hint muted">manage BBS accounts</span>
        </router-link>
        <router-link class="quick-link" to="/rooms">
          <span class="ql-title">rooms</span>
          <span class="ql-hint muted">manage message rooms</span>
        </router-link>
        <router-link class="quick-link" to="/reports">
          <span class="ql-title">reports</span>
          <span class="ql-hint muted">aggregate statistics</span>
        </router-link>
        <router-link class="quick-link" to="/backups">
          <span class="ql-title">backups</span>
          <span class="ql-hint muted">database snapshots</span>
        </router-link>
        <router-link class="quick-link" to="/logs">
          <span class="ql-title">logs</span>
          <span class="ql-hint muted">live event stream</span>
        </router-link>
      </div>
    </section>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 1.6rem; }
.page-header { display: flex; flex-direction: column; gap: 0.2rem; }
h1, h2 { margin: 0; }

.stat-row { display: flex; gap: 0.8rem; flex-wrap: wrap; }
.stat-card {
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 0.8rem 1.2rem;
  background: var(--bg);
  min-width: 120px;
}
.stat-label { font-size: 0.75em; color: var(--muted); text-transform: uppercase; letter-spacing: 0.06em; margin-bottom: 0.3rem; }
.stat-value { font-size: 1.4em; font-weight: 600; }

.quick-links { display: flex; flex-direction: column; gap: 0.7rem; }
.link-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); gap: 0.7rem; }
.quick-link {
  display: flex; flex-direction: column; gap: 0.25rem;
  padding: 0.8rem 1rem;
  border: 1px solid var(--border); border-radius: 4px;
  background: var(--row-alt); color: var(--fg);
  transition: border-color 0.1s, background 0.1s;
}
.quick-link:hover { border-color: var(--accent); background: var(--accent-bg); text-decoration: none; }
.ql-title { font-weight: 600; font-size: 0.95em; }
.ql-hint { font-size: 0.8em; }
</style>
