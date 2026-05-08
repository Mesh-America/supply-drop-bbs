// Apply saved theme preference to <html> before first paint.
const saved = localStorage.getItem('bbs-theme')
if (saved === 'light' || saved === 'dark') {
  document.documentElement.setAttribute('data-theme', saved)
}
