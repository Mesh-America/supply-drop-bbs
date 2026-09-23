<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { api } from '../api/client'

interface AuditArchive {
  filename: string
  size_bytes: number
  created_at: string
  entry_count?: number
}

interface Settings {
  audit_archive_dir: string | null
}

const archives = ref<AuditArchive[]>([])
const settings = ref<Settings | null>(null)
const loading = ref(false)
const deleting = ref<string | null>(null)
const error = ref<string | null>(null)

const dirConfigured = computed(() => settings.value?.audit_archive_dir != null)

function fmtSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`
}

function fmtDate(iso: string): string {
  return iso.slice(0, 19).replace('T', ' ') + ' UTC'
}

// The month an archive covers, read off its name rather than its timestamp:
// the file is written at the start of the following month.
function coveredMonth(filename: string): string {
  const m = filename.match(/^audit-(\d{4})-(\d{2})\.zip$/)
  if (!m) return filename
  const months = [
    'January', 'February', 'March', 'April', 'May', 'June',
    'July', 'August', 'September', 'October', 'November', 'December',
  ]
  const monthName = months[Number(m[2]) - 1] ?? m[2]
  return `${monthName} ${m[1]}`
}

function downloadUrl(filename: string): string {
  return '/api/v1/audit-archives/' + encodeURIComponent(filename)
}

async function load() {
  loading.value = true
  error.value = null
  try {
    settings.value = await api.get<Settings>('/api/v1/settings')
    archives.value = dirConfigured.value
      ? await api.get<AuditArchive[]>('/api/v1/audit-archives')
      : []
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load'
  } finally {
    loading.value = false
  }
}

async function deleteArchive(a: AuditArchive) {
  // Deliberately blunt: an archive is the only remaining copy of that month's
  // privileged actions, and nothing else deletes one.
  const what = coveredMonth(a.filename)
  if (
    !confirm(
      `Delete the audit archive for ${what}?\n\n` +
        `${a.filename}\n\n` +
        `This is the only copy of that month's audit log. It cannot be undone. ` +
        `Download it first if you might need it.`
    )
  ) {
    return
  }
  deleting.value = a.filename
  error.value = null
  try {
    await api.del(`/api/v1/audit-archives/${encodeURIComponent(a.filename)}`)
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
        <h1>audit archives</h1>
        <p class="muted">monthly snapshots of the audit log</p>
      </div>
      <div class="controls">
        <button @click="load" :disabled="loading">
          {{ loading ? 'loading…' : 'refresh' }}
        </button>
      </div>
    </header>

    <div v-if="settings && !dirConfigured" class="config-notice">
      <strong>Audit archive directory not configured.</strong>
      The server resolves it from the <code>[audit]</code> section of your config file. Set
      <code>audit.directory</code> (or leave it unset to use the default under the data
      directory) and restart the server.
    </div>

    <div v-if="dirConfigured" class="dir-info muted small">
      directory: <code>{{ settings!.audit_archive_dir }}</code>
    </div>

    <p class="muted small explain">
      On the first of each month the audit log is archived to a zip here and the live log
      starts fresh. Click an archive's name to download it; inside is a tab-separated text
      file of that month's entries. Archives are never removed automatically — deleting one
      is a sysop's decision, and it can't be undone.
    </p>

    <p v-if="error" class="error">{{ error }}</p>

    <p v-if="dirConfigured && !loading && archives.length === 0 && !error" class="muted">
      No archives yet. The first one appears after the turn of the month.
    </p>

    <table v-if="archives.length > 0">
      <thead>
        <tr>
          <th>covers</th>
          <th>file</th>
          <th>entries</th>
          <th>size</th>
          <th>archived</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="a in archives" :key="a.filename">
          <td>{{ coveredMonth(a.filename) }}</td>
          <td>
            <a :href="downloadUrl(a.filename)" class="dl-link" :download="a.filename">
              {{ a.filename }}
            </a>
          </td>
          <td class="muted small">{{ a.entry_count ?? '—' }}</td>
          <td class="size-col">{{ fmtSize(a.size_bytes) }}</td>
          <td class="muted small">{{ fmtDate(a.created_at) }}</td>
          <td class="action-col">
            <button
              class="danger small-btn"
              @click="deleteArchive(a)"
              :disabled="deleting !== null"
              title="Permanently delete this month's audit archive"
            >
              {{ deleting === a.filename ? '…' : 'delete' }}
            </button>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>

<style scoped>
.explain {
  margin: 0.75rem 0 1rem;
  max-width: 70ch;
}
.dir-info {
  margin-bottom: 0.5rem;
}
.config-notice {
  border: 1px solid var(--warn, #b58900);
  padding: 0.75rem;
  margin-bottom: 1rem;
}
.dl-link {
  font-family: var(--mono, monospace);
}
.size-col,
.action-col {
  white-space: nowrap;
}
.action-col {
  text-align: right;
}
.small-btn {
  font-size: 0.85em;
}
</style>
