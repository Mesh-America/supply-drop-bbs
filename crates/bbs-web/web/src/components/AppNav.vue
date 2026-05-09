<script setup lang="ts">
import { ref, computed } from 'vue'
import { useAuthStore } from '../stores/auth'
import { useRouter } from 'vue-router'
import { useTheme } from '../composables/useTheme'

const open = ref(false)
const auth = useAuthStore()
const router = useRouter()
const { theme, toggle, label } = useTheme()

function close() { open.value = false }

async function logout() {
  await auth.logout()
  router.replace({ name: 'login' })
}

const themeLabel = computed(() => label[theme.value])

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
      <span class="brand-text">supply drop bbs</span>
      <span class="brand-sub muted">admin</span>
    </div>
    <div class="user-area">
      <button class="secondary small-btn theme-btn" @click="toggle" :title="`theme: ${theme}`">{{ themeLabel }}</button>
      <span class="muted small">{{ auth.user?.username }}</span>
      <button class="secondary small-btn" @click="logout">logout</button>
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
.brand { display: flex; align-items: baseline; gap: 0.5rem; font-weight: 700; }
.brand-sub { font-size: 0.75em; font-weight: 400; }
.user-area { margin-left: auto; display: flex; align-items: center; gap: 0.8rem; font-size: 0.85em; }
.small-btn { padding: 0.25rem 0.6rem; font-size: 0.85em; }
.theme-btn { font-size: 0.78em; min-width: 4.5rem; }

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
