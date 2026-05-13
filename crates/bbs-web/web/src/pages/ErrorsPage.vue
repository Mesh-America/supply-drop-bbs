<script setup lang="ts">
import { ref, watch, onMounted, onUnmounted } from 'vue'
import { api } from '../api/client'
import { useStatsStore } from '../stores/stats'

interface ErrorEntry {
  level: string
  target: string
  message: string
  count: number
  first_seen: number
  last_seen: number
}

const stats = useStatsStore()
const entries = ref<ErrorEntry[]>([])
const loading = ref(false)
const error = ref<string | null>(null)

async function load() {
  loading.value = true
  error.value = null
  try {
    entries.value = await api.get<ErrorEntry[]>('/api/v1/errors')
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load error report'
  } finally {
    loading.value = false
  }
}

function fmtTs(secs: number): string {
  if (!secs) return '—'
  return new Date(secs * 1000).toLocaleString()
}

function levelClass(level: string): string {
  if (level === 'ERROR') return 'level-error'
  if (level === 'WARN') return 'level-warn'
  return ''
}

let pollTimer: ReturnType<typeof setInterval> | null = null

// Reload the list whenever a new error fires via SSE so the page stays
// current without waiting up to 30 s for the poll timer.
watch(() => stats.errorAlerts, (next, prev) => {
  if (next > prev) {
    load()
    stats.clearErrorAlerts()
  }
})

onMounted(() => {
  stats.clearErrorAlerts()
  load()
  pollTimer = setInterval(() => { load(); stats.clearErrorAlerts() }, 30_000)
})
onUnmounted(() => {
  if (pollTimer !== null) clearInterval(pollTimer)
})
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>errors</h1>
        <p class="muted">Deduplicated WARN/ERROR log entries — sorted by frequency</p>
      </div>
      <button @click="load" :disabled="loading">refresh</button>
    </header>

    <p v-if="error" class="error">{{ error }}</p>
    <p v-if="loading && entries.length === 0" class="muted">Loading…</p>

    <div v-if="!loading && entries.length === 0 && !error" class="empty-state">
      <p class="healthy-icon">✓</p>
      <p>No errors recorded</p>
      <p class="muted">No WARN or ERROR events since the last restart — the server is healthy.</p>
    </div>

    <div v-if="entries.length > 0" class="table-wrap">
      <table class="data-table">
        <thead>
          <tr>
            <th>level</th>
            <th>count</th>
            <th>target</th>
            <th>message</th>
            <th>first seen</th>
            <th>last seen</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="(e, i) in entries" :key="i">
            <td><span class="level-badge" :class="levelClass(e.level)">{{ e.level }}</span></td>
            <td class="count-cell">{{ e.count }}</td>
            <td class="target-cell muted">{{ e.target }}</td>
            <td class="message-cell">{{ e.message }}</td>
            <td class="ts-cell muted">{{ fmtTs(e.first_seen) }}</td>
            <td class="ts-cell muted">{{ fmtTs(e.last_seen) }}</td>
          </tr>
        </tbody>
      </table>
    </div>
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

.empty-state { padding: 2rem; text-align: center; display: flex; flex-direction: column; align-items: center; gap: 0.4rem; }
.healthy-icon { font-size: 2rem; color: #2a8a2a; margin: 0; }

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
.data-table td { padding: 0.45rem 0.6rem; border-bottom: 1px solid var(--border); vertical-align: top; }
.data-table tr:last-child td { border-bottom: none; }
.data-table tr:hover td { background: var(--row-alt); }

.level-badge {
  display: inline-block;
  font-size: 0.78em;
  font-weight: 700;
  letter-spacing: 0.05em;
  padding: 0.15em 0.55em;
  border-radius: 3px;
  background: var(--row-alt);
  color: var(--muted);
  white-space: nowrap;
}
.level-badge.level-error { background: rgba(220, 38, 38, 0.15); color: #dc2626; }
.level-badge.level-warn  { background: rgba(180, 83, 9, 0.15);  color: var(--warn, #b45309); }

.count-cell { font-weight: 700; text-align: right; white-space: nowrap; min-width: 50px; }
.target-cell { font-size: 0.82em; white-space: nowrap; max-width: 200px; overflow: hidden; text-overflow: ellipsis; }
.message-cell { word-break: break-word; max-width: 400px; }
.ts-cell { font-size: 0.82em; white-space: nowrap; }
</style>
