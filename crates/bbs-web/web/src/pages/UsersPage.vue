<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api } from '../api/client'
import { useToast } from '../composables/useToast'
import { useStatsStore } from '../stores/stats'
import { useAuthStore } from '../stores/auth'

interface UserInfo {
  id: number
  username: string
  display_name: string | null
  status: string
  permission_level: number
  created_at: string
  last_login_at: string | null
}

const ALL_USERS = ref<UserInfo[]>([])
const loading = ref(false)
const error = ref<string | null>(null)
const filterMode = ref<string>('')
const acting = ref(false)
const bulkValidating = ref(false)
const toast = useToast()
const stats = useStatsStore()
const auth = useAuthStore()
const route = useRoute()
const router = useRouter()

// Search driven from query param (cross-linked from SessionsPage)
const searchText = ref((route.query.search as string) ?? '')
watch(() => route.query.search, v => { searchText.value = (v as string) ?? '' })

const users = computed<UserInfo[]>(() => {
  let list = ALL_USERS.value
  if (filterMode.value === 'pending') list = list.filter(u => u.permission_level === 0 && u.status !== 'banned')
  else if (filterMode.value === '0') list = list.filter(u => u.status === 'active')
  else if (filterMode.value === '1') list = list.filter(u => u.status === 'banned')
  const q = searchText.value.trim().toLowerCase()
  if (q) list = list.filter(u => u.username.toLowerCase().includes(q) || (u.display_name ?? '').toLowerCase().includes(q))
  return list
})

const pendingCount = computed(() =>
  ALL_USERS.value.filter(u => u.permission_level === 0 && u.status !== 'banned').length
)

const pendingUsers = computed(() =>
  ALL_USERS.value.filter(u => u.permission_level === 0 && u.status !== 'banned')
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
  try {
    await api.patch(`/api/v1/users/${encodeURIComponent(username)}`, body)
    toast.ok(okMsg)
    await load()
    stats.refresh()
  } catch (e: any) {
    toast.error(e?.message ?? 'action failed')
  } finally {
    acting.value = false
  }
}

const validate   = (u: string) => doAction(u, { status: 0, permission_level: 10 }, `${u} verified`)
const ban        = (u: string) => doAction(u, { status: 1 }, `${u} banned`)
const unban      = (u: string) => doAction(u, { status: 0 }, `${u} unbanned`)
const del        = (u: string) => {
  if (!confirm(`Delete user "${u}"? They will be removed from this list. Recovery is only possible via SQL.`)) return
  doAction(u, { status: 2 }, `${u} deleted`)
}

async function setLevel(u: UserInfo, level: number) {
  if (level === u.permission_level) return
  await doAction(u.username, { permission_level: level }, `${u.username} set to ${levelLabel(level)}`)
}

async function bulkValidateAll() {
  if (!pendingUsers.value.length) return
  if (!confirm(`Validate all ${pendingUsers.value.length} pending users?`)) return
  bulkValidating.value = true
  let ok = 0, fail = 0
  for (const u of pendingUsers.value) {
    try {
      await api.patch(`/api/v1/users/${encodeURIComponent(u.username)}`, { status: 0, permission_level: 10 })
      ok++
    } catch { fail++ }
  }
  bulkValidating.value = false
  if (fail === 0) toast.ok(`${ok} user${ok !== 1 ? 's' : ''} verified`)
  else toast.error(`${ok} verified, ${fail} failed`)
  await load()
  stats.refresh()
}

// ── Detail drawer ─────────────────────────────────────────────────────────────
const drawerUser = ref<UserInfo | null>(null)
function openDrawer(u: UserInfo) {
  drawerUser.value = u
  showPwReset.value = false
  pwNew.value = ''
  pwConfirm.value = ''
  pwError.value = null
}
function closeDrawer() { drawerUser.value = null }

// ── Password reset (drawer) ───────────────────────────────────────────────────
const showPwReset = ref(false)
const pwNew = ref('')
const pwConfirm = ref('')
const pwError = ref<string | null>(null)
const resettingPw = ref(false)

async function submitPasswordReset() {
  pwError.value = null
  if (pwNew.value.length < 6) { pwError.value = 'Password must be at least 6 characters'; return }
  if (pwNew.value !== pwConfirm.value) { pwError.value = 'Passwords do not match'; return }
  if (!drawerUser.value) return
  resettingPw.value = true
  try {
    await api.patch(`/api/v1/users/${encodeURIComponent(drawerUser.value.username)}`, { password: pwNew.value })
    toast.ok(`Password reset for ${drawerUser.value.username}`)
    showPwReset.value = false
    pwNew.value = ''
    pwConfirm.value = ''
  } catch (e: any) {
    pwError.value = e?.message ?? 'password reset failed'
  } finally {
    resettingPw.value = false
  }
}

let pollTimer: ReturnType<typeof setInterval> | null = null
onMounted(() => {
  if (route.query.filter) filterMode.value = route.query.filter as string
  load()
  pollTimer = setInterval(load, 30_000)
})
onUnmounted(() => { if (pollTimer !== null) clearInterval(pollTimer) })
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
        <input
          v-model="searchText"
          placeholder="search username…"
          class="search-input"
          @input="() => router.replace({ query: { ...route.query, search: searchText || undefined } })"
        />
        <select v-model="filterMode">
          <option value="">all</option>
          <option value="pending">pending verification</option>
          <option value="0">active</option>
          <option value="1">banned</option>
        </select>
        <button
          v-if="pendingCount > 0 && auth.isAide"
          class="secondary"
          :disabled="bulkValidating"
          @click="bulkValidateAll"
        >verify all ({{ pendingCount }})</button>
      </div>
    </header>

    <p v-if="error" class="error">{{ error }}</p>
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
          <th>joined</th>
          <th>last login</th>
          <th>actions</th>
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="u in users"
          :key="u.id"
          :class="u.permission_level === 0 && u.status !== 'banned' ? 'row-pending' : ''"
        >
          <td>
            <button class="link-btn" @click="openDrawer(u)"><strong>{{ u.username }}</strong></button>
          </td>
          <td>{{ u.display_name ?? '—' }}</td>
          <td :class="u.status === 'banned' ? 'error' : ''">{{ u.status }}</td>
          <td :class="u.permission_level === 0 ? 'warn' : ''">
            <!-- Sysops get an inline dropdown; Aides see plain text (can't promote to sysop) -->
            <select
              v-if="auth.isSysop"
              :value="u.permission_level"
              :disabled="acting"
              class="level-select"
              @change="(e) => setLevel(u, Number((e.target as HTMLSelectElement).value))"
            >
              <option :value="0">unvalidated</option>
              <option :value="10">user</option>
              <option :value="50">aide</option>
              <option :value="100">sysop</option>
            </select>
            <span v-else>{{ levelLabel(u.permission_level) }}</span>
          </td>
          <td class="muted small">{{ u.created_at.slice(0, 10) }}</td>
          <td class="muted small">{{ u.last_login_at ? u.last_login_at.slice(0, 10) : '—' }}</td>
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
            <button
              class="small-btn danger"
              :disabled="acting"
              @click="del(u.username)"
            >delete</button>
          </td>
        </tr>
      </tbody>
    </table>

    <!-- Detail drawer -->
    <Teleport to="body">
      <div v-if="drawerUser" class="drawer-backdrop" @click.self="closeDrawer">
        <aside class="drawer">
          <div class="drawer-header">
            <h2>{{ drawerUser.username }}</h2>
            <button class="secondary small-btn" @click="closeDrawer">✕</button>
          </div>
          <dl class="detail-list">
            <dt>display name</dt>
            <dd>{{ drawerUser.display_name ?? '—' }}</dd>
            <dt>status</dt>
            <dd :class="drawerUser.status === 'banned' ? 'error' : ''">{{ drawerUser.status }}</dd>
            <dt>permission level</dt>
            <dd>{{ levelLabel(drawerUser.permission_level) }} ({{ drawerUser.permission_level }})</dd>
            <dt>joined</dt>
            <dd>{{ new Date(drawerUser.created_at).toLocaleString() }}</dd>
            <dt>last login</dt>
            <dd>{{ drawerUser.last_login_at ? new Date(drawerUser.last_login_at).toLocaleString() : 'never' }}</dd>
          </dl>
          <div class="drawer-links">
            <router-link
              :to="{ path: '/messages', query: { sender: drawerUser.username } }"
              @click="closeDrawer"
            >view messages →</router-link>
            <router-link
              :to="{ path: '/audit', query: { actor: drawerUser.username } }"
              @click="closeDrawer"
            >audit history →</router-link>
          </div>
          <div class="drawer-actions">
            <button
              v-if="drawerUser.status !== 'banned' && drawerUser.permission_level === 0"
              :disabled="acting"
              @click="validate(drawerUser.username); closeDrawer()"
            >verify</button>
            <button
              v-if="drawerUser.status !== 'banned'"
              class="danger"
              :disabled="acting"
              @click="ban(drawerUser.username); closeDrawer()"
            >ban</button>
            <button
              v-if="drawerUser.status === 'banned'"
              class="secondary"
              :disabled="acting"
              @click="unban(drawerUser.username); closeDrawer()"
            >unban</button>
            <button
              v-if="auth.isSysop"
              class="secondary"
              @click="showPwReset = !showPwReset; pwError = null"
            >{{ showPwReset ? 'cancel' : 'reset password' }}</button>
          </div>

          <form v-if="auth.isSysop && showPwReset" @submit.prevent="submitPasswordReset" class="pw-reset-form">
            <label>
              new password
              <input v-model="pwNew" type="password" autocomplete="new-password" placeholder="min 6 chars" required />
            </label>
            <label>
              confirm password
              <input v-model="pwConfirm" type="password" autocomplete="new-password" placeholder="repeat password" required />
            </label>
            <p v-if="pwError" class="error pw-error">{{ pwError }}</p>
            <button type="submit" :disabled="resettingPw">
              {{ resettingPw ? 'resetting…' : 'set password' }}
            </button>
          </form>
        </aside>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 1rem; }
.page-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; flex-wrap: wrap; }
.page-header div { display: flex; flex-direction: column; gap: 0.2rem; }
h1 { margin: 0; display: flex; align-items: center; gap: 0.6rem; }
p { margin: 0; }
.controls { display: flex; align-items: center; gap: 0.5rem; flex-wrap: wrap; }
.search-input { min-width: 160px; }
.small { font-size: 0.85em; }
.small-btn { padding: 0.2rem 0.5rem; font-size: 0.8em; margin-right: 0.3rem; }
.actions { white-space: nowrap; }
.warn { color: var(--warn, #b45309); font-weight: 600; }
.pending-badge {
  display: inline-block; font-size: 0.55em; font-weight: 600;
  background: var(--warn, #b45309); color: #fff; border-radius: 999px;
  padding: 0.15em 0.65em; vertical-align: middle; letter-spacing: 0.02em;
}
tr.row-pending { background: var(--accent-bg); }

.link-btn {
  background: transparent; border: none; padding: 0; cursor: pointer;
  color: var(--accent); font: inherit; text-decoration: underline;
}

.level-select {
  font-size: 0.82em; padding: 0.1rem 0.2rem;
  background: var(--bg); color: var(--fg); border: 1px solid var(--border);
  border-radius: 3px;
}

/* Drawer */
.drawer-backdrop {
  position: fixed; inset: 0; background: rgba(0,0,0,0.4); z-index: 200;
}
.drawer {
  position: fixed; top: 0; right: 0; bottom: 0; width: 320px; max-width: 90vw;
  background: var(--bg); border-left: 1px solid var(--border);
  padding: 1.2rem 1.4rem; overflow-y: auto;
  display: flex; flex-direction: column; gap: 1rem;
}
.drawer-header { display: flex; align-items: center; justify-content: space-between; }
.drawer-header h2 { margin: 0; }
.detail-list { display: grid; grid-template-columns: auto 1fr; gap: 0.3rem 1rem; font-size: 0.9em; margin: 0; }
dt { color: var(--muted); font-size: 0.82em; text-transform: uppercase; letter-spacing: 0.04em; align-self: center; }
dd { margin: 0; }
.drawer-links { display: flex; flex-direction: column; gap: 0.4rem; font-size: 0.9em; }
.drawer-actions { display: flex; gap: 0.5rem; flex-wrap: wrap; padding-top: 0.5rem; border-top: 1px solid var(--border); }

.pw-reset-form {
  display: flex; flex-direction: column; gap: 0.5rem;
  padding: 0.8rem; background: var(--row-alt);
  border: 1px solid var(--border); border-radius: 4px;
}
.pw-reset-form label {
  display: flex; flex-direction: column; gap: 0.2rem;
  font-size: 0.82em; color: var(--muted);
}
.pw-reset-form input { font-size: 0.9em; }
.pw-error { font-size: 0.85em; margin: 0; }
</style>
