<script setup lang="ts">
import { computed, ref } from 'vue';
import AppIcon from './AppIcon.vue';
import { tokenizeSyntaxLine, type SyntaxToken } from './syntaxHighlight';

interface Frame {
  filename?: string;
  abs_path?: string;
  function?: string;
  module?: string;
  lineno?: number;
  colno?: number;
  context_line?: string;
  pre_context?: string[];
  post_context?: string[];
  in_app?: boolean;
}

interface SourceLine {
  content: string;
  lineNumber?: number;
  current: boolean;
  tokens: SyntaxToken[];
}

const props = defineProps<{ body: Record<string, unknown> }>();
const expanded = ref(false);
const INITIAL_FRAMES = 40;

const frames = computed<Frame[]>(() => {
  const exception = props.body.exception as { values?: unknown[] } | undefined;
  const values = Array.isArray(exception?.values) ? exception.values : [];
  const exceptionFrames = values.flatMap((value) => {
    const item = value as { stacktrace?: { frames?: unknown[] } };
    return Array.isArray(item.stacktrace?.frames) ? (item.stacktrace.frames as Frame[]) : [];
  });
  if (exceptionFrames.length) return exceptionFrames.toReversed();
  const stacktrace = props.body.stacktrace as { frames?: unknown[] } | undefined;
  return Array.isArray(stacktrace?.frames) ? (stacktrace.frames as Frame[]).toReversed() : [];
});

const visibleFrames = computed(() =>
  expanded.value ? frames.value : frames.value.slice(0, INITIAL_FRAMES),
);

function sourceLines(frame: Frame): SourceLine[] {
  const before = Array.isArray(frame.pre_context) ? frame.pre_context : [];
  const after = Array.isArray(frame.post_context) ? frame.post_context : [];
  const content = [...before, ...(frame.context_line ? [frame.context_line] : []), ...after];
  return content.map((line, index) => ({
    content: line,
    lineNumber: frame.lineno ? frame.lineno - before.length + index : undefined,
    current: Boolean(frame.context_line) && index === before.length,
    tokens: tokenizeSyntaxLine(line),
  }));
}
</script>

<template>
  <section class="stack-section" aria-labelledby="stack-heading">
    <div class="section-heading">
      <div>
        <p class="eyebrow">Stack trace</p>
        <h2 id="stack-heading">{{ frames.length }} frames</h2>
      </div>
      <button
        v-if="frames.length > INITIAL_FRAMES"
        class="button button--secondary"
        type="button"
        :aria-expanded="expanded"
        @click="expanded = !expanded"
      >
        <AppIcon name="view" :size="16" />
        {{ expanded ? 'Show first 40' : `Show all ${frames.length}` }}
      </button>
    </div>
    <div v-if="frames.length" class="stack-list">
      <article
        v-for="(frame, index) in visibleFrames"
        :key="`${frame.filename}-${frame.lineno}-${index}`"
        class="stack-frame"
        :class="{ 'stack-frame--app': frame.in_app }"
      >
        <div class="stack-frame__number">{{ index + 1 }}</div>
        <div>
          <strong>{{ frame.function || '&lt;unknown function&gt;' }}</strong>
          <p>
            {{ frame.filename || frame.abs_path || frame.module || 'unknown source' }}
            <span v-if="frame.lineno"
              >:{{ frame.lineno }}<template v-if="frame.colno">:{{ frame.colno }}</template></span
            >
          </p>
          <ol
            v-if="sourceLines(frame).length"
            class="source-context"
            tabindex="0"
            :aria-label="`Source context for ${frame.filename || frame.abs_path || 'frame'}`"
          >
            <li
              v-for="(line, lineIndex) in sourceLines(frame)"
              :key="`${line.lineNumber}-${lineIndex}`"
              :class="{ 'source-context__current': line.current }"
            >
              <span class="source-context__number" aria-hidden="true">{{
                line.lineNumber ?? '·'
              }}</span>
              <code
                ><span
                  v-for="(token, tokenIndex) in line.tokens"
                  :key="tokenIndex"
                  :class="`token--${token.type}`"
                  >{{ token.text }}</span
                ><template v-if="!line.content"> </template
              ></code>
            </li>
          </ol>
        </div>
        <span v-if="frame.in_app" class="frame-label">in app</span>
      </article>
    </div>
    <p v-else class="muted">This event does not contain stack frames.</p>
  </section>
</template>
