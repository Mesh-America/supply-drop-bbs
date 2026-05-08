<script setup lang="ts">
</script>

<template>
  <div class="page">
    <header class="page-header">
      <h1>users</h1>
      <p class="muted">Manage BBS user accounts.</p>
    </header>
    <div class="placeholder">
      <p class="muted">User management coming soon — wires up once BBS auth is complete.</p>
    </div>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 1rem; }
.page-header { display: flex; flex-direction: column; gap: 0.2rem; }
h1 { margin: 0; }
.placeholder { border: 1px dashed var(--border); border-radius: 4px; padding: 2rem; text-align: center; }
</style>
