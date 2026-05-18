<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { api } from '../api/client'

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
