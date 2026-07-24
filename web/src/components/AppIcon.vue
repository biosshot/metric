<script setup lang="ts">
import {
  Activity,
  AlertTriangle,
  ArrowLeft,
  Ban,
  Bug,
  Building2,
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
  History,
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
  ScrollText,
  Workflow,
  UserPlus,
  Users,
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
  organization: Building2,
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
  history: History,
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
  logs: ScrollText,
  traces: Workflow,
  userPlus: UserPlus,
  users: Users,
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
