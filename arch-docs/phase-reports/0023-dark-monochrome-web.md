# Phase 23 report: dark monochrome Web redesign

Status: complete
Date: 2026-07-24
Contracts: `arch-docs/0040-post-mvp-vertical-product-plan.md`,
`arch-docs/0041-monochrome-web-design-system.md`

## Implemented

- Replaced the former colored visual layer with the ADR-0041 neutral token palette.
  Production CSS contains no colored status palette, gradient or decorative shadow.
- Migrated authentication, navigation, Issues, Issue detail, Event detail, SDK setup,
  project settings, first-project onboarding and system status to the shared responsive
  presentation layer.
- Added a curated, tree-shaken `@lucide/vue` icon surface and a central `AppIcon`
  component. Status remains understandable through text, icon/shape, border and
  luminance without color-only meaning.
- Added a fully custom `BaseSelect` listbox/combobox used by project switching, Issue
  filtering, IP policy selection and SDK language selection. It supports pointer,
  Arrow Up/Down, Home/End, Enter/Space, Escape and Tab interaction with visible focus.
- Added a reusable `CodeBlock` with local syntax tokenization, exact clipboard copy,
  visible success/failure state and a keyboard-focusable horizontal scroll region.
- SDK setup now selects the first active project key, constructs the real DSN and
  inserts it into Browser JavaScript, Node.js, Python, Java and C#/.NET examples.
- Stack traces now render `pre_context`, `context_line` and `post_context`, calculate
  source line numbers, highlight the active line and preserve bounded initial frame
  rendering.
- Reworked project deletion confirmation into a spaced, bounded confirmation panel
  with a readable prompt and explicit destructive action.
- Added subtle bounded transitions, a mobile navigation drawer, narrow layouts,
  reduced-motion handling, visible focus and focusable overflow regions.

No DTO, API route, permission, storage or backend behavior changed.

## Reference renders

The explicit Chromium capture gate writes 20 retained reference images to
`arch-docs/phase-reports/assets/0023`:

- authentication, Issues, Issue detail, Event detail, SDK setup, project settings and
  system status at 1440x1000 and 390x844;
- focused references for the open custom SDK selector and the project deletion panel;
- print-media references for the Issues and Event detail routes.

The captured palette is natively grayscale. Labels, Lucide icons, geometry, borders
and luminance preserve statuses and actions without chroma.

## Built asset delta

The baseline was measured before Phase 23 from the existing production bundle. The
final values are exact bytes; gzip uses Node zlib over the built assets.

| Asset | Before raw | After raw | Raw delta | Before gzip | After gzip | Gzip delta |
|---|---:|---:|---:|---:|---:|---:|
| CSS | 18,725 | 25,091 | +6,366 (+34.00%) | 4,691 | 5,426 | +735 (+15.67%) |
| JS | 180,054 | 203,826 | +23,772 (+13.20%) | 61,473 | 70,512 | +9,039 (+14.70%) |

The JS increase includes the curated tree-shaken icon set, custom select behavior,
syntax tokenization and the expanded SDK/stack-trace presentation. No remote asset or
runtime highlighter was added.

## Verification

- `npm run format` and `npm run format:check`: pass.
- `npm run lint`: pass with zero warnings.
- `npm test`: 13 tests pass across six files, including keyboard select, exact code
  copy and complete stack source context.
- `npm run build`: TypeScript and Vite production build pass.
- `npm run test:e2e`: 12 behavior/accessibility tests pass in Chromium and Firefox;
  two explicit reference-capture cases are skipped unless enabled.
- The explicit Chromium reference-capture case passes and leaves no Vite listener.
- Axe reports no serious or critical violation on every existing route at desktop and
  narrow widths in Chromium and Firefox, including color contrast.
- A production-source scan finds no native `select`/`option`, prohibited color name,
  gradient or `box-shadow`.

## Exit gate

| Phase 23 row | Evidence | Status |
|---|---|---|
| All existing routes pass dark monochrome review | 14 route renders plus selector, deletion and print references were generated and inspected | Pass |
| No colored/gradient/color-only state | Neutral-only CSS scan; status labels use icon/shape/border/luminance | Pass |
| Keyboard, focus, contrast and reduced motion | Custom-select unit gate, visible global focus, all-route Axe gate, reduced-motion media rule | Pass |
| Grayscale screenshot/print semantics | Native grayscale screen references and two print-media references retain explicit text/icons | Pass |
| Production asset delta published | Exact raw/gzip table above | Pass |
| Existing Web behavior unchanged | Final Chromium/Firefox E2E suite passes | Pass |

Phase 23 is complete. The next sequential phase is Phase 24, structured Logs end to
end; it has not been started.

## Post-gate presentation exception

On 2026-07-24 the product owner explicitly approved a narrowly scoped chromatic
exception for syntax-highlighted code. Keywords, strings, numbers and comments use
muted colors only inside code/source presentation; navigation, statuses, actions and
all other product semantics remain monochrome and never depend on color alone.
