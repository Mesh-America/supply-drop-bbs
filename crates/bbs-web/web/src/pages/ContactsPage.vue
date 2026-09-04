<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { api } from '../api/client'
import { useAuthStore } from '../stores/auth'
import { useToast } from '../composables/useToast'
import DataTable from '../components/DataTable.vue'
import { fmtLocal } from '../utils/datetime'

interface Contact {
  ts: number
  pubkey: string
  name: string
  type: number
  type_name: string
  lat: number
  lon: number
  transport: string
  protected: boolean
}

const auth = useAuthStore()
const toast = useToast()

const rows = ref<Contact[]>([])
const error = ref<string | null>(null)
const deletingKeys = ref<Set<string>>(new Set())
let timer: number | undefined

// Monotonic request-sequence guard (Phase 6 hostile-audit fix, Verifier
// pass): the 5-second poll's GET can still be in flight when a delete
// completes — without this, that GET's response could land afterward and
// silently "resurrect" the just-deleted contact in the UI for up to one
// more poll cycle. Bumping this on every successful delete invalidates any
// older in-flight load() response, without needing to track specific
// pubkeys (which would risk permanently hiding a contact that legitimately
// gets re-protected before such tracking expired).
let loadSeq = 0

async function load() {
  const seq = ++loadSeq
  try {
    const fresh = await api.get<Contact[]>('/api/v1/contacts')
    if (seq !== loadSeq) return
    rows.value = fresh
    error.value = null
  } catch (e: any) {
    if (seq !== loadSeq) return
    error.value = e?.message ?? 'failed to load contacts'
  }
}

async function deleteContact(row: Contact) {
  // Guard against a double-click firing two concurrent requests — the
  // :disabled binding alone can't prevent this, since confirm() blocks
  // before it's ever applied.
  if (deletingKeys.value.has(row.pubkey)) return
  if (
    !confirm(
      `Delete "${row.name}" from Contacts? This clears its protected status and best-effort removes it from the radio's own contact list. It can re-become protected if it messages the BBS again.`,
    )
  )
    return
  deletingKeys.value.add(row.pubkey)
  try {
    await api.del(`/api/v1/contacts/${row.pubkey}`)
    loadSeq++
    toast.ok(`"${row.name}" deleted from Contacts`)
    rows.value = rows.value.filter((r) => r.pubkey !== row.pubkey)
  } catch (e: any) {
    toast.error(e?.message ?? 'failed to delete contact')
  } finally {
    deletingKeys.value.delete(row.pubkey)
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

// ── Transport / type filters ────────────────────────────────────────────────
const filterTransport = ref('')
const filterType = ref('')

const transportOptions = computed(() =>
  [...new Set(rows.value.map((r) => r.transport).filter(Boolean))].sort(),
)
const typeOptions = computed(() =>
  [...new Set(rows.value.map((r) => r.type_name).filter(Boolean))].sort(),
)

const filteredRows = computed(() =>
  rows.value.filter(
    (r) =>
      (!filterTransport.value || r.transport === filterTransport.value) &&
      (!filterType.value || r.type_name === filterType.value),
  ),
)

onMounted(() => {
  load()
  timer = window.setInterval(load, 5000)
})
onUnmounted(() => { if (timer !== undefined) window.clearInterval(timer) })

const columns = computed(() => {
  const base = [
    { key: 'ts', label: 'last seen' },
    { key: 'name', label: 'name' },
    { key: 'transport', label: 'transport' },
    { key: 'type_name', label: 'type' },
    { key: 'pubkey', label: 'pubkey' },
    { key: 'location', label: 'location' },
  ]
  return auth.isAide ? [...base, { key: 'actions', label: '' }] : base
})
</script>

<template>
  <div class="page">
    <header class="page-header">
      <h1>contacts</h1>
      <p class="muted small">
        Protected contacts only — these survive node-database flooding on the
        radio and stay favorited across a BBS restart. See
        <router-link to="/adverts">discovered contacts</router-link> for
        every node heard, protected or not.
      </p>
    </header>

    <!-- Transport / type filters -->
    <div class="contact-filters">
      <label>
        transport
        <select v-model="filterTransport">
          <option value="">all</option>
          <option v-for="t in transportOptions" :key="t" :value="t">{{ t }}</option>
        </select>
      </label>
      <label>
        type
        <select v-model="filterType">
          <option value="">all</option>
          <option v-for="t in typeOptions" :key="t" :value="t">{{ t }}</option>
        </select>
      </label>
      <button
        v-if="filterTransport || filterType"
        type="button"
        class="link-btn"
        @click="filterTransport = ''; filterType = ''"
      >clear filters</button>
    </div>

    <p v-if="error" class="error">{{ error }}</p>

    <DataTable
      :columns="columns"
      :rows="filteredRows"
      :row-key="(r) => r.pubkey"
      :page-size="50"
      empty="No protected contacts yet. A contact becomes protected the first time it messages the BBS."
    >
      <template #[`cell:ts`]="{ row }">{{ fmtLocal(row.ts) }}</template>
      <template #[`cell:transport`]="{ row }">
        <span v-if="row.transport" class="badge" :class="`transport-${row.transport}`">{{ row.transport }}</span>
        <span v-else class="muted">—</span>
      </template>
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
      <template #[`cell:actions`]="{ row }">
        <button
          class="btn-danger btn-small"
          :disabled="deletingKeys.has(row.pubkey)"
          @click="deleteContact(row)"
        >
          {{ deletingKeys.has(row.pubkey) ? 'deleting…' : 'delete' }}
        </button>
      </template>
    </DataTable>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 0.7rem; }
.page-header { display: flex; flex-direction: column; gap: 0.2rem; }
h1 { margin: 0; }
.small { font-size: 0.85em; }

.contact-filters {
  display: flex;
  align-items: center;
  gap: 1rem;
  flex-wrap: wrap;
  font-size: 0.85em;
  color: var(--muted);
}
.contact-filters label { display: flex; align-items: center; gap: 0.4rem; }
.contact-filters select { font: inherit; padding: 0.15rem 0.3rem; }
.link-btn {
  background: none; border: none; padding: 0;
  color: var(--accent, #4a90d9); cursor: pointer; font: inherit;
  text-decoration: underline;
}

.badge {
  display: inline-block; padding: 0.05rem 0.45rem;
  border-radius: 3px; background: var(--row-alt); font-size: 0.85em;
}
.badge.type-chat     { background: #d2efd2; color: #205020; }
.badge.type-repeater { background: #d2dff0; color: #203560; }
.badge.type-room     { background: #f0e2c2; color: #604010; }
.badge.type-sensor   { background: #f0d2d2; color: #602020; }
.badge.type-unknown  { background: var(--row-alt); color: var(--muted); }
/* Meshtastic device roles */
.badge.type-client        { background: #d2efd2; color: #205020; }
.badge.type-client_mute   { background: #e0e8d2; color: #404f20; }
.badge.type-client_hidden { background: var(--row-alt); color: var(--muted); }
.badge.type-client_base   { background: #d2efd2; color: #205020; }
.badge.type-router        { background: #d2dff0; color: #203560; }
.badge.type-router_client { background: #d2dff0; color: #203560; }
.badge.type-router_late   { background: #d2dff0; color: #203560; }
.badge.type-tracker       { background: #e2d2f0; color: #402060; }
.badge.type-tak           { background: #f0e2c2; color: #604010; }
.badge.type-tak_tracker   { background: #f0e2c2; color: #604010; }
.badge.type-lost_and_found { background: #f0d2d2; color: #602020; }
.badge.transport-meshcore   { background: #d2dff0; color: #203560; }
.badge.transport-meshtastic { background: #e2d2f0; color: #402060; }

.btn-danger {
  background: var(--error);
  color: #fff;
  border-color: var(--error);
}
.btn-danger:hover:not(:disabled) { filter: brightness(1.08); }
.btn-danger:disabled { opacity: 0.5; }
.btn-small { padding: 0.15rem 0.5rem; font-size: 0.85em; }
</style>
