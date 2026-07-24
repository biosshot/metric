<script setup lang="ts">
import {
  Activity,
  AlertTriangle,
  ArrowLeft,
  Ban,
  Bug,
  Check,
  CheckCircle2,
  ChevronDown,
  Circle,
  CircleDot,
  CircleX,
  Clipboard,
  Code2,
  Copy,
  Eye,
  FileCode2,
  Gauge,
  Info,
  KeyRound,
  ListFilter,
  LoaderCircle,
  LogOut,
  Menu,
  PlugZap,
  Plus,
  RefreshCw,
  Save,
  Search,
  Server,
  Settings,
  Shield,
  Trash2,
  X,
} from '@lucide/vue';
import type { Component } from 'vue';

const icons = {
  activity: Activity,
  alert: AlertTriangle,
  back: ArrowLeft,
  blocked: Ban,
  bug: Bug,
  check: Check,
  success: CheckCircle2,
  chevronDown: ChevronDown,
  circle: Circle,
  status: CircleDot,
  failure: CircleX,
  clipboard: Clipboard,
  code: Code2,
  copy: Copy,
  view: Eye,
  fileCode: FileCode2,
  gauge: Gauge,
  info: Info,
  key: KeyRound,
  filter: ListFilter,
  loading: LoaderCircle,
  signOut: LogOut,
  menu: Menu,
  connect: PlugZap,
  plus: Plus,
  refresh: RefreshCw,
  save: Save,
  search: Search,
  server: Server,
  settings: Settings,
  shield: Shield,
  delete: Trash2,
  close: X,
} satisfies Record<string, Component>;

export type AppIconName = keyof typeof icons;

withDefaults(
  defineProps<{
    name: AppIconName;
    size?: number;
    strokeWidth?: number;
    label?: string;
  }>(),
  {
    size: 18,
    strokeWidth: 1.8,
    label: undefined,
  },
);
</script>

<template>
  <component
    :is="icons[name]"
    class="app-icon"
    :size="size"
    :stroke-width="strokeWidth"
    :aria-hidden="label ? undefined : true"
    :aria-label="label"
  />
</template>
