<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'

const lines = ref<string[]>([])
const connected = ref(false)
const error = ref<string | null>(null)
let es: EventSource | null = null

function connect() {
  if (es) return
  try {
    es = new EventSource('/api/v1/sse/logs', { withCredentials: true })
    es.onopen = () => { connected.value = true; error.value = null }
    es.onmessage = (e) => {
      lines.value.push(e.data)
      if (lines.value.length > 500) lines.value.shift()
    }
    es.onerror = () => {
      connected.value = false
      error.value = 'Connection lost — retrying…'
    }
  } catch (e: any) {
    error.value = e?.message ?? 'failed to connect to log stream'
  }
}

function disconnect() {
  if (es) { es.close(); es = null }
  connected.value = false
}

function clear() { lines.value = [] }

onMounted(connect)
onUnmounted(disconnect)
</script>

<template>
  <div class="page">
    <header class="page-header">
      <h1>logs</h1>
      <div class="controls">
        <span class="indicator" :class="{ live: connected }">
          {{ connected ? '● live' : '○ offline' }}
        </span>
        <button class="secondary" @click="clear">clear</button>
      </div>
    </header>
    <p v-if="error" class="error small">{{ error }}</p>
    <div class="log-box">
      <pre v-if="lines.length" class="log-content">{{ lines.join('\n') }}</pre>
      <p v-else class="muted empty">Waiting for log events…</p>
    </div>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 0.7rem; height: calc(100vh - var(--topbar-h) - 3rem); }
.page-header { display: flex; align-items: center; gap: 1rem; flex-wrap: wrap; }
h1 { margin: 0; }
.controls { display: flex; align-items: center; gap: 0.8rem; margin-left: auto; }
.indicator { font-size: 0.85em; color: var(--muted); }
.indicator.live { color: #2a8a2a; }
.small { font-size: 0.85em; }

.log-box {
  flex: 1;
  border: 1px solid var(--border);
  border-radius: 3px;
  overflow: auto;
  background: var(--code-bg);
}
.log-content {
  margin: 0;
  padding: 0.6rem 0.8rem;
  font-size: 0.8em;
  white-space: pre-wrap;
  word-break: break-all;
  background: transparent;
}
.empty { padding: 1rem; margin: 0; }
</style>
