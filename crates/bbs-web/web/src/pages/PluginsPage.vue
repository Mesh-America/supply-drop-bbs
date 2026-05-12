<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import { api } from '../api/client'
import { useToast } from '../composables/useToast'
import { useAuthStore } from '../stores/auth'

interface PluginStatus {
  name: string
  command: string
  args: string[]
  enabled: boolean
  restart_on_crash: boolean
  state: string          // 'running' | 'stopped' | 'crashed' | 'disabled'
  reason?: string        // only when state === 'crashed'
  restart_count: number
  recent_logs: string[]
}

interface NativePlugin {
  name: string
  label: string
  compiled_in: boolean
  enabled: boolean
  connection_type?: string
}

const plugins = ref<PluginStatus[]>([])
const nativePlugins = ref<NativePlugin[]>([])
const pendingRestart = ref(false)
const loading = ref(false)
const error = ref<string | null>(null)
const acting = ref(false)
const restarting = ref(false)
const toast = useToast()
const auth = useAuthStore()

// ── Add plugin form ────────────────────────────────────────────────────────────
const showAdd = ref(false)
const addName = ref('')
const addCommand = ref('')
const addArgs = ref('')
const addEnabled = ref(true)
const addRestart = ref(true)
const addDelay = ref(5)
const adding = ref(false)

// ── Log drawer ────────────────────────────────────────────────────────────────
const logPlugin = ref<PluginStatus | null>(null)
const logLines = ref<string[]>([])
const loadingLogs = ref(false)

async function load() {
  loading.value = true
  error.value = null
  try {
    plugins.value = await api.get<PluginStatus[]>('/api/v1/plugins')
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load plugins'
  } finally {
    loading.value = false
  }
}

async function loadNative() {
  try {
    const res = await api.get<{ plugins: NativePlugin[]; pending_restart: boolean }>('/api/v1/native-plugins')
    nativePlugins.value = res.plugins
    pendingRestart.value = res.pending_restart
  } catch {}
}

function stateClass(p: PluginStatus): string {
  if (p.state === 'running') return 'ok'
  if (p.state === 'crashed') return 'error'
  return 'muted'
}

async function toggleNative(p: NativePlugin) {
  if (acting.value || !p.compiled_in) return
  acting.value = true
  try {
    await api.patch(`/api/v1/native-plugins/${p.name}`, { enabled: !p.enabled })
    const verb = p.enabled ? 'disabled' : 'enabled'
    toast.ok(`${p.label} will be ${verb} after restart`)
    await loadNative()
  } catch (e: any) {
    toast.error(e?.message ?? 'action failed')
  } finally {
    acting.value = false
  }
}

async function restartServer() {
  if (restarting.value) return
  if (!confirm('Restart the BBS server now? All active sessions will be disconnected.')) return
  restarting.value = true
  try {
    await api.post('/api/v1/restart', {})
    toast.ok('Restart initiated — page will reload in a few seconds…')
    setTimeout(() => window.location.reload(), 5000)
  } catch (e: any) {
    toast.error(e?.message ?? 'restart failed')
    restarting.value = false
  }
}

async function toggleEnabled(p: PluginStatus) {
  if (acting.value) return
  acting.value = true
  try {
    await api.patch(`/api/v1/plugins/${encodeURIComponent(p.name)}`, { enabled: !p.enabled })
    toast.ok(`${p.name} ${p.enabled ? 'disabled' : 'enabled'}`)
    await load()
  } catch (e: any) {
    toast.error(e?.message ?? 'action failed')
  } finally {
    acting.value = false
  }
}

async function restart(p: PluginStatus) {
  if (acting.value) return
  if (!confirm(`Restart plugin "${p.name}"?`)) return
  acting.value = true
  try {
    await api.post(`/api/v1/plugins/${encodeURIComponent(p.name)}/restart`, {})
    toast.ok(`${p.name} restarted`)
    await load()
  } catch (e: any) {
    toast.error(e?.message ?? 'restart failed')
  } finally {
    acting.value = false
  }
}

async function remove(p: PluginStatus) {
  if (acting.value) return
  if (!confirm(`Remove plugin "${p.name}"? This will stop the process and delete its config entry.`)) return
  acting.value = true
  try {
    await api.del(`/api/v1/plugins/${encodeURIComponent(p.name)}`)
    toast.ok(`${p.name} removed`)
    await load()
  } catch (e: any) {
    toast.error(e?.message ?? 'remove failed')
  } finally {
    acting.value = false
  }
}

async function submitAdd() {
  if (adding.value) return
  adding.value = true
  try {
    const args = addArgs.value.trim()
      ? addArgs.value.trim().split(/\s+/)
      : []
    await api.post('/api/v1/plugins', {
      name: addName.value.trim(),
      command: addCommand.value.trim(),
      args,
      enabled: addEnabled.value,
      restart_on_crash: addRestart.value,
      restart_delay_secs: addDelay.value,
    })
    toast.ok(`Plugin "${addName.value}" added`)
    showAdd.value = false
    addName.value = ''
    addCommand.value = ''
    addArgs.value = ''
    addEnabled.value = true
    addRestart.value = true
    addDelay.value = 5
    await load()
  } catch (e: any) {
    toast.error(e?.message ?? 'failed to add plugin')
  } finally {
    adding.value = false
  }
}

async function openLogs(p: PluginStatus) {
  logPlugin.value = p
  logLines.value = []
  loadingLogs.value = true
  try {
    const res = await api.get<{ lines: string[] }>(`/api/v1/plugins/${encodeURIComponent(p.name)}/logs?lines=100`)
    logLines.value = res.lines
  } catch (e: any) {
    logLines.value = [`error: ${e?.message ?? 'failed to load logs'}`]
  } finally {
    loadingLogs.value = false
  }
}

let pollTimer: ReturnType<typeof setInterval> | null = null
onMounted(() => {
  load()
  loadNative()
  pollTimer = setInterval(() => { load(); loadNative() }, 15_000)
})
onUnmounted(() => { if (pollTimer !== null) clearInterval(pollTimer) })
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>plugins</h1>
        <p class="muted">Manage transports and externally-spawned plugins</p>
      </div>
      <div class="controls">
        <button v-if="auth.isSysop" @click="showAdd = !showAdd" class="secondary">
          {{ showAdd ? 'cancel' : '+ add plugin' }}
        </button>
      </div>
    </header>

    <!-- Restart banner -->
    <div v-if="pendingRestart" class="restart-banner">
      <span>Transport config changed — a restart is required for changes to take effect.</span>
      <button v-if="auth.isSysop" :disabled="restarting" @click="restartServer" class="restart-btn">
        {{ restarting ? 'restarting…' : 'restart now' }}
      </button>
    </div>

    <!-- Built-in transports -->
    <section class="plugin-section">
      <h2 class="section-title">built-in transports</h2>
      <table class="native-table">
        <thead>
          <tr>
            <th>transport</th>
            <th>connection</th>
            <th>status</th>
            <th v-if="auth.isSysop">actions</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="p in nativePlugins" :key="p.name" :class="{ dimmed: !p.compiled_in }">
            <td>
              <strong>{{ p.label }}</strong>
              <span v-if="!p.compiled_in" class="badge muted">not in build</span>
            </td>
            <td class="muted small">{{ p.connection_type ?? '—' }}</td>
            <td>
              <span v-if="!p.compiled_in" class="muted">—</span>
              <span v-else-if="p.enabled" class="ok">enabled</span>
              <span v-else class="muted">disabled</span>
            </td>
            <td v-if="auth.isSysop" class="actions">
              <button
                v-if="p.compiled_in"
                class="small-btn secondary"
                :disabled="acting"
                @click="toggleNative(p)"
              >{{ p.enabled ? 'disable' : 'enable' }}</button>
              <span v-else class="muted small">—</span>
            </td>
          </tr>
        </tbody>
      </table>
      <p class="muted small config-hint">
        Connection settings are managed via <router-link to="/settings">settings</router-link> or
        <code>config.toml</code> directly. Enable/disable changes take effect after restart.
      </p>
    </section>

    <!-- Process transport plugins -->
    <section class="plugin-section">
      <h2 class="section-title">process plugins</h2>

      <!-- Add plugin form -->
      <div v-if="showAdd" class="add-form">
        <h3>Add process transport plugin</h3>
        <form @submit.prevent="submitAdd" class="form-grid">
          <label>name <input v-model="addName" placeholder="my-transport" required /></label>
          <label>command <input v-model="addCommand" placeholder="/usr/bin/my-transport" required /></label>
          <label>args <input v-model="addArgs" placeholder="--port 2323 --verbose" /></label>
          <label class="inline"><input type="checkbox" v-model="addEnabled" /> start enabled</label>
          <label class="inline"><input type="checkbox" v-model="addRestart" /> restart on crash</label>
          <label>restart delay (s) <input type="number" v-model="addDelay" min="1" max="300" /></label>
          <div class="form-actions">
            <button type="submit" :disabled="adding">{{ adding ? 'adding…' : 'add plugin' }}</button>
            <button type="button" class="secondary" @click="showAdd = false">cancel</button>
          </div>
        </form>
      </div>

      <p v-if="error" class="error">{{ error }}</p>
      <p v-if="!loading && plugins.length === 0 && !error" class="muted empty">
        No process plugins configured. Add one above or via <code>[[plugins.process]]</code> in config.toml.
      </p>

      <table v-if="plugins.length > 0">
        <thead>
          <tr>
            <th>name</th>
            <th>command</th>
            <th>state</th>
            <th>restarts</th>
            <th v-if="auth.isSysop">actions</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="p in plugins" :key="p.name">
            <td><strong>{{ p.name }}</strong></td>
            <td class="muted small">
              <code>{{ p.command }}{{ p.args.length ? ' ' + p.args.join(' ') : '' }}</code>
            </td>
            <td>
              <span :class="stateClass(p)">{{ p.state }}</span>
              <span v-if="p.state === 'crashed'" class="muted small"> — {{ p.reason }}</span>
            </td>
            <td class="muted small">{{ p.restart_count }}</td>
            <td v-if="auth.isSysop" class="actions">
              <button class="small-btn secondary" @click="openLogs(p)">logs</button>
              <button
                class="small-btn secondary"
                :disabled="acting"
                @click="toggleEnabled(p)"
              >{{ p.enabled ? 'disable' : 'enable' }}</button>
              <button
                v-if="p.state === 'running' || p.state === 'crashed'"
                class="small-btn"
                :disabled="acting"
                @click="restart(p)"
              >restart</button>
              <button
                class="small-btn danger"
                :disabled="acting"
                @click="remove(p)"
              >remove</button>
            </td>
          </tr>
        </tbody>
      </table>
    </section>

    <!-- Log drawer -->
    <Teleport to="body">
      <div v-if="logPlugin" class="drawer-backdrop" @click.self="logPlugin = null">
        <aside class="drawer">
          <div class="drawer-header">
            <h2>{{ logPlugin.name }} — stderr</h2>
            <button class="secondary small-btn" @click="logPlugin = null">✕</button>
          </div>
          <p v-if="loadingLogs" class="muted">loading…</p>
          <p v-else-if="logLines.length === 0" class="muted">No log lines captured yet.</p>
          <pre v-else class="log-pre">{{ logLines.join('\n') }}</pre>
        </aside>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 1.5rem; }
.page-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; flex-wrap: wrap; }
.page-header div { display: flex; flex-direction: column; gap: 0.2rem; }
h1 { margin: 0; }
p { margin: 0; }
.controls { display: flex; align-items: center; gap: 0.5rem; }
.empty { padding-top: 0.5rem; }
.small { font-size: 0.85em; }
.small-btn { padding: 0.2rem 0.5rem; font-size: 0.8em; margin-right: 0.3rem; }
.actions { white-space: nowrap; }
.ok { color: var(--accent); font-weight: 600; }

/* Restart banner */
.restart-banner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  padding: 0.7rem 1rem;
  background: var(--row-alt);
  border: 1px solid var(--accent);
  border-radius: 4px;
  font-size: 0.9em;
}
.restart-btn {
  white-space: nowrap;
  padding: 0.3rem 0.8rem;
  font-size: 0.85em;
}

/* Section layout */
.plugin-section { display: flex; flex-direction: column; gap: 0.5rem; }

.section-title {
  margin: 0 0 0.4rem;
  font-size: 0.8em;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--muted);
  font-weight: 600;
}

.native-table { width: 100%; }

.badge {
  display: inline-block;
  font-size: 0.7em;
  padding: 0.1em 0.4em;
  border: 1px solid var(--border);
  border-radius: 3px;
  margin-left: 0.4rem;
  vertical-align: middle;
}

.dimmed { opacity: 0.45; }

.config-hint { margin-top: 0.25rem; }
.config-hint a { color: var(--accent); }

/* Add form */
.add-form {
  border: 1px solid var(--border); border-radius: 4px;
  padding: 1rem 1.2rem; background: var(--row-alt);
}
.add-form h3 { margin: 0 0 0.8rem; font-size: 0.95em; }
.form-grid { display: flex; flex-direction: column; gap: 0.5rem; }
.form-grid label { display: flex; flex-direction: column; gap: 0.2rem; font-size: 0.85em; color: var(--muted); }
.form-grid label.inline { flex-direction: row; align-items: center; gap: 0.4rem; color: var(--fg); }
.form-grid input[type="text"], .form-grid input:not([type="checkbox"]):not([type="number"]) { font-size: 0.9em; }
.form-actions { display: flex; gap: 0.5rem; padding-top: 0.3rem; }

/* Drawer */
.drawer-backdrop { position: fixed; inset: 0; background: rgba(0,0,0,0.4); z-index: 200; }
.drawer {
  position: fixed; top: 0; right: 0; bottom: 0; width: 480px; max-width: 95vw;
  background: var(--bg); border-left: 1px solid var(--border);
  padding: 1.2rem 1.4rem; overflow-y: auto;
  display: flex; flex-direction: column; gap: 1rem;
}
.drawer-header { display: flex; align-items: center; justify-content: space-between; }
.drawer-header h2 { margin: 0; font-size: 1em; }
.log-pre {
  font-size: 0.78em; white-space: pre-wrap; word-break: break-all;
  background: var(--row-alt); border: 1px solid var(--border);
  border-radius: 3px; padding: 0.6rem 0.8rem; margin: 0;
  flex: 1; overflow-y: auto;
}
</style>
