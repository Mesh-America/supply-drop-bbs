// Apply saved theme preferences to <html> before first paint to prevent flash.
const savedMode = localStorage.getItem('bbs-theme')
if (savedMode === 'light' || savedMode === 'dark') {
  document.documentElement.setAttribute('data-theme', savedMode)
}

const savedColor = localStorage.getItem('bbs-color')
if (savedColor === 'green' || savedColor === 'purple') {
  document.documentElement.setAttribute('data-color', savedColor)
}
