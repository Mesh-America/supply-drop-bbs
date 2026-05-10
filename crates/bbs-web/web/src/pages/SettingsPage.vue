<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { api, ApiError } from '../api/client'

interface ConfigData {
  config_file: string | null
  writable: boolean
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

// Flat form model — each field tracks what's in the text box / input.
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
const loadError = ref<string | null>(null)
const saveOk = ref<string | null>(null)
const saveError = ref<string | null>(null)

const LOG_LEVELS = ['TRACE', 'DEBUG', 'INFO', 'WARN', 'ERROR']

function populateForm(c: ConfigData) {
  configFile.value = c.config_file
  writable.value = c.writable

  form.value.bbs_name            = c.bbs_name            ?? 'Supply Drop BBS'
  form.value.bbs_starting_room   = c.bbs_starting_room   ?? 'Lobby'
  form.value.bbs_welcome_msg     = c.bbs_welcome_msg      ?? 'Welcome to {name}.'
  form.value.bbs_timezone        = c.bbs_timezone         ?? 'UTC'

  form.value.location_enabled    = c.location_latitude != null && c.location_longitude != null
  form.value.location_latitude   = c.location_latitude  != null ? String(c.location_latitude)  : ''
  form.value.location_longitude  = c.location_longitude != null ? String(c.location_longitude) : ''

  form.value.backup_enabled         = c.backup_enabled         ?? true
  form.value.backup_interval_hours  = c.backup_interval_hours  ?? 6
  form.value.backup_keep_daily      = c.backup_keep_daily      ?? 7
  form.value.backup_keep_weekly     = c.backup_keep_weekly      ?? 4

  // Convert seconds → display units
  form.value.security_session_web_hours   = Math.round((c.security_session_web_secs  ?? 43200)  / 3600)
  form.value.security_session_mesh_days   = Math.round((c.security_session_mesh_secs ?? 259200) / 86400)
  form.value.security_login_rate_per_min  = c.security_login_rate_per_min  ?? 5
  form.value.security_command_rate_per_min = c.security_command_rate_per_min ?? 60

  form.value.logging_level = c.logging_level ?? 'INFO'
}

async function load() {
  loading.value = true
  loadError.value = null
  try {
    const c = await api.get<ConfigData>('/api/v1/config')
    populateForm(c)
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

async function save() {
  saving.value = true
  saveOk.value = null
  saveError.value = null
  try {
    const patch: Record<string, unknown> = {
      bbs_name:            form.value.bbs_name,
      bbs_starting_room:   form.value.bbs_starting_room,
      bbs_welcome_msg:     form.value.bbs_welcome_msg,
      bbs_timezone:        form.value.bbs_timezone,
      backup_enabled:           form.value.backup_enabled,
      backup_interval_hours:    form.value.backup_interval_hours,
      backup_keep_daily:        form.value.backup_keep_daily,
      backup_keep_weekly:       form.value.backup_keep_weekly,
      security_session_web_secs:    form.value.security_session_web_hours * 3600,
      security_session_mesh_secs:   form.value.security_session_mesh_days * 86400,
      security_login_rate_per_min:  form.value.security_login_rate_per_min,
      security_command_rate_per_min: form.value.security_command_rate_per_min,
      logging_level:       form.value.logging_level,
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
  } catch (e: any) {
    saveError.value = e?.message ?? 'failed to save config'
  } finally {
    saving.value = false
  }
}

onMounted(load)
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
      Config file is not writable by the server process. Changes cannot be saved.
    </div>

    <div v-if="saveOk" class="notice ok-notice">{{ saveOk }}</div>
    <div v-if="saveError" class="notice error-notice">{{ saveError }}</div>

    <form v-if="!loadError" @submit.prevent="save" class="settings-form">

      <!-- BBS Identity -->
      <section class="card">
        <h2>BBS identity</h2>
        <div class="field">
          <label>Name</label>
          <input v-model="form.bbs_name" type="text" />
          <p class="hint">Display name shown to users on connect.</p>
        </div>
        <div class="field">
          <label>Starting room</label>
          <input v-model="form.bbs_starting_room" type="text" />
          <p class="hint">Room a newly logged-in user lands in.</p>
        </div>
        <div class="field">
          <label>Welcome message</label>
          <textarea v-model="form.bbs_welcome_msg" rows="2"></textarea>
          <p class="hint"><code>{name}</code> expands to the BBS name.</p>
        </div>
        <div class="field">
          <label>Timezone</label>
          <input v-model="form.bbs_timezone" type="text" placeholder="UTC" />
          <p class="hint">IANA timezone name, e.g. <code>America/New_York</code>. Used for display timestamps.</p>
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
          <div class="field">
            <label>Latitude</label>
            <input v-model="form.location_latitude" type="number" step="any" min="-90" max="90"
              placeholder="e.g. 37.7749" />
          </div>
          <div class="field">
            <label>Longitude</label>
            <input v-model="form.location_longitude" type="number" step="any" min="-180" max="180"
              placeholder="e.g. -122.4194" />
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
          <div class="field">
            <label>Interval (hours)</label>
            <input v-model.number="form.backup_interval_hours" type="number" min="1" max="168" />
          </div>
          <div class="field">
            <label>Keep daily backups</label>
            <input v-model.number="form.backup_keep_daily" type="number" min="0" max="365" />
          </div>
          <div class="field">
            <label>Keep weekly backups</label>
            <input v-model.number="form.backup_keep_weekly" type="number" min="0" max="52" />
          </div>
        </div>
      </section>

      <!-- Security -->
      <section class="card">
        <h2>Security</h2>
        <div class="field-row">
          <div class="field">
            <label>Web session lifetime (hours)</label>
            <input v-model.number="form.security_session_web_hours" type="number" min="1" max="8760" />
          </div>
          <div class="field">
            <label>Mesh session lifetime (days)</label>
            <input v-model.number="form.security_session_mesh_days" type="number" min="1" max="365" />
            <p class="hint">Mesh sessions persist longer — radio users disconnect frequently.</p>
          </div>
        </div>
        <div class="field-row">
          <div class="field">
            <label>Max login attempts / min</label>
            <input v-model.number="form.security_login_rate_per_min" type="number" min="1" max="100" />
          </div>
          <div class="field">
            <label>Max commands / min / session</label>
            <input v-model.number="form.security_command_rate_per_min" type="number" min="1" max="600" />
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
          <p class="hint">Requires restart to take effect.</p>
        </div>
      </section>

      <div class="actions">
        <button type="submit" :disabled="saving || !writable">
          {{ saving ? 'saving…' : 'save settings' }}
        </button>
        <span v-if="!writable" class="hint">config file is not writable</span>
      </div>

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
}
.ok-notice    { border: 1px solid #2a8a2a; background: rgba(42,138,42,0.08); color: #2a8a2a; }
.warn-notice  { border: 1px solid var(--warning, #b88b00); background: color-mix(in srgb, var(--warning, #b88b00) 8%, transparent); }
.error-notice { border: 1px solid var(--error); background: rgba(200,60,60,0.08); color: var(--error); }

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
