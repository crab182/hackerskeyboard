# web/ — diyRAG browser GUI

One **React 18 + TypeScript + Vite + Tailwind + shadcn/ui** codebase that serves browsers (Chrome + Firefox) and is wrapped by Tauri 2 for the native client (`../native`). Presentation is fully decoupled from business logic via the REST/WS API (spec §11); adding a client type never touches backend services.

## Stack
- React 18 + TypeScript (strict), Vite, Tailwind, shadcn/ui design tokens.
- Dark mode default, light available. **WCAG 2.1 AA**: full keyboard nav, visible focus rings, sufficient contrast (audit dark mode), correct ARIA roles, reduced-motion support.
- Data: typed client generated from the gateway's OpenAPI; realtime over **WebSocket (WSS)** with backoff + SSE/poll fallback.

## Screens (spec §10.2)
1. **Dashboard** — corpus size, ingestion throughput, queue depth, error rate, node/sync status, GPU utilization, runtime/service status.
2. **Search & Answer** — query box; search-only vs grounded-answer toggle; result cards (score/source/page); citation chips opening the source chunk; "show conflicts" panel.
3. **Library / Files** — browse documents/roots; status badges; add/remove roots & files; reingest; per-doc detail with chunks + provenance.
4. **Batch / Jobs** — submit archives; live progress; per-unit drill-down; retry/requeue.
5. **Errors / Debug** — searchable `error_log`; deep-linkable by `reference_code`; correlation-id trace view; quarantine queue + re-inject.
6. **Admin** — users, API keys, RBAC matrix, peer/node management + cert pinning, model/config, snapshot/backup, **service control (install/start/stop on Windows)**; step-up auth for privileged actions.
7. **Settings** — theme, chunking/retrieval params (per collection), help-bubble delay, language.

## Help-bubble system (spec §10.3)
- `<HelpAnchor id="module.element.param">` used on every interactive control, header, configurable field, and technical term; hover + focus + `?` affordance trigger it; configurable delay (default 600 ms, up to 2 s); dismiss on blur/leave/Esc.
- Content comes from a **decoupled, versioned store** (`help/*.json` keyed by `module.element.param`), never hardcoded; a missing key shows a visible dev warning so coverage stays complete.
- `<Term>` auto-attaches glossary definitions from the same store.

## Error visualization (spec §10.4)
API errors use the standard envelope (§11.3). Render two sections: a plain-language **user explanation**, and a clickable **reference code** that navigates to `Errors/Debug` pre-filtered to that `error_log` row. Non-admins never see raw stack traces; admins get technical detail + correlation trace.

## Dev
```bash
pnpm install
pnpm dev        # browser dev server
pnpm build      # emits dist/ consumed by ../native (Tauri) and Caddy
pnpm test       # vitest
pnpm e2e        # Playwright (Chrome + Firefox)
```
> Scaffolding placeholder — implemented in phase **M5** (spec §20).
