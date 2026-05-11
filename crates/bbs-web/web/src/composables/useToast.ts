import { ref } from 'vue'

export interface Toast {
  id: number
  message: string
  type: 'ok' | 'error'
}

const toasts = ref<Toast[]>([])
let nextId = 0

export function useToast() {
  function push(message: string, type: Toast['type'] = 'ok', duration = 3500) {
    const id = ++nextId
    toasts.value.push({ id, message, type })
    setTimeout(() => dismiss(id), duration)
  }

  function dismiss(id: number) {
    const idx = toasts.value.findIndex(t => t.id === id)
    if (idx !== -1) toasts.value.splice(idx, 1)
  }

  const ok    = (msg: string) => push(msg, 'ok')
  const error = (msg: string) => push(msg, 'error', 5000)

  return { toasts, ok, error, dismiss }
}
