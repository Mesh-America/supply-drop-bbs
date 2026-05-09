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

interface TopSender {
  username: string
  message_count: number
}

interface TopRoom {
  room_id: number
  room_name: string
  message_count: number
}

interface DailyVolume {
  day: string
  count: number
}

interface StaleRoom {
  room_id: number
  room_name: string
  last_message_at: string | null
}

interface Reports {
  top_senders: TopSender[]
  top_rooms: TopRoom[]
  daily_volume: DailyVolume[]
  stale_rooms: StaleRoom[]
}

const stats = ref<Stats | null>(null)
const reports = ref<Reports | null>(null)
const loading = ref(false)
const error = ref<string | null>(null)

async function load() {
  loading.value = true
  error.value = null
  try {
    const [s, r] = await Promise.all([
      api.get<Stats>('/api/v1/stats'),
      api.get<Reports>('/api/v1/reports'),
    ])
    stats.value = s
    reports.value = r
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load reports'
  } finally {
    loading.value = false
  }
}

function maxCount(items: { message_count: number }[]) {
  return items.reduce((m, i) => Math.max(m, i.message_count), 1)
}

function maxDaily(items: DailyVolume[]) {
  return items.reduce((m, i) => Math.max(m, i.count), 1)
}

function pct(n: number, max: number) {
  return Math.round((n / max) * 100)
}

function fmtDate(iso: string | null) {
  if (!iso) return 'never'
  return iso.slice(0, 10)
}

onMounted(load)
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>reports</h1>
        <p class="muted">Aggregate BBS analytics</p>
      </div>
      <button class="secondary" @click="load" :disabled="loading">refresh</button>
    </header>

    <p v-if="error" class="error">{{ error }}</p>
    <p v-if="loading" class="muted">Loading…</p>

    <!-- System stats -->
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

    <div v-if="reports" class="analytics-grid">
      <!-- Top senders -->
      <section class="panel">
        <h2 class="panel-title">top senders</h2>
        <div v-if="reports.top_senders.length === 0" class="muted empty">no data</div>
        <div v-for="s in reports.top_senders" :key="s.username" class="bar-row">
          <span class="bar-label">{{ s.username }}</span>
          <div class="bar-track">
            <div class="bar-fill" :style="{ width: pct(s.message_count, maxCount(reports.top_senders)) + '%' }"></div>
          </div>
          <span class="bar-val">{{ s.message_count }}</span>
        </div>
      </section>

      <!-- Top rooms -->
      <section class="panel">
        <h2 class="panel-title">top rooms</h2>
        <div v-if="reports.top_rooms.length === 0" class="muted empty">no data</div>
        <div v-for="r in reports.top_rooms" :key="r.room_id" class="bar-row">
          <span class="bar-label">{{ r.room_name }}</span>
          <div class="bar-track">
            <div class="bar-fill" :style="{ width: pct(r.message_count, maxCount(reports.top_rooms)) + '%' }"></div>
          </div>
          <span class="bar-val">{{ r.message_count }}</span>
        </div>
      </section>

      <!-- Daily volume (last 30 days) -->
      <section class="panel panel-wide">
        <h2 class="panel-title">daily volume <span class="panel-sub">(last 30 days)</span></h2>
        <div v-if="reports.daily_volume.length === 0" class="muted empty">no messages in this period</div>
        <div v-else class="spark-wrap">
          <div
            v-for="d in reports.daily_volume"
            :key="d.day"
            class="spark-col"
            :title="`${d.day}: ${d.count}`"
          >
            <div class="spark-bar" :style="{ height: pct(d.count, maxDaily(reports.daily_volume)) + '%' }"></div>
          </div>
        </div>
        <div v-if="reports.daily_volume.length" class="spark-labels">
          <span>{{ reports.daily_volume[0].day.slice(5) }}</span>
          <span>{{ reports.daily_volume[reports.daily_volume.length - 1].day.slice(5) }}</span>
        </div>
      </section>

      <!-- Stale rooms -->
      <section class="panel">
        <h2 class="panel-title">stale rooms <span class="panel-sub">(no activity in 30+ days)</span></h2>
        <div v-if="reports.stale_rooms.length === 0" class="muted empty">all rooms active</div>
        <table v-else class="data-table">
          <thead><tr><th>room</th><th>last message</th></tr></thead>
          <tbody>
            <tr v-for="r in reports.stale_rooms" :key="r.room_id">
              <td>{{ r.room_name }}</td>
              <td class="muted">{{ fmtDate(r.last_message_at) }}</td>
            </tr>
          </tbody>
        </table>
      </section>
    </div>
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

.analytics-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 1.25rem;
}
.panel-wide { grid-column: 1 / -1; }

.panel {
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 1rem 1.2rem;
  background: var(--bg);
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
}
.panel-title {
  font-size: 0.78em;
  text-transform: uppercase;
  letter-spacing: 0.07em;
  color: var(--muted);
  margin: 0 0 0.2rem;
}
.panel-sub { font-weight: 400; text-transform: none; letter-spacing: 0; }
.empty { font-size: 0.88em; }

/* Horizontal bar chart */
.bar-row { display: flex; align-items: center; gap: 0.5rem; font-size: 0.85em; }
.bar-label { min-width: 90px; max-width: 120px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.bar-track { flex: 1; height: 10px; background: var(--row-alt); border-radius: 2px; overflow: hidden; }
.bar-fill { height: 100%; background: var(--accent); border-radius: 2px; transition: width 0.3s; }
.bar-val { min-width: 30px; text-align: right; color: var(--muted); }

/* Spark chart */
.spark-wrap {
  display: flex;
  align-items: flex-end;
  gap: 2px;
  height: 80px;
}
.spark-col { flex: 1; display: flex; align-items: flex-end; height: 100%; }
.spark-bar {
  width: 100%;
  background: var(--accent);
  border-radius: 1px 1px 0 0;
  min-height: 2px;
  transition: height 0.3s;
}
.spark-labels {
  display: flex;
  justify-content: space-between;
  font-size: 0.72em;
  color: var(--muted);
}

/* Data table */
.data-table { width: 100%; border-collapse: collapse; font-size: 0.875em; }
.data-table th { text-align: left; font-size: 0.75em; text-transform: uppercase; letter-spacing: 0.05em; color: var(--muted); padding: 0 0 0.4rem; border-bottom: 1px solid var(--border); }
.data-table td { padding: 0.35rem 0; border-bottom: 1px solid var(--border); }
.data-table tr:last-child td { border-bottom: none; }
</style>
