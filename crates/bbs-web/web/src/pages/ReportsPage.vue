<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
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

// SVG chart geometry
const CHART_W = 600
const CHART_H = 140
const PAD = { top: 12, right: 12, bottom: 26, left: 36 }
const plotW = CHART_W - PAD.left - PAD.right
const plotH = CHART_H - PAD.top - PAD.bottom

const dailyPoints = computed(() => {
  const data = reports.value?.daily_volume ?? []
  if (data.length === 0) return []
  const maxVal = Math.max(...data.map(d => d.count), 1)
  const n = data.length
  return data.map((d, i) => ({
    x: PAD.left + (n > 1 ? (i / (n - 1)) * plotW : plotW / 2),
    y: PAD.top + plotH - (d.count / maxVal) * plotH,
    day: d.day,
    count: d.count,
  }))
})

const linePoints = computed(() =>
  dailyPoints.value.map(p => `${p.x},${p.y}`).join(' ')
)

const areaPoints = computed(() => {
  const pts = dailyPoints.value
  if (pts.length === 0) return ''
  const bottom = PAD.top + plotH
  return [
    `${pts[0].x},${bottom}`,
    ...pts.map(p => `${p.x},${p.y}`),
    `${pts[pts.length - 1].x},${bottom}`,
  ].join(' ')
})

const yTicks = computed(() => {
  const data = reports.value?.daily_volume ?? []
  const max = Math.max(...data.map(d => d.count), 1)
  return [0, Math.ceil(max / 2), max].map(v => ({
    y: PAD.top + plotH - (v / max) * plotH,
    label: String(v),
  }))
})

const xLabels = computed(() => {
  const data = reports.value?.daily_volume ?? []
  if (data.length === 0) return []
  const indices = data.length >= 3
    ? [0, Math.floor((data.length - 1) / 2), data.length - 1]
    : [0, data.length - 1]
  return [...new Set(indices)].map(i => ({
    x: PAD.left + (data.length > 1 ? (i / (data.length - 1)) * plotW : plotW / 2),
    label: data[i].day.slice(5),
  }))
})

const peakDay = computed(() => {
  const data = reports.value?.daily_volume ?? []
  if (data.length === 0) return null
  return data.reduce((best, d) => (d.count > best.count ? d : best), data[0])
})

const weekTrend = computed(() => {
  const data = reports.value?.daily_volume ?? []
  if (data.length < 14) return null
  const recent = data.slice(-7).reduce((s, d) => s + d.count, 0)
  const prior = data.slice(-14, -7).reduce((s, d) => s + d.count, 0)
  if (prior === 0) return null
  return Math.round(((recent - prior) / prior) * 100)
})

function maxCount(items: { message_count: number }[]) {
  return Math.max(...items.map(i => i.message_count), 1)
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

    <!-- System stat cards -->
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
        <div class="stat-label">banned</div>
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

      <!-- Daily volume SVG line chart -->
      <section class="panel panel-wide">
        <div class="panel-header-row">
          <h2 class="panel-title">daily volume <span class="panel-sub">(last 30 days)</span></h2>
          <div class="panel-callouts">
            <span v-if="peakDay" class="callout">
              peak: <strong>{{ peakDay.day.slice(5) }}</strong> ({{ peakDay.count }})
            </span>
            <span v-if="weekTrend !== null" class="callout" :class="weekTrend >= 0 ? 'pos' : 'neg'">
              7d: {{ weekTrend >= 0 ? '+' : '' }}{{ weekTrend }}%
            </span>
          </div>
        </div>

        <div v-if="(reports.daily_volume?.length ?? 0) === 0" class="muted empty">no messages in this period</div>
        <svg
          v-else
          :viewBox="`0 0 ${CHART_W} ${CHART_H}`"
          class="line-chart"
          role="img"
          aria-label="Daily message volume"
        >
          <!-- Y grid lines + labels -->
          <g v-for="tick in yTicks" :key="tick.label">
            <line
              :x1="PAD.left" :y1="tick.y"
              :x2="CHART_W - PAD.right" :y2="tick.y"
              class="grid-line"
            />
            <text :x="PAD.left - 5" :y="tick.y + 3.5" text-anchor="end" class="axis-label">
              {{ tick.label }}
            </text>
          </g>

          <!-- X axis labels -->
          <text
            v-for="xl in xLabels"
            :key="xl.label"
            :x="xl.x" :y="CHART_H - 4"
            text-anchor="middle"
            class="axis-label"
          >{{ xl.label }}</text>

          <!-- Area fill -->
          <polygon :points="areaPoints" class="area-fill" />

          <!-- Line -->
          <polyline :points="linePoints" class="data-line" />

          <!-- Dots with native hover tooltips -->
          <circle
            v-for="p in dailyPoints"
            :key="p.day"
            :cx="p.x" :cy="p.y"
            r="3.5"
            class="data-dot"
          >
            <title>{{ p.day }}: {{ p.count }} messages</title>
          </circle>
        </svg>
      </section>

      <!-- Top senders -->
      <section class="panel">
        <h2 class="panel-title">top senders</h2>
        <div v-if="reports.top_senders.length === 0" class="muted empty">no data</div>
        <div v-for="(s, i) in reports.top_senders" :key="s.username" class="bar-row">
          <span class="bar-rank">{{ i + 1 }}</span>
          <span class="bar-label">{{ s.username }}</span>
          <div class="bar-track">
            <div class="bar-fill" :style="{ width: pct(s.message_count, maxCount(reports.top_senders)) + '%' }" />
          </div>
          <span class="bar-val">{{ s.message_count }}</span>
        </div>
      </section>

      <!-- Top rooms -->
      <section class="panel">
        <h2 class="panel-title">top rooms</h2>
        <div v-if="reports.top_rooms.length === 0" class="muted empty">no data</div>
        <div v-for="(r, i) in reports.top_rooms" :key="r.room_id" class="bar-row">
          <span class="bar-rank">{{ i + 1 }}</span>
          <span class="bar-label">{{ r.room_name }}</span>
          <div class="bar-track">
            <div class="bar-fill" :style="{ width: pct(r.message_count, maxCount(reports.top_rooms)) + '%' }" />
          </div>
          <span class="bar-val">{{ r.message_count }}</span>
        </div>
      </section>

      <!-- Stale rooms -->
      <section class="panel">
        <h2 class="panel-title">stale rooms <span class="panel-sub">(30+ days inactive)</span></h2>
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

/* Stat cards */
.stat-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(145px, 1fr)); gap: 0.9rem; }
.stat-card {
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 0.9rem 1.1rem;
  background: var(--bg);
}
.stat-label { font-size: 0.72em; color: var(--muted); text-transform: uppercase; letter-spacing: 0.06em; margin-bottom: 0.35rem; }
.stat-value { font-size: 2em; font-weight: 700; }
.stat-value.warn { color: var(--warning); }

/* Layout */
.analytics-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
  gap: 1.1rem;
}
.panel-wide { grid-column: 1 / -1; }
.panel {
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 1rem 1.2rem;
  background: var(--bg);
  display: flex;
  flex-direction: column;
  gap: 0.65rem;
}
.panel-header-row {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: 0.5rem;
}
.panel-title {
  font-size: 0.78em;
  text-transform: uppercase;
  letter-spacing: 0.07em;
  color: var(--muted);
  margin: 0;
}
.panel-sub { font-weight: 400; text-transform: none; letter-spacing: 0; }
.panel-callouts { display: flex; gap: 0.75rem; align-items: center; }
.callout { font-size: 0.8em; color: var(--muted); }
.callout strong { color: var(--fg); }
.callout.pos { color: #2a8a2a; }
.callout.neg { color: var(--error); }
.empty { font-size: 0.88em; }

/* SVG line chart */
.line-chart {
  display: block;
  width: 100%;
  height: auto;
}
.grid-line { stroke: var(--border); stroke-width: 1; }
.area-fill { fill: var(--accent); opacity: 0.13; }
.data-line { fill: none; stroke: var(--accent); stroke-width: 2; stroke-linejoin: round; stroke-linecap: round; }
.data-dot { fill: var(--accent); cursor: default; }
.axis-label { fill: var(--muted); font-size: 10px; font-family: inherit; }

/* Horizontal bar chart */
.bar-row { display: flex; align-items: center; gap: 0.5rem; font-size: 0.85em; }
.bar-rank { min-width: 16px; color: var(--muted); text-align: right; font-size: 0.8em; }
.bar-label { min-width: 80px; max-width: 110px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.bar-track { flex: 1; height: 10px; background: var(--row-alt); border-radius: 2px; overflow: hidden; }
.bar-fill { height: 100%; background: var(--accent); border-radius: 2px; transition: width 0.4s; }
.bar-val { min-width: 32px; text-align: right; color: var(--muted); }

/* Stale rooms table */
.data-table { width: 100%; border-collapse: collapse; font-size: 0.875em; }
.data-table th { text-align: left; font-size: 0.75em; text-transform: uppercase; letter-spacing: 0.05em; color: var(--muted); padding: 0 0 0.4rem; border-bottom: 1px solid var(--border); }
.data-table td { padding: 0.35rem 0; border-bottom: 1px solid var(--border); }
.data-table tr:last-child td { border-bottom: none; }
</style>
