import { ref, watchEffect } from 'vue'

export type Mode = 'light' | 'dark' | 'system'
export type ColorTheme = 'blue' | 'green' | 'purple'

const mode = ref<Mode>((localStorage.getItem('bbs-theme') as Mode) ?? 'dark')
const color = ref<ColorTheme>((localStorage.getItem('bbs-color') as ColorTheme) ?? 'blue')

watchEffect(() => {
  localStorage.setItem('bbs-theme', mode.value)
  const html = document.documentElement
  if (mode.value === 'system') {
    html.removeAttribute('data-theme')
  } else {
    html.setAttribute('data-theme', mode.value)
  }
})

watchEffect(() => {
  localStorage.setItem('bbs-color', color.value)
  const html = document.documentElement
  if (color.value === 'blue') {
    html.removeAttribute('data-color')
  } else {
    html.setAttribute('data-color', color.value)
  }
})

export function useTheme() {
  const modeLabel: Record<Mode, string> = {
    light: '☀ light',
    dark: '● dark',
    system: '◐ auto',
  }

  return { mode, color, modeLabel }
}
