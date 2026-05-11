<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
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

interface HourlyActivity {
  hour: number
  count: number
}

interface WeeklySignups {
  week: string
  count: number
}

interface Reports {
  top_senders: TopSender[]
  top_rooms: TopRoom[]
  daily_volume: DailyVolume[]
  stale_rooms: StaleRoom[]
  hourly_activity: HourlyActivity[]
  new_users_by_week: WeeklySignups[]
  msgs_last_24h: number
  msgs_last_7d: number
  msgs_last_30d: number
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

// ── Daily volume SVG line chart ──────────────────────────────────────────────
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
  if (data.length === 0) {
    return [0, 5, 10].map((v, i) => ({
      y: PAD.top + plotH - (i / 2) * plotH,
      label: String(v),
    }))
  }
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

// ── Hourly activity SVG bar chart ────────────────────────────────────────────
const HR_W = 600
const HR_H = 100
const HR_PAD = { top: 10, right: 12, bottom: 20, left: 36 }
const hrPlotW = HR_W - HR_PAD.left - HR_PAD.right
const hrPlotH = HR_H - HR_PAD.top - HR_PAD.bottom

const hourlyBars = computed(() => {
  const data = reports.value?.hourly_activity ?? []
  if (data.length === 0) return []
  const maxVal = Math.max(...data.map(d => d.count), 1)
  const barW = hrPlotW / 24
  const gap = 2
  return data.map(d => ({
    x: HR_PAD.left + d.hour * barW + gap / 2,
    y: HR_PAD.top + hrPlotH - Math.max((d.count / maxVal) * hrPlotH, 1),
    w: barW - gap,
    h: Math.max((d.count / maxVal) * hrPlotH, 1),
    hour: d.hour,
    count: d.count,
  }))
})

const hrYTicks = computed(() => {
  const data = reports.value?.hourly_activity ?? []
  const max = data.length ? Math.max(...data.map(d => d.count), 1) : 10
  return [0, max].map(v => ({
    y: HR_PAD.top + hrPlotH - (v / max) * hrPlotH,
    label: String(v),
  }))
})

function hourLabel(h: number): string {
  if (h === 0) return '12a'
  if (h === 12) return '12p'
  return h < 12 ? `${h}a` : `${h - 12}p`
}

const hrXLabels = computed(() =>
  [0, 6, 12, 18, 23].map(h => ({
    x: HR_PAD.left + h * (hrPlotW / 24) + (hrPlotW / 24) / 2,
    label: hourLabel(h),
  }))
)

const peakHour = computed(() => {
  const data = reports.value?.hourly_activity ?? []
  if (data.length === 0) return null
  return data.reduce((best, d) => (d.count > best.count ? d : best), data[0])
})

// ── Weekly signups SVG line chart ─────────────────────────────────────────────
const WK_W = 600
const WK_H = 100
const WK_PAD = { top: 10, right: 12, bottom: 20, left: 36 }
const wkPlotW = WK_W - WK_PAD.left - WK_PAD.right
const wkPlotH = WK_H - WK_PAD.top - WK_PAD.bottom

const weeklyPoints = computed(() => {
  const data = reports.value?.new_users_by_week ?? []
  if (data.length === 0) return []
  const maxVal = Math.max(...data.map(d => d.count), 1)
  const n = data.length
  return data.map((d, i) => ({
    x: WK_PAD.left + (n > 1 ? (i / (n - 1)) * wkPlotW : wkPlotW / 2),
    y: WK_PAD.top + wkPlotH - (d.count / maxVal) * wkPlotH,
    week: d.week,
    count: d.count,
  }))
})

const wkLinePoints = computed(() =>
  weeklyPoints.value.map(p => `${p.x},${p.y}`).join(' ')
)

const wkAreaPoints = computed(() => {
  const pts = weeklyPoints.value
  if (pts.length === 0) return ''
  const bottom = WK_PAD.top + wkPlotH
  return [
    `${pts[0].x},${bottom}`,
    ...pts.map(p => `${p.x},${p.y}`),
    `${pts[pts.length - 1].x},${bottom}`,
  ].join(' ')
})

const wkYTicks = computed(() => {
  const data = reports.value?.new_users_by_week ?? []
  const max = data.length ? Math.max(...data.map(d => d.count), 1) : 5
  return [0, max].map(v => ({
    y: WK_PAD.top + wkPlotH - (v / max) * wkPlotH,
    label: String(v),
  }))
})

const totalNewUsers = computed(() =>
  (reports.value?.new_users_by_week ?? []).reduce((s, w) => s + w.count, 0)
)

// ── Shared helpers ───────────────────────────────────────────────────────────
function maxCount(items: { message_count: number }[]) {
  return Math.max(...items.map(i => i.message_count), 1)
}

function pct(n: number, max: number) {
  if (max === 0) return 3
  const p = Math.round((n / max) * 100)
  return p === 0 ? 3 : p
}

function fmtDate(iso: string | null) {
  if (!iso) return 'never'
  return iso.slice(0, 10)
}

let pollTimer: ReturnType<typeof setInterval> | null = null
onMounted(() => { load(); pollTimer = setInterval(load, 60_000) })
onUnmounted(() => { if (pollTimer !== null) clearInterval(pollTimer) })
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>reports</h1>
        <p class="muted">Aggregate BBS analytics</p>
      </div>
    </header>

    <p v-if="error" class="error">{{ error }}</p>
    <p v-if="loading && !stats" class="muted">Loading…</p>

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
        <div class="stat-label">msgs (24 h)</div>
        <div class="stat-value">{{ reports?.msgs_last_24h ?? '—' }}</div>
      </div>
      <div class="stat-card">
        <div class="stat-label">msgs (7 d)</div>
        <div class="stat-value">{{ reports?.msgs_last_7d ?? '—' }}</div>
      </div>
      <div class="stat-card">
        <div class="stat-label">msgs (30 d)</div>
        <div class="stat-value">{{ reports?.msgs_last_30d ?? '—' }}</div>
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

        <svg
          :viewBox="`0 0 ${CHART_W} ${CHART_H}`"
          class="line-chart"
          role="img"
          aria-label="Daily message volume"
        >
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
          <line
            :x1="PAD.left" :y1="PAD.top + plotH"
            :x2="CHART_W - PAD.right" :y2="PAD.top + plotH"
            class="grid-line"
          />
          <template v-if="(reports.daily_volume?.length ?? 0) === 0">
            <line
              :x1="PAD.left" :y1="PAD.top + plotH"
              :x2="CHART_W - PAD.right" :y2="PAD.top + plotH"
              class="data-line no-data-line"
            />
            <text
              :x="CHART_W / 2" :y="PAD.top + plotH / 2 + 4"
              text-anchor="middle"
              class="no-data-label"
            >no messages yet</text>
          </template>
          <template v-else>
            <text
              v-for="xl in xLabels"
              :key="xl.label"
              :x="xl.x" :y="CHART_H - 4"
              text-anchor="middle"
              class="axis-label"
            >{{ xl.label }}</text>
            <polygon :points="areaPoints" class="area-fill" />
            <polyline :points="linePoints" class="data-line" />
            <circle
              v-for="p in dailyPoints"
              :key="p.day"
              :cx="p.x" :cy="p.y"
              r="3.5"
              class="data-dot"
            >
              <title>{{ p.day }}: {{ p.count }} messages</title>
            </circle>
          </template>
        </svg>
      </section>

      <!-- Hourly activity SVG bar chart -->
      <section class="panel panel-wide">
        <div class="panel-header-row">
          <h2 class="panel-title">hourly activity <span class="panel-sub">(all time, UTC)</span></h2>
          <div class="panel-callouts">
            <span v-if="peakHour" class="callout">
              peak: <strong>{{ hourLabel(peakHour.hour) }}</strong> ({{ peakHour.count }})
            </span>
          </div>
        </div>

        <svg
          :viewBox="`0 0 ${HR_W} ${HR_H}`"
          class="line-chart"
          role="img"
          aria-label="Hourly message activity"
        >
          <g v-for="tick in hrYTicks" :key="tick.label">
            <line
              :x1="HR_PAD.left" :y1="tick.y"
              :x2="HR_W - HR_PAD.right" :y2="tick.y"
              class="grid-line"
            />
            <text :x="HR_PAD.left - 5" :y="tick.y + 3.5" text-anchor="end" class="axis-label">
              {{ tick.label }}
            </text>
          </g>
          <line
            :x1="HR_PAD.left" :y1="HR_PAD.top + hrPlotH"
            :x2="HR_W - HR_PAD.right" :y2="HR_PAD.top + hrPlotH"
            class="grid-line"
          />

          <template v-if="hourlyBars.length === 0">
            <text
              :x="HR_W / 2" :y="HR_PAD.top + hrPlotH / 2 + 4"
              text-anchor="middle" class="no-data-label"
            >no messages yet</text>
          </template>
          <template v-else>
            <rect
              v-for="b in hourlyBars"
              :key="b.hour"
              :x="b.x" :y="b.y"
              :width="b.w" :height="b.h"
              class="hour-bar"
            >
              <title>{{ hourLabel(b.hour) }} ({{ b.hour }}:00–{{ b.hour }}:59): {{ b.count }} messages</title>
            </rect>
            <text
              v-for="xl in hrXLabels"
              :key="xl.label"
              :x="xl.x" :y="HR_H - 3"
              text-anchor="middle" class="axis-label"
            >{{ xl.label }}</text>
          </template>
        </svg>
      </section>

      <!-- Top senders -->
      <section class="panel">
        <h2 class="panel-title">top senders</h2>
        <div v-if="reports.top_senders.length === 0" class="muted empty">No messages yet.</div>
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

      <!-- New user growth -->
      <section class="panel">
        <div class="panel-header-row">
          <h2 class="panel-title">new users <span class="panel-sub">(last 8 weeks)</span></h2>
          <div class="panel-callouts">
            <span v-if="totalNewUsers > 0" class="callout">
              total: <strong>{{ totalNewUsers }}</strong>
            </span>
          </div>
        </div>

        <svg
          :viewBox="`0 0 ${WK_W} ${WK_H}`"
          class="line-chart"
          role="img"
          aria-label="Weekly new user registrations"
        >
          <g v-for="tick in wkYTicks" :key="tick.label">
            <line
              :x1="WK_PAD.left" :y1="tick.y"
              :x2="WK_W - WK_PAD.right" :y2="tick.y"
              class="grid-line"
            />
            <text :x="WK_PAD.left - 5" :y="tick.y + 3.5" text-anchor="end" class="axis-label">
              {{ tick.label }}
            </text>
          </g>
          <line
            :x1="WK_PAD.left" :y1="WK_PAD.top + wkPlotH"
            :x2="WK_W - WK_PAD.right" :y2="WK_PAD.top + wkPlotH"
            class="grid-line"
          />
          <template v-if="weeklyPoints.length === 0">
            <text
              :x="WK_W / 2" :y="WK_PAD.top + wkPlotH / 2 + 4"
              text-anchor="middle" class="no-data-label"
            >no signups in last 8 weeks</text>
          </template>
          <template v-else>
            <polygon :points="wkAreaPoints" class="area-fill area-fill-green" />
            <polyline :points="wkLinePoints" class="data-line data-line-green" />
            <circle
              v-for="p in weeklyPoints"
              :key="p.week"
              :cx="p.x" :cy="p.y"
              r="3.5"
              class="data-dot data-dot-green"
            >
              <title>{{ p.week }}: {{ p.count }} new users</title>
            </circle>
          </template>
        </svg>
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
.stat-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(130px, 1fr)); gap: 0.9rem; }
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

/* SVG charts */
.line-chart { display: block; width: 100%; height: auto; }
.grid-line { stroke: var(--border); stroke-width: 1; }
.area-fill { fill: var(--accent); opacity: 0.13; }
.area-fill-green { fill: #2a8a2a; opacity: 0.13; }
.data-line { fill: none; stroke: var(--accent); stroke-width: 2; stroke-linejoin: round; stroke-linecap: round; }
.data-line-green { stroke: #2a8a2a; }
.no-data-line { opacity: 0.25; stroke-dasharray: 4 4; }
.data-dot { fill: var(--accent); cursor: default; }
.data-dot-green { fill: #2a8a2a; }
.axis-label { fill: var(--muted); font-size: 10px; font-family: inherit; }
.no-data-label { fill: var(--muted); font-size: 11px; font-family: inherit; font-style: italic; }
.hour-bar { fill: var(--accent); opacity: 0.8; cursor: default; }

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
