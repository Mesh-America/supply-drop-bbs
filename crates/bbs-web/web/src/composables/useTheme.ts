import { ref, watchEffect } from 'vue'

type Theme = 'light' | 'dark' | 'system'

const theme = ref<Theme>((localStorage.getItem('bbs-theme') as Theme) ?? 'system')

watchEffect(() => {
  localStorage.setItem('bbs-theme', theme.value)
  const html = document.documentElement
  if (theme.value === 'system') {
    html.removeAttribute('data-theme')
  } else {
    html.setAttribute('data-theme', theme.value)
  }
})

export function useTheme() {
  function toggle() {
    if (theme.value === 'light') theme.value = 'dark'
    else if (theme.value === 'dark') theme.value = 'system'
    else theme.value = 'light'
  }

  const label = {
    light: '☀ light',
    dark: '● dark',
    system: '◐ auto',
  } satisfies Record<Theme, string>

  return { theme, toggle, label }
}
