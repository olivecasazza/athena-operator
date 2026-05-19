<template>
  <div class="min-h-screen flex flex-col items-center justify-center">
    <header class="mb-8 text-center">
      <h1 class="text-4xl font-bold text-pink-400">Athena Console</h1>
      <p class="text-gray-400 mt-2">Kubernetes Research Operator Dashboard</p>
    </header>
    
    <main class="w-full max-w-4xl bg-gray-800 p-6 rounded-lg shadow-lg">
      <div v-if="loading" class="text-gray-500">Loading identity...</div>
      <div v-else-if="user" class="space-y-4">
        <div class="p-4 bg-gray-700 rounded border border-gray-600">
          <h2 class="text-xl font-semibold mb-2">Authenticated User</h2>
          <p><strong>Subject:</strong> {{ user.subject }}</p>
          <p><strong>Roles:</strong> <span class="bg-blue-900 text-blue-200 px-2 py-1 rounded text-sm mr-2" v-for="role in user.roles" :key="role">{{ role }}</span></p>
        </div>
        <div class="text-sm text-gray-500">
          <p>Phase 1 read-only components are scaffolding. Navigation and data binding will be implemented next.</p>
        </div>
      </div>
      <div v-else class="text-red-400">Failed to load session.</div>
    </main>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'

const user = ref<any>(null)
const loading = ref(true)

onMounted(async () => {
  try {
    const res = await fetch('/api/v1/me')
    if (res.ok) {
      user.value = await res.json()
    }
  } catch (e) {
    console.error(e)
  } finally {
    loading.value = false
  }
})
</script>