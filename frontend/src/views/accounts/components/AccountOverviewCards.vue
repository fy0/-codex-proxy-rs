<script setup lang="ts">
import type { getAccounts } from '@/api'
import { BaseCard, BaseMotionIcon } from '@codex-proxy/ui'

import { AlertTriangle, Gauge, ShieldCheck, Users } from '@lucide/vue'
import { computed } from 'vue'
import { formatInteger } from '@/utils/format'

const props = defineProps<{
  summary: Awaited<ReturnType<typeof getAccounts>>['summary']
}>()

const overviewItems = computed(() => [
  {
    label: '总账号',
    value: formatInteger(props.summary.total),
    caption: '账号池规模',
    tone: 'neutral',
    icon: Users,
  },
  {
    label: '正常账号',
    value: formatInteger(props.summary.normal),
    caption: '可参与调度',
    tone: 'success',
    icon: ShieldCheck,
  },
  {
    label: '额度受限',
    value: formatInteger((props.summary.quotaExhausted ?? 0) + (props.summary.rateLimited ?? 0)),
    caption: '等待额度恢复',
    tone: 'warning',
    icon: Gauge,
  },
  {
    label: '待处理',
    value: formatInteger((props.summary.disabled ?? 0) + (props.summary.error ?? 0)),
    caption: '已停用 / 错误',
    tone: 'danger',
    icon: AlertTriangle,
  },
])

function overviewIconClass(tone: string) {
  if (tone === 'success') {
    return 'bg-cp-green-container text-cp-green-on-container'
  }
  if (tone === 'warning') {
    return 'bg-cp-orange-container text-cp-orange-on-container'
  }
  if (tone === 'danger') {
    return 'bg-cp-error-container text-cp-error-on-container'
  }
  return 'bg-cp-blue-container text-cp-blue-on-container'
}
</script>

<template>
  <div class="mt-5 grid shrink-0 grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-4">
    <BaseCard v-for="item in overviewItems" :key="item.label" as="article" padding="compact">
      <div class="flex items-stretch justify-between gap-3">
        <div class="flex min-w-0 flex-col">
          <p class="m-0 text-cp-sm leading-none font-heavy text-cp-text-secondary">
            {{ item.label }}
          </p>
          <strong class="my-2 block font-mono text-[26px] leading-none font-extrabold text-cp-text">
            {{ item.value }}
          </strong>
          <p class="m-0 truncate text-cp-sm leading-none font-emphasis text-cp-text-quaternary">
            {{ item.caption }}
          </p>
        </div>
        <BaseMotionIcon
          class="inline-flex size-9 shrink-0 items-center justify-center self-start rounded-lg"
          :class="overviewIconClass(item.tone)"
        >
          <component :is="item.icon" class="size-4.5" />
        </BaseMotionIcon>
      </div>
    </BaseCard>
  </div>
</template>
