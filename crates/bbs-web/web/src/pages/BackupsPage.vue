<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { api, request, ApiError } from '../api/client'
import { fetchBootId, showRestoreWait } from '../api/restoreWaitOverlay'

interface BackupRecord {
  filename: string
  size_bytes: number
  created_at: string
  config_filename?: string
  config_size_bytes?: number
}

interface Settings {
  backup_dir: string | null
}

const backups = ref<BackupRecord[]>([])
const settings = ref<Settings | null>(null)
const loading = ref(false)
const triggering = ref(false)
const deleting = ref<string | null>(null)
const restoring = ref<string | null>(null)
const error = ref<string | null>(null)
const actionOk = ref<string | null>(null)

// Restore the backup's settings (config.toml) along with the database. On by
// default: a restore that leaves the settings behind looks half done.
const restoreSettings = ref(true)
const restoreFile = ref<File | null>(null)
const uploading = ref(false)
const applying = ref(false)
const restoreStaged = ref(false)
// Set once a restore has been confirmed: the list is stale and the service is
// about to restart (or waiting for the operator to restart it).
const restoreDone = ref(false)

const fileInput = ref<HTMLInputElement | null>(null)

const backupDirConfigured = computed(() => settings.value?.backup_dir != null)
// Any action in flight, or a confirmed restore waiting on the restart. Every
// button that changes backups or the database is disabled while this is true.
const busy = computed(
  () =>
    triggering.value ||
    deleting.value !== null ||
    restoring.value !== null ||
    uploading.value ||
    applying.value ||
    restoreDone.value
)

function fmtSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`
}

function fmtDate(iso: string): string {
  return iso.slice(0, 19).replace('T', ' ') + ' UTC'
}

function downloadUrl(filename: string): string {
  return '/api/v1/backups/' + encodeURIComponent(filename)
}

async function load() {
  loading.value = true
  error.value = null
  try {
    settings.value = await api.get<Settings>('/api/v1/settings')
    if (backupDirConfigured.value) {
      backups.value = await api.get<BackupRecord[]>('/api/v1/backups')
    } else {
      backups.value = []
    }
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load'
  } finally {
    loading.value = false
  }
}

async function triggerBackup() {
  triggering.value = true
  error.value = null
  actionOk.value = null
  try {
    const record = await api.post<BackupRecord>('/api/v1/backups')
    actionOk.value = `Backup created: ${record.filename} (${fmtSize(record.size_bytes)})`
    await load()
  } catch (e: any) {
    error.value = e?.message ?? 'backup failed'
  } finally {
    triggering.value = false
  }
}

async function deleteBackup(filename: string) {
  if (!confirm(`Delete ${filename}?`)) return
  deleting.value = filename
  error.value = null
  try {
    await api.del(`/api/v1/backups/${encodeURIComponent(filename)}`)
    await load()
  } catch (e: any) {
    error.value = e?.message ?? 'delete failed'
  } finally {
    deleting.value = null
  }
}

// After a confirmed restore the whole UI is blocked behind a full-screen
// notice until the restarted service answers, then the page reloads (to the
// login screen: sessions don't survive a restart). The notice lives outside
// this component so navigating away can't drop it while the restart is still
// pending. Shared by every path that can confirm a restore.
function startRestoreWait(
  message: string,
  restartByHand: boolean,
  baselineBootId: string | null,
  unknownOutcome = false,
  // Whether the request acted on the upload staged on the server (Apply), so
  // that upload is still there to apply if the notice gives up.
  actsOnStagedUpload = false
) {
  restoreDone.value = true
  // What was staged before, to give back if the notice gives up: the upload is
  // still on the server and the Apply button must not vanish with the notice.
  const wasStaged = restoreStaged.value
  restoreStaged.value = false
  showRestoreWait({
    message,
    restartByHand,
    baselineBootId,
    // A confirmed restore makes the service exit within a moment. If the same
    // process is still answering well after that, the request never got
    // through: say so and give the buttons back.
    giveUpAfterSeconds: unknownOutcome ? GIVE_UP_AFTER_SECONDS : undefined,
    onGiveUp: () => {
      restoreDone.value = false
      restoreStaged.value = actsOnStagedUpload && wasStaged
      // The request may have got through and confirmed the restore for the next
      // start even though nothing restarted (no systemd, or the exit failed).
      error.value =
        'The connection failed and the service did not restart, so the restore has ' +
        'not been applied. It may still have been confirmed: if a file named ' +
        'pending_restore.db is in the data directory, it is applied the next time ' +
        'the BBS starts. Check the service, then try again.'
    },
  })
}

const GIVE_UP_AFTER_SECONDS = 20

// Answers that leave it unclear whether the restore was triggered: a proxy
// error (502/503/504) in front of the service. A 409 means the server itself
// says a restore is already confirmed and restarting. Everything else is a
// plain error.
const UNCLEAR_STATUS = [502, 503, 504]

// What the confirmation dialogs say. `hasSettings` is whether the backup being
// restored carries a config.toml (undefined when that isn't known, as for an
// upload that is already staged).
function restoreLimits(hasSettings?: boolean): string {
  const snapshot =
    'A safety snapshot of the current database is saved in the data directory ' +
    'first; the last three snapshots are kept.'
  if (!restoreSettings.value) {
    return (
      'This REPLACES the current database only. The current settings ' +
      '(config.toml) are left as they are. Anything written since the backup ' +
      'was made is lost. ' + snapshot
    )
  }
  const settings =
    hasSettings === false
      ? 'This backup has no settings (config.toml), so only the database is restored. '
      : 'Its settings (config.toml, such as the BBS name) are restored too; paths, ' +
        "the web and CLI plugins, the database, backup and security sections and the radios (connection, settings, on or off, node names) keep this machine's values. "
  return (
    'This REPLACES the current database. ' + settings +
    'Anything written since the backup was made is lost. ' + snapshot
  )
}

// Restores a backup that is already on the server. One request stages it
// (the server validates it first) and confirms it under a single lock, so a
// concurrent upload can't be applied in its place; after one confirmation here
// the service restarts on the restored database.
async function restoreBackup(filename: string) {
  if (busy.value) return
  const record = backups.value.find((b) => b.filename === filename)
  const hasSettings = filename.endsWith('.zip') && !!record?.config_filename
  if (!confirm(
    `Restore ${filename}?\n\n${restoreLimits(hasSettings)}\n\n` +
    'The service then restarts (without systemd you restart it yourself).'
  )) return
  restoring.value = filename
  error.value = null
  actionOk.value = null
  // Read before triggering the restore, so a restart quick enough to be over
  // before the first poll is still recognised.
  const bootId = await fetchBootId()
  try {
    const res = await api.post<{ message: string; restart_required?: boolean }>(
      `/api/v1/backups/${encodeURIComponent(filename)}/restore?apply=true&config=${restoreSettings.value}`
    )
    startRestoreWait(res.message, res.restart_required === true, bootId)
  } catch (e: any) {
    handleRestoreError(e, bootId)
  } finally {
    restoring.value = null
  }
}

// A failed restore request. The server answers a confirmed restore before it
// exits, so most failures mean nothing was triggered; but a connection that
// dropped without an answer, or a proxy error, can hide a restore that was
// confirmed, and a retry would then replace the safety snapshot of the original
// data. For those, wait behind the notice and see whether the service restarts
// (reload) or not (the notice gives up and says the restore was not applied).
function handleRestoreError(e: unknown, bootId: string | null, actsOnStagedUpload = false) {
  if (e instanceof ApiError && e.status === 409) {
    // The server says a restore is already confirmed and restarting.
    startRestoreWait(e.message, false, bootId, false, actsOnStagedUpload)
    return
  }
  if (e instanceof ApiError && !UNCLEAR_STATUS.includes(e.status)) {
    error.value = e.message
    return
  }
  startRestoreWait(
    'Connection lost: checking whether the service is restarting',
    false,
    bootId,
    true,
    actsOnStagedUpload
  )
}

function pickRestoreFile(e: Event) {
  const input = e.target as HTMLInputElement
  restoreFile.value = input.files?.[0] ?? null
  error.value = null
  actionOk.value = null
}

// Uploads and validates the file WITHOUT restarting anything — the server
// checks it's a real, migration-compatible database before staging it, and
// nothing about the live system changes until applyRestore is confirmed
// separately.
async function uploadRestoreFile() {
  if (!restoreFile.value) return
  uploading.value = true
  error.value = null
  actionOk.value = null
  try {
    const form = new FormData()
    form.append('file', restoreFile.value)
    await request('/api/v1/backups/restore', { method: 'POST', body: form })
    restoreStaged.value = true
    actionOk.value = `${restoreFile.value.name} validated and staged. Review, then apply below to restore it.`
    restoreFile.value = null
    if (fileInput.value) fileInput.value.value = ''
  } catch (e: any) {
    error.value = e?.message ?? 'upload failed: the file was not staged'
  } finally {
    uploading.value = false
  }
}

// The destructive step: exits the process so it restarts with the staged
// file swapped in. A pre-restore safety snapshot of the CURRENT database is
// taken automatically before anything is overwritten.
async function applyRestore() {
  if (busy.value) return
  if (!confirm(
    `Apply the staged backup?\n\n${restoreLimits()}\n\n` +
    'The service then restarts (without systemd you restart it yourself).'
  )) return
  applying.value = true
  error.value = null
  actionOk.value = null
  const bootId = await fetchBootId()
  try {
    const res = await api.post<{ message: string; restart_required?: boolean }>(
      `/api/v1/backups/restore/apply?config=${restoreSettings.value}`
    )
    startRestoreWait(res.message, res.restart_required === true, bootId, false, true)
  } catch (e: any) {
    handleRestoreError(e, bootId, true)
  } finally {
    applying.value = false
  }
}

onMounted(load)
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div class="title-block">
        <h1>backups</h1>
        <p class="muted">SQLite database + config snapshots</p>
      </div>
      <div class="controls">
        <button @click="triggerBackup" :disabled="busy || !backupDirConfigured"
          :title="!backupDirConfigured ? 'backup_dir not configured' : ''">
          {{ triggering ? 'backing up…' : 'create backup' }}
        </button>
      </div>
    </header>

    <div v-if="settings && !backupDirConfigured" class="config-notice">
      <strong>Backup directory not configured.</strong>
      The server resolves the backup directory from the <code>[backup]</code> section of your
      config file. Ensure <code>backup.enabled = true</code> and optionally set
      <code>backup.directory</code>; then restart the server.
    </div>

    <div v-if="backupDirConfigured" class="dir-info muted small">
      directory: <code>{{ settings!.backup_dir }}</code>
    </div>

    <section class="restore-panel">
      <h2>restore from backup</h2>
      <p class="muted small">
        To restore a backup listed below, use its <strong>restore</strong> button. To restore
        one from another system, upload its <code>.db</code> or <code>.zip</code> here. The file
        is validated before anything changes; nothing is applied until you confirm below.
      </p>
      <label class="settings-toggle">
        <input type="checkbox" v-model="restoreSettings" :disabled="busy" />
        Also restore the backup's settings (<code>config.toml</code>: BBS name, welcome
        message, and so on). Settings that belong to this machine (paths, the web and CLI
        plugins, the database, backup and security sections, the radios' connection, settings, on or off switch and node names) are kept.
      </label>
      <div class="restore-controls">
        <input
          ref="fileInput"
          type="file"
          accept=".db,.zip"
          :disabled="busy"
          @change="pickRestoreFile"
        />
        <button @click="uploadRestoreFile" :disabled="!restoreFile || busy">
          {{ uploading ? 'validating…' : 'upload & validate' }}
        </button>
      </div>
      <div v-if="restoreStaged" class="restore-staged">
        <p>
          A validated backup is staged and ready. Applying it <strong>replaces the current
          database</strong> and restarts the service. A safety snapshot of the current
          database is taken automatically first.
        </p>
        <button class="danger" @click="applyRestore" :disabled="busy">
          {{ applying ? 'applying…' : 'apply restore (restarts service)' }}
        </button>
      </div>
    </section>

    <p v-if="error" class="error">{{ error }}</p>
    <p v-if="actionOk" class="ok">{{ actionOk }}</p>

    <p v-if="backupDirConfigured && !loading && backups.length === 0 && !error" class="muted">
      No backups found. Automatic backups (`.zip` bundles of the database and settings) are created on the configured interval
      and will appear here. You can also create one manually above.
    </p>

    <table v-if="backups.length > 0">
      <thead>
        <tr>
          <th>files</th>
          <th>size</th>
          <th>created</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="b in backups" :key="b.filename">
          <td>
            <div class="file-cell">
              <a :href="downloadUrl(b.filename)" class="dl-link" :download="b.filename">
                {{ b.filename }}
              </a>
              <a v-if="b.config_filename && b.filename.endsWith('.db')"
                :href="downloadUrl(b.config_filename)"
                class="dl-link config-link" :download="b.config_filename">
                config
              </a>
              <span v-else-if="b.config_filename" class="muted small"
                title="This backup includes config.toml, so a restore can bring the settings back">
                + settings
              </span>
            </div>
          </td>
          <td class="size-col">
            {{ fmtSize(b.size_bytes) }}
            <span v-if="b.config_size_bytes && b.filename.endsWith('.db')" class="muted small">
              + {{ fmtSize(b.config_size_bytes) }}
            </span>
          </td>
          <td class="muted small">{{ fmtDate(b.created_at) }}</td>
          <td class="action-col">
            <button class="small-btn" @click="restoreBackup(b.filename)"
              :disabled="busy"
              title="Replace the current database with this backup and restart the service">
              {{ restoring === b.filename ? '…' : 'restore' }}
            </button>
            <button class="danger small-btn" @click="deleteBackup(b.filename)"
              :disabled="busy">
              {{ deleting === b.filename ? '…' : 'delete' }}
            </button>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 1rem; }
.page-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; flex-wrap: wrap; }
.page-header .title-block { display: flex; flex-direction: column; gap: 0.2rem; }
h1 { margin: 0; }
p { margin: 0; }
.controls { display: flex; flex-direction: row; align-items: center; gap: 0.5rem; }
.small { font-size: 0.85em; }
.ok { color: #2a8a2a; }

.dir-info { margin-top: -0.25rem; }

.restore-panel {
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
  padding: 0.9rem 1.1rem;
  border: 1px solid var(--border);
  border-radius: 4px;
}
.restore-panel h2 { margin: 0; font-size: 1em; }
.settings-toggle { display: flex; align-items: flex-start; gap: 0.5rem; font-size: 0.9em; line-height: 1.4; }
.settings-toggle input { margin-top: 0.2rem; }
.restore-controls { display: flex; align-items: center; gap: 0.6rem; flex-wrap: wrap; }
.restore-staged {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  padding-top: 0.5rem;
  border-top: 1px solid var(--border);
}
.restore-staged p { line-height: 1.5; }
.config-notice {
  padding: 0.9rem 1.1rem;
  border: 1px solid var(--warning);
  border-radius: 4px;
  background: color-mix(in srgb, var(--warning) 8%, transparent);
  font-size: 0.9em;
  line-height: 1.6;
}

.file-cell { display: flex; flex-direction: column; gap: 0.2rem; }
.dl-link { color: var(--accent); text-decoration: none; font-family: monospace; font-size: 0.85em; }
.dl-link:hover { text-decoration: underline; }
.config-link { font-size: 0.78em; color: var(--muted); }
.config-link:hover { color: var(--accent); }

.size-col { white-space: nowrap; }
.action-col { text-align: right; white-space: nowrap; }
.action-col .small-btn + .small-btn { margin-left: 0.4rem; }
.small-btn { padding: 0.2rem 0.55rem; font-size: 0.8em; }
.danger { border-color: var(--error, #c0392b); color: var(--error, #c0392b); background: transparent; }
.danger:hover:not(:disabled) { background: color-mix(in srgb, var(--error, #c0392b) 10%, transparent); }
</style>
