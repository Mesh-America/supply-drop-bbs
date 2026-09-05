<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { api, request } from '../api/client'

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
const error = ref<string | null>(null)
const actionOk = ref<string | null>(null)

const restoreFile = ref<File | null>(null)
const uploading = ref(false)
const applying = ref(false)
const restoreStaged = ref(false)
const fileInput = ref<HTMLInputElement | null>(null)

const backupDirConfigured = computed(() => settings.value?.backup_dir != null)

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
  if (!confirm(
    'This will REPLACE the current database with the staged backup and ' +
    'restart the service now. A safety snapshot of the current database ' +
    'is taken first. Continue?'
  )) return
  applying.value = true
  error.value = null
  actionOk.value = null
  try {
    await api.post('/api/v1/backups/restore/apply')
    actionOk.value = 'Restore applying. The service is restarting, and this page will stop responding for a few seconds.'
    restoreStaged.value = false
  } catch (e: any) {
    error.value = e?.message ?? 'restore failed to apply'
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
        <button @click="triggerBackup" :disabled="triggering || !backupDirConfigured"
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
        Upload a <code>.db</code> or <code>.zip</code> backup, from this system or another, to
        replace the current database. The file is validated before anything changes; nothing
        is applied until you confirm below.
      </p>
      <div class="restore-controls">
        <input
          ref="fileInput"
          type="file"
          accept=".db,.zip"
          :disabled="uploading"
          @change="pickRestoreFile"
        />
        <button @click="uploadRestoreFile" :disabled="!restoreFile || uploading">
          {{ uploading ? 'validating…' : 'upload & validate' }}
        </button>
      </div>
      <div v-if="restoreStaged" class="restore-staged">
        <p>
          A validated backup is staged and ready. Applying it <strong>replaces the current
          database</strong> and restarts the service. A safety snapshot of the current
          database is taken automatically first.
        </p>
        <button class="danger" @click="applyRestore" :disabled="applying">
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
            <button class="danger small-btn" @click="deleteBackup(b.filename)"
              :disabled="deleting === b.filename">
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
.action-col { text-align: right; }
.small-btn { padding: 0.2rem 0.55rem; font-size: 0.8em; }
.danger { border-color: var(--error, #c0392b); color: var(--error, #c0392b); background: transparent; }
.danger:hover:not(:disabled) { background: color-mix(in srgb, var(--error, #c0392b) 10%, transparent); }
</style>
