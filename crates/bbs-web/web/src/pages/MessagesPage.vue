<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { useRoute } from 'vue-router'
import { api } from '../api/client'
import { useToast } from '../composables/useToast'

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

const MAIL_ROOM_ID = 2
const PAGE_SIZE = 50

const route = useRoute()
const toast = useToast()

const allRooms = ref<Room[]>([])
// Exclude mail room — DMs are private
const rooms = computed(() => allRooms.value.filter(r => r.id !== MAIL_ROOM_ID))

const selectedRoomId = ref<number | null>(null)
const selectedRoomName = ref<string>('')
const messages = ref<Message[]>([])
const loadingRooms = ref(false)
const loadingMsgs = ref(false)
const error = ref<string | null>(null)

// Search mode
const searchMode = ref(false)
const searchSender = ref((route.query.sender as string) ?? '')
const searchQuery = ref('')
const searchResults = ref<Message[]>([])
const searching = ref(false)

// Pagination
const pages = ref<(number | null)[]>([null])
const pageIdx = ref(0)
const hasNextPage = ref(false)
const hasPrevPage = computed(() => pageIdx.value > 0)
const currentCursor = computed(() => pages.value[pageIdx.value])

async function loadRooms() {
  loadingRooms.value = true
  try {
    allRooms.value = await api.get<Room[]>('/api/v1/rooms')
    if (searchSender.value) {
      searchMode.value = true
      runSearch()
      return
    }
    const roomParam = route.query.room
    const nameParam = route.query.name
    if (roomParam) {
      const id = Number(roomParam)
      if (id !== MAIL_ROOM_ID) {
        selectedRoomId.value = id
        selectedRoomName.value = String(nameParam ?? '')
        loadMessages()
      }
    } else if (rooms.value.length > 0) {
      selectRoom(rooms.value[0])
    }
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load rooms'
  } finally {
    loadingRooms.value = false
  }
}

function selectRoom(room: Room) {
  searchMode.value = false
  selectedRoomId.value = room.id
  selectedRoomName.value = room.name
  messages.value = []
  pages.value = [null]
  pageIdx.value = 0
  hasNextPage.value = false
  loadMessages()
}

async function loadMessages() {
  if (!selectedRoomId.value) return
  loadingMsgs.value = true
  error.value = null
  try {
    const cursor = currentCursor.value
    const qs = cursor !== null
      ? `?limit=${PAGE_SIZE}&after_id=${cursor}`
      : `?limit=${PAGE_SIZE}`
    const page = await api.get<Message[]>(`/api/v1/rooms/${selectedRoomId.value}/messages${qs}`)
    messages.value = page
    hasNextPage.value = page.length === PAGE_SIZE
  } catch (e: any) {
    error.value = e?.message ?? 'failed to load messages'
  } finally {
    loadingMsgs.value = false
  }
}

function goNext() {
  if (!hasNextPage.value || messages.value.length === 0) return
  const nextCursor = messages.value[messages.value.length - 1].id
  pages.value = [...pages.value.slice(0, pageIdx.value + 1), nextCursor]
  pageIdx.value++
  loadMessages()
}

function goPrev() {
  if (pageIdx.value === 0) return
  pageIdx.value--
  loadMessages()
}

async function runSearch() {
  searching.value = true
  error.value = null
  try {
    const params = new URLSearchParams({ limit: '100' })
    if (searchSender.value.trim()) params.set('sender', searchSender.value.trim())
    if (searchQuery.value.trim()) params.set('q', searchQuery.value.trim())
    searchResults.value = await api.get<Message[]>(`/api/v1/messages/search?${params}`)
  } catch (e: any) {
    error.value = e?.message ?? 'search failed'
  } finally {
    searching.value = false
  }
}

function clearSearch() {
  searchMode.value = false
  searchSender.value = ''
  searchQuery.value = ''
  searchResults.value = []
  if (rooms.value.length > 0 && !selectedRoomId.value) {
    selectRoom(rooms.value[0])
  }
}

async function deleteMessage(id: number) {
  if (!confirm('Delete this message?')) return
  try {
    await api.del(`/api/v1/messages/${id}`)
    toast.ok('Message deleted')
    if (searchMode.value) {
      searchResults.value = searchResults.value.filter(m => m.id !== id)
    } else {
      messages.value = messages.value.filter(m => m.id !== id)
    }
  } catch (e: any) {
    toast.error(e?.message ?? 'failed to delete message')
  }
}

let pollTimer: ReturnType<typeof setInterval> | null = null

onMounted(async () => {
  await loadRooms()
  pollTimer = setInterval(() => {
    if (!searchMode.value && selectedRoomId.value) loadMessages()
  }, 15_000)
})
onUnmounted(() => { if (pollTimer !== null) clearInterval(pollTimer) })
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>messages</h1>
        <p class="muted">Browse and moderate room messages — mail DMs are private</p>
      </div>
      <div class="search-bar">
        <input
          v-model="searchSender"
          placeholder="sender (exact)"
          class="search-input"
        />
        <input
          v-model="searchQuery"
          placeholder="content search…"
          class="search-input"
          @keydown.enter="() => { searchMode = true; runSearch() }"
        />
        <button @click="() => { searchMode = true; runSearch() }" :disabled="searching">search</button>
        <button v-if="searchMode" class="secondary" @click="clearSearch">clear</button>
      </div>
    </header>

    <p v-if="error" class="error">{{ error }}</p>

    <!-- Search results panel -->
    <div v-if="searchMode" class="search-results">
      <div class="results-header muted small">
        {{ searching ? 'searching…' : `${searchResults.length} result${searchResults.length !== 1 ? 's' : ''}` }}
      </div>
      <table v-if="searchResults.length > 0">
        <thead>
          <tr><th>id</th><th>room-msg</th><th>from</th><th>message</th><th>time</th><th></th></tr>
        </thead>
        <tbody>
          <tr v-for="m in searchResults" :key="m.id">
            <td class="muted small">{{ m.id }}</td>
            <td class="muted small">—</td>
            <td>{{ m.sender }}</td>
            <td class="content-cell">{{ m.content }}</td>
            <td class="muted small">{{ m.timestamp.slice(0, 16).replace('T', ' ') }}</td>
            <td><button class="small-btn danger" @click="deleteMessage(m.id)">del</button></td>
          </tr>
        </tbody>
      </table>
      <p v-else-if="!searching" class="muted empty-hint">No messages match your search.</p>
    </div>

    <!-- Room browser -->
    <div v-else class="layout">
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
        </div>
        <p v-if="!selectedRoomId" class="muted empty-hint">Select a room to browse messages.</p>

        <table v-if="messages.length > 0">
          <thead>
            <tr><th>id</th><th>from</th><th>message</th><th>time</th><th></th></tr>
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
        <p v-if="selectedRoomId && !loadingMsgs && messages.length === 0" class="muted empty-hint">
          No messages in this room yet.
        </p>

        <div v-if="hasPrevPage || hasNextPage" class="pagination">
          <button class="secondary small-btn" @click="goPrev" :disabled="!hasPrevPage || loadingMsgs">← prev</button>
          <span class="muted small">page {{ pageIdx + 1 }}</span>
          <button class="secondary small-btn" @click="goNext" :disabled="!hasNextPage || loadingMsgs">next →</button>
        </div>
      </section>
    </div>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 1rem; height: calc(100vh - var(--topbar-h) - 2rem); }
.page-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; flex-wrap: wrap; }
.page-header div { display: flex; flex-direction: column; gap: 0.2rem; }
h1 { margin: 0; }
p { margin: 0; }

.page-header .search-bar { display: flex; flex-direction: row; gap: 0.4rem; align-items: center; flex-wrap: wrap; }
.search-input { min-width: 130px; }

.search-results { flex: 1; overflow-y: auto; border: 1px solid var(--border); border-radius: 4px; }
.results-header { padding: 0.5rem 0.8rem; border-bottom: 1px solid var(--border); background: var(--row-alt); }

.layout { display: flex; gap: 0; flex: 1; border: 1px solid var(--border); border-radius: 4px; overflow: hidden; }

.room-list {
  width: 160px; flex-shrink: 0; border-right: 1px solid var(--border);
  overflow-y: auto; background: var(--row-alt); padding: 0.5rem 0;
}
.room-list-title { padding: 0.3rem 0.8rem 0.3rem; text-transform: uppercase; letter-spacing: 0.06em; font-size: 0.7em; }
.room-list ul { list-style: none; margin: 0; padding: 0; }
.room-list li {
  padding: 0.4rem 0.8rem; cursor: pointer; font-size: 0.9em; border-left: 2px solid transparent;
}
.room-list li:hover { background: var(--accent-bg); }
.room-list li.active { border-left-color: var(--accent); color: var(--accent); font-weight: 600; background: var(--accent-bg); }

.msg-panel { flex: 1; overflow-y: auto; display: flex; flex-direction: column; }
.msg-header { display: flex; align-items: center; gap: 0.8rem; padding: 0.5rem 0.8rem; border-bottom: 1px solid var(--border); }

.empty-hint { padding: 1rem 0.8rem; }
.small { font-size: 0.85em; }
.small-btn { padding: 0.2rem 0.5rem; font-size: 0.8em; }
.content-cell { max-width: 500px; word-break: break-word; white-space: pre-wrap; }

.pagination {
  display: flex; align-items: center; gap: 0.75rem;
  padding: 0.6rem 0.8rem; border-top: 1px solid var(--border); margin-top: auto;
}
</style>
