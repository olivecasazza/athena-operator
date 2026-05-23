<template>
  <div v-if="isOpen" class="fixed inset-0 z-50 bg-black/90 p-4">
    <div
      class="flex h-full w-full flex-col border border-gray-800 bg-black text-gray-400"
    >
      <header
        class="flex items-center justify-between border-b border-gray-800 px-4 py-3"
      >
        <div>
          <h3 class="font-mono text-sm uppercase tracking-[0.2em] text-white">
            Athena IDE
          </h3>
          <p class="mt-1 font-mono text-xs text-gray-500">
            /athena · local draft editor · no Kubernetes writes until a future
            deploy action
          </p>
        </div>
        <button
          class="border border-gray-700 px-3 py-2 font-mono text-xs text-gray-300 hover:border-white hover:text-white"
          @click="requestClose"
        >
          Close
        </button>
      </header>

      <div class="grid min-h-0 flex-1 grid-cols-[18rem_minmax(0,1fr)]">
        <aside
          class="min-h-0 overflow-y-auto border-r border-gray-800 bg-gray-950 p-3"
        >
          <div
            class="mb-3 flex items-center justify-between px-2 font-mono text-xs uppercase tracking-widest text-gray-500"
          >
            <span>Root</span>
            <span>{{ dirtyCount }} dirty</span>
          </div>
          <button
            v-for="file in virtualFiles"
            :key="file.path"
            class="mb-1 block w-full border px-3 py-2 text-left font-mono text-xs"
            :class="
              file.path === currentFilePath
                ? 'border-pink-300 bg-blue-950/40 text-white'
                : 'border-transparent text-zinc-400 hover:border-zinc-700 hover:text-white'
            "
            @click="currentFilePath = file.path"
          >
            <span class="block truncate">{{ file.path }}</span>
            <span
              v-if="file.content !== file.initialContent"
              class="text-yellow-400"
              >modified</span
            >
          </button>
        </aside>

        <section class="flex min-h-0 flex-col bg-black">
          <div
            class="flex items-center justify-between border-b border-gray-800 px-4 py-2 font-mono text-xs text-gray-500"
          >
            <span class="truncate">{{
              currentFile?.path || "select a file"
            }}</span>
            <span
              v-if="
                currentFile &&
                currentFile.content !== currentFile.initialContent
              "
              class="text-yellow-400"
              >draft only</span
            >
          </div>
          <MonacoEditor
            v-if="currentFile"
            v-model="currentFile.content"
            class="min-h-0 flex-1"
            :language="currentFile.language"
          />
          <div
            v-else
            class="flex flex-1 items-center justify-center p-6 font-mono text-sm text-gray-600"
          >
            Select a file from /athena.
          </div>
        </section>
      </div>
    </div>

    <div
      v-if="showConfirm"
      class="absolute inset-0 z-60 flex items-center justify-center bg-black/80 p-4"
    >
      <div
        class="w-full max-w-lg border border-gray-700 bg-gray-950 p-6 font-mono"
      >
        <h4 class="text-sm uppercase tracking-[0.2em] text-white">
          Save local draft changes?
        </h4>
        <p class="mt-3 text-sm text-gray-400">
          {{ dirtyCount }} file(s) changed. Saving only writes a browser-local
          draft snapshot; it does not deploy or apply anything to Kubernetes.
        </p>
        <div class="mt-6 flex justify-end gap-3">
          <button
            class="border border-zinc-700 px-4 py-2 text-xs text-zinc-300 hover:border-blue-200 hover:text-blue-200"
            @click="showConfirm = false"
          >
            Cancel
          </button>
          <button
            class="border border-zinc-700 px-4 py-2 text-xs text-zinc-300 hover:border-blue-200 hover:text-blue-200"
            @click="discardAndClose"
          >
            Discard
          </button>
          <button
            class="border border-pink-300 px-4 py-2 text-xs text-pink-200 hover:bg-pink-200 hover:text-black"
            @click="saveDraftAndClose"
          >
            Save draft
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import yaml from "js-yaml";
import MonacoEditor from "./MonacoEditor.vue";

interface VirtualFile {
  path: string;
  content: string;
  initialContent: string;
  language: string;
}

const props = defineProps<{
  isOpen: boolean;
  experiment: any | null;
  runtimeProfiles: any[];
  benchmarkSuites: any[];
  benchmarkRuns: any[];
}>();

const emit = defineEmits<{
  close: [];
  saveDraft: [files: Pick<VirtualFile, "path" | "content">[]];
}>();

const virtualFiles = ref<VirtualFile[]>([]);
const currentFilePath = ref("");
const showConfirm = ref(false);

const currentFile = computed(
  () =>
    virtualFiles.value.find((file) => file.path === currentFilePath.value) ||
    null,
);
const dirtyFiles = computed(() =>
  virtualFiles.value.filter((file) => file.content !== file.initialContent),
);
const dirtyCount = computed(() => dirtyFiles.value.length);

const dumpYaml = (value: any) =>
  yaml.dump(value ?? {}, {
    indent: 2,
    noRefs: true,
    lineWidth: 120,
    sortKeys: false,
  });

const buildFiles = () => {
  const exp = props.experiment;
  if (!exp) return [];

  const expName = exp.metadata?.name || "experiment";
  const namespace = exp.metadata?.namespace || "default";
  const campaign = exp.spec?.campaignRef?.name;
  const matchingRuns = props.benchmarkRuns.filter(
    (run) =>
      run.spec?.targetRef?.name === expName ||
      run.metadata?.labels?.experiment === expName,
  );

  return [
    {
      path: `/athena/experiments/${namespace}/${expName}.yaml`,
      content: dumpYaml(exp),
      initialContent: dumpYaml(exp),
      language: "yaml",
    },
    {
      path: `/athena/config/parameterization.yaml`,
      content: dumpYaml({
        experiment: { namespace, name: expName },
        editable: true,
        deployOnEdit: false,
        saveSemantics: "local-draft-only",
        parameters: exp.spec?.parameters || {},
        runtimeProfile:
          exp.spec?.runtimeProfileRef || exp.spec?.runtimeProfile || null,
        campaignRef: exp.spec?.campaignRef || null,
      }),
      initialContent: dumpYaml({
        experiment: { namespace, name: expName },
        editable: true,
        deployOnEdit: false,
        saveSemantics: "local-draft-only",
        parameters: exp.spec?.parameters || {},
        runtimeProfile:
          exp.spec?.runtimeProfileRef || exp.spec?.runtimeProfile || null,
        campaignRef: exp.spec?.campaignRef || null,
      }),
      language: "yaml",
    },
    {
      path: `/athena/config/runtime-profiles.yaml`,
      content: dumpYaml(props.runtimeProfiles),
      initialContent: dumpYaml(props.runtimeProfiles),
      language: "yaml",
    },
    {
      path: `/athena/resources/benchmark-suites.yaml`,
      content: dumpYaml(props.benchmarkSuites),
      initialContent: dumpYaml(props.benchmarkSuites),
      language: "yaml",
    },
    {
      path: `/athena/resources/benchmark-runs.yaml`,
      content: dumpYaml(matchingRuns),
      initialContent: dumpYaml(matchingRuns),
      language: "yaml",
    },
    {
      path: `/athena/resources/summary.yaml`,
      content: dumpYaml({
        experiment: `${namespace}/${expName}`,
        campaign,
        phase: exp.status?.phase || "Unknown",
        workspacePath: exp.status?.workspacePath || null,
        files: 5,
      }),
      initialContent: dumpYaml({
        experiment: `${namespace}/${expName}`,
        campaign,
        phase: exp.status?.phase || "Unknown",
        workspacePath: exp.status?.workspacePath || null,
        files: 5,
      }),
      language: "yaml",
    },
  ];
};

watch(
  () => [props.isOpen, props.experiment],
  () => {
    if (!props.isOpen || !props.experiment) return;

    const key = `athena-ide-draft:${props.experiment.metadata?.namespace || "default"}:${props.experiment.metadata?.name || "experiment"}`;
    const saved = localStorage.getItem(key);
    if (saved) {
      try {
        const parsed = JSON.parse(saved) as VirtualFile[];
        virtualFiles.value = parsed.map((file) => ({
          ...file,
          initialContent: file.initialContent ?? file.content,
        }));
      } catch {
        virtualFiles.value = buildFiles();
      }
    } else {
      virtualFiles.value = buildFiles();
    }
    currentFilePath.value = virtualFiles.value[0]?.path || "";
  },
  { immediate: true },
);

const requestClose = () => {
  if (dirtyCount.value > 0) {
    showConfirm.value = true;
  } else {
    emit("close");
  }
};

const saveDraftAndClose = () => {
  if (props.experiment) {
    const key = `athena-ide-draft:${props.experiment.metadata?.namespace || "default"}:${props.experiment.metadata?.name || "experiment"}`;
    localStorage.setItem(key, JSON.stringify(virtualFiles.value));
  }
  emit(
    "saveDraft",
    dirtyFiles.value.map((file) => ({
      path: file.path,
      content: file.content,
    })),
  );
  showConfirm.value = false;
  emit("close");
};

const discardAndClose = () => {
  showConfirm.value = false;
  emit("close");
};
</script>
