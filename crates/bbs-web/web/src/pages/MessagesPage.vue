<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useRoute } from 'vue-router'
import { api } from '../api/client'

interface Room {
  id: number
  name: string
}

interface Message {
  id: number
  sender: string
  recipient: string | null
  content: string
  timestamp: string
}

const route = useRoute()

const rooms = ref<Room[]>([])
const selectedRoomId = ref<number | null>(null)
const selectedRoomName = ref<string>('')
const messages = ref<Message[]>([])
const loadingRooms = ref(false)
const loadingMsgs = ref(false)
const error = ref<string | null>(null)
const actionError = ref<string | null>(null)
const actionOk = ref<string | null>(null)
const afterId = ref<number | null>(null)

async function loadRooms() {
  loadingRooms.value = true
  try {
    rooms.value = await api.get<Room[]>('/api/v1/rooms')
    if (!selectedRoomId.value && rooms.value.length > 0) {
      selectRoom(rooms.value[0])
    }
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load rooms'
  } finally {
    loadingRooms.value = false
  }
}

function selectRoom(room: Room) {
  selectedRoomId.value = room.id
  selectedRoomName.value = room.name
  messages.value = []
  afterId.value = null
  loadMessages()
}

async function loadMessages(more = false) {
  if (!selectedRoomId.value) return
  loadingMsgs.value = true
  error.value = null
  try {
    const qs = more && afterId.value ? `?after_id=${afterId.value}` : ''
    const page = await api.get<Message[]>(`/api/v1/rooms/${selectedRoomId.value}/messages${qs}`)
    if (more) {
      messages.value.push(...page)
    } else {
      messages.value = page
    }
    if (page.length > 0) {
      afterId.value = page[page.length - 1].id
    }
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load messages'
  } finally {
    loadingMsgs.value = false
  }
}

async function deleteMessage(id: number) {
  if (!confirm('Delete this message?')) return
  actionError.value = null
  actionOk.value = null
  try {
    await api.del(`/api/v1/messages/${id}`)
    actionOk.value = 'Message deleted'
    messages.value = messages.value.filter(m => m.id !== id)
  } catch (e: any) {
    actionError.value = e?.message ?? 'failed to delete message'
  }
}

onMounted(async () => {
  // Support ?room=ID&name=NAME from rooms page links
  const roomParam = route.query.room
  const nameParam = route.query.name
  if (roomParam) {
    selectedRoomId.value = Number(roomParam)
    selectedRoomName.value = String(nameParam ?? '')
  }
  await loadRooms()
  if (selectedRoomId.value) loadMessages()
})
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>messages</h1>
        <p class="muted">Browse and moderate room messages</p>
      </div>
    </header>

    <p v-if="error" class="error">{{ error }}</p>
    <p v-if="actionError" class="error">{{ actionError }}</p>
    <p v-if="actionOk" class="ok">{{ actionOk }}</p>

    <div class="layout">
      <aside class="room-list">
        <div class="room-list-title muted small">rooms</div>
        <ul>
          <li
            v-for="r in rooms"
            :key="r.id"
            :class="{ active: r.id === selectedRoomId }"
            @click="selectRoom(r)"
          >{{ r.name }}</li>
        </ul>
      </aside>

      <section class="msg-panel">
        <div class="msg-header" v-if="selectedRoomId">
          <strong>#{{ selectedRoomName }}</strong>
          <button class="secondary small-btn" @click="loadMessages()">refresh</button>
        </div>
        <p v-if="!selectedRoomId" class="muted">Select a room to browse messages.</p>

        <table v-if="messages.length > 0">
          <thead>
            <tr>
              <th>id</th>
              <th>from</th>
              <th>message</th>
              <th>time</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="m in messages" :key="m.id">
              <td class="muted small">{{ m.id }}</td>
              <td>{{ m.sender }}</td>
              <td class="content-cell">{{ m.content }}</td>
              <td class="muted small">{{ m.timestamp.slice(0, 16).replace('T', ' ') }}</td>
              <td><button class="small-btn danger" @click="deleteMessage(m.id)">del</button></td>
            </tr>
          </tbody>
        </table>
        <p v-if="selectedRoomId && !loadingMsgs && messages.length === 0" class="muted">No messages in this room.</p>
        <button
          v-if="messages.length > 0"
          class="secondary load-more"
          @click="loadMessages(true)"
          :disabled="loadingMsgs"
        >load more</button>
      </section>
    </div>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 1rem; height: calc(100vh - var(--topbar-h) - 2rem); }
.page-header { display: flex; align-items: flex-start; justify-content: space-between; }
.page-header div { display: flex; flex-direction: column; gap: 0.2rem; }
h1 { margin: 0; }
p { margin: 0; }

.layout { display: flex; gap: 0; flex: 1; border: 1px solid var(--border); border-radius: 4px; overflow: hidden; }

.room-list {
  width: 160px;
  flex-shrink: 0;
  border-right: 1px solid var(--border);
  overflow-y: auto;
  background: var(--row-alt);
  padding: 0.5rem 0;
}
.room-list-title { padding: 0.3rem 0.8rem 0.3rem; text-transform: uppercase; letter-spacing: 0.06em; font-size: 0.7em; }
.room-list ul { list-style: none; margin: 0; padding: 0; }
.room-list li {
  padding: 0.4rem 0.8rem;
  cursor: pointer;
  font-size: 0.9em;
  border-left: 2px solid transparent;
}
.room-list li:hover { background: var(--accent-bg); }
.room-list li.active { border-left-color: var(--accent); color: var(--accent); font-weight: 600; background: var(--accent-bg); }

.msg-panel { flex: 1; overflow-y: auto; display: flex; flex-direction: column; }
.msg-header { display: flex; align-items: center; gap: 0.8rem; padding: 0.5rem 0.8rem; border-bottom: 1px solid var(--border); }

.small { font-size: 0.85em; }
.small-btn { padding: 0.2rem 0.5rem; font-size: 0.8em; }
.content-cell { max-width: 500px; word-break: break-word; white-space: pre-wrap; }
.load-more { margin: 0.5rem auto; display: block; }
.ok { color: #2a8a2a; }
</style>
