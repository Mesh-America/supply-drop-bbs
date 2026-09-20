<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { api, request, ApiError } from '../api/client'

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

// What to tell the sysop after a confirmed restore: either the service is
// restarting itself (systemd), or they have to restart it.
function restartNote(res: { message: string; restart_required?: boolean }): string {
  return res.restart_required
    ? `${res.message}. Reload this page after you restart it.`
    : `${res.message}. This page may stop responding for a few seconds.`
}

// Restores a backup that is already on the server. One request stages it
// (the server validates it first) and confirms it under a single lock, so a
// concurrent upload can't be applied in its place; after one confirmation here
// the service restarts on the restored database. Only the newest pre-restore
// safety snapshot is kept, so a second restore replaces the first one's.
async function restoreBackup(filename: string) {
  if (busy.value) return
  if (!confirm(
    `Restore ${filename}?\n\n` +
    'This REPLACES the current database with this backup and restarts the ' +
    'service (without systemd you restart it yourself). Anything written ' +
    'since the backup was made is lost. A safety snapshot of the current ' +
    'database is saved in the data directory first, but only the most recent ' +
    'snapshot is kept, so a second restore replaces it.'
  )) return
  restoring.value = filename
  error.value = null
  actionOk.value = null
  try {
    const res = await api.post<{ message: string; restart_required?: boolean }>(
      `/api/v1/backups/${encodeURIComponent(filename)}/restore?apply=true`
    )
    actionOk.value = restartNote(res)
    restoreDone.value = true
    restoreStaged.value = false
  } catch (e: any) {
    if (e instanceof ApiError) {
      error.value = e.message
    } else {
      connectionDropped()
    }
  } finally {
    restoring.value = null
  }
}

// The request never got an answer (the connection dropped or timed out). A
// confirmed restore restarts the service, which drops connections, so the
// restore may well have been applied. Don't offer a retry: a second restore
// would replace the safety snapshot of the original data.
function connectionDropped() {
  actionOk.value =
    'The connection dropped before the server answered. If the restore was ' +
    'confirmed the service is restarting: reload this page in a few seconds ' +
    'and check the data before restoring again.'
  restoreDone.value = true
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
    'This will REPLACE the current database with the staged backup and ' +
    'restart the service (without systemd you restart it yourself). A safety ' +
    'snapshot of the current database is saved in the data directory first, ' +
    'but only the most recent snapshot is kept. Continue?'
  )) return
  applying.value = true
  error.value = null
  actionOk.value = null
  try {
    const res = await api.post<{ message: string; restart_required?: boolean }>(
      '/api/v1/backups/restore/apply'
    )
    actionOk.value = restartNote(res)
    restoreDone.value = true
    restoreStaged.value = false
  } catch (e: any) {
    if (e instanceof ApiError) {
      error.value = e.message
    } else {
      connectionDropped()
    }
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
      No backups found. Automatic backups (`.db` files) are created on the configured interval
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
              <a v-if="b.config_filename" :href="downloadUrl(b.config_filename)"
                class="dl-link config-link" :download="b.config_filename">
                config
              </a>
            </div>
          </td>
          <td class="size-col">
            {{ fmtSize(b.size_bytes) }}
            <span v-if="b.config_size_bytes" class="muted small">
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
