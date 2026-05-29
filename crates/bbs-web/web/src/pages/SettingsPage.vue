<script setup lang="ts">
import { ref, computed, watch, onMounted } from 'vue'
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

interface AccessPolicyData {
  require_verify: boolean
  guest_room: string | null
  guest_room_id: number | null
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

// ── Access policy ─────────────────────────────────────────────────────────────

const accessPolicy = ref<AccessPolicyData | null>(null)
const accessPolicyLoading = ref(false)
const accessPolicyError = ref<string | null>(null)

// Working copy, updated independently of the main config form.
const apRequireVerify = ref(true)
const apGuestRoom = ref('')
const apGuestRoomEnabled = ref(false)

const accessPolicySaving = ref(false)
const accessPolicySaveOk = ref<string | null>(null)
const accessPolicySaveError = ref<string | null>(null)

async function loadAccessPolicy() {
  accessPolicyLoading.value = true
  accessPolicyError.value = null
  try {
    const p = await api.get<AccessPolicyData>('/api/v1/access-policy')
    accessPolicy.value = p
    apRequireVerify.value    = p.require_verify
    apGuestRoomEnabled.value = p.guest_room != null
    apGuestRoom.value        = p.guest_room ?? ''
  } catch (e: any) {
    accessPolicyError.value = e?.message ?? 'failed to load access policy'
  } finally {
    accessPolicyLoading.value = false
  }
}

async function saveAccessPolicy() {
  accessPolicySaving.value   = true
  accessPolicySaveOk.value   = null
  accessPolicySaveError.value = null
  try {
    const patch: Record<string, unknown> = {
      require_verify: apRequireVerify.value,
      guest_room: apGuestRoomEnabled.value
        ? (apGuestRoom.value.trim() || null)
        : null,
    }
    const updated = await api.patch<AccessPolicyData>('/api/v1/access-policy', patch)
    accessPolicy.value       = updated
    apRequireVerify.value    = updated.require_verify
    apGuestRoomEnabled.value = updated.guest_room != null
    apGuestRoom.value        = updated.guest_room ?? ''
    accessPolicySaveOk.value = 'Access policy updated. Changes take effect immediately.'
  } catch (e: any) {
    accessPolicySaveError.value = e?.message ?? 'failed to save access policy'
  } finally {
    accessPolicySaving.value = false
  }
}

interface RadioPresetDetail {
  name: string
  frequency_hz: number
  bandwidth_hz: number
  spreading_factor: number
  coding_rate: number
  tx_power_dbm: number
}

interface RadioConfigData {
  preset: string | null
  frequency_hz: number | null
  bandwidth_hz: number | null
  spreading_factor: number | null
  coding_rate: number | null
  tx_power_dbm: number | null
  connection_type: string | null
  serial_port: string | null
  presets: RadioPresetDetail[]
}

const radioConfig = ref<RadioConfigData | null>(null)
const radioPresets = ref<RadioPresetDetail[]>([])
const radioLoading = ref(false)
const radioError = ref<string | null>(null)
const radioSaving = ref(false)
const radioSaveOk = ref<string | null>(null)
const radioSaveError = ref<string | null>(null)

// Working copy — always show all fields; preset just fills them in
const radioPreset = ref<string>('')  // '' means no preset selected
const radioFrequencyHz = ref<string>('')
const radioBandwidthHz = ref<string>('')
const radioSpreadingFactor = ref<string>('')
const radioCodingRate = ref<string>('')
const radioTxPowerDbm = ref<string>('')

function applyPreset(name: string) {
  const p = radioPresets.value.find(p => p.name === name)
  if (!p) return
  radioFrequencyHz.value     = String(p.frequency_hz)
  radioBandwidthHz.value     = String(p.bandwidth_hz)
  radioSpreadingFactor.value = String(p.spreading_factor)
  radioCodingRate.value      = String(p.coding_rate)
  radioTxPowerDbm.value      = String(p.tx_power_dbm)
}

watch(radioPreset, (name) => {
  if (name) applyPreset(name)
})

async function loadRadioConfig() {
  radioLoading.value = true
  radioError.value = null
  try {
    const r = await api.get<RadioConfigData>('/api/v1/radio-config')
    radioConfig.value = r
    radioPresets.value = r.presets
    radioPreset.value = r.preset ?? ''
    // Prefer stored individual values; if none, fall back to preset values
    if (r.frequency_hz != null || r.bandwidth_hz != null || r.spreading_factor != null ||
        r.coding_rate != null || r.tx_power_dbm != null) {
      radioFrequencyHz.value     = r.frequency_hz     != null ? String(r.frequency_hz)     : ''
      radioBandwidthHz.value     = r.bandwidth_hz     != null ? String(r.bandwidth_hz)     : ''
      radioSpreadingFactor.value = r.spreading_factor != null ? String(r.spreading_factor) : ''
      radioCodingRate.value      = r.coding_rate      != null ? String(r.coding_rate)      : ''
      radioTxPowerDbm.value      = r.tx_power_dbm     != null ? String(r.tx_power_dbm)     : ''
    } else if (r.preset) {
      applyPreset(r.preset)
    }
  } catch (e: any) {
    radioError.value = e?.message ?? 'failed to load radio config'
  } finally {
    radioLoading.value = false
  }
}

async function saveRadioConfig() {
  radioSaving.value    = true
  radioSaveOk.value    = null
  radioSaveError.value = null
  try {
    const patch: Record<string, unknown> = {
      preset:           radioPreset.value || null,
      frequency_hz:     radioFrequencyHz.value     ? parseInt(radioFrequencyHz.value, 10)     : null,
      bandwidth_hz:     radioBandwidthHz.value     ? parseInt(radioBandwidthHz.value, 10)     : null,
      spreading_factor: radioSpreadingFactor.value ? parseInt(radioSpreadingFactor.value, 10) : null,
      coding_rate:      radioCodingRate.value      ? parseInt(radioCodingRate.value, 10)      : null,
      tx_power_dbm:     radioTxPowerDbm.value      ? parseInt(radioTxPowerDbm.value, 10)      : null,
    }
    const updated = await api.patch<RadioConfigData>('/api/v1/radio-config', patch)
    radioConfig.value = updated
    radioSaveOk.value = 'Radio config saved to config.toml.'
  } catch (e: any) {
    radioSaveError.value = e?.message ?? 'failed to save radio config'
  } finally {
    radioSaving.value = false
  }
}

const radioApplying = ref(false)
const radioApplyOk = ref<string | null>(null)
const radioApplyError = ref<string | null>(null)

async function applyRadioConfig() {
  radioApplying.value  = true
  radioApplyOk.value   = null
  radioApplyError.value = null
  try {
    await api.post('/api/v1/radio-config/apply', {
      frequency_hz:     radioFrequencyHz.value     ? parseInt(radioFrequencyHz.value, 10)     : 0,
      bandwidth_hz:     radioBandwidthHz.value     ? parseInt(radioBandwidthHz.value, 10)     : 0,
      spreading_factor: radioSpreadingFactor.value ? parseInt(radioSpreadingFactor.value, 10) : 0,
      coding_rate:      radioCodingRate.value      ? parseInt(radioCodingRate.value, 10)      : 0,
      tx_power_dbm:     radioTxPowerDbm.value      ? parseInt(radioTxPowerDbm.value, 10)      : 0,
    })
    radioApplyOk.value = 'Radio parameters applied to device.'
  } catch (e: any) {
    radioApplyError.value = e?.message ?? 'failed to apply radio config'
  } finally {
    radioApplying.value = false
  }
}

// ── Meshtastic radio ──────────────────────────────────────────────────────────

const meshtasticRadioLoading = ref(false)
const meshtasticRadioSaving = ref(false)
const meshtasticRadioError = ref<string | null>(null)
const meshtasticRadioOk = ref<string | null>(null)

const meshtasticUsePreset = ref(false)
const meshtasticModemPreset = ref(0)
const meshtasticBandwidth = ref(0)
const meshtasticSpreadFactor = ref(11)
const meshtasticCodingRate = ref(8)
const meshtasticFrequencyOffset = ref(0)
const meshtasticRegion = ref(0)
const meshtasticHopLimit = ref(3)
const meshtasticTxEnabled = ref(true)
const meshtasticTxPower = ref(17)
const meshtasticChannelNum = ref(0)
const meshtasticOverrideFrequency = ref(0)

async function loadMeshtasticRadio() {
  meshtasticRadioLoading.value = true
  meshtasticRadioError.value = null
  meshtasticRadioOk.value = null
  try {
    const r = await api.get<any>('/api/v1/meshtastic-radio-config')
    meshtasticUsePreset.value         = r.use_preset ?? false
    meshtasticModemPreset.value       = r.modem_preset ?? 0
    meshtasticBandwidth.value         = r.bandwidth ?? 0
    meshtasticSpreadFactor.value      = r.spread_factor ?? 11
    meshtasticCodingRate.value        = r.coding_rate ?? 8
    meshtasticFrequencyOffset.value   = r.frequency_offset ?? 0
    meshtasticRegion.value            = r.region ?? 0
    meshtasticHopLimit.value          = r.hop_limit ?? 3
    meshtasticTxEnabled.value         = r.tx_enabled ?? true
    meshtasticTxPower.value           = r.tx_power ?? 17
    meshtasticChannelNum.value        = r.channel_num ?? 0
    meshtasticOverrideFrequency.value = r.override_frequency ?? 0
    meshtasticRadioOk.value = 'Loaded from device.'
  } catch (e: any) {
    meshtasticRadioError.value = e?.message ?? 'failed to load meshtastic radio config'
  } finally {
    meshtasticRadioLoading.value = false
  }
}

async function saveMeshtasticRadio() {
  meshtasticRadioSaving.value = true
  meshtasticRadioError.value = null
  meshtasticRadioOk.value = null
  try {
    await api.patch('/api/v1/meshtastic-radio-config', {
      use_preset:         meshtasticUsePreset.value,
      modem_preset:       meshtasticModemPreset.value,
      bandwidth:          meshtasticBandwidth.value,
      spread_factor:      meshtasticSpreadFactor.value,
      coding_rate:        meshtasticCodingRate.value,
      frequency_offset:   meshtasticFrequencyOffset.value,
      region:             meshtasticRegion.value,
      hop_limit:          meshtasticHopLimit.value,
      tx_enabled:         meshtasticTxEnabled.value,
      tx_power:           meshtasticTxPower.value,
      channel_num:        meshtasticChannelNum.value,
      override_frequency: meshtasticOverrideFrequency.value,
    })
    meshtasticRadioOk.value = 'Radio config saved to device.'
  } catch (e: any) {
    meshtasticRadioError.value = e?.message ?? 'failed to save meshtastic radio config'
  } finally {
    meshtasticRadioSaving.value = false
  }
}

// ── Node identity ─────────────────────────────────────────────────────────────

interface NodeIdentityData {
  pubkey: string | null
}

const nodeIdentity = ref<NodeIdentityData | null>(null)
const nodeIdentityLoading = ref(false)
const nodeIdentityError = ref<string | null>(null)

// Export state
const exportedKey = ref<string | null>(null)
const exportKeyLoading = ref(false)
const exportKeyError = ref<string | null>(null)
const exportKeyVisible = ref(false)

// Inline node-key edit state (replaces the old separate import form)
const editingNodeKey = ref(false)
const nodeKeyInput = ref('')
const nodeKeyLoading = ref(false)
const nodeKeyOk = ref<string | null>(null)
const nodeKeyError = ref<string | null>(null)

function startEditNodeKey() {
  nodeKeyInput.value = ''
  nodeKeyOk.value = null
  nodeKeyError.value = null
  editingNodeKey.value = true
}

function cancelEditNodeKey() {
  editingNodeKey.value = false
  nodeKeyInput.value = ''
  nodeKeyOk.value = null
  nodeKeyError.value = null
}

function validateHex64(value: string): string | null {
  const h = value.trim()
  if (h.length !== 64) return `Must be exactly 64 hex characters (${h.length} given).`
  if (!/^[0-9a-fA-F]+$/.test(h)) return 'Contains invalid characters — only 0-9 and a-f are allowed.'
  return null
}

const nodeKeyInputError = computed(() => {
  if (!nodeKeyInput.value.trim()) return null
  return validateHex64(nodeKeyInput.value)
})

async function loadNodeIdentity() {
  nodeIdentityLoading.value = true
  nodeIdentityError.value = null
  try {
    const r = await api.get<NodeIdentityData>('/api/v1/node-identity')
    nodeIdentity.value = r
  } catch (e: any) {
    nodeIdentityError.value = e?.message ?? 'failed to load node identity'
  } finally {
    nodeIdentityLoading.value = false
  }
}

async function exportNodeKey() {
  exportKeyLoading.value = true
  exportKeyError.value = null
  exportedKey.value = null
  try {
    const r = await api.post<{ key: string }>('/api/v1/node-identity/export-key', {})
    exportedKey.value = r.key
    exportKeyVisible.value = false
  } catch (e: any) {
    exportKeyError.value = e?.message ?? 'failed to export key'
  } finally {
    exportKeyLoading.value = false
  }
}

async function saveNodeKey() {
  nodeKeyOk.value = null
  nodeKeyError.value = null
  const err = validateHex64(nodeKeyInput.value)
  if (err) { nodeKeyError.value = err; return }
  const hex = nodeKeyInput.value.trim()
  nodeKeyLoading.value = true
  try {
    await api.post('/api/v1/node-identity/import-key', { key: hex })
    nodeKeyOk.value = 'Node key saved. The public key will update on the next mesh connection.'
    nodeKeyInput.value = ''
    editingNodeKey.value = false
    await loadNodeIdentity()
  } catch (e: any) {
    nodeKeyError.value = e?.message ?? 'failed to save node key'
  } finally {
    nodeKeyLoading.value = false
  }
}

function copyToClipboard(text: string) {
  navigator.clipboard.writeText(text).catch(() => {})
}

onMounted(() => {
  load()
  loadRooms()
  loadAccessPolicy()
  loadRadioConfig()
  loadNodeIdentity()
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

      <!-- Access policy -->
      <section class="card">
        <h2>Access policy</h2>
        <p class="hint">
          Controls how new registrations are handled. Changes take effect
          immediately — no restart required. See also the in-BBS
          <code>OPENACCESS</code> / <code>CLOSEACCESS</code> / <code>GUESTROOM</code> commands.
        </p>

        <div v-if="accessPolicyError" class="notice error-notice">{{ accessPolicyError }}</div>
        <div v-if="accessPolicySaveOk" class="notice ok-notice">{{ accessPolicySaveOk }}</div>
        <div v-if="accessPolicySaveError" class="notice error-notice">{{ accessPolicySaveError }}</div>

        <div class="field checkbox-field">
          <label>
            <input type="checkbox" v-model="apRequireVerify" :disabled="accessPolicyLoading" />
            Require sysop verification before users can access rooms
          </label>
          <p class="hint">
            Uncheck for SHTF mode — new users get full access immediately on
            registration. When unchecked, the guest room (below) still exists but
            has no access restriction.
          </p>
        </div>

        <div class="field checkbox-field">
          <label>
            <input type="checkbox" v-model="apGuestRoomEnabled" :disabled="accessPolicyLoading" />
            Allow unverified users to access a guest room
          </label>
          <p class="hint">
            Unverified users can only read and post in the named room.
            The room is created automatically on startup if it does not exist.
          </p>
        </div>

        <div v-if="apGuestRoomEnabled" class="field">
          <label>Guest room name</label>
          <input
            v-model="apGuestRoom"
            type="text"
            placeholder="e.g. Guests"
            :disabled="accessPolicyLoading"
            style="max-width: 260px"
          />
          <p v-if="accessPolicy?.guest_room_id != null" class="hint">
            Room ID: {{ accessPolicy.guest_room_id }}
          </p>
        </div>

        <div class="actions">
          <button
            type="button"
            :disabled="accessPolicySaving || accessPolicyLoading"
            @click="saveAccessPolicy"
          >
            {{ accessPolicySaving ? 'saving…' : 'save access policy' }}
          </button>
        </div>
      </section>

      <!-- MeshCore Radio — shown for all MeshCore connection types -->
      <section v-if="radioConfig" class="card">
        <h2>MeshCore radio</h2>
        <p class="hint">
          LoRa parameters for the MeshCore companion device.
          Save here to record them in config.toml. Use <strong>Apply to device</strong>
          to push the current values directly to the live companion device over the
          existing connection.
        </p>

        <div v-if="radioError" class="notice error-notice">{{ radioError }}</div>
        <div v-if="radioSaveOk" class="notice ok-notice">{{ radioSaveOk }}</div>
        <div v-if="radioSaveError" class="notice error-notice">{{ radioSaveError }}</div>
        <div v-if="radioApplyOk" class="notice ok-notice">{{ radioApplyOk }}</div>
        <div v-if="radioApplyError" class="notice error-notice">{{ radioApplyError }}</div>

        <div class="field">
          <label>Region preset</label>
          <select v-model="radioPreset" :disabled="radioLoading" style="max-width: 320px">
            <option value="">(select a preset to fill values below)</option>
            <option v-for="p in radioPresets" :key="p.name" :value="p.name">{{ p.name }}</option>
          </select>
          <p class="hint">Selecting a preset fills in the parameters below. You can then adjust individual values.</p>
        </div>

        <div class="field-row">
          <div class="field">
            <label>Frequency (Hz)</label>
            <input v-model="radioFrequencyHz" type="number" min="1" placeholder="e.g. 910525000" :disabled="radioLoading" />
            <p class="hint">e.g. 910525000 for 910.525 MHz</p>
          </div>
          <div class="field">
            <label>Bandwidth (Hz)</label>
            <input v-model="radioBandwidthHz" type="number" min="1" placeholder="e.g. 62500" :disabled="radioLoading" />
            <p class="hint">e.g. 62500 for 62.5 kHz</p>
          </div>
        </div>
        <div class="field-row">
          <div class="field">
            <label>Spreading factor (7–12)</label>
            <input v-model="radioSpreadingFactor" type="number" min="7" max="12" :disabled="radioLoading" />
          </div>
          <div class="field">
            <label>Coding rate (5–8)</label>
            <input v-model="radioCodingRate" type="number" min="5" max="8" :disabled="radioLoading" />
            <p class="hint">Denominator: 5 = 4/5, 8 = 4/8</p>
          </div>
          <div class="field">
            <label>TX power (dBm)</label>
            <input v-model="radioTxPowerDbm" type="number" min="-10" max="30" :disabled="radioLoading" />
          </div>
        </div>

        <div class="actions">
          <button type="button" :disabled="radioSaving || radioLoading || !writable" @click="saveRadioConfig">
            {{ radioSaving ? 'saving…' : 'save radio config' }}
          </button>
          <button type="button" :disabled="radioApplying || radioLoading || !radioConfig" @click="applyRadioConfig">
            {{ radioApplying ? 'applying…' : 'apply to device' }}
          </button>
          <span v-if="!writable" class="hint">config file is not writable</span>
        </div>
      </section>

      <!-- Meshtastic radio -->
      <section class="card">
        <h2>Meshtastic radio</h2>
        <p class="hint">
          LoRa radio configuration for the connected Meshtastic device.
          Use <strong>Load from device</strong> to read the current settings,
          edit them, then <strong>Save to device</strong> to push them back.
        </p>

        <div v-if="meshtasticRadioError" class="notice error-notice">{{ meshtasticRadioError }}</div>
        <div v-if="meshtasticRadioOk" class="notice ok-notice">{{ meshtasticRadioOk }}</div>

        <div class="field-row">
          <div class="field">
            <label>Spread factor</label>
            <input v-model.number="meshtasticSpreadFactor" type="number" min="7" max="12" :disabled="meshtasticRadioLoading" />
          </div>
          <div class="field">
            <label>Bandwidth</label>
            <input v-model.number="meshtasticBandwidth" type="number" min="0" :disabled="meshtasticRadioLoading" />
          </div>
          <div class="field">
            <label>TX power (dBm)</label>
            <input v-model.number="meshtasticTxPower" type="number" :disabled="meshtasticRadioLoading" />
          </div>
        </div>
        <div class="field-row">
          <div class="field">
            <label>Region</label>
            <input v-model.number="meshtasticRegion" type="number" min="0" :disabled="meshtasticRadioLoading" />
            <p class="hint">Meshtastic region enum value</p>
          </div>
          <div class="field">
            <label>Hop limit</label>
            <input v-model.number="meshtasticHopLimit" type="number" min="0" max="7" :disabled="meshtasticRadioLoading" />
          </div>
          <div class="field">
            <label>Modem preset</label>
            <input v-model.number="meshtasticModemPreset" type="number" min="0" :disabled="meshtasticRadioLoading || meshtasticUsePreset" />
          </div>
        </div>
        <div class="field-row">
          <div class="field">
            <label>Override frequency (MHz)</label>
            <input v-model.number="meshtasticOverrideFrequency" type="number" step="0.001" :disabled="meshtasticRadioLoading" />
          </div>
          <div class="field">
            <label style="display:flex;align-items:center;gap:0.5rem;">
              <input type="checkbox" v-model="meshtasticUsePreset" :disabled="meshtasticRadioLoading" />
              Use preset
            </label>
          </div>
          <div class="field">
            <label style="display:flex;align-items:center;gap:0.5rem;">
              <input type="checkbox" v-model="meshtasticTxEnabled" :disabled="meshtasticRadioLoading" />
              TX enabled
            </label>
          </div>
        </div>

        <div class="actions">
          <button type="button" :disabled="meshtasticRadioLoading" @click="loadMeshtasticRadio">
            {{ meshtasticRadioLoading ? 'loading…' : 'load from device' }}
          </button>
          <button type="button" :disabled="meshtasticRadioLoading || meshtasticRadioSaving" @click="saveMeshtasticRadio">
            {{ meshtasticRadioSaving ? 'saving…' : 'save to device' }}
          </button>
        </div>
      </section>

      <!-- Node identity -->
      <section class="card">
        <h2>Node identity</h2>
        <p class="hint">
          The MeshCore companion device's identity keypair. The public key identifies
          your node on the mesh network and is shared with other stations to contact you.
          Use <strong>Set node key</strong> to paste a known 64-character hex key (e.g. when
          migrating to new hardware). Export the private key for backup before a firmware
          flash. <strong>Keep the private key secret.</strong>
        </p>

        <div v-if="nodeIdentityError" class="notice error-notice">{{ nodeIdentityError }}</div>

        <!-- Public key display + inline edit -->
        <div class="field">
          <label>Public key</label>
          <div v-if="!editingNodeKey" class="key-display">
            <code v-if="nodeIdentity?.pubkey" class="key-hex">{{ nodeIdentity.pubkey }}</code>
            <span v-else-if="nodeIdentityLoading" class="muted">loading…</span>
            <span v-else class="muted">not connected — start the mesh transport to read the device key</span>
            <button
              v-if="nodeIdentity?.pubkey"
              type="button"
              class="icon-btn"
              title="Copy public key"
              @click="copyToClipboard(nodeIdentity!.pubkey!)"
            >⎘</button>
            <button
              type="button"
              class="icon-btn"
              title="Set node key"
              style="margin-left: 0.25rem"
              @click="startEditNodeKey"
            >✏️</button>
          </div>

          <!-- Inline edit form -->
          <div v-if="editingNodeKey" style="display: flex; flex-direction: column; gap: 0.5rem; margin-top: 0.25rem">
            <div class="notice warn-notice" style="font-size: 0.85em">
              ⚠ Setting a new node key replaces the device's current identity on the mesh.
              Back up the current private key first if you may need to restore it.
            </div>
            <input
              v-model="nodeKeyInput"
              type="text"
              placeholder="paste 64-character hex node key"
              style="max-width: 480px; font-family: monospace; font-size: 0.85em"
              autocomplete="off"
              spellcheck="false"
              autofocus
            />
            <div v-if="nodeKeyInputError" class="notice error-notice" style="font-size: 0.85em">{{ nodeKeyInputError }}</div>
            <div v-if="nodeKeyError" class="notice error-notice">{{ nodeKeyError }}</div>
            <div class="actions" style="padding-top: 0">
              <button
                type="button"
                :disabled="nodeKeyLoading || !nodeKeyInput.trim() || !!nodeKeyInputError"
                @click="saveNodeKey"
              >{{ nodeKeyLoading ? 'saving…' : 'set node key' }}</button>
              <button type="button" class="secondary" :disabled="nodeKeyLoading" @click="cancelEditNodeKey">cancel</button>
            </div>
          </div>

          <div v-if="nodeKeyOk" class="notice ok-notice" style="margin-top: 0.5rem">{{ nodeKeyOk }}</div>
        </div>

        <!-- Export private key -->
        <div class="field">
          <label>Private key backup</label>
          <div v-if="exportedKey" class="key-display">
            <code v-if="exportKeyVisible" class="key-hex">{{ exportedKey }}</code>
            <span v-else class="muted key-hex">••••••••••••••••••••••••••••••••••••••••••••••••••••••••••••••••</span>
            <button type="button" class="icon-btn" :title="exportKeyVisible ? 'Hide' : 'Reveal'" @click="exportKeyVisible = !exportKeyVisible">
              {{ exportKeyVisible ? '🙈' : '👁' }}
            </button>
            <button v-if="exportKeyVisible" type="button" class="icon-btn" title="Copy private key" @click="copyToClipboard(exportedKey!)">⎘</button>
          </div>
          <div v-if="exportKeyError" class="notice error-notice" style="margin-top: 0.4rem">{{ exportKeyError }}</div>
          <div class="actions" style="padding-top: 0.4rem">
            <button type="button" class="secondary" :disabled="exportKeyLoading" @click="exportNodeKey">
              {{ exportKeyLoading ? 'exporting…' : 'export private key' }}
            </button>
          </div>
        </div>
      </section>

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

.key-display {
  display: flex;
  align-items: center;
  gap: 0.4rem;
  flex-wrap: wrap;
}
.key-hex {
  font-family: monospace;
  font-size: 0.82em;
  word-break: break-all;
  color: var(--fg);
}
.icon-btn {
  background: none;
  border: none;
  cursor: pointer;
  font-size: 1em;
  padding: 0.1rem 0.3rem;
  color: var(--muted);
}
.icon-btn:hover { color: var(--fg); }
</style>
