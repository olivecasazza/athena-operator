<template>
  <div class="min-h-screen bg-gray-950 text-gray-100 flex flex-col">
    <header class="px-8 pt-8 pb-6 text-center shrink-0">
      <h1 class="text-4xl font-bold text-pink-400">Athena Console</h1>
      <p class="text-gray-400 mt-2">Kubernetes Research Operator Dashboard</p>
    </header>

    <main class="w-full max-w-6xl mx-auto bg-gray-800 p-6 rounded-lg shadow-lg space-y-8 flex-1">
      <nav class="grid grid-cols-2 gap-2 font-mono text-xs text-gray-500 md:grid-cols-5" aria-label="Athena resource navigation">
        <button
          v-for="tab in navigationTabs"
          :key="tab.id"
          class="border px-3 py-3 text-left transition-colors hover:border-white hover:text-white"
          :class="activeTab === tab.id ? 'border-white bg-white text-black' : 'border-gray-700 bg-black/40'"
          type="button"
          @click="activeTab = tab.id"
        >
          <span class="block text-[10px] uppercase tracking-[0.18em]">{{ tab.label }}</span>
          <span class="mt-2 block text-2xl leading-none" :class="activeTab === tab.id ? 'text-black' : 'text-white'">{{ tab.count }}</span>
        </button>
      </nav>

      <section v-if="activeTab === 'experiments' || activeTab === 'running'">
        <div class="mb-6 flex justify-between items-center gap-4">
          <div>
            <h2 class="text-2xl font-semibold">{{ activeTab === 'running' ? 'Running Experiments' : 'Experiments' }}</h2>
            <p class="text-gray-400 text-sm">Kubernetes-native experiment resources, phases, workspace refs, and local IDE drafts.</p>
          </div>
          <button @click="refreshAll" class="bg-pink-600 hover:bg-pink-500 px-4 py-2 rounded text-sm transition-colors">
            Refresh
          </button>
        </div>

        <div v-if="loading" class="border border-gray-800 bg-black/40 px-4 py-5 font-mono text-xs text-gray-500">
          Loading Athena resources…
        </div>

        <div v-else-if="error" class="border border-red-800 bg-red-950/40 px-4 py-5 font-mono text-xs text-red-200 mb-4 whitespace-pre-wrap break-words">
          {{ error }}
        </div>

        <div v-else-if="visibleExperiments.length === 0" class="text-gray-500 text-center py-8 px-4 border-2 border-dashed border-gray-700 rounded">
          No {{ activeTab === 'running' ? 'running ' : '' }}experiments found in the cluster.
        </div>

        <div v-else class="overflow-x-auto">
          <table class="w-full text-left text-sm">
            <thead class="text-gray-300 border-b border-gray-700">
              <tr>
                <th class="py-2 pr-4">Experiment</th>
                <th class="py-2 pr-4">Phase</th>
                <th class="py-2 pr-4">Workspace</th>
                <th class="py-2 pr-4">Actions</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="exp in visibleExperiments" :key="exp.metadata.uid" class="border-b border-gray-700/70">
                <td class="py-3 pr-4">
                  <div class="font-medium text-pink-300">{{ exp.metadata.name }}</div>
                  <div class="text-xs text-gray-500">{{ exp.metadata.namespace }}</div>
                </td>
                <td class="py-3 pr-4">
                  <span class="px-2 py-1 text-xs rounded bg-blue-900 text-blue-200">
                    {{ exp.status?.phase || 'Unknown' }}
                  </span>
                </td>
                <td class="py-3 pr-4 font-mono text-xs text-gray-400">{{ exp.status?.workspacePath || '—' }}</td>
                <td class="py-3 pr-4">
                  <button class="border border-gray-700 px-3 py-1 font-mono text-xs text-gray-300 hover:border-pink-400 hover:text-white" @click="openIde(exp)">
                    Open IDE
                  </button>
                  <span v-if="draftCounts[draftKey(exp)]" class="ml-2 font-mono text-xs text-yellow-400">
                    {{ draftCounts[draftKey(exp)] }} draft file(s)
                  </span>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>

      <section v-if="activeTab === 'resources'" class="grid gap-4 md:grid-cols-2">
        <div class="bg-gray-900/60 p-4 rounded border border-gray-700">
          <h2 class="text-xl font-semibold mb-3">Benchmark Suites</h2>
          <div v-if="benchmarkSuites.length === 0" class="text-sm text-gray-500">No benchmark suites found.</div>
          <div v-for="suite in benchmarkSuites" :key="suite.metadata.uid" class="py-3 border-b border-gray-800 last:border-b-0">
            <div class="flex justify-between gap-4">
              <span class="text-pink-300 font-medium">{{ suite.metadata.name }}</span>
              <span class="text-xs text-gray-400">{{ suite.spec?.taxonomy || 'unknown' }}</span>
            </div>
            <div class="text-xs text-gray-500 mt-1">
              version {{ suite.spec?.suiteVersion || '—' }} · tasks {{ suite.status?.taskCount ?? suite.spec?.tasks?.length ?? 0 }} · ready {{ suite.status?.ready ?? false }}
            </div>
          </div>
        </div>

        <div class="bg-gray-900/60 p-4 rounded border border-gray-700">
          <h2 class="text-xl font-semibold mb-3">Runtime Profiles</h2>
          <div v-if="runtimeProfiles.length === 0" class="text-sm text-gray-500">No runtime profiles found.</div>
          <div v-for="profile in runtimeProfiles" :key="profile.metadata.uid" class="py-3 border-b border-gray-800 last:border-b-0">
            <div class="flex justify-between gap-4">
              <span class="text-pink-300 font-medium">{{ profile.metadata.name }}</span>
              <span class="text-xs text-gray-400">{{ profile.spec?.image || 'runtime' }}</span>
            </div>
            <div class="text-xs text-gray-500 mt-1">{{ profile.metadata.namespace }}</div>
          </div>
        </div>
      </section>

      <section v-if="activeTab === 'attention'" class="bg-gray-900/60 p-4 rounded border border-yellow-900/70">
        <h2 class="text-xl font-semibold mb-3">Attention</h2>
        <div v-if="attentionItems.length === 0" class="text-sm text-gray-500">No resources currently need attention.</div>
        <div v-for="item in attentionItems" :key="item.key" class="py-3 border-b border-gray-800 last:border-b-0">
          <div class="flex justify-between gap-4">
            <span class="text-yellow-300 font-medium">{{ item.name }}</span>
            <span class="text-xs text-gray-400">{{ item.phase }}</span>
          </div>
          <div class="text-xs text-gray-500 mt-1">{{ item.kind }} · {{ item.namespace }}</div>
        </div>
      </section>

      <section v-if="activeTab === 'configurations'" class="grid gap-4 md:grid-cols-2">
        <div class="bg-gray-900/60 p-4 rounded border border-gray-700">
          <h2 class="text-xl font-semibold mb-3">Configurations</h2>
          <div class="space-y-2 font-mono text-xs text-gray-400">
            <div class="flex justify-between border-b border-gray-800 pb-2"><span>Benchmark suites</span><span class="text-white">{{ benchmarkSuites.length }}</span></div>
            <div class="flex justify-between border-b border-gray-800 pb-2"><span>Runtime profiles</span><span class="text-white">{{ runtimeProfiles.length }}</span></div>
            <div class="flex justify-between border-b border-gray-800 pb-2"><span>Benchmark runs</span><span class="text-white">{{ benchmarkRuns.length }}</span></div>
            <div class="flex justify-between"><span>Experiments</span><span class="text-white">{{ experiments.length }}</span></div>
          </div>
        </div>
      </section>

      <section id="metrics" class="bg-gray-900/60 p-4 rounded border border-gray-700">
        <div class="flex items-center justify-between gap-4">
          <div>
            <h2 class="text-xl font-semibold mb-1">Metrics debugging</h2>
            <p class="text-sm text-gray-500">Use embedded Grafana panels for metric chart debugging instead of custom chart code.</p>
          </div>
          <a class="border border-gray-700 px-3 py-2 font-mono text-xs text-gray-300 hover:border-white hover:text-white" href="/grafana/d/athena-athena-experiment-debugging" target="_blank" rel="noreferrer">
            Open Grafana
          </a>
        </div>
      </section>
    </main>

    <footer class="sticky bottom-0 mt-auto z-40 border-t border-gray-800 bg-black/95 px-4 py-3 backdrop-blur">
      <div class="mx-auto flex max-w-6xl items-center justify-between gap-4 font-mono text-xs text-gray-500">
        <span>athena console</span>
        <button class="border border-transparent px-3 py-2 hover:border-white hover:text-white" type="button" @click="refreshAll">refresh</button>
      </div>
    </footer>

    <IdeModal
      :is-open="ideOpen"
      :experiment="selectedExperiment"
      :runtime-profiles="runtimeProfiles"
      :benchmark-suites="benchmarkSuites"
      :benchmark-runs="benchmarkRuns"
      @close="ideOpen = false"
      @save-draft="recordDraftSave"
    />
  </div>
</template>

<script setup lang="ts">
import { context, trace } from '@opentelemetry/api'
import { computed, ref, onMounted } from 'vue'
import IdeModal from './components/IdeModal.vue'
import { injectTraceHeaders, startUiSpan } from './telemetry'

const experiments = ref<any[]>([])
const benchmarkSuites = ref<any[]>([])
const benchmarkRuns = ref<any[]>([])
const runtimeProfiles = ref<any[]>([])
const loading = ref(true)
const error = ref<string | null>(null)
const ideOpen = ref(false)
const selectedExperiment = ref<any | null>(null)
const draftCounts = ref<Record<string, number>>({})
const activeTab = ref('experiments')

const runningCount = computed(() =>
  experiments.value.filter((exp) => (exp.status?.phase || '').toLowerCase() === 'running').length,
)
const resourceCount = computed(() => benchmarkSuites.value.length + runtimeProfiles.value.length)
const configurationCount = computed(() =>
  experiments.value.length + benchmarkRuns.value.length + benchmarkSuites.value.length + runtimeProfiles.value.length,
)
const visibleExperiments = computed(() =>
  activeTab.value === 'running'
    ? experiments.value.filter((exp) => (exp.status?.phase || '').toLowerCase() === 'running')
    : experiments.value,
)
const attentionItems = computed(() => {
  const isAttentionPhase = (phase: string) => ['failed', 'error', 'crashed', 'blocked', 'unknown'].includes(phase.toLowerCase())
  return [
    ...experiments.value
      .filter((exp) => isAttentionPhase(exp.status?.phase || 'Unknown'))
      .map((exp) => ({
        key: `experiment:${exp.metadata?.uid || exp.metadata?.name}`,
        kind: 'Experiment',
        name: exp.metadata?.name || 'unknown',
        namespace: exp.metadata?.namespace || 'default',
        phase: exp.status?.phase || 'Unknown',
      })),
    ...benchmarkRuns.value
      .filter((run) => isAttentionPhase(run.status?.phase || 'Unknown'))
      .map((run) => ({
        key: `benchmark-run:${run.metadata?.uid || run.metadata?.name}`,
        kind: 'BenchmarkRun',
        name: run.metadata?.name || 'unknown',
        namespace: run.metadata?.namespace || 'default',
        phase: run.status?.phase || 'Unknown',
      })),
  ]
})
const navigationTabs = computed(() => [
  { id: 'experiments', label: 'Experiments', count: experiments.value.length },
  { id: 'running', label: 'Running', count: runningCount.value },
  { id: 'attention', label: 'Attention', count: attentionItems.value.length },
  { id: 'resources', label: 'Resources', count: resourceCount.value },
  { id: 'configurations', label: 'Configurations', count: configurationCount.value },
])

const isJsonContentType = (contentType: string | null) =>
  Boolean(contentType && (contentType.includes('application/json') || contentType.includes('+json')))

const summarizeResponseBody = (body: string, contentType: string | null) => {
  const trimmed = body.trim()
  if (!trimmed) return ''

  if (trimmed.startsWith('<!DOCTYPE html') || trimmed.startsWith('<html') || contentType?.includes('text/html')) {
    const title = trimmed.match(/<title>(.*?)<\/title>/i)?.[1]?.replace(/\s+/g, ' ').trim()
    return title ? `upstream returned HTML instead of JSON (${title})` : 'upstream returned HTML instead of JSON'
  }

  return trimmed.replace(/\s+/g, ' ').slice(0, 240)
}

const fetchJson = async (path: string, fallbackData: any = []) => {
  const span = startUiSpan(`fetch ${path}`)
  return await context.with(trace.setSpan(context.active(), span), async () => {
    try {
      const headers = injectTraceHeaders()
      const res = await fetch(path, { headers })
      const contentType = res.headers.get('content-type')

      if (!res.ok) {
        if (res.status === 404) {
          console.warn(`[fallback] Kubernetes API unavailable from browser path (${path}: 404); rendering cached/mock projection.`)
          return fallbackData
        }

        const errText = await res.text()
        const summary = summarizeResponseBody(errText, contentType)
        throw new Error(
          `Error ${res.status} from ${path}${summary ? `: ${summary}` : ''}`,
        )
      }

      if (!isJsonContentType(contentType)) {
        const responseText = await res.text()
        const summary = summarizeResponseBody(responseText, contentType)
        throw new Error(
          `Invalid response from ${path}: expected JSON but received ${contentType || 'unknown content type'}${summary ? ` (${summary})` : ''}`,
        )
      }

      return await res.json()
    } catch (e) {
      span.recordException(e as Error)
      throw e
    } finally {
      span.end()
    }
  })
}

const draftKey = (exp: any) => `athena-ide-draft:${exp.metadata?.namespace || 'default'}:${exp.metadata?.name || 'experiment'}`

const refreshDraftCounts = () => {
  const next: Record<string, number> = {}
  for (const exp of experiments.value) {
    const raw = localStorage.getItem(draftKey(exp))
    if (!raw) continue
    try {
      next[draftKey(exp)] = JSON.parse(raw).length
    } catch {
      next[draftKey(exp)] = 1
    }
  }
  draftCounts.value = next
}

const openIde = (exp: any) => {
  selectedExperiment.value = exp
  ideOpen.value = true
}

const recordDraftSave = () => {
  refreshDraftCounts()
}

const refreshAll = async () => {
  const span = startUiSpan('refresh athena resources')
  loading.value = true
  error.value = null
  try {
    const [expData, suiteData, runData, runtimeData] = await Promise.all([
      fetchJson('/api/v1/experiments'),
      fetchJson('/api/v1/benchmark-suites'),
      fetchJson('/api/v1/benchmark-runs'),
      fetchJson('/api/v1/runtime-profiles'),
    ])
    experiments.value = expData
    benchmarkSuites.value = suiteData
    benchmarkRuns.value = runData
    runtimeProfiles.value = runtimeData
    refreshDraftCounts()
  } catch (e: any) {
    span.recordException(e)
    console.error(e)
    error.value = e.message || 'Failed to fetch Athena resources'
  } finally {
    span.end()
    loading.value = false
  }
}

onMounted(() => {
  refreshAll()
})
</script>
