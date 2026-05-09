<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { api } from '../api/client'

interface BackupRecord {
  filename: string
  size_bytes: number
  created_at: string
}

const backups = ref<BackupRecord[]>([])
const loading = ref(false)
const triggering = ref(false)
const error = ref<string | null>(null)
const actionOk = ref<string | null>(null)

function fmtSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`
}

async function load() {
  loading.value = true
  error.value = null
  try {
    backups.value = await api.get<BackupRecord[]>('/api/v1/backups')
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load backups'
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

onMounted(load)
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>backups</h1>
        <p class="muted">SQLite database snapshots via VACUUM INTO</p>
      </div>
      <div class="controls">
        <button class="secondary" @click="load" :disabled="loading">refresh</button>
        <button @click="triggerBackup" :disabled="triggering">
          {{ triggering ? 'backing up…' : 'create backup' }}
        </button>
      </div>
    </header>

    <p v-if="error" class="error">{{ error }}</p>
    <p v-if="actionOk" class="ok">{{ actionOk }}</p>
    <p v-if="!loading && backups.length === 0 && !error" class="muted">No backups found. Ensure <code>backup_dir</code> is set in <code>[plugins.web]</code>.</p>

    <table v-if="backups.length > 0">
      <thead>
        <tr>
          <th>filename</th>
          <th>size</th>
          <th>created</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="b in backups" :key="b.filename">
          <td><code>{{ b.filename }}</code></td>
          <td>{{ fmtSize(b.size_bytes) }}</td>
          <td class="muted small">{{ b.created_at.slice(0, 19).replace('T', ' ') }} UTC</td>
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
.ok { color: #2a8a2a; }
</style>
