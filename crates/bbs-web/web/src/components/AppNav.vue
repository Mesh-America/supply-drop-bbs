<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import { useAuthStore } from '../stores/auth'
import { useRouter } from 'vue-router'
import { useTheme } from '../composables/useTheme'
import type { Mode, ColorTheme } from '../composables/useTheme'
import logoUrl from '../assets/supply-drop-icon-transparent.svg'

const open = ref(false)
const menuOpen = ref(false)
const menuRef = ref<HTMLElement>()
const auth = useAuthStore()
const router = useRouter()
const { mode, color, modeLabel } = useTheme()

function close() { open.value = false }

async function logout() {
  menuOpen.value = false
  await auth.logout()
  router.replace({ name: 'login' })
}

function handleClickOutside(e: MouseEvent) {
  if (menuRef.value && !menuRef.value.contains(e.target as Node)) {
    menuOpen.value = false
  }
}

onMounted(() => document.addEventListener('click', handleClickOutside))
onUnmounted(() => document.removeEventListener('click', handleClickOutside))

const colors: { value: ColorTheme; label: string }[] = [
  { value: 'blue',   label: 'Blue' },
  { value: 'green',  label: 'Green' },
  { value: 'purple', label: 'Purple' },
]

const modes: { value: Mode; label: string }[] = [
  { value: 'light',  label: modeLabel.light },
  { value: 'dark',   label: modeLabel.dark },
  { value: 'system', label: modeLabel.system },
]

const groups = [
  {
    title: 'overview',
    items: [{ to: '/', label: 'dashboard' }],
  },
  {
    title: 'mesh',
    items: [{ to: '/adverts', label: 'adverts' }],
  },
  {
    title: 'sessions',
    items: [{ to: '/sessions', label: 'sessions' }],
  },
  {
    title: 'admin',
    items: [
      { to: '/users', label: 'users' },
      { to: '/rooms', label: 'rooms' },
      { to: '/messages', label: 'messages' },
    ],
  },
  {
    title: 'ops',
    items: [
      { to: '/reports', label: 'reports' },
      { to: '/backups', label: 'backups' },
      { to: '/logs', label: 'logs' },
      { to: '/audit', label: 'audit' },
    ],
  },
]
</script>

<template>
  <header class="topbar">
    <button class="menu-toggle secondary" @click="open = !open" aria-label="toggle menu">
      <span></span><span></span><span></span>
    </button>
    <div class="brand">
      <img :src="logoUrl" alt="Supply Drop BBS" class="brand-logo" />
      <span class="brand-sub muted">admin</span>
    </div>
    <div class="user-area">
      <div class="user-menu" ref="menuRef">
        <button
          class="secondary small-btn user-btn"
          @click.stop="menuOpen = !menuOpen"
          :aria-expanded="menuOpen"
        >
          {{ auth.user?.username }} <span class="caret">▾</span>
        </button>
        <div v-if="menuOpen" class="dropdown" role="menu">
          <div class="dropdown-section">
            <div class="dropdown-label">Theme</div>
            <button
              v-for="c in colors"
              :key="c.value"
              class="dropdown-item"
              :class="{ active: color === c.value }"
              @click="color = c.value"
            >
              <span class="dot" :data-color-swatch="c.value"></span>{{ c.label }}
            </button>
          </div>
          <div class="dropdown-section">
            <div class="dropdown-label">Mode</div>
            <button
              v-for="m in modes"
              :key="m.value"
              class="dropdown-item"
              :class="{ active: mode === m.value }"
              @click="mode = m.value"
            >
              {{ m.label }}
            </button>
          </div>
          <div class="dropdown-divider"></div>
          <button class="dropdown-item logout-item" @click="logout">logout</button>
        </div>
      </div>
    </div>
  </header>

  <aside class="sidebar" :class="{ open }">
    <nav>
      <div v-for="g in groups" :key="g.title" class="group">
        <div class="group-title">{{ g.title }}</div>
        <ul>
          <li v-for="item in g.items" :key="item.to">
            <router-link :to="item.to" @click="close">{{ item.label }}</router-link>
          </li>
        </ul>
      </div>
    </nav>
    <div class="sidebar-footer muted">
      <span>supply drop bbs</span>
      <p>open source project by <a href="http://meshamerica.com" target="_blank">Mesh America</a></p>
      <p>please consider <a href="https://meshamerica.com/pitch-in/" target="_blank">supporting our mission</a></p>
    </div>
  </aside>

  <div v-if="open" class="scrim" @click="close"></div>
</template>

<style scoped>
.topbar {
  position: sticky;
  top: 0;
  z-index: 30;
  display: flex;
  align-items: center;
  gap: 1rem;
  padding: 0 1rem;
  height: var(--topbar-h);
  background: var(--row-alt);
  border-bottom: 1px solid var(--border);
}
.brand { display: flex; align-items: center; gap: 0.5rem; }
.brand-logo { height: 32px; width: 32px; object-fit: contain; border-radius: 4px; }
.brand-sub { font-size: 0.75em; font-weight: 400; }
.user-area { margin-left: auto; display: flex; align-items: center; font-size: 0.85em; }

/* ── Username button ─────────────────────────────────────────────────────── */
.user-menu { position: relative; }

.user-btn {
  padding: 0.25rem 0.6rem;
  font-size: 0.85em;
  display: flex;
  align-items: center;
  gap: 0.25rem;
}
.caret { font-size: 0.75em; opacity: 0.7; }

/* ── Dropdown ────────────────────────────────────────────────────────────── */
.dropdown {
  position: absolute;
  top: calc(100% + 6px);
  right: 0;
  min-width: 170px;
  background: var(--bg);
  border: 1px solid var(--border);
  border-radius: 4px;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.15);
  z-index: 100;
  padding: 0.25rem 0;
}

.dropdown-section { padding: 0.25rem 0; }
.dropdown-section + .dropdown-section { border-top: 1px solid var(--border); }

.dropdown-label {
  font-size: 0.7em;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  color: var(--muted);
  padding: 0.3rem 0.9rem 0.15rem;
}

.dropdown-item {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  width: 100%;
  text-align: left;
  background: transparent;
  border: none;
  color: var(--fg);
  padding: 0.35rem 0.9rem;
  font-size: 0.88em;
  cursor: pointer;
  border-radius: 0;
}
.dropdown-item:hover { background: var(--row-alt); filter: none; }
.dropdown-item.active { color: var(--accent); font-weight: 600; }

.dropdown-divider { border-top: 1px solid var(--border); margin: 0.15rem 0; }

.logout-item { color: var(--error); }
.logout-item:hover { background: var(--row-alt); }

/* ── Color swatches ──────────────────────────────────────────────────────── */
.dot {
  display: inline-block;
  width: 10px;
  height: 10px;
  border-radius: 50%;
  flex-shrink: 0;
}
.dot[data-color-swatch="blue"]   { background: #0066cc; }
.dot[data-color-swatch="green"]  { background: #00a550; }
.dot[data-color-swatch="purple"] { background: #7c3aed; }

/* ── Sidebar ─────────────────────────────────────────────────────────────── */
.menu-toggle {
  display: none;
  background: transparent;
  border: 1px solid var(--border);
  color: var(--fg);
  padding: 0.35rem 0.45rem;
  flex-direction: column;
  gap: 3px;
  align-items: stretch;
}
.menu-toggle span { display: block; width: 18px; height: 2px; background: var(--fg); }

.sidebar {
  position: fixed;
  top: var(--topbar-h);
  left: 0;
  bottom: 0;
  width: 200px;
  border-right: 1px solid var(--border);
  background: var(--bg);
  padding: 0.8rem 0;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  z-index: 20;
}
.sidebar nav { flex: 1; }

.group { padding: 0.2rem 0 0.5rem; }
.group + .group { border-top: 1px dashed var(--border); margin-top: 0.4rem; padding-top: 0.6rem; }
.group-title {
  font-size: 0.7em;
  text-transform: uppercase;
  letter-spacing: 0.1em;
  color: var(--muted);
  padding: 0 1rem 0.25rem;
}
.sidebar ul { list-style: none; margin: 0; padding: 0; }
.sidebar li a {
  display: block;
  padding: 0.4rem 1rem;
  color: var(--fg);
  border-left: 2px solid transparent;
  font-size: 0.92em;
}
.sidebar li a:hover { background: var(--row-alt); text-decoration: none; }
.sidebar li a.router-link-active {
  color: var(--accent);
  font-weight: 600;
  border-left-color: var(--accent);
  background: var(--accent-bg);
}
.sidebar-footer {
  font-size: 0.75em;
  text-align: center;
  padding: 0.6rem 1rem 0;
  border-top: 1px dashed var(--border);
  margin-top: 0.4rem;
}

.scrim { display: none; position: fixed; top: var(--topbar-h); inset-inline: 0; bottom: 0; background: rgba(0,0,0,0.4); z-index: 15; }

@media (max-width: 800px) {
  .menu-toggle { display: flex; }
  .sidebar { transform: translateX(-100%); transition: transform 0.18s ease; width: 80%; max-width: 260px; }
  .sidebar.open { transform: translateX(0); }
  .scrim { display: block; }
  .sidebar:not(.open) ~ .scrim { display: none; }
}
</style>
