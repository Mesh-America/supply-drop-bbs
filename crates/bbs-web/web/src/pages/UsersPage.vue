<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { api } from '../api/client'

interface UserInfo {
  id: number
  username: string
  display_name: string | null
  status: string
  permission_level: number
  created_at: string
  last_login_at: string | null
}

// 'pending' is a client-side filter (permission_level === 0); all other
// values are passed to the server as ?status=<n>.
const ALL_USERS = ref<UserInfo[]>([])
const loading = ref(false)
const error = ref<string | null>(null)
const filterMode = ref<string>('') // '' | 'pending' | '0' | '1'
const actionError = ref<string | null>(null)
const actionOk = ref<string | null>(null)
const acting = ref(false)

const users = computed<UserInfo[]>(() => {
  if (filterMode.value === 'pending') {
    return ALL_USERS.value.filter(u => u.permission_level === 0 && u.status !== 'banned')
  }
  if (filterMode.value === '0') return ALL_USERS.value.filter(u => u.status === 'active')
  if (filterMode.value === '1') return ALL_USERS.value.filter(u => u.status === 'banned')
  return ALL_USERS.value
})

const pendingCount = computed(() =>
  ALL_USERS.value.filter(u => u.permission_level === 0 && u.status !== 'banned').length
)

function levelLabel(l: number): string {
  if (l >= 100) return 'sysop'
  if (l >= 50) return 'aide'
  if (l >= 10) return 'user'
  return 'unvalidated'
}

async function load() {
  loading.value = true
  error.value = null
  try {
    // Always fetch all users; filtering is done client-side so the
    // pending count stays accurate regardless of the active filter.
    ALL_USERS.value = await api.get<UserInfo[]>('/api/v1/users')
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load users'
  } finally {
    loading.value = false
  }
}

async function doAction(username: string, body: object, okMsg: string) {
  if (acting.value) return
  acting.value = true
  actionError.value = null
  actionOk.value = null
  try {
    await api.patch(`/api/v1/users/${encodeURIComponent(username)}`, body)
    actionOk.value = okMsg
    await load()
  } catch (e: any) {
    actionError.value = e?.message ?? 'action failed'
  } finally {
    acting.value = false
  }
}

const validate = (u: string) => doAction(u, { status: 0, permission_level: 10 }, `${u} verified`)
const ban      = (u: string) => doAction(u, { status: 1 }, `${u} banned`)
const unban    = (u: string) => doAction(u, { status: 0 }, `${u} unbanned`)

onMounted(load)
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>
          users
          <span v-if="pendingCount > 0" class="pending-badge" title="pending verification">
            {{ pendingCount }} pending
          </span>
        </h1>
        <p class="muted">Manage BBS user accounts</p>
      </div>
      <div class="controls">
        <select v-model="filterMode">
          <option value="">all</option>
          <option value="pending">pending verification</option>
          <option value="0">active</option>
          <option value="1">banned</option>
        </select>
        <button class="secondary" @click="load" :disabled="loading">refresh</button>
      </div>
    </header>

    <p v-if="error" class="error">{{ error }}</p>
    <p v-if="actionError" class="error">{{ actionError }}</p>
    <p v-if="actionOk" class="ok">{{ actionOk }}</p>
    <p v-if="!loading && users.length === 0 && !error" class="muted">
      {{ filterMode === 'pending' ? 'No users awaiting verification.' : 'No users found.' }}
    </p>

    <table v-if="users.length > 0">
      <thead>
        <tr>
          <th>username</th>
          <th>display name</th>
          <th>status</th>
          <th>level</th>
          <th>created</th>
          <th>actions</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="u in users" :key="u.id" :class="u.permission_level === 0 && u.status !== 'banned' ? 'row-pending' : ''">
          <td><strong>{{ u.username }}</strong></td>
          <td>{{ u.display_name ?? '—' }}</td>
          <td :class="u.status === 'banned' ? 'error' : ''">{{ u.status }}</td>
          <td :class="u.permission_level === 0 ? 'warn' : ''">
            {{ levelLabel(u.permission_level) }}
          </td>
          <td class="muted small">{{ u.created_at.slice(0, 10) }}</td>
          <td class="actions">
            <button
              v-if="u.status !== 'banned' && u.permission_level === 0"
              class="small-btn"
              :disabled="acting"
              @click="validate(u.username)"
            >verify</button>
            <button
              v-if="u.status !== 'banned'"
              class="small-btn danger"
              :disabled="acting"
              @click="ban(u.username)"
            >ban</button>
            <button
              v-if="u.status === 'banned'"
              class="small-btn secondary"
              :disabled="acting"
              @click="unban(u.username)"
            >unban</button>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 1rem; }
.page-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; flex-wrap: wrap; }
.page-header div { display: flex; flex-direction: column; gap: 0.2rem; }
h1 { margin: 0; display: flex; align-items: center; gap: 0.6rem; }
p { margin: 0; }
.controls { display: flex; align-items: center; gap: 0.5rem; }
.small { font-size: 0.85em; }
.small-btn { padding: 0.2rem 0.5rem; font-size: 0.8em; margin-right: 0.3rem; }
.actions { white-space: nowrap; }
.ok { color: #2a8a2a; }
.warn { color: var(--warn, #b45309); font-weight: 600; }
.pending-badge {
  display: inline-block;
  font-size: 0.55em;
  font-weight: 600;
  background: var(--warn, #b45309);
  color: #fff;
  border-radius: 999px;
  padding: 0.15em 0.65em;
  vertical-align: middle;
  letter-spacing: 0.02em;
}
tr.row-pending { background: var(--accent-bg); }
</style>
