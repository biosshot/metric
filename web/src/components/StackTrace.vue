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
  instruction_addr?: string;
}

interface DerivedTrace {
  origin: { kind: string; index?: number };
  frames: Array<{
    original_index: number;
    function?: string;
    filename?: string;
    module?: string;
    line?: number;
    column?: number;
    context_line?: string;
    pre_context?: string[];
    post_context?: string[];
  }>;
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
const showOriginal = ref(false);
const symbolication = computed(
  () =>
    (
      props.body._metric as
        | {
            symbolication?: { status?: string; derived?: DerivedTrace[] };
          }
        | undefined
    )?.symbolication,
);
const hasSymbols = computed(
  () =>
    ['complete', 'partial'].includes(symbolication.value?.status ?? '') &&
    symbolication.value?.derived?.some((trace) => trace.frames.length),
);

function resolvedFrames(raw: Frame[], kind: string, index?: number): Frame[] {
  if (showOriginal.value || !hasSymbols.value) return raw;
  const trace = symbolication.value?.derived?.find(
    (value) => value.origin.kind === kind && value.origin.index === index,
  );
  return raw.flatMap((frame, originalIndex) => {
    const derived = trace?.frames.filter((value) => value.original_index === originalIndex);
    if (!derived?.length) return [frame];
    return derived.map((value) => ({
      ...frame,
      function: value.function ?? frame.function,
      filename: value.filename ?? frame.filename,
      module: value.module ?? frame.module,
      lineno: value.line,
      colno: value.column,
      context_line: value.context_line,
      pre_context: value.pre_context,
      post_context: value.post_context,
    }));
  });
}

const frames = computed<Frame[]>(() => {
  const exception = props.body.exception as { values?: unknown[] } | undefined;
  const values = Array.isArray(exception?.values) ? exception.values : [];
  const exceptionFrames = values.flatMap((value, index) => {
    const item = value as {
      stacktrace?: { frames?: Frame[] };
      raw_stacktrace?: { frames?: Frame[] };
    };
    if (Array.isArray(item.stacktrace?.frames) && item.stacktrace.frames.length) {
      return resolvedFrames(item.stacktrace.frames, 'exception', index);
    }
    return Array.isArray(item.raw_stacktrace?.frames)
      ? resolvedFrames(item.raw_stacktrace.frames, 'exception_raw', index)
      : [];
  });
  if (exceptionFrames.length) return exceptionFrames.toReversed();
  const stacktrace = props.body.stacktrace as { frames?: unknown[] } | undefined;
  if (Array.isArray(stacktrace?.frames) && stacktrace.frames.length) {
    return resolvedFrames(stacktrace.frames as Frame[], 'event').toReversed();
  }
  // The thread the server groups by (`normalize_thread_stacktrace`): crashed, else
  // current, else the only one. Threads stay as the SDK sent them, so every value is
  // checked here; `threads` and its `stacktrace` may also be bare arrays.
  const threads = props.body.threads;
  const threadList = isRecord(threads) ? threads.values : threads;
  const threadValues = (Array.isArray(threadList) ? threadList : []).filter(isRecord);
  const selectedThread =
    threadValues.find((thread) => thread.crashed === true) ??
    threadValues.find((thread) => thread.current === true) ??
    (threadValues.length === 1 ? threadValues[0] : undefined);
  const threadStack = selectedThread?.stacktrace;
  const threadFrames = isRecord(threadStack) ? threadStack.frames : threadStack;
  return Array.isArray(threadFrames)
    ? threadFrames.filter(isRecord).map(threadFrame).toReversed()
    : [];
});

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

// A thread frame gives the view only the fields of the type it expects.
function threadFrame(frame: Record<string, unknown>): Frame {
  const text = (value: unknown) => (typeof value === 'string' ? value : undefined);
  const numeric = (value: unknown) => (typeof value === 'number' ? value : undefined);
  const lines = (value: unknown) =>
    Array.isArray(value) ? value.map((line: unknown) => text(line) ?? '') : undefined;
  return {
    filename: text(frame.filename),
    abs_path: text(frame.abs_path),
    function: text(frame.function),
    module: text(frame.module),
    lineno: numeric(frame.lineno),
    colno: numeric(frame.colno),
    context_line: text(frame.context_line),
    pre_context: lines(frame.pre_context),
    post_context: lines(frame.post_context),
    in_app: frame.in_app === true,
    instruction_addr: text(frame.instruction_addr),
  };
}

const visibleFrames = computed(() =>
  expanded.value ? frames.value : frames.value.slice(0, INITIAL_FRAMES),
);

function sourceLines(frame: Frame): SourceLine[] {
  const before = Array.isArray(frame.pre_context) ? frame.pre_context : [];
  const after = Array.isArray(frame.post_context) ? frame.post_context : [];
  const currentLine = frame.context_line;
  const hasCurrentLine = typeof currentLine === 'string';
  const content = [...before, ...(hasCurrentLine ? [currentLine] : []), ...after];
  return content.map((line, index) => ({
    content: line,
    lineNumber: frame.lineno ? frame.lineno - before.length + index : undefined,
    current: hasCurrentLine && index === before.length,
    tokens: tokenizeSyntaxLine(line),
  }));
}
</script>

<template>
  <section class="stack-section" aria-labelledby="stack-heading">
    <div class="section-heading">
      <div>
        <p class="eyebrow">{{ $t('stack.title') }}</p>
        <h2 id="stack-heading">{{ $t('stack.frames', frames.length) }}</h2>
        <p v-if="hasSymbols" class="muted">
          {{ $t(symbolication?.status === 'partial' ? 'stack.partial' : 'stack.symbolicated') }}
        </p>
      </div>
      <button
        v-if="hasSymbols"
        class="button button--secondary"
        type="button"
        @click="showOriginal = !showOriginal"
      >
        {{ $t(showOriginal ? 'stack.showSymbolicated' : 'stack.showOriginal') }}
      </button>
      <button
        v-if="frames.length > INITIAL_FRAMES"
        class="button button--secondary"
        type="button"
        :aria-expanded="expanded"
        @click="expanded = !expanded"
      >
        <AppIcon name="view" :size="16" />
        {{ expanded ? $t('stack.showFirst') : $t('stack.showAll', { count: frames.length }) }}
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
          <strong>{{ frame.function || $t('stack.unknownFunction') }}</strong>
          <p>
            {{
              frame.filename ||
              frame.abs_path ||
              frame.module ||
              frame.instruction_addr ||
              $t('stack.unknownSource')
            }}
            <span v-if="frame.lineno"
              >:{{ frame.lineno }}<template v-if="frame.colno">:{{ frame.colno }}</template></span
            >
          </p>
          <ol
            v-if="sourceLines(frame).length"
            class="source-context"
            tabindex="0"
            :aria-label="
              $t('stack.sourceContext', {
                source: frame.filename || frame.abs_path || $t('stack.frame'),
              })
            "
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
        <span v-if="frame.in_app" class="frame-label">{{ $t('stack.inApp') }}</span>
      </article>
    </div>
    <p v-else class="muted">{{ $t('stack.empty') }}</p>
  </section>
</template>
