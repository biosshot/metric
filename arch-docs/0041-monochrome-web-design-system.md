# ADR-0041: Dark neutral Web design system with muted semantic accents

- Status: Accepted
- Date: 2026-07-24
- Amended: 2026-07-24

## Context

Metric positions itself as a smaller, calmer and operationally simpler alternative
to Sentry. The Web interface must express that product identity immediately rather
than visually imitating Sentry or growing into a colorful general-purpose dashboard
framework.

The existing MVP stylesheet predates this decision and contains a purple accent,
colored status surfaces, gradients and decorative shadows. It must be migrated before
new post-MVP product screens establish more incompatible visual patterns.

## Decision

Metric Web uses a dark neutral design system:

- black, white and neutral gray remain the foundation;
- muted semantic accents may identify error, warning, success, information and
  selected/active states;
- accent colors are deliberately desaturated and subordinate to investigation data;
- semantic state is never communicated by hue alone;
- typography, spacing, border weight, icon shape, fill pattern and explicit text carry
  hierarchy and meaning;
- data density is preferred over decorative chrome;
- the default and initially implemented theme is dark monochrome;
- an optional future light theme may only override the same tokens and must not create
  separate components, assets or semantics.

This decision applies to every existing and future Web screen, including Issues,
Errors, Logs, Traces, charts, Dashboards, Alerts, Profiles, Replays, settings and
system status.

## Palette

The initial token family is intentionally small. Semantic accents are centralized
alongside the neutral tokens rather than introduced as component-local literals:

```css
:root {
  --canvas: #0a0a0a;
  --surface: #101010;
  --surface-subtle: #171717;
  --surface-strong: #242424;

  --text: #f2f2f2;
  --text-muted: #a8a8a8;
  --text-faint: #787878;
  --text-inverse: #0a0a0a;

  --border: #303030;
  --border-strong: #777777;
  --accent: #ffffff;
  --focus: #ffffff;

  --semantic-error: #c88f93;
  --semantic-warning: #c6aa78;
  --semantic-success: #83ad91;
  --semantic-info: #82a8c2;

  --semantic-error-surface: #211719;
  --semantic-warning-surface: #211d15;
  --semantic-success-surface: #162019;
  --semantic-info-surface: #151d22;

  --syntax-comment: #82909a;
  --syntax-keyword: #b6a6d9;
  --syntax-number: #d0ad86;
  --syntax-string: #91b39a;
}
```

Components consume semantic tokens. Raw color literals outside the token definition
require an explicit visual-system change. Alpha values are permitted for functional
overlays, focus treatment and subtle semantic surfaces derived from accepted tokens.

Pure white is reserved for the strongest emphasis, selected state, primary action and
high-contrast data. Ordinary long-form text uses near-white to reduce visual fatigue.
Black and near-black surfaces remain visually distinct without decorative shadows.
The bounded syntax tokens are permitted only inside code, stack traces and raw
payload views; they are not a second general UI palette.

## Semantic states

Every state includes explicit text and a stable icon/shape. Color reinforces the
state but never replaces the label or shape:

| State | Visual treatment |
| --- | --- |
| new/error/failed | muted error accent, error icon, strong border, explicit label |
| warning/retry/degraded | muted warning accent, triangle icon, medium border, explicit label |
| resolved/success/healthy | muted success accent, check icon, outline treatment, explicit label |
| information/pending | muted information accent, information icon, explicit label |
| ignored/disabled/unknown | muted text, dashed border where useful, explicit label |
| selected/active | high-contrast fill or strong underline, optionally reinforced by an accent |

Severity is distinguishable in grayscale print and for users with any form of color
vision deficiency. A badge containing only a differently colored dot is prohibited.

## Typography and icons

- The interface typeface is JetBrains Mono Variable, self-hosted from
  `web/public/fonts/` (woff2 subsets, SIL OFL 1.1 with the license text beside
  the files). No remote Web font is downloaded at runtime.
- Font subsets are split by `unicode-range` (latin, latin-ext, Cyrillic,
  Cyrillic-ext, Greek, Vietnamese); the browser fetches only the subsets a page
  actually renders. Multilingual UI text, including Cyrillic, must render from
  the bundled subsets without a system-font fallback.
- Code, identifiers, stack traces and raw payloads use the same bundled
  monospace through `--font-mono`.
- Use font weight sparingly; hierarchy starts with spacing and size.
- Icons use named, tree-shaken imports from `@lucide/vue` with a consistent stroke.
- Icon fonts, remote icon services and runtime icon requests are prohibited.
- The icon dependency remains subject to the production asset-size gate; importing
  the whole icon catalog is prohibited.
- An icon without adjacent text has an accessible name or is explicitly decorative.

## Layout and components

- Prefer flat surfaces separated by one-pixel borders and whitespace.
- Avoid card-inside-card nesting when a section heading or divider is sufficient.
- Use a restrained radius scale; controls do not become decorative pills unless their
  semantics require a compact badge.
- Tables and timelines remain readable at investigation density.
- Primary actions use either white or the muted information accent with dark text.
  Secondary actions use a dark surface with a light border. Destructive actions use
  explicit wording and confirmation. A muted error accent may reinforce them, but
  must not be the only warning.
- Focus state uses a high-contrast outline and is never removed.
- Empty and loading states are concise and do not use decorative illustrations.

## Charts and technical visualization

Charts remain neutral-first and distinguish series using:

- solid, dashed and dotted strokes;
- line width;
- point shape;
- neutral-gray luminance;
- hatch/pattern fill;
- direct labels and a readable legend.

Muted semantic accents may reinforce a small number of series when the same data
remains distinguishable through stroke, point shape, pattern and labels.

Hover is supplemental. Exact values and series identity remain available to keyboard,
touch and assistive-technology users. A chart must not require color perception to
separate Error, Log, Span or environment series.

## Motion, effects and assets

- Gradients, glass effects and decorative background images are prohibited.
- Decorative shadows are prohibited; overlays may use the minimum shadow or backdrop
  required to communicate layering.
- Motion is limited to functional progress, disclosure and navigation feedback.
- `prefers-reduced-motion` disables non-essential animation and transition.
- No screen adds video, decorative raster art or a remote asset request to product
  chrome.

## Weight and runtime discipline

The restrained visual system is a product and implementation constraint, not only a
palette:

- no general-purpose UI framework is added for future product screens;
- no runtime theming library is added;
- design tokens and primitive components are centralized;
- existing Vue/API boundaries remain unchanged by visual work;
- each Web phase publishes built CSS/JS asset sizes and their delta from the previous
  accepted baseline;
- a visual dependency must justify its compressed bytes, runtime work and replacement
  cost;
- route-level code splitting is used when Profiles, Replay or other heavy screens
  require specialized viewers;
- large chart/replay/profile dependencies are not loaded on Issue/Error/Log routes.

The goal is not a misleading zero-byte claim. The goal is that visual consistency and
new capabilities do not require an accumulating theme/component framework.

## Accessibility and responsive behavior

- Text and controls meet accepted WCAG contrast requirements.
- State and validation are represented by text in addition to icon/shape.
- All actions are keyboard reachable with visible focus.
- Dense tables have a bounded narrow-screen alternative rather than uncontrolled
  horizontal clipping.
- Zoom and browser font-size changes do not hide investigation actions.
- Automated accessibility checks are supplemented by keyboard and grayscale/print
  review of critical flows.

## Migration of the MVP Web

Phase 23 is the dedicated migration of the complete existing MVP Web:

1. replace the current purple/status variables with the accepted neutral and muted
   semantic tokens;
2. remove gradients and decorative shadows;
3. update buttons, links, badges, alerts, spinner, sidebar and authentication view;
4. make status components use label plus shape/icon rather than hue;
5. convert charts/timelines to grayscale-safe styles with optional muted reinforcement;
6. remove duplicated raw color literals from components;
7. capture reference renders for every existing route at desktop and narrow width;
8. run accessibility, keyboard, reduced-motion and grayscale checks;
9. record CSS/JS production asset sizes before and after migration.

The migration changes presentation only. Native DTOs, permissions, API routes and
application behavior do not change.

## Gate for every future Web phase

A phase with Web changes does not close until:

- it uses only accepted tokens and primitives;
- no product meaning depends on color;
- existing critical routes show no visual regression;
- desktop and narrow-width renders are reviewed;
- keyboard focus/order and accessible names pass;
- asset-size changes are recorded and justified;
- heavy route-specific code is absent from unrelated initial bundles.

## Consequences

- Metric gains a distinct, recognizable visual identity aligned with its small and
  stable positioning.
- Error and success states use restrained accents together with labels and icons,
  avoiding conventional saturated red and green shortcuts.
- Charts need explicit grayscale series design.
- A future light theme remains possible without changing semantic components.
- Named Lucide imports provide a coherent local icon language while making their
  bundle cost visible and bounded.
- The current MVP stylesheet requires a deliberate migration before additional
  product UI expands it.
