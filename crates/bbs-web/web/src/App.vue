<script setup lang="ts">
import { onMounted, onUnmounted, computed } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useAuthStore } from './stores/auth'
import { SIGNED_OUT_EVENT } from './api/client'
import AppNav from './components/AppNav.vue'
import ToastStack from './components/ToastStack.vue'

const route = useRoute()
const router = useRouter()
const auth = useAuthStore()

const showChrome = computed(() => route.name !== 'login' && auth.user !== null)

function toSignIn() {
  if (route.name === 'login') return
  auth.forget()
  router.replace({ name: 'login', query: { next: route.fullPath } })
}

onMounted(async () => {
  window.addEventListener(SIGNED_OUT_EVENT, toSignIn)
  if (route.name === 'login') return
  try {
    await auth.whoami()
  } catch (err: any) {
    if (err?.status === 401) toSignIn()
  }
})

onUnmounted(() => window.removeEventListener(SIGNED_OUT_EVENT, toSignIn))
</script>

<template>
  <div class="layout">
    <AppNav v-if="showChrome" />
    <main :class="{ 'with-nav': showChrome }">
      <div class="content">
        <router-view />
      </div>
    </main>
    <ToastStack />
  </div>
</template>

<style scoped>
.layout { min-height: 100vh; }

main { padding: 1.4rem 1.6rem 2.5rem; }

main.with-nav {
  margin-left: 200px;
  padding-top: 1.4rem;
}

.content { max-width: 1400px; margin: 0 auto; }

@media (max-width: 800px) {
  main.with-nav { margin-left: 0; padding: 1rem 1rem 2rem; }
}
</style>
