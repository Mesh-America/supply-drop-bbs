<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { api } from '../api/client'

interface AuditEntry {
  id: number
  actor: string
  action: string
  target: string | null
  detail: string | null
  created_at: string
}

const entries = ref<AuditEntry[]>([])
const loading = ref(false)
const error = ref<string | null>(null)
const actionFilter = ref('')
const limit = ref(100)
const offset = ref(0)
const hasMore = ref(false)

const ACTION_OPTIONS = [
  { value: '', label: 'all actions' },
  { value: 'ban', label: 'ban' },
  { value: 'unban', label: 'unban' },
  { value: 'validate', label: 'validate' },
  { value: 'set_permission', label: 'set_permission' },
  { value: 'delete_message', label: 'delete_message' },
  { value: 'create_room', label: 'create_room' },
  { value: 'delete_room', label: 'delete_room' },
]

function actionClass(action: string): string {
  switch (action) {
    case 'ban':            return 'badge-ban'
    case 'unban':          return 'badge-unban'
    case 'validate':       return 'badge-validate'
    case 'set_permission': return 'badge-perm'
    case 'delete_message': return 'badge-delete'
    case 'create_room':    return 'badge-create'
    case 'delete_room':    return 'badge-delete'
    default:               return 'badge-default'
  }
}

function formatTs(iso: string): string {
  try {
    return new Date(iso).toLocaleString()
  } catch {
    return iso
  }
}

async function load(reset = true) {
  if (reset) offset.value = 0
  loading.value = true
  error.value = null
  try {
    const params = new URLSearchParams({
      limit: String(limit.value + 1),
      offset: String(offset.value),
    })
    if (actionFilter.value) params.set('action', actionFilter.value)
    const rows = await api.get<AuditEntry[]>(`/api/v1/audit-log?${params}`)
    hasMore.value = rows.length > limit.value
    entries.value = rows.slice(0, limit.value)
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load audit log'
  } finally {
    loading.value = false
  }
}

function prev() {
  offset.value = Math.max(0, offset.value - limit.value)
  load(false)
}

function next() {
  offset.value += limit.value
  load(false)
}

onMounted(() => load())
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>audit log</h1>
        <p class="muted">Durable record of privileged actions</p>
      </div>
      <div class="controls">
        <select v-model="actionFilter" @change="load()">
          <option v-for="o in ACTION_OPTIONS" :key="o.value" :value="o.value">{{ o.label }}</option>
        </select>
        <button class="secondary" @click="load()" :disabled="loading">refresh</button>
      </div>
    </header>

    <p v-if="error" class="error">{{ error }}</p>

    <div class="table-wrap">
      <table v-if="entries.length">
        <thead>
          <tr>
            <th class="col-id">#</th>
            <th class="col-ts">timestamp</th>
            <th class="col-actor">actor</th>
            <th class="col-action">action</th>
            <th class="col-target">target</th>
            <th class="col-detail">detail</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="e in entries" :key="e.id">
            <td class="col-id muted">{{ e.id }}</td>
            <td class="col-ts mono">{{ formatTs(e.created_at) }}</td>
            <td class="col-actor mono">{{ e.actor }}</td>
            <td class="col-action">
              <span class="badge" :class="actionClass(e.action)">{{ e.action }}</span>
            </td>
            <td class="col-target mono">{{ e.target ?? '' }}</td>
            <td class="col-detail muted">{{ e.detail ?? '' }}</td>
          </tr>
        </tbody>
      </table>
      <div v-else-if="!loading" class="empty-state">
        <p class="muted">No audit entries found.</p>
      </div>
      <div v-else class="empty-state">
        <p class="muted">Loading…</p>
      </div>
    </div>

    <div class="pagination" v-if="entries.length || offset > 0">
      <button class="secondary" :disabled="offset === 0" @click="prev">← prev</button>
      <span class="muted page-info">offset {{ offset }}</span>
      <button class="secondary" :disabled="!hasMore" @click="next">next →</button>
    </div>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 0.7rem; }
.page-header { display: flex; align-items: flex-start; gap: 1rem; flex-wrap: wrap; }
.page-header > div:first-child { flex: 1; }
h1 { margin: 0 0 0.15rem; }
p { margin: 0; }

.controls { display: flex; align-items: center; gap: 0.5rem; flex-wrap: wrap; }

.table-wrap { overflow-x: auto; }

table { width: 100%; border-collapse: collapse; font-size: 0.88em; }
th, td { padding: 0.45rem 0.6rem; text-align: left; border-bottom: 1px solid var(--border); }
th { font-size: 0.75em; text-transform: uppercase; letter-spacing: 0.06em; color: var(--muted); background: var(--row-alt); }
tr:nth-child(even) td { background: var(--row-alt); }

.col-id     { width: 3.5rem; }
.col-ts     { width: 12rem; }
.col-actor  { width: 10rem; }
.col-action { width: 9rem; }
.col-target { width: 10rem; }
.col-detail { }

.mono { font-family: monospace; font-size: 0.95em; }

.badge {
  display: inline-block;
  padding: 0.15em 0.5em;
  border-radius: 3px;
  font-size: 0.82em;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}
.badge-ban      { background: #fee2e2; color: #b91c1c; }
.badge-unban    { background: #dcfce7; color: #15803d; }
.badge-validate { background: #dbeafe; color: #1d4ed8; }
.badge-perm     { background: #ede9fe; color: #6d28d9; }
.badge-delete   { background: #fef3c7; color: #b45309; }
.badge-create   { background: #dcfce7; color: #15803d; }
.badge-default  { background: var(--row-alt); color: var(--muted); }

/* dark-mode badge adjustments */
:global(.dark) .badge-ban      { background: #450a0a; color: #fca5a5; }
:global(.dark) .badge-unban    { background: #052e16; color: #86efac; }
:global(.dark) .badge-validate { background: #1e3a5f; color: #93c5fd; }
:global(.dark) .badge-perm     { background: #2e1065; color: #c4b5fd; }
:global(.dark) .badge-delete   { background: #451a03; color: #fcd34d; }
:global(.dark) .badge-create   { background: #052e16; color: #86efac; }

.empty-state { padding: 2rem 0.5rem; text-align: center; }

.pagination {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  padding-top: 0.25rem;
}
.page-info { font-size: 0.85em; }
</style>
