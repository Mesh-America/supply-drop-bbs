<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { api, ApiError } from '../api/client'

interface ConfigData {
  config_file: string | null
  writable: boolean
  server_timezone: string
  bbs_name: string | null
  bbs_starting_room: string | null
  bbs_welcome_msg: string | null
  bbs_timezone: string | null
  location_latitude: number | null
  location_longitude: number | null
  backup_enabled: boolean | null
  backup_interval_hours: number | null
  backup_keep_daily: number | null
  backup_keep_weekly: number | null
  security_session_web_secs: number | null
  security_session_mesh_secs: number | null
  security_login_rate_per_min: number | null
  security_command_rate_per_min: number | null
  logging_level: string | null
}

interface RoomSummary {
  name: string
}

const form = ref({
  bbs_name: '',
  bbs_starting_room: '',
  bbs_welcome_msg: '',
  bbs_timezone: '',
  location_enabled: false,
  location_latitude: '',
  location_longitude: '',
  backup_enabled: true,
  backup_interval_hours: 6,
  backup_keep_daily: 7,
  backup_keep_weekly: 4,
  security_session_web_hours: 12,
  security_session_mesh_days: 3,
  security_login_rate_per_min: 5,
  security_command_rate_per_min: 60,
  logging_level: 'INFO',
})

const configFile = ref<string | null>(null)
const writable = ref(false)
const loading = ref(false)
const saving = ref(false)

// Snapshot taken after each successful load/save; used for dirty detection.
const savedForm = ref<string>('')
const isDirty = computed(() => savedForm.value !== '' && JSON.stringify(form.value) !== savedForm.value)

const isFormValid = computed<boolean>(() => {
  const f = form.value
  if (!f.bbs_name.trim()) return false
  if (!f.bbs_starting_room.trim()) return false
  if (!f.bbs_timezone.trim()) return false
  if (timezones.value.length && !timezones.value.includes(f.bbs_timezone)) return false
  if (f.location_enabled) {
    const lat = parseFloat(f.location_latitude)
    const lon = parseFloat(f.location_longitude)
    if (isNaN(lat) || lat < -90 || lat > 90) return false
    if (isNaN(lon) || lon < -180 || lon > 180) return false
  }
  const { backup_interval_hours, backup_keep_daily, backup_keep_weekly,
          security_session_web_hours, security_session_mesh_days,
          security_login_rate_per_min, security_command_rate_per_min } = f
  if (!Number.isInteger(backup_interval_hours) || backup_interval_hours < 1 || backup_interval_hours > 168) return false
  if (!Number.isInteger(backup_keep_daily) || backup_keep_daily < 0 || backup_keep_daily > 365) return false
  if (!Number.isInteger(backup_keep_weekly) || backup_keep_weekly < 0 || backup_keep_weekly > 52) return false
  if (!Number.isInteger(security_session_web_hours) || security_session_web_hours < 1 || security_session_web_hours > 8760) return false
  if (!Number.isInteger(security_session_mesh_days) || security_session_mesh_days < 1 || security_session_mesh_days > 365) return false
  if (!Number.isInteger(security_login_rate_per_min) || security_login_rate_per_min < 1 || security_login_rate_per_min > 100) return false
  if (!Number.isInteger(security_command_rate_per_min) || security_command_rate_per_min < 1 || security_command_rate_per_min > 600) return false
  return true
})
const restarting = ref(false)
const restartOk = ref(false)
const loadError = ref<string | null>(null)
const saveOk = ref<string | null>(null)
const saveError = ref<string | null>(null)
const restartError = ref<string | null>(null)
const validationErrors = ref<Record<string, string>>({})

const rooms = ref<string[]>([])
const roomsLoading = ref(false)

const LOG_LEVELS = ['TRACE', 'DEBUG', 'INFO', 'WARN', 'ERROR']

const timezones = computed<string[]>(() => {
  try {
    return (Intl as any).supportedValuesOf('timeZone') as string[]
  } catch {
    return []
  }
})

function populateForm(c: ConfigData) {
  configFile.value = c.config_file
  writable.value = c.writable

  form.value.bbs_name          = c.bbs_name          ?? 'Supply Drop BBS'
  form.value.bbs_starting_room = c.bbs_starting_room ?? 'Lobby'
  form.value.bbs_welcome_msg   = c.bbs_welcome_msg   ?? 'Welcome to {name}.'
  form.value.bbs_timezone      = c.bbs_timezone      ?? c.server_timezone ?? 'UTC'

  form.value.location_enabled   = c.location_latitude != null && c.location_longitude != null
  form.value.location_latitude  = c.location_latitude  != null ? String(c.location_latitude)  : ''
  form.value.location_longitude = c.location_longitude != null ? String(c.location_longitude) : ''

  form.value.backup_enabled        = c.backup_enabled        ?? true
  form.value.backup_interval_hours = c.backup_interval_hours ?? 6
  form.value.backup_keep_daily     = c.backup_keep_daily     ?? 7
  form.value.backup_keep_weekly    = c.backup_keep_weekly    ?? 4

  form.value.security_session_web_hours    = Math.round((c.security_session_web_secs  ?? 43200)  / 3600)
  form.value.security_session_mesh_days    = Math.round((c.security_session_mesh_secs ?? 259200) / 86400)
  form.value.security_login_rate_per_min   = c.security_login_rate_per_min  ?? 5
  form.value.security_command_rate_per_min = c.security_command_rate_per_min ?? 60

  form.value.logging_level = c.logging_level ?? 'INFO'
}

async function load() {
  loading.value = true
  loadError.value = null
  try {
    const c = await api.get<ConfigData>('/api/v1/config')
    populateForm(c)
    savedForm.value = JSON.stringify(form.value)
  } catch (e: any) {
    if (e instanceof ApiError && e.status === 404) {
      loadError.value = 'Config file path not set. Add config_path to [plugins.web] in your config.toml.'
    } else {
      loadError.value = e?.message ?? 'failed to load config'
    }
  } finally {
    loading.value = false
  }
}

async function loadRooms() {
  roomsLoading.value = true
  try {
    const list = await api.get<RoomSummary[]>('/api/v1/rooms')
    rooms.value = list.map(r => r.name)
  } catch {
    // degrades gracefully to text input
  } finally {
    roomsLoading.value = false
  }
}

function validate(): boolean {
  const errs: Record<string, string> = {}

  if (!form.value.bbs_name.trim())
    errs.bbs_name = 'Name is required.'

  if (!form.value.bbs_starting_room.trim())
    errs.bbs_starting_room = 'Starting room is required.'

  if (!form.value.bbs_timezone.trim())
    errs.bbs_timezone = 'Timezone is required.'
  else if (timezones.value.length && !timezones.value.includes(form.value.bbs_timezone))
    errs.bbs_timezone = 'Not a valid IANA timezone name.'

  if (form.value.location_enabled) {
    const lat = parseFloat(form.value.location_latitude)
    const lon = parseFloat(form.value.location_longitude)
    if (isNaN(lat) || lat < -90 || lat > 90)
      errs.location_latitude = 'Must be a number between -90 and 90.'
    if (isNaN(lon) || lon < -180 || lon > 180)
      errs.location_longitude = 'Must be a number between -180 and 180.'
  }

  const { backup_interval_hours, backup_keep_daily, backup_keep_weekly,
          security_session_web_hours, security_session_mesh_days,
          security_login_rate_per_min, security_command_rate_per_min } = form.value

  if (!Number.isInteger(backup_interval_hours) || backup_interval_hours < 1 || backup_interval_hours > 168)
    errs.backup_interval_hours = 'Must be 1–168.'
  if (!Number.isInteger(backup_keep_daily) || backup_keep_daily < 0 || backup_keep_daily > 365)
    errs.backup_keep_daily = 'Must be 0–365.'
  if (!Number.isInteger(backup_keep_weekly) || backup_keep_weekly < 0 || backup_keep_weekly > 52)
    errs.backup_keep_weekly = 'Must be 0–52.'
  if (!Number.isInteger(security_session_web_hours) || security_session_web_hours < 1 || security_session_web_hours > 8760)
    errs.security_session_web_hours = 'Must be 1–8760.'
  if (!Number.isInteger(security_session_mesh_days) || security_session_mesh_days < 1 || security_session_mesh_days > 365)
    errs.security_session_mesh_days = 'Must be 1–365.'
  if (!Number.isInteger(security_login_rate_per_min) || security_login_rate_per_min < 1 || security_login_rate_per_min > 100)
    errs.security_login_rate_per_min = 'Must be 1–100.'
  if (!Number.isInteger(security_command_rate_per_min) || security_command_rate_per_min < 1 || security_command_rate_per_min > 600)
    errs.security_command_rate_per_min = 'Must be 1–600.'

  validationErrors.value = errs
  return Object.keys(errs).length === 0
}

async function save() {
  saveOk.value = null
  saveError.value = null
  if (!validate()) return

  saving.value = true
  try {
    const patch: Record<string, unknown> = {
      bbs_name:            form.value.bbs_name,
      bbs_starting_room:   form.value.bbs_starting_room,
      bbs_welcome_msg:     form.value.bbs_welcome_msg,
      bbs_timezone:        form.value.bbs_timezone,
      backup_enabled:                form.value.backup_enabled,
      backup_interval_hours:         form.value.backup_interval_hours,
      backup_keep_daily:             form.value.backup_keep_daily,
      backup_keep_weekly:            form.value.backup_keep_weekly,
      security_session_web_secs:     form.value.security_session_web_hours * 3600,
      security_session_mesh_secs:    form.value.security_session_mesh_days * 86400,
      security_login_rate_per_min:   form.value.security_login_rate_per_min,
      security_command_rate_per_min: form.value.security_command_rate_per_min,
      logging_level: form.value.logging_level,
    }

    if (form.value.location_enabled) {
      patch.location_latitude  = parseFloat(form.value.location_latitude)
      patch.location_longitude = parseFloat(form.value.location_longitude)
    } else {
      patch.location_latitude  = null
      patch.location_longitude = null
    }

    const res = await api.patch<{ message: string }>('/api/v1/config', patch)
    saveOk.value = res.message
    savedForm.value = JSON.stringify(form.value)
  } catch (e: any) {
    saveError.value = e?.message ?? 'failed to save config'
  } finally {
    saving.value = false
  }
}

async function restartService() {
  restarting.value = true
  restartOk.value = false
  restartError.value = null
  try {
    await api.post('/api/v1/restart')
  } catch (e: any) {
    // A network error here means the process died before responding —
    // that's fine, the restart happened. Any other error is a real failure.
    if (!(e instanceof TypeError)) {
      restarting.value = false
      restartError.value = e?.message ?? 'restart request failed'
      return
    }
  }
  // Poll /api/v1/health until the server comes back (up to 60 s).
  for (let i = 0; i < 30; i++) {
    await new Promise(r => setTimeout(r, 2000))
    try {
      const r = await fetch('/api/v1/health')
      if (r.ok) {
        restartOk.value = true
        restarting.value = false
        setTimeout(() => window.location.reload(), 1500)
        return
      }
    } catch {
      // still coming back up — keep polling
    }
  }
  restarting.value = false
  restartError.value = 'Service did not come back within 60 s. Check: journalctl -u supply-drop-bbs -f'
}

onMounted(() => {
  load()
  loadRooms()
})
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>settings</h1>
        <p class="muted small">
          Editing
          <code v-if="configFile">{{ configFile }}</code>
          <span v-else>config file</span>.
          Most changes require a server restart to take effect.
        </p>
      </div>
    </header>

    <div v-if="loadError" class="notice error-notice">{{ loadError }}</div>

    <div v-if="!loadError && !writable && !loading" class="notice warn-notice">
      <p><strong>Config file is not writable by the server process.</strong> Changes cannot be saved.</p>
      <p class="hint-block">Fix this by running one of the following as root, then restarting the service:</p>
      <pre v-if="configFile">chown &lt;service-user&gt; {{ configFile }}
  # or, to make it group-writable:
chmod g+w {{ configFile }}</pre>
      <pre v-else>chown &lt;service-user&gt; /etc/supply-drop-bbs/config.toml</pre>
      <p class="hint-block">Replace <code>&lt;service-user&gt;</code> with the user the server process runs as (e.g. <code>supply-drop-bbs</code>, <code>www-data</code>).</p>
    </div>

    <div v-if="saveOk" class="notice ok-notice">{{ saveOk }}</div>
    <div v-if="saveError" class="notice error-notice">{{ saveError }}</div>

    <form v-if="!loadError" @submit.prevent="save" class="settings-form" novalidate>

      <!-- BBS Identity -->
      <section class="card">
        <h2>BBS identity</h2>

        <div class="field" :class="{ 'has-error': validationErrors.bbs_name }">
          <label>Name</label>
          <input v-model="form.bbs_name" type="text" />
          <p v-if="validationErrors.bbs_name" class="field-error">{{ validationErrors.bbs_name }}</p>
          <p v-else class="hint">Display name shown to users on connect.</p>
        </div>

        <div class="field" :class="{ 'has-error': validationErrors.bbs_starting_room }">
          <label>Starting room</label>
          <select v-if="rooms.length" v-model="form.bbs_starting_room">
            <option
              v-if="form.bbs_starting_room && !rooms.includes(form.bbs_starting_room)"
              :value="form.bbs_starting_room"
            >{{ form.bbs_starting_room }}</option>
            <option v-for="r in rooms" :key="r" :value="r">{{ r }}</option>
          </select>
          <input v-else v-model="form.bbs_starting_room" type="text" :placeholder="roomsLoading ? 'loading rooms…' : 'e.g. Lobby'" />
          <p v-if="validationErrors.bbs_starting_room" class="field-error">{{ validationErrors.bbs_starting_room }}</p>
          <p v-else class="hint">Room a newly logged-in user lands in.</p>
        </div>

        <div class="field">
          <label>Welcome message</label>
          <textarea v-model="form.bbs_welcome_msg" rows="2"></textarea>
          <p class="hint"><code>{name}</code> expands to the BBS name.</p>
        </div>

        <div class="field" :class="{ 'has-error': validationErrors.bbs_timezone }">
          <label>Timezone</label>
          <select v-if="timezones.length" v-model="form.bbs_timezone">
            <option
              v-if="form.bbs_timezone && !timezones.includes(form.bbs_timezone)"
              :value="form.bbs_timezone"
            >{{ form.bbs_timezone }}</option>
            <option v-for="tz in timezones" :key="tz" :value="tz">{{ tz }}</option>
          </select>
          <input
            v-else
            v-model="form.bbs_timezone"
            type="text"
            placeholder="e.g. America/New_York"
          />
          <p v-if="validationErrors.bbs_timezone" class="field-error">{{ validationErrors.bbs_timezone }}</p>
          <p v-else class="hint">IANA timezone name. Used for display timestamps.</p>
        </div>
      </section>

      <!-- GPS location -->
      <section class="card">
        <h2>GPS location</h2>
        <p class="hint">
          When set, the mesh transport sends your coordinates to the radio on connect so your
          node appears on the map in LoRa adverts.
          <strong>Takes effect on the next mesh transport reconnect — no restart needed.</strong>
        </p>
        <div class="field checkbox-field">
          <label>
            <input type="checkbox" v-model="form.location_enabled" />
            Set GPS coordinates
          </label>
        </div>
        <div v-if="form.location_enabled" class="field-row">
          <div class="field" :class="{ 'has-error': validationErrors.location_latitude }">
            <label>Latitude</label>
            <input v-model="form.location_latitude" type="number" step="any" min="-90" max="90"
              placeholder="e.g. 37.7749" />
            <p v-if="validationErrors.location_latitude" class="field-error">{{ validationErrors.location_latitude }}</p>
          </div>
          <div class="field" :class="{ 'has-error': validationErrors.location_longitude }">
            <label>Longitude</label>
            <input v-model="form.location_longitude" type="number" step="any" min="-180" max="180"
              placeholder="e.g. -122.4194" />
            <p v-if="validationErrors.location_longitude" class="field-error">{{ validationErrors.location_longitude }}</p>
          </div>
        </div>
      </section>

      <!-- Backup -->
      <section class="card">
        <h2>Automatic backups</h2>
        <div class="field checkbox-field">
          <label>
            <input type="checkbox" v-model="form.backup_enabled" />
            Enable automatic periodic backups
          </label>
        </div>
        <div class="field-row">
          <div class="field" :class="{ 'has-error': validationErrors.backup_interval_hours }">
            <label>Interval (hours)</label>
            <input v-model.number="form.backup_interval_hours" type="number" min="1" max="168" />
            <p v-if="validationErrors.backup_interval_hours" class="field-error">{{ validationErrors.backup_interval_hours }}</p>
          </div>
          <div class="field" :class="{ 'has-error': validationErrors.backup_keep_daily }">
            <label>Keep daily backups</label>
            <input v-model.number="form.backup_keep_daily" type="number" min="0" max="365" />
            <p v-if="validationErrors.backup_keep_daily" class="field-error">{{ validationErrors.backup_keep_daily }}</p>
          </div>
          <div class="field" :class="{ 'has-error': validationErrors.backup_keep_weekly }">
            <label>Keep weekly backups</label>
            <input v-model.number="form.backup_keep_weekly" type="number" min="0" max="52" />
            <p v-if="validationErrors.backup_keep_weekly" class="field-error">{{ validationErrors.backup_keep_weekly }}</p>
          </div>
        </div>
      </section>

      <!-- Security -->
      <section class="card">
        <h2>Security</h2>
        <div class="field-row">
          <div class="field" :class="{ 'has-error': validationErrors.security_session_web_hours }">
            <label>Web session lifetime (hours)</label>
            <input v-model.number="form.security_session_web_hours" type="number" min="1" max="8760" />
            <p v-if="validationErrors.security_session_web_hours" class="field-error">{{ validationErrors.security_session_web_hours }}</p>
          </div>
          <div class="field" :class="{ 'has-error': validationErrors.security_session_mesh_days }">
            <label>Mesh session lifetime (days)</label>
            <input v-model.number="form.security_session_mesh_days" type="number" min="1" max="365" />
            <p v-if="validationErrors.security_session_mesh_days" class="field-error">{{ validationErrors.security_session_mesh_days }}</p>
            <p v-else class="hint">Mesh sessions persist longer — radio users disconnect frequently.</p>
          </div>
        </div>
        <div class="field-row">
          <div class="field" :class="{ 'has-error': validationErrors.security_login_rate_per_min }">
            <label>Max login attempts / min</label>
            <input v-model.number="form.security_login_rate_per_min" type="number" min="1" max="100" />
            <p v-if="validationErrors.security_login_rate_per_min" class="field-error">{{ validationErrors.security_login_rate_per_min }}</p>
          </div>
          <div class="field" :class="{ 'has-error': validationErrors.security_command_rate_per_min }">
            <label>Max commands / min / session</label>
            <input v-model.number="form.security_command_rate_per_min" type="number" min="1" max="600" />
            <p v-if="validationErrors.security_command_rate_per_min" class="field-error">{{ validationErrors.security_command_rate_per_min }}</p>
          </div>
        </div>
      </section>

      <!-- Logging -->
      <section class="card">
        <h2>Logging</h2>
        <div class="field">
          <label>Log level</label>
          <select v-model="form.logging_level">
            <option v-for="l in LOG_LEVELS" :key="l" :value="l">{{ l }}</option>
          </select>
          <p class="hint">Takes effect immediately — no restart needed.</p>
        </div>
      </section>

      <div class="actions">
        <button type="submit" :disabled="saving || !writable || !isDirty || !isFormValid">
          {{ saving ? 'saving…' : 'save settings' }}
        </button>
        <span v-if="!writable" class="hint">config file is not writable</span>
        <span v-else-if="!isDirty" class="hint">no unsaved changes</span>
      </div>

      <!-- Service restart -->
      <section class="card">
        <h2>Service</h2>
        <p class="hint">
          Restart the systemd service to apply config changes. The web UI will
          reconnect automatically when the service comes back up (~5 s).
        </p>
        <div v-if="restartOk" class="notice ok-notice">Service restarted. Reloading…</div>
        <div v-if="restartError" class="notice error-notice">{{ restartError }}</div>
        <div class="actions">
          <button type="button" class="secondary" :disabled="restarting" @click="restartService">
            {{ restarting ? 'restarting…' : 'restart service' }}
          </button>
          <span v-if="restarting" class="hint">waiting for service to come back…</span>
        </div>
      </section>

    </form>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 1.2rem; }
.page-header { display: flex; flex-direction: column; gap: 0.2rem; }
h1 { margin: 0; }
h2 { margin: 0 0 1rem; font-size: 1em; text-transform: uppercase; letter-spacing: 0.06em; color: var(--muted); }
.small { font-size: 0.85em; }
p { margin: 0; }

.notice {
  padding: 0.7rem 1rem;
  border-radius: 4px;
  font-size: 0.9em;
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
}
.ok-notice    { border: 1px solid #2a8a2a; background: rgba(42,138,42,0.08); color: #2a8a2a; }
.warn-notice  { border: 1px solid var(--warning, #b88b00); background: color-mix(in srgb, var(--warning, #b88b00) 8%, transparent); }
.error-notice { border: 1px solid var(--error); background: rgba(200,60,60,0.08); color: var(--error); }

.notice pre {
  margin: 0.2rem 0;
  padding: 0.5rem 0.75rem;
  background: rgba(0,0,0,0.15);
  border-radius: 4px;
  font-size: 0.85em;
  white-space: pre-wrap;
  word-break: break-all;
}
.hint-block { color: inherit; opacity: 0.85; font-size: 0.85em; }

.settings-form { display: flex; flex-direction: column; gap: 1rem; }

.card {
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 1.1rem 1.2rem;
  background: var(--bg);
  display: flex;
  flex-direction: column;
  gap: 0.9rem;
}

.field { display: flex; flex-direction: column; gap: 0.3rem; }
.field label { font-size: 0.85em; font-weight: 600; }
.field input[type="text"],
.field input[type="number"],
.field textarea,
.field select {
  width: 100%;
  max-width: 420px;
  padding: 0.4rem 0.55rem;
  border: 1px solid var(--border);
  border-radius: 4px;
  background: var(--row-alt);
  color: var(--fg);
  font-size: 0.9em;
  font-family: inherit;
}
.field textarea { resize: vertical; }
.field select { cursor: pointer; }
.hint { font-size: 0.78em; color: var(--muted); margin: 0; }
.field-error { font-size: 0.78em; color: var(--error); margin: 0; }
.has-error input,
.has-error select,
.has-error textarea { border-color: var(--error); }

.checkbox-field label {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  font-size: 0.9em;
  font-weight: normal;
  cursor: pointer;
}

.field-row {
  display: flex;
  gap: 1.5rem;
  flex-wrap: wrap;
}
.field-row .field { flex: 1; min-width: 160px; }

.actions { display: flex; align-items: center; gap: 1rem; padding-top: 0.4rem; }
</style>
