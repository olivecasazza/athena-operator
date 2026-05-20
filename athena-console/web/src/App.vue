<template>
  <div class="min-h-screen flex flex-col items-center justify-center p-8">
    <header class="mb-8 text-center">
      <h1 class="text-4xl font-bold text-pink-400">Athena Console</h1>
      <p class="text-gray-400 mt-2">Kubernetes Research Operator Dashboard</p>
    </header>
    
    <main class="w-full max-w-6xl bg-gray-800 p-6 rounded-lg shadow-lg">
      <div class="mb-6 flex justify-between items-center">
        <h2 class="text-2xl font-semibold">Experiments</h2>
        <button @click="fetchExperiments" class="bg-pink-600 hover:bg-pink-500 px-4 py-2 rounded text-sm transition-colors">
          Refresh
        </button>
      </div>

      <div v-if="loading" class="text-gray-500 text-center py-8">Loading experiments...</div>
      
      <div v-else-if="error" class="bg-red-900/50 border border-red-500 text-red-200 p-4 rounded mb-4">
        {{ error }}
      </div>

      <div v-else-if="experiments.length === 0" class="text-gray-500 text-center py-8 border-2 border-dashed border-gray-700 rounded">
        No experiments found in the cluster.
      </div>
      
      <div v-else class="grid gap-4 md:grid-cols-2">
        <div v-for="exp in experiments" :key="exp.metadata.uid" class="bg-gray-700 p-4 rounded border border-gray-600">
          <div class="flex justify-between items-start mb-2">
            <h3 class="text-lg font-medium text-pink-300">{{ exp.metadata.name }}</h3>
            <span class="px-2 py-1 text-xs rounded bg-blue-900 text-blue-200">
              {{ exp.status?.phase || 'Unknown' }}
            </span>
          </div>
          <div class="text-sm text-gray-400 space-y-1">
            <p><strong>Namespace:</strong> {{ exp.metadata.namespace }}</p>
            <p><strong>Created:</strong> {{ new Date(exp.metadata.creationTimestamp).toLocaleString() }}</p>
            <div v-if="exp.status?.workspacePath" class="mt-2 text-xs font-mono bg-gray-900 p-2 rounded overflow-x-auto">
              {{ exp.status.workspacePath }}
            </div>
          </div>
        </div>
      </div>
    </main>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'

const experiments = ref<any[]>([])
const loading = ref(true)
const error = ref<string | null>(null)

const fetchExperiments = async () => {
  loading.value = true
  error.value = null
  try {
    const res = await fetch('/api/v1/experiments')
    if (res.ok) {
      experiments.value = await res.json()
    } else {
      const errText = await res.text()
      error.value = `Error ${res.status}: ${errText}`
    }
  } catch (e: any) {
    console.error(e)
    error.value = e.message || 'Failed to fetch experiments'
  } finally {
    loading.value = false
  }
}

onMounted(() => {
  fetchExperiments()
})
</script>
