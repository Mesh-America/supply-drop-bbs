<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { api } from '../api/client'

interface Room {
  id: number
  name: string
  description: string | null
  read_only: boolean
  min_permission_level: number
  message_count: number
  created_at: string
}

const rooms = ref<Room[]>([])
const loading = ref(false)
const error = ref<string | null>(null)
const actionError = ref<string | null>(null)
const actionOk = ref<string | null>(null)

const newName = ref('')
const newDesc = ref('')
const creating = ref(false)

const SYSTEM_ROOMS = [1, 2, 3]

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
  actionError.value = null
  actionOk.value = null
  creating.value = true
  try {
    await api.post('/api/v1/rooms', {
      name: newName.value.trim(),
      description: newDesc.value.trim() || null,
    })
    actionOk.value = `Room "${newName.value}" created`
    newName.value = ''
    newDesc.value = ''
    await load()
  } catch (e: any) {
    actionError.value = e?.message ?? 'failed to create room'
  } finally {
    creating.value = false
  }
}

async function remove(room: Room) {
  if (!confirm(`Delete room "${room.name}"? This cannot be undone.`)) return
  actionError.value = null
  actionOk.value = null
  try {
    await api.del(`/api/v1/rooms/${room.id}`)
    actionOk.value = `Room "${room.name}" deleted`
    await load()
  } catch (e: any) {
    actionError.value = e?.message ?? 'failed to delete room'
  }
}

onMounted(load)
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>rooms</h1>
        <p class="muted">Message rooms and their settings</p>
      </div>
      <button class="secondary" @click="load" :disabled="loading">refresh</button>
    </header>

    <section class="create-form">
      <h2>create room</h2>
      <div class="form-row">
        <input v-model="newName" placeholder="room name" maxlength="64" />
        <input v-model="newDesc" placeholder="description (optional)" maxlength="256" style="flex:2" />
        <button @click="create" :disabled="creating || !newName.trim()">create</button>
      </div>
    </section>

    <p v-if="error" class="error">{{ error }}</p>
    <p v-if="actionError" class="error">{{ actionError }}</p>
    <p v-if="actionOk" class="ok">{{ actionOk }}</p>
    <p v-if="!loading && rooms.length === 0 && !error" class="muted">No rooms found.</p>

    <table v-if="rooms.length > 0">
      <thead>
        <tr>
          <th>id</th>
          <th>name</th>
          <th>description</th>
          <th>msgs</th>
          <th>min level</th>
          <th>created</th>
          <th>actions</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="r in rooms" :key="r.id">
          <td class="muted small">{{ r.id }}</td>
          <td>
            <router-link :to="`/messages?room=${r.id}&name=${encodeURIComponent(r.name)}`">{{ r.name }}</router-link>
          </td>
          <td class="muted small">{{ r.description ?? '—' }}</td>
          <td>{{ r.message_count }}</td>
          <td>{{ r.min_permission_level }}</td>
          <td class="muted small">{{ r.created_at.slice(0, 10) }}</td>
          <td>
            <span v-if="SYSTEM_ROOMS.includes(r.id)" class="muted small">system</span>
            <button v-else class="small-btn danger" @click="remove(r)">delete</button>
          </td>
        </tr>
      </tbody>
    </table>
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
.small-btn { padding: 0.2rem 0.5rem; font-size: 0.8em; }
.ok { color: #2a8a2a; }
</style>
