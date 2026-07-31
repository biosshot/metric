<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, useId } from 'vue';
import AppIcon, { type AppIconName } from './AppIcon.vue';

export interface SelectOption {
  value: string;
  label: string;
  description?: string;
  icon?: AppIconName;
  action?: boolean;
}

const props = withDefaults(
  defineProps<{
    modelValue: string;
    options: SelectOption[];
    label?: string;
    ariaLabel?: string;
    selectedLabel?: string;
    disabled?: boolean;
  }>(),
  {
    label: undefined,
    ariaLabel: undefined,
    selectedLabel: undefined,
    disabled: false,
  },
);

const emit = defineEmits<{ 'update:modelValue': [value: string] }>();
const id = useId();
const root = ref<HTMLElement>();
const trigger = ref<HTMLButtonElement>();
const open = ref(false);
const highlightedIndex = ref(0);

const selected = computed(
  () => props.options.find((option) => option.value === props.modelValue) ?? props.options[0],
);
const activeOptionId = computed(() =>
  open.value ? `${id}-option-${highlightedIndex.value}` : undefined,
);

function showOptions(): void {
  if (props.disabled || props.options.length === 0) return;
  highlightedIndex.value = Math.max(
    0,
    props.options.findIndex((option) => option.value === props.modelValue),
  );
  open.value = true;
}

function closeOptions(focusTrigger = false): void {
  open.value = false;
  if (focusTrigger) void nextTick(() => trigger.value?.focus());
}

function choose(option: SelectOption): void {
  emit('update:modelValue', option.value);
  closeOptions(true);
}

function moveHighlight(delta: number): void {
  if (!open.value) showOptions();
  if (props.options.length === 0) return;
  highlightedIndex.value =
    (highlightedIndex.value + delta + props.options.length) % props.options.length;
}

function onKeydown(event: KeyboardEvent): void {
  switch (event.key) {
    case 'ArrowDown':
      event.preventDefault();
      moveHighlight(1);
      break;
    case 'ArrowUp':
      event.preventDefault();
      moveHighlight(-1);
      break;
    case 'Home':
      if (!open.value) return;
      event.preventDefault();
      highlightedIndex.value = 0;
      break;
    case 'End':
      if (!open.value) return;
      event.preventDefault();
      highlightedIndex.value = props.options.length - 1;
      break;
    case 'Enter':
    case ' ':
      event.preventDefault();
      if (!open.value) showOptions();
      else if (props.options[highlightedIndex.value]) choose(props.options[highlightedIndex.value]);
      break;
    case 'Escape':
      if (!open.value) return;
      event.preventDefault();
      closeOptions(true);
      break;
    case 'Tab':
      closeOptions();
      break;
  }
}

function onDocumentPointerDown(event: PointerEvent): void {
  if (!root.value?.contains(event.target as Node)) closeOptions();
}

onMounted(() => document.addEventListener('pointerdown', onDocumentPointerDown));
onBeforeUnmount(() => document.removeEventListener('pointerdown', onDocumentPointerDown));
</script>

<template>
  <div ref="root" class="base-select" :class="{ 'base-select--open': open }">
    <span v-if="label" :id="`${id}-label`" class="field-label">{{ label }}</span>
    <button
      ref="trigger"
      class="base-select__trigger"
      type="button"
      role="combobox"
      aria-haspopup="listbox"
      :aria-expanded="open"
      :aria-controls="`${id}-listbox`"
      :aria-activedescendant="activeOptionId"
      :aria-labelledby="label ? `${id}-label` : undefined"
      :aria-label="label ? undefined : ariaLabel"
      :disabled="disabled"
      @click="open ? closeOptions() : showOptions()"
      @keydown="onKeydown"
    >
      <span class="base-select__value">
        <AppIcon v-if="selected?.icon" :name="selected.icon" :size="16" />
        <span>{{ selectedLabel ?? selected?.label ?? $t('common.selectOption') }}</span>
      </span>
      <AppIcon class="base-select__chevron" name="chevronDown" :size="16" />
    </button>

    <Transition name="select-popover">
      <ul
        v-if="open"
        :id="`${id}-listbox`"
        class="base-select__menu"
        role="listbox"
        :aria-labelledby="label ? `${id}-label` : undefined"
      >
        <li
          v-for="(option, index) in options"
          :id="`${id}-option-${index}`"
          :key="option.value"
          class="base-select__option"
          :class="{
            'base-select__option--active': index === highlightedIndex,
            'base-select__option--action': option.action,
          }"
          role="option"
          :aria-selected="!option.action && option.value === modelValue"
          @mouseenter="highlightedIndex = index"
          @click="choose(option)"
        >
          <AppIcon v-if="option.icon" :name="option.icon" :size="16" />
          <span>
            <strong>{{ option.label }}</strong>
            <small v-if="option.description">{{ option.description }}</small>
          </span>
          <AppIcon
            v-if="!option.action && option.value === modelValue"
            class="base-select__check"
            name="check"
            :size="16"
          />
        </li>
      </ul>
    </Transition>
  </div>
</template>
