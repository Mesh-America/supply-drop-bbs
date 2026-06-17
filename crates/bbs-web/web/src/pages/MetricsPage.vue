<script setup lang="ts">
import { ref, onMounted, onUnmounted, computed } from 'vue'
import { api } from '../api/client'
import { useStatsStore } from '../stores/stats'

const stats = useStatsStore()

interface DiskInfo {
  name: string
  mount: string
  fs: string
  total_bytes: number
  available_bytes: number
}

interface NetworkInfo {
  name: string
  rx_bytes: number
  tx_bytes: number
}

interface MetricsSnapshot {
  cpu_usage_pct: number
  memory_used_bytes: number
  memory_total_bytes: number
  swap_used_bytes: number
  swap_total_bytes: number
  process_rss_bytes: number | null
  disks: DiskInfo[]
  networks: NetworkInfo[]
  sampled_at: number
}

interface DeliveryStats {
  sends_total: number
  retransmits: number
  dropped: number
  accepted: number
  failed_no_route: number
  confirmed: number
  gave_up: number
  confirm_rate: number | null
  route_failure_rate: number | null
  latency_count: number
  avg_latency_ms: number | null
  min_latency_ms: number | null
  max_latency_ms: number | null
}

const snap = ref<MetricsSnapshot | null>(null)
const delivery = ref<DeliveryStats | null>(null)
const loading = ref(false)
const error = ref<string | null>(null)

async function load() {
  loading.value = true
  error.value = null
  try {
    snap.value = await api.get<MetricsSnapshot>('/api/v1/metrics')
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load metrics'
  } finally {
    loading.value = false
  }
  // Mesh delivery counters are optional — a 404 just means the mesh transport
  // is not compiled in / enabled, so hide the section rather than erroring.
  try {
    delivery.value = await api.get<DeliveryStats>('/api/v1/transports/meshcore/stats')
  } catch {
    delivery.value = null
  }
}

function ratePct(r: number | null): string {
  return r === null ? '—' : `${(r * 100).toFixed(1)}%`
}

// Width of a rate bar, 0–100. `null` (no data yet) renders an empty track.
function rateWidth(r: number | null): number {
  return r === null ? 0 : Math.round(r * 100)
}

// Confirm rate: higher is better, so the colour scale is inverted relative to
// the usage bars — green when most replies are getting through, red when few are.
function confirmBarClass(r: number | null): string {
  if (r === null) return ''
  if (r < 0.7) return 'bar-critical'
  if (r < 0.9) return 'bar-warn'
  return ''
}

// Route-failure rate: higher is worse, same direction as the usage bars.
function failBarClass(r: number | null): string {
  if (r === null) return ''
  if (r >= 0.3) return 'bar-critical'
  if (r >= 0.1) return 'bar-warn'
  return ''
}

// Round-trip latency: ms under a second, otherwise seconds.
function fmtMs(ms: number | null): string {
  if (ms === null) return '—'
  if (ms >= 1000) return `${(ms / 1000).toFixed(1)} s`
  return `${Math.round(ms)} ms`
}

function fmt(bytes: number): string {
  if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(1)} GB`
  if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(1)} MB`
  if (bytes >= 1_024) return `${(bytes / 1_024).toFixed(0)} KB`
  return `${bytes} B`
}

function pct(used: number, total: number): number {
  if (total === 0) return 0
  return Math.min(100, Math.round((used / total) * 100))
}

const memPct = computed(() =>
  snap.value ? pct(snap.value.memory_used_bytes, snap.value.memory_total_bytes) : 0
)
const swapPct = computed(() =>
  snap.value ? pct(snap.value.swap_used_bytes, snap.value.swap_total_bytes) : 0
)

function sampledAt(): string {
  if (!snap.value?.sampled_at) return ''
  return new Date(snap.value.sampled_at * 1000).toLocaleTimeString()
}

function barClass(p: number): string {
  if (p >= 90) return 'bar-critical'
  if (p >= 70) return 'bar-warn'
  return ''
}

let pollTimer: ReturnType<typeof setInterval> | null = null

onMounted(() => {
  load()
  pollTimer = setInterval(load, 30_000)
})
onUnmounted(() => {
  if (pollTimer !== null) clearInterval(pollTimer)
})
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>metrics</h1>
        <p class="muted">System resource snapshot — refreshes every 30 s</p>
      </div>
      <button @click="load" :disabled="loading">refresh</button>
    </header>

    <p v-if="error" class="error">{{ error }}</p>
    <p v-if="loading && !snap" class="muted">Loading…</p>

    <div v-if="stats.rssAlertActive" class="rss-alert-banner">
      <strong>Memory growth detected</strong> — process RSS has increased
      monotonically for at least 5 minutes
      <span v-if="stats.rssGrowthBytes > 0"> (+{{ fmt(stats.rssGrowthBytes) }})</span>.
      This may indicate a memory leak. Monitor closely or consider restarting.
    </div>

    <div v-if="snap" class="metrics-grid">

      <!-- CPU -->
      <div class="card">
        <div class="card-label">CPU</div>
        <div class="big-num">{{ snap.cpu_usage_pct.toFixed(1) }}<span class="unit">%</span></div>
        <div class="bar-track"><div class="bar-fill" :style="{ width: snap.cpu_usage_pct + '%' }" :class="barClass(snap.cpu_usage_pct)"></div></div>
        <div class="card-sub muted">global average</div>
      </div>

      <!-- Memory -->
      <div class="card">
        <div class="card-label">Memory</div>
        <div class="big-num">{{ memPct }}<span class="unit">%</span></div>
        <div class="bar-track"><div class="bar-fill" :style="{ width: memPct + '%' }" :class="barClass(memPct)"></div></div>
        <div class="card-sub muted">{{ fmt(snap.memory_used_bytes) }} / {{ fmt(snap.memory_total_bytes) }}</div>
      </div>

      <!-- Swap -->
      <div class="card" v-if="snap.swap_total_bytes > 0">
        <div class="card-label">Swap</div>
        <div class="big-num">{{ swapPct }}<span class="unit">%</span></div>
        <div class="bar-track"><div class="bar-fill" :style="{ width: swapPct + '%' }" :class="barClass(swapPct)"></div></div>
        <div class="card-sub muted">{{ fmt(snap.swap_used_bytes) }} / {{ fmt(snap.swap_total_bytes) }}</div>
      </div>

      <!-- Process RSS -->
      <div class="card" v-if="snap.process_rss_bytes !== null">
        <div class="card-label">Process RSS</div>
        <div class="big-num rss">{{ fmt(snap.process_rss_bytes!) }}</div>
        <div class="card-sub muted">BBS server resident memory</div>
      </div>

    </div>

    <!-- Mesh link health -->
    <section v-if="delivery" class="section">
      <h2 class="section-title">Mesh link health (cumulative since start)</h2>
      <div class="metrics-grid">
        <div class="card">
          <div class="card-label">Confirm rate</div>
          <div class="big-num">{{ ratePct(delivery.confirm_rate) }}</div>
          <div class="bar-track"><div class="bar-fill" :style="{ width: rateWidth(delivery.confirm_rate) + '%' }" :class="confirmBarClass(delivery.confirm_rate)"></div></div>
          <div class="card-sub muted">{{ delivery.confirmed }} ACK'd / {{ delivery.accepted }} accepted</div>
        </div>
        <div class="card">
          <div class="card-label">Route failures</div>
          <div class="big-num">{{ ratePct(delivery.route_failure_rate) }}</div>
          <div class="bar-track"><div class="bar-fill" :style="{ width: rateWidth(delivery.route_failure_rate) + '%' }" :class="failBarClass(delivery.route_failure_rate)"></div></div>
          <div class="card-sub muted">{{ delivery.failed_no_route }} of {{ delivery.accepted + delivery.failed_no_route }} had no route</div>
        </div>
        <div class="card">
          <div class="card-label">Sends</div>
          <div class="big-num">{{ delivery.sends_total }}</div>
          <div class="card-sub muted">{{ delivery.retransmits }} retransmit(s), {{ delivery.dropped }} dropped</div>
        </div>
        <div class="card">
          <div class="card-label">Gave up</div>
          <div class="big-num">{{ delivery.gave_up }}</div>
          <div class="card-sub muted">undelivered after all retries</div>
        </div>
        <div class="card" v-if="delivery.latency_count > 0">
          <div class="card-label">Avg round-trip</div>
          <div class="big-num">{{ fmtMs(delivery.avg_latency_ms) }}</div>
          <div class="card-sub muted">min {{ fmtMs(delivery.min_latency_ms) }} / max {{ fmtMs(delivery.max_latency_ms) }}</div>
        </div>
      </div>
    </section>

    <!-- Disks -->
    <section v-if="snap && snap.disks.length > 0" class="section">
      <h2 class="section-title">Disks</h2>
      <div class="table-wrap">
        <table class="data-table">
          <thead>
            <tr>
              <th>mount</th>
              <th>filesystem</th>
              <th>name</th>
              <th>used</th>
              <th>free</th>
              <th>total</th>
              <th style="min-width:120px">usage</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="d in snap.disks" :key="d.mount">
              <td class="mono">{{ d.mount }}</td>
              <td class="muted">{{ d.fs }}</td>
              <td class="muted">{{ d.name }}</td>
              <td>{{ fmt(d.total_bytes - d.available_bytes) }}</td>
              <td>{{ fmt(d.available_bytes) }}</td>
              <td>{{ fmt(d.total_bytes) }}</td>
              <td>
                <div class="bar-track inline">
                  <div class="bar-fill" :style="{ width: pct(d.total_bytes - d.available_bytes, d.total_bytes) + '%' }" :class="barClass(pct(d.total_bytes - d.available_bytes, d.total_bytes))"></div>
                </div>
                <span class="muted pct-label">{{ pct(d.total_bytes - d.available_bytes, d.total_bytes) }}%</span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <!-- Network -->
    <section v-if="snap && snap.networks.length > 0" class="section">
      <h2 class="section-title">Network (cumulative since boot)</h2>
      <div class="table-wrap">
        <table class="data-table">
          <thead>
            <tr>
              <th>interface</th>
              <th>RX</th>
              <th>TX</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="n in snap.networks" :key="n.name">
              <td class="mono">{{ n.name }}</td>
              <td>{{ fmt(n.rx_bytes) }}</td>
              <td>{{ fmt(n.tx_bytes) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <p v-if="snap" class="muted sampled-at">Sampled at {{ sampledAt() }}</p>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 1.5rem; }
.page-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 1rem;
  flex-wrap: wrap;
}
.page-header div { display: flex; flex-direction: column; gap: 0.2rem; }
h1 { margin: 0; }
p { margin: 0; }

.metrics-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
  gap: 1rem;
}

.card {
  background: var(--card-bg, var(--bg2));
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 1rem 1.2rem;
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
}
.card-label {
  font-size: 0.75em;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--muted);
}
.big-num {
  font-size: 2em;
  font-weight: 700;
  line-height: 1;
}
.big-num.rss { font-size: 1.4em; }
.unit { font-size: 0.5em; font-weight: 400; color: var(--muted); margin-left: 0.1em; }
.card-sub { font-size: 0.8em; margin-top: 0.2rem; }

.bar-track {
  height: 6px;
  background: var(--border);
  border-radius: 3px;
  overflow: hidden;
}
.bar-track.inline { display: inline-block; width: 80px; vertical-align: middle; height: 6px; }
.bar-fill {
  height: 100%;
  background: var(--accent, #22c55e);
  border-radius: 3px;
  transition: width 0.4s ease;
}
.bar-fill.bar-warn { background: var(--warn, #f59e0b); }
.bar-fill.bar-critical { background: #dc2626; }

.section { display: flex; flex-direction: column; gap: 0.75rem; }
.section-title { margin: 0; font-size: 0.95em; font-weight: 600; }

.table-wrap { overflow-x: auto; }
.data-table { width: 100%; border-collapse: collapse; font-size: 0.875em; }
.data-table th {
  text-align: left;
  font-size: 0.75em;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--muted);
  padding: 0.4rem 0.6rem;
  border-bottom: 1px solid var(--border);
  white-space: nowrap;
}
.data-table td { padding: 0.45rem 0.6rem; border-bottom: 1px solid var(--border); }
.data-table tr:last-child td { border-bottom: none; }
.data-table tr:hover td { background: var(--row-alt); }

.mono { font-family: monospace; }
.pct-label { font-size: 0.82em; margin-left: 0.4rem; }

.sampled-at { font-size: 0.8em; }

.rss-alert-banner {
  background: rgba(217, 119, 6, 0.12);
  border: 1px solid #d97706;
  border-radius: 6px;
  padding: 0.75rem 1rem;
  font-size: 0.9em;
  color: var(--fg);
}
.rss-alert-banner strong { color: #d97706; }
</style>
