<script setup lang="ts">
import { ref, onMounted } from 'vue'
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

const users = ref<UserInfo[]>([])
const loading = ref(false)
const error = ref<string | null>(null)
const statusFilter = ref<string>('')
const actionError = ref<string | null>(null)
const actionOk = ref<string | null>(null)

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
    const qs = statusFilter.value !== '' ? `?status=${statusFilter.value}` : ''
    users.value = await api.get<UserInfo[]>(`/api/v1/users${qs}`)
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load users'
  } finally {
    loading.value = false
  }
}

async function validate(username: string) {
  actionError.value = null
  actionOk.value = null
  try {
    await api.patch(`/api/v1/users/${encodeURIComponent(username)}`, { status: 0, permission_level: 10 })
    actionOk.value = `${username} validated`
    await load()
  } catch (e: any) {
    actionError.value = e?.message ?? 'action failed'
  }
}

async function ban(username: string) {
  actionError.value = null
  actionOk.value = null
  try {
    await api.patch(`/api/v1/users/${encodeURIComponent(username)}`, { status: 1 })
    actionOk.value = `${username} banned`
    await load()
  } catch (e: any) {
    actionError.value = e?.message ?? 'action failed'
  }
}

async function unban(username: string) {
  actionError.value = null
  actionOk.value = null
  try {
    await api.patch(`/api/v1/users/${encodeURIComponent(username)}`, { status: 0 })
    actionOk.value = `${username} unbanned`
    await load()
  } catch (e: any) {
    actionError.value = e?.message ?? 'action failed'
  }
}

onMounted(load)
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>users</h1>
        <p class="muted">Manage BBS user accounts</p>
      </div>
      <div class="controls">
        <select v-model="statusFilter" @change="load">
          <option value="">all</option>
          <option value="0">active</option>
          <option value="1">banned</option>
        </select>
        <button class="secondary" @click="load" :disabled="loading">refresh</button>
      </div>
    </header>

    <p v-if="error" class="error">{{ error }}</p>
    <p v-if="actionError" class="error">{{ actionError }}</p>
    <p v-if="actionOk" class="ok">{{ actionOk }}</p>
    <p v-if="!loading && users.length === 0 && !error" class="muted">No users found.</p>

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
        <tr v-for="u in users" :key="u.id">
          <td><strong>{{ u.username }}</strong></td>
          <td>{{ u.display_name ?? '—' }}</td>
          <td :class="u.status === 'banned' ? 'error' : ''">{{ u.status }}</td>
          <td>{{ levelLabel(u.permission_level) }} ({{ u.permission_level }})</td>
          <td class="muted small">{{ u.created_at.slice(0, 10) }}</td>
          <td class="actions">
            <button
              v-if="u.status !== 'banned' && u.permission_level === 0"
              class="small-btn"
              @click="validate(u.username)"
            >validate</button>
            <button
              v-if="u.status !== 'banned'"
              class="small-btn danger"
              @click="ban(u.username)"
            >ban</button>
            <button
              v-if="u.status === 'banned'"
              class="small-btn secondary"
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
h1 { margin: 0; }
p { margin: 0; }
.controls { display: flex; align-items: center; gap: 0.5rem; }
.small { font-size: 0.85em; }
.small-btn { padding: 0.2rem 0.5rem; font-size: 0.8em; margin-right: 0.3rem; }
.actions { white-space: nowrap; }
.ok { color: #2a8a2a; }
</style>
