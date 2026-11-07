import { ref } from 'vue'

export interface Toast {
  id: number
  message: string
  type: 'success' | 'error'
}

const toasts = ref<Toast[]>([])
let nextId = 1

export function useToast() {
  function addToast(message: string, type: Toast['type'] = 'success') {
    const id = nextId++
    toasts.value.push({ id, message, type })
    const duration = type === 'error' ? 8000 : 4000
    setTimeout(() => {
      toasts.value = toasts.value.filter(t => t.id !== id)
    }, duration)
  }

  function success(message: string) { addToast(message, 'success') }
  function error(message: string) { addToast(message, 'error') }

  return { toasts, success, error }
}
