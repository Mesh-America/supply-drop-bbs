<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import { api } from '../api/client'
import { useToast } from '../composables/useToast'

interface Room {
  id: number
  name: string
  description: string | null
  read_only: boolean
  min_permission_level: number
  message_count: number
  created_at: string
  deletable: boolean
}

const rooms = ref<Room[]>([])
const loading = ref(false)
const error = ref<string | null>(null)
const toast = useToast()

const newName = ref('')
const newDesc = ref('')
const creating = ref(false)

// ── Edit modal ───────────────────────────────────────────────────────────────
const editTarget = ref<Room | null>(null)
const editDesc = ref('')
const editReadOnly = ref(false)
const editMinLevel = ref(0)
const saving = ref(false)

function openEdit(room: Room) {
  editTarget.value = room
  editDesc.value = room.description ?? ''
  editReadOnly.value = room.read_only
  editMinLevel.value = room.min_permission_level
}

function closeEdit() { editTarget.value = null }

async function saveEdit() {
  if (!editTarget.value) return
  saving.value = true
  try {
    const body: Record<string, unknown> = {
      read_only: editReadOnly.value,
      min_permission_level: editMinLevel.value,
    }
    // Send null to clear, string to set
    body.description = editDesc.value.trim() === '' ? null : editDesc.value.trim()

    await api.patch(`/api/v1/rooms/${editTarget.value.id}`, body)
    toast.ok(`Room "${editTarget.value.name}" updated`)
    closeEdit()
    await load()
  } catch (e: any) {
    toast.error(e?.message ?? 'failed to update room')
  } finally {
    saving.value = false
  }
}

async function load() {
  loading.value = true
  error.value = null
  try {
    rooms.value = await api.get<Room[]>('/api/v1/rooms')
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load rooms'
  } finally {
    loading.value = false
  }
}

async function create() {
  if (!newName.value.trim()) return
  creating.value = true
  try {
    await api.post('/api/v1/rooms', {
      name: newName.value.trim(),
      description: newDesc.value.trim() || null,
    })
    toast.ok(`Room "${newName.value}" created`)
    newName.value = ''
    newDesc.value = ''
    await load()
  } catch (e: any) {
    toast.error(e?.message ?? 'failed to create room')
  } finally {
    creating.value = false
  }
}

async function remove(room: Room) {
  if (!confirm(`Delete room "${room.name}"? This cannot be undone.`)) return
  try {
    await api.del(`/api/v1/rooms/${room.id}`)
    toast.ok(`Room "${room.name}" deleted`)
    await load()
  } catch (e: any) {
    toast.error(e?.message ?? 'failed to delete room')
  }
}

let pollTimer: ReturnType<typeof setInterval> | null = null
onMounted(() => { load(); pollTimer = setInterval(load, 30_000) })
onUnmounted(() => { if (pollTimer !== null) clearInterval(pollTimer) })
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>rooms</h1>
        <p class="muted">Message rooms and their settings</p>
      </div>
    </header>

    <section class="create-form">
      <h2>create room</h2>
      <div class="form-row">
        <input v-model="newName" placeholder="room name" maxlength="64" />
        <input v-model="newDesc" placeholder="description (optional)" maxlength="512" style="flex:2" />
        <button @click="create" :disabled="creating || !newName.trim()">create</button>
      </div>
    </section>

    <p v-if="error" class="error">{{ error }}</p>
    <p v-if="!loading && rooms.length === 0 && !error" class="muted">No rooms found.</p>

    <table v-if="rooms.length > 0">
      <thead>
        <tr>
          <th>id</th>
          <th>name</th>
          <th>description</th>
          <th>msgs</th>
          <th>read-only</th>
          <th>min level</th>
          <th>created</th>
          <th>actions</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="r in rooms" :key="r.id">
          <td class="muted small">{{ r.id }}</td>
          <td>
            <router-link :to="{ name: 'messages', query: { room: r.id, name: r.name } }">{{ r.name }}</router-link>
          </td>
          <td class="muted small">{{ r.description ?? '—' }}</td>
          <td>{{ r.message_count }}</td>
          <td>{{ r.read_only ? '✓' : '' }}</td>
          <td>{{ r.min_permission_level }}</td>
          <td class="muted small">{{ r.created_at.slice(0, 10) }}</td>
          <td class="actions">
            <button class="small-btn secondary" @click="openEdit(r)">edit</button>
            <span v-if="!r.deletable" class="muted small"> system</span>
            <button v-else class="small-btn danger" @click="remove(r)">delete</button>
          </td>
        </tr>
      </tbody>
    </table>

    <!-- Edit modal -->
    <Teleport to="body">
      <div v-if="editTarget" class="modal-backdrop" @click.self="closeEdit">
        <div class="modal">
          <h2>edit room: {{ editTarget.name }}</h2>

          <label class="field">
            <span>description</span>
            <input v-model="editDesc" placeholder="leave blank to clear" maxlength="512" />
          </label>

          <label class="field field-row">
            <input type="checkbox" v-model="editReadOnly" />
            <span>read-only (only aides/sysops can post)</span>
          </label>

          <label class="field">
            <span>minimum permission level</span>
            <select v-model="editMinLevel">
              <option :value="0">0 — unvalidated</option>
              <option :value="10">10 — user</option>
              <option :value="50">50 — aide</option>
              <option :value="100">100 — sysop</option>
            </select>
          </label>

          <div class="modal-actions">
            <button @click="closeEdit" class="secondary">cancel</button>
            <button @click="saveEdit" :disabled="saving">save</button>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 1rem; }
.page-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; flex-wrap: wrap; }
.page-header div { display: flex; flex-direction: column; gap: 0.2rem; }
h1, h2 { margin: 0; }
p { margin: 0; }
.create-form { display: flex; flex-direction: column; gap: 0.5rem; border: 1px solid var(--border); border-radius: 4px; padding: 0.8rem 1rem; }
.form-row { display: flex; gap: 0.5rem; flex-wrap: wrap; }
.form-row input { flex: 1; min-width: 120px; }
.small { font-size: 0.85em; }
.small-btn { padding: 0.2rem 0.5rem; font-size: 0.8em; margin-right: 0.2rem; }
.actions { white-space: nowrap; }

/* Modal */
.modal-backdrop {
  position: fixed; inset: 0; background: rgba(0,0,0,0.5);
  z-index: 200; display: flex; align-items: center; justify-content: center;
}
.modal {
  background: var(--bg); border: 1px solid var(--border); border-radius: 6px;
  padding: 1.5rem; min-width: 340px; max-width: 480px; width: 100%;
  display: flex; flex-direction: column; gap: 1rem;
}
.modal h2 { margin: 0; font-size: 1rem; }
.field { display: flex; flex-direction: column; gap: 0.3rem; font-size: 0.9em; }
.field span { color: var(--muted); font-size: 0.82em; text-transform: uppercase; letter-spacing: 0.05em; }
.field input, .field select { width: 100%; }
.field-row { flex-direction: row; align-items: center; gap: 0.5rem; }
.field-row span { text-transform: none; letter-spacing: normal; font-size: 0.9em; color: var(--fg); }
.modal-actions { display: flex; justify-content: flex-end; gap: 0.5rem; padding-top: 0.5rem; }
</style>
