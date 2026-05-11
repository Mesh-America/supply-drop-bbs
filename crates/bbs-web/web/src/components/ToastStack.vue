<script setup lang="ts">
import { useToast } from '../composables/useToast'
const { toasts, dismiss } = useToast()
</script>

<template>
  <Teleport to="body">
    <div class="toast-stack">
      <TransitionGroup name="toast">
        <div
          v-for="t in toasts"
          :key="t.id"
          class="toast"
          :class="t.type"
          @click="dismiss(t.id)"
        >{{ t.message }}</div>
      </TransitionGroup>
    </div>
  </Teleport>
</template>

<style scoped>
.toast-stack {
  position: fixed;
  bottom: 1.5rem;
  right: 1.5rem;
  z-index: 9999;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  pointer-events: none;
}
.toast {
  pointer-events: all;
  padding: 0.6rem 1rem;
  border-radius: 4px;
  font-size: 0.88em;
  font-weight: 500;
  cursor: pointer;
  box-shadow: 0 2px 10px rgba(0,0,0,0.18);
  max-width: 340px;
  word-break: break-word;
}
.toast.ok    { background: #166534; color: #dcfce7; }
.toast.error { background: #7f1d1d; color: #fee2e2; }

:global(.light) .toast.ok    { background: #dcfce7; color: #14532d; }
:global(.light) .toast.error { background: #fee2e2; color: #7f1d1d; }

.toast-enter-active { transition: all 0.22s ease; }
.toast-leave-active { transition: all 0.18s ease; }
.toast-enter-from   { opacity: 0; transform: translateY(12px); }
.toast-leave-to     { opacity: 0; transform: translateX(20px); }
</style>
