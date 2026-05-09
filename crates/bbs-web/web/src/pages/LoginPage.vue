<script setup lang="ts">
import { ref } from 'vue'
import { useRouter, useRoute } from 'vue-router'
import { useAuthStore } from '../stores/auth'
import { ApiError } from '../api/client'
import logoUrl from '../assets/logo.png'

const router = useRouter()
const route = useRoute()
const auth = useAuthStore()

const username = ref('admin')
const password = ref('')
const error = ref<string | null>(null)
const loading = ref(false)

async function submit() {
  error.value = null
  loading.value = true
  try {
    await auth.login(username.value, password.value)
    const next = (route.query.next as string) || '/'
    router.replace(next)
  } catch (e: any) {
    if (e instanceof ApiError && e.status === 401) {
      error.value = 'Invalid credentials.'
    } else {
      error.value = e?.message ?? 'Login failed.'
    }
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <div class="login-wrap">
    <div class="login-box">
      <div class="logo-wrap">
        <img :src="logoUrl" alt="Supply Drop BBS" class="logo" />
      </div>
      <p class="muted sub">admin panel</p>
      <form @submit.prevent="submit">
        <div class="field">
          <label>username</label>
          <input v-model="username" type="text" autocomplete="username" required />
        </div>
        <div class="field">
          <label>password</label>
          <input v-model="password" type="password" autocomplete="current-password" required />
        </div>
        <p v-if="error" class="error">{{ error }}</p>
        <button type="submit" :disabled="loading">
          {{ loading ? 'signing in…' : 'sign in' }}
        </button>
      </form>
    </div>
  </div>
</template>

<style scoped>
.login-wrap {
  min-height: 100vh;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 2rem;
}
.login-box {
  width: 100%;
  max-width: 340px;
  display: flex;
  flex-direction: column;
  gap: 1rem;
}
.logo-wrap { display: flex; justify-content: center; margin-bottom: 0.25rem; }
.logo {
  width: 160px;
  height: 160px;
  object-fit: contain;
  border-radius: 16px;
}
.sub { margin: 0; font-size: 0.85em; text-align: center; }
form { display: flex; flex-direction: column; gap: 0.75rem; }
.field { display: flex; flex-direction: column; gap: 0.25rem; }
label { font-size: 0.8em; color: var(--muted); text-transform: uppercase; letter-spacing: 0.05em; }
input { width: 100%; }
button { margin-top: 0.25rem; }
</style>
