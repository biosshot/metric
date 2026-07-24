<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue';
import AppIcon from './AppIcon.vue';

type TokenType = 'comment' | 'keyword' | 'number' | 'plain' | 'string';
interface Token {
  text: string;
  type: TokenType;
}

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
const keywords = new Set([
  'const',
  'let',
  'var',
  'import',
  'from',
  'new',
  'true',
  'false',
  'null',
  'class',
  'public',
  'static',
  'void',
  'string',
  'using',
  'def',
  'None',
  'package',
  'func',
  'return',
  'use',
  'mut',
]);

function tokenize(line: string): Token[] {
  const tokens: Token[] = [];
  const pattern =
    /("(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|\/\/.*$|#.*$|\b\d+(?:\.\d+)?\b|\b[A-Za-z_][A-Za-z0-9_]*\b)/g;
  let cursor = 0;

  for (const match of line.matchAll(pattern)) {
    const index = match.index ?? 0;
    if (index > cursor) tokens.push({ text: line.slice(cursor, index), type: 'plain' });
    const text = match[0];
    let type: TokenType = 'plain';
    if (text.startsWith('//') || text.startsWith('#')) type = 'comment';
    else if (text.startsWith('"') || text.startsWith("'")) type = 'string';
    else if (/^\d/.test(text)) type = 'number';
    else if (keywords.has(text)) type = 'keyword';
    tokens.push({ text, type });
    cursor = index + text.length;
  }

  if (cursor < line.length) tokens.push({ text: line.slice(cursor), type: 'plain' });
  return tokens;
}

const lines = computed(() => props.code.split('\n').map(tokenize));

async function copyCode(): Promise<void> {
  copied.value = false;
  copyFailed.value = false;
  clearTimeout(copyTimer);
  try {
    await navigator.clipboard.writeText(props.code);
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
  <section class="code-block" :aria-label="`${language} code example`">
    <header class="code-block__header">
      <span>
        <AppIcon name="code" :size="15" />
        {{ title ?? language }}
      </span>
      <button class="code-block__copy" type="button" @click="copyCode">
        <AppIcon :name="copyFailed ? 'alert' : copied ? 'check' : 'copy'" :size="15" />
        {{ copyFailed ? 'Copy failed' : copied ? 'Copied' : 'Copy' }}
      </button>
    </header>
    <ol class="code-block__lines" tabindex="0" aria-label="Scrollable code lines">
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
