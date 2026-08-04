<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue';
import AppIcon from './AppIcon.vue';
import { tokenizeSyntaxLine } from './syntaxHighlight';
import { copyText } from '../lib/clipboard';

const props = withDefaults(
  defineProps<{
    code: string;
    language: string;
    title?: string;
  }>(),
  { title: undefined },
);

const copied = ref(false);
const copyFailed = ref(false);
let copyTimer: ReturnType<typeof setTimeout> | undefined;
const lines = computed(() => props.code.split('\n').map(tokenizeSyntaxLine));

async function copyCode(): Promise<void> {
  copied.value = false;
  copyFailed.value = false;
  clearTimeout(copyTimer);
  try {
    await copyText(props.code);
    copied.value = true;
    copyTimer = setTimeout(() => {
      copied.value = false;
    }, 1800);
  } catch {
    copyFailed.value = true;
  }
}

onBeforeUnmount(() => clearTimeout(copyTimer));
</script>

<template>
  <section class="code-block" :aria-label="$t('common.codeExample', { language })">
    <header class="code-block__header">
      <span>
        <AppIcon name="code" :size="15" />
        {{ title ?? language }}
      </span>
      <button class="code-block__copy" type="button" @click="copyCode">
        <AppIcon :name="copyFailed ? 'alert' : copied ? 'check' : 'copy'" :size="15" />
        {{
          copyFailed ? $t('common.copyFailed') : copied ? $t('common.copied') : $t('common.copy')
        }}
      </button>
    </header>
    <ol class="code-block__lines" tabindex="0" :aria-label="$t('common.scrollableCode')">
      <li v-for="(line, lineIndex) in lines" :key="lineIndex">
        <code
          ><span
            v-for="(token, tokenIndex) in line"
            :key="tokenIndex"
            :class="`token--${token.type}`"
            >{{ token.text }}</span
          ></code
        >
      </li>
    </ol>
  </section>
</template>
