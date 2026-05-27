<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { api, ApiError } from '../api/client'
import { useAuthStore } from '../stores/auth'
import DataTable from '../components/DataTable.vue'
import { fmtLocal } from '../utils/datetime'

interface Advert {
  ts: number
  pubkey: string
  name: string
  type: number
  type_name: string
  lat: number
  lon: number
}

const auth = useAuthStore()

const rows = ref<Advert[]>([])
const error = ref<string | null>(null)
let timer: number | undefined

const flood = ref(true)
const sending = ref(false)
const sendStatus = ref<{ kind: 'ok' | 'error'; message: string } | null>(null)
const clearing = ref(false)

async function load() {
  try {
    rows.value = await api.get<Advert[]>('/api/v1/adverts')
    error.value = null
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load adverts'
  }
}

async function sendAdvert() {
  sending.value = true
  sendStatus.value = null
  try {
    await api.post('/api/v1/adverts/send', { flood: flood.value })
    const ts = new Date().toLocaleTimeString()
    sendStatus.value = {
      kind: 'ok',
      message: `Advert sent (${flood.value ? 'flood' : 'direct'}) at ${ts}.`,
    }
    await load()
  } catch (e: any) {
    let msg = e?.message ?? 'failed to send advert'
    if (e instanceof ApiError) {
      if (e.status === 403) msg = 'sysop required'
      else if (e.status === 503) msg = 'mesh transport not running'
    }
    sendStatus.value = { kind: 'error', message: msg }
  } finally {
    sending.value = false
  }
}

async function clearAdverts() {
  if (!confirm('Clear all advert records from memory? The list will repopulate as nodes are heard.')) return
  clearing.value = true
  try {
    await api.del('/api/v1/adverts')
    await load()
  } finally {
    clearing.value = false
  }
}

function fmtCoord(lat: number, lon: number): string {
  return `${lat.toFixed(4)}, ${lon.toFixed(4)}`
}

function hasCoord(lat: number, lon: number): boolean {
  return lat !== 0 || lon !== 0
}

function shortKey(k: string): string {
  return k.length > 16 ? k.slice(0, 16) + '…' : k
}

const groups = computed(() => {
  const m: Record<string, number> = {}
  for (const r of rows.value) m[r.type_name] = (m[r.type_name] ?? 0) + 1
  return m
})

onMounted(() => {
  load()
  timer = window.setInterval(load, 5000)
})
onUnmounted(() => { if (timer !== undefined) window.clearInterval(timer) })

const columns = [
  { key: 'ts', label: 'last seen' },
  { key: 'name', label: 'name' },
  { key: 'type_name', label: 'type' },
  { key: 'pubkey', label: 'pubkey' },
  { key: 'location', label: 'location' },
]
</script>

<template>
  <div class="page">
    <header class="page-header">
      <h1>adverts</h1>
      <p class="muted small">
        Nodes heard over the mesh. Polls every 5 s.
        You must have received a node's advert before you can communicate with it.
      </p>
    </header>

    <!-- Send our own advert -->
    <section class="send-block">
      <div class="send-controls">
        <button
          @click="sendAdvert"
          :disabled="sending || !auth.isSysop"
          :title="!auth.isSysop ? 'sysop required' : ''"
        >
          {{ sending ? 'sending…' : 'send advert' }}
        </button>
        <label class="checkbox">
          <input type="checkbox" v-model="flood" :disabled="sending" />
          flood (multi-hop)
        </label>
        <span class="muted small">
          {{ flood
            ? 'rebroadcast hop-by-hop — more reach, more airtime'
            : 'neighbours only — single hop' }}
        </span>
        <button
          class="btn-danger"
          @click="clearAdverts"
          :disabled="clearing || !auth.isSysop"
          :title="!auth.isSysop ? 'sysop required' : 'Clear all advert records from memory'"
        >
          {{ clearing ? 'clearing…' : 'clear list' }}
        </button>
      </div>
      <p v-if="sendStatus" class="status-line small" :class="sendStatus.kind">
        {{ sendStatus.message }}
      </p>
    </section>

    <!-- Type summary badges -->
    <p class="muted small type-summary">
      <span v-if="!rows.length">No adverts heard yet.</span>
      <span v-for="(count, type) in groups" :key="type" class="badge" :class="`type-${type}`">
        {{ type }}: {{ count }}
      </span>
    </p>

    <p v-if="error" class="error">{{ error }}</p>

    <DataTable
      :columns="columns"
      :rows="rows"
      :row-key="(r) => `${r.ts}-${r.pubkey}`"
      :page-size="50"
      empty="No adverts heard yet (is mesh transport enabled and connected?)."
    >
      <template #[`cell:ts`]="{ row }">{{ fmtLocal(row.ts) }}</template>
      <template #[`cell:type_name`]="{ row }">
        <span class="badge" :class="`type-${row.type_name}`">{{ row.type_name }}</span>
      </template>
      <template #[`cell:pubkey`]="{ row }">
        <code :title="row.pubkey">{{ shortKey(row.pubkey) }}</code>
      </template>
      <template #[`cell:location`]="{ row }">
        <span v-if="hasCoord(row.lat, row.lon)">{{ fmtCoord(row.lat, row.lon) }}</span>
        <span v-else class="muted">—</span>
      </template>
    </DataTable>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 0.7rem; }
.page-header { display: flex; flex-direction: column; gap: 0.2rem; }
h1 { margin: 0; }
.small { font-size: 0.85em; }

.send-block {
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 0.7rem 0.9rem;
  background: var(--row-alt);
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
}
.send-controls { display: flex; align-items: center; gap: 0.9rem; flex-wrap: wrap; }
.send-controls .checkbox { display: flex; align-items: center; gap: 0.35rem; font-size: 0.9em; }

.status-line { margin: 0; padding: 0.25rem 0.5rem; border-radius: 3px; }
.status-line.ok    { color: #2a8a2a; border: 1px solid #2a8a2a; background: rgba(42,138,42,0.08); }
.status-line.error { color: var(--error); border: 1px solid var(--error); background: rgba(200,60,60,0.08); }
.btn-danger { color: var(--error); border-color: var(--error); }

.type-summary { margin: 0; display: flex; flex-wrap: wrap; gap: 0.4rem; }

.badge {
  display: inline-block; padding: 0.05rem 0.45rem;
  border-radius: 3px; background: var(--row-alt); font-size: 0.85em;
}
.badge.type-chat     { background: #d2efd2; color: #205020; }
.badge.type-repeater { background: #d2dff0; color: #203560; }
.badge.type-room     { background: #f0e2c2; color: #604010; }
.badge.type-sensor   { background: #f0d2d2; color: #602020; }
.badge.type-unknown  { background: var(--row-alt); color: var(--muted); }
</style>
