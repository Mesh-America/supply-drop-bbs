<script setup lang="ts">
import { ref, nextTick, onMounted, onUnmounted } from 'vue'
import { api } from '../api/client'

interface LogLine {
  ts: string
  text: string
}

interface LogsResponse {
  cursor: number
  lines: string[]
}

const lines = ref<LogLine[]>([])
const running = ref(false)
const error = ref<string | null>(null)
const logBox = ref<HTMLElement | null>(null)
let cursor = 0
let timer: ReturnType<typeof setInterval> | null = null

function now(): string {
  return new Date().toTimeString().slice(0, 8)
}

async function poll() {
  try {
    const resp = await api.get<LogsResponse>(`/api/v1/logs?after=${cursor}`)
    if (resp.lines.length > 0) {
      for (const text of resp.lines) {
        lines.value.push({ ts: now(), text })
      }
      if (lines.value.length > 500) lines.value.splice(0, lines.value.length - 500)
      cursor = resp.cursor
      nextTick(() => {
        if (logBox.value) logBox.value.scrollTop = logBox.value.scrollHeight
      })
    }
    error.value = null
  } catch (e: any) {
    error.value = e?.message ?? 'poll failed'
  }
}

function start() {
  if (running.value) return
  running.value = true
  error.value = null
  poll() // immediate first fetch
  timer = setInterval(poll, 3000)
}

function stop() {
  running.value = false
  if (timer !== null) { clearInterval(timer); timer = null }
}

function clear() {
  lines.value = []
  cursor = 0
}

function lineClass(text: string): string {
  if (text.startsWith('[auth]')) return 'log-auth'
  if (text.startsWith('[msg]')) return 'log-msg'
  if (text.startsWith('[user]')) return 'log-user'
  if (text.startsWith('[warn]')) return 'log-warn'
  if (text.startsWith('[session]')) return 'log-session'
  if (text.startsWith('[system]')) return 'log-system'
  return 'log-event'
}

onMounted(start)
onUnmounted(stop)
</script>

<template>
  <div class="page">
    <header class="page-header">
      <h1>logs</h1>
      <div class="controls">
        <span class="indicator" :class="{ live: running }">
          {{ running ? '● live' : '○ stopped' }}
        </span>
        <button v-if="!running" @click="start">start</button>
        <button v-else class="secondary" @click="stop">stop</button>
        <button class="secondary" @click="clear" :disabled="lines.length === 0">clear</button>
      </div>
    </header>

    <p v-if="error" class="error small">{{ error }}</p>

    <div class="log-box" ref="logBox">
      <div v-if="lines.length" class="log-content">
        <div v-for="(line, i) in lines" :key="i" class="log-line" :class="lineClass(line.text)">
          <span class="log-ts">{{ line.ts }}</span>
          <span class="log-text">{{ line.text }}</span>
        </div>
      </div>
      <div v-else class="empty-state">
        <p class="muted" v-if="!running">Press <strong>start</strong> to begin streaming log events.</p>
        <p class="muted" v-else>Waiting for events…</p>
      </div>
    </div>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 0.7rem; height: calc(100vh - var(--topbar-h) - 3rem); }
.page-header { display: flex; align-items: center; gap: 1rem; flex-wrap: wrap; }
h1 { margin: 0; }
.controls { display: flex; flex-direction: row; align-items: center; gap: 0.6rem; margin-left: auto; }
.indicator { font-size: 0.85em; color: var(--muted); }
.indicator.live { color: #2a8a2a; }
.small { font-size: 0.85em; margin: 0; }

.log-box {
  flex: 1;
  border: 1px solid var(--border);
  border-radius: 3px;
  overflow: auto;
  background: var(--code-bg);
}
.log-content {
  padding: 0.4rem 0;
  font-family: monospace;
  font-size: 0.8em;
}
.log-line {
  display: flex;
  gap: 0.75rem;
  padding: 0.15rem 0.8rem;
  border-left: 2px solid transparent;
  line-height: 1.5;
}
.log-ts {
  color: var(--muted);
  flex-shrink: 0;
  user-select: none;
}
.log-text { word-break: break-all; white-space: pre-wrap; }

.log-auth    { border-left-color: #2a8a2a; }
.log-auth .log-ts { color: #2a8a2a; opacity: 0.7; }
.log-msg     { border-left-color: var(--accent); }
.log-user    { border-left-color: #7c5cbc; }
.log-warn    { border-left-color: var(--warning); color: var(--warning); }
.log-session { border-left-color: var(--muted); }
.log-system  { border-left-color: var(--border); opacity: 0.5; font-style: italic; }
.log-event   { opacity: 0.65; }

.empty-state { padding: 1.5rem 1rem; }
.empty-state p { margin: 0; }
</style>
