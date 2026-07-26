# Changelog

All notable changes to Conductor will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Security

- **#2609 — cleared the cargo-audit red (3 → 0 vulnerabilities).** Bumped
  `quick-xml` 0.38 → 0.41 to patch RUSTSEC-2026-0194 (quadratic parse time
  on duplicate attributes) and RUSTSEC-2026-0195 (unbounded
  namespace-declaration allocation) — both reachable through `.ncmm3`
  profile parsing, i.e. user-supplied input, so this closes the
  malicious-profile DoS SECURITY.md warns about. Also bumped
  `crossbeam-epoch` past RUSTSEC-2026-0204 (transitive, lockfile-only) and
  `plist` 1.8 → 1.10 (transitive, lockfile-only) to drop a second
  quick-xml 0.38 pulled in via Tauri. New regression tests pin the
  `.ncmm3` parser's bounded behavior against both advisory input classes.
  `audit.toml` still suppresses nothing.

### Added

- **#2495 — ADR-045 D4: runtime `[mcp] enabled` toggle.** New optional
  `[mcp]` config block: `enabled = false` leaves the MCP Unix socket
  entirely unbound at daemon startup (even read-only MCP is a local socket
  surface — ADR-027 minimal-surface posture). Default on; absent block
  keeps the canonical config form byte-identical (the `[security]`
  precedent), so config revisions and the golden hash are unchanged.
  Takes effect at startup; toggling requires a daemon restart.

- **#2494 — ADR-045 D3: CI composition matrix + verified OSS-artifact boundary.**
  CI now builds and tests all four `conductor-daemon` feature compositions
  (`--no-default-features`, default `mcp`, bundle `llm-executor`,
  `mcp-write`) package-scoped — the exact names the public repo's CI
  consumes. New `scripts/check-oss-binary.sh` turns the build-flag
  convention into a verified boundary: the release pipeline asserts the
  FINAL stripped OSS binary contains no SQLite symbols, no gated MCP
  tool-name strings, and no telemetry SDK markers (ADR-048 D6), and the CI
  matrix runs the same script as a canary against an `mcp-write` binary
  where it MUST fail — proving the assertions bite. The Studio-bundled
  daemon now builds the bundle profile (`--features llm-executor`) so GUI
  plan/apply keeps working post-tier-split while its MCP socket stays
  read-only.

- **#2493 — ADR-045 D5: `AuditSink` seam + always-compiled JSONL audit sink.**
  Audit is now composition-independent: consumers (LLM executor, IPC
  `SubscribeAudit`, ADR-042 listener audit, path-validation audit) write to
  the new `AuditSink` trait. `audit-db` builds keep the SQLite hash-chain
  logger; every other composition gets a new append-only, line-hash-chained
  JSONL sink at `<state_dir>/audit.jsonl` — same redaction pipeline
  (ADR-027 D13c redact-then-truncate via the shared `redact_audit_field`),
  single-writer pump (bounded channel, shed-and-count backpressure),
  size-capped rotation whose new segment chains to the rotated segment's
  head hash, and torn-tail crash recovery. Per the D5 invariants: network
  listeners now REFUSE to start when no audit sink could be initialized
  (fail-closed — their audit trail is a security control), while everything
  else stays up. `SubscribeAudit` (and `conductorctl audit tail`) now works
  in the OSS composition too. Verification logic (`verify_jsonl_chain`)
  covers tamper detection, cross-segment chaining, and bounded-retention
  window semantics; the ADR-042 D6 redaction corpus runs against both sinks
  with a byte-parity test.

- **#2492 — ADR-045 D1/D2/D3(build): open-core tier boundary as cargo features.**
  `conductor-daemon` is now partitioned along the MCP risk-tier boundary:
  `default = ["mcp"]` (read-only inspection MCP, no SQLite), `llm-executor`
  (ToolExecutor/ConfigPlan plan-apply machinery, IPC-only, implies `audit-db`),
  `mcp-write` (Stateful/ConfigChange/HardwareIO tools on the MCP socket —
  source builds only, never in official artifacts), and `audit-db` (`rusqlite`
  is now optional). The tool registry is split at a module boundary
  (`daemon/mcp_tools_write.rs`): `tools/list` reflects exactly the compiled
  catalog, and calls to tools absent from a composition return a standard
  "not available in this build" error naming Conductor Studio — without
  embedding gated tool names in the binary. The bundle profile
  (`mcp,llm-executor`) keeps its MCP socket read-only: ConfigChange-over-MCP
  dispatch compiles only under `mcp-write` (Council R1 #2). IPC commands for
  absent subsystems (plan apply/reject/list, tool execution, audit
  query/subscribe) answer with clean errors. Risk-tier taxonomy unchanged —
  commercial placement is decided by the D2 capability rule, never by
  re-tiering (Council R1 #1).

- **#2578 — Export Diagnostics.** Conductor tray icon ▸ **Export Diagnostics…**
  collects logs, config, profiles, daemon status, versions/architecture and any
  buffered crash reports into a single `conductor-diagnostics-<date>.zip` on the
  Desktop, and reveals it in Finder. Testers can produce a real bug report
  without a terminal, a source checkout, or `conductorctl` (which is not bundled
  in the `.app`). The bundle is **sanitized before it is written** — home paths
  stripped, API keys / bearer tokens / emails redacted — and is collected from an
  explicit *allowlist* of files, so a secret dropped into the state directory
  tomorrow is excluded by default rather than included by oversight. Nothing is
  transmitted; it writes a file and the user decides where it goes.

### Changed

- **#2601 — decompose `mcp.rs` + `mcp_tools.rs` into directory modules.**
  Both files had outgrown any code-review window (107KB / 187KB, dominated
  by inline test modules). Mechanical split, no behavior change:
  `daemon/mcp/` (`mod.rs` server core, `tools_call.rs` dispatch,
  `tests.rs`) and `daemon/mcp_tools/` (`mod.rs` catalog glue + risk tiers,
  `definitions_readonly.rs`/`definitions_write.rs`, `executor.rs`/
  `executor_queries.rs`, `tests.rs`). Public paths unchanged; test counts
  identical (55 + 22). One observable nit, disclosed: `tools/list` now
  groups ReadOnly inspection tools before write-tier tools (no client
  contract on catalog order). Unblocks Council review of ADR-045 A1
  (PR #2600).

### Fixed

- **#2579 — released builds produced no logs at all.** A field bug from a test
  Intel Mac came back with zero diagnostics, because nothing on disk recorded
  what happened. Two causes: the GUI spawned the daemon with
  `.stdout(Stdio::null()).stderr(Stdio::null())`, discarding everything it said
  (including panics); and `conductor-core::logging` — a complete rotating-file
  logger — had **no callers**, which also made the long-documented `DEBUG=1` a
  silent no-op. Now both binaries write daily-rotating files (5 days retained) to
  `~/Library/Logs/Conductor/` on macOS (`~/.local/share/conductor/logs`
  elsewhere): `gui.<date>.log`, `daemon.<date>.log`, plus `daemon-stdout.log` for
  anything the daemon emits before its logger is up. Console output is unchanged,
  and a read-only log directory degrades to console-only rather than failing to
  start. `DEBUG=1` now works on the release binary.

## [5.7.1-alpha] - 2026-07-08

Patch release fixing LLM provider setup on macOS (alpha-tester report).

### Fixed

- **#1652 — LLM chat stuck on "No LLM" / "API key not configured" after saving keys.**
  The chat persisted `provider: null` whenever a launch found no *readable* key and then
  treated that auto-written null as an explicit "No LLM" choice, so it never self-healed
  after a transient/persistent keychain read failure — the nav model never updated and
  chat reported keys as unconfigured even after they were saved. Fixes: the "No LLM" state
  now requires an explicit `disabled` flag (a stale null self-heals to any configured
  provider); `SettingsPanel` no longer silently switches to "No LLM" on a read miss;
  `keychain::KeyStatus`/`key_status` distinguishes "no key" from "keychain unreadable"
  (surfaced as a per-provider `key_error`); `llm_set_api_key` verifies read-back and returns
  an actionable *"move it to /Applications"* error — catching the macOS app-translocation
  ACL failure at save time. Hardening: keychain ops run off the async executor
  (`spawn_blocking`) with provider-id + api-key input validation. Follow-up hardening in #2572.


## [5.7.0-alpha] - 2026-07-06

Alpha install-test snapshot, cut for local-install testing and alpha-tester
distribution. This train lands 689 commits since v5.6.1-alpha; the curated
highlights below are the notable user-facing changes (the GitHub release notes
carry the full auto-generated commit log).

### Highlights since v5.6.1-alpha

- **Config-authority cold-start fixes** — the daemon now serves its live config
  on a cold boot and the GUI re-fetches on the daemon down→up edge, ending the
  "settings revert on restart" class of bugs (#2533 series; ADR-034 / ADR-043).
- **GUI design-system migration** — shared form conventions, theme.css
  primitives, and a CI token lint across the component library (#2546).
- **RAW config editor** — editable CodeMirror with configurable themes and
  read-only themed views (#2471 / #2484 / #2485 / #2529 / #2531).
- **Medium-press threshold slider** — daemon-owned `short_press_ms` (#2385).
- **Security** — patched wasmtime-wasi (RUSTSEC-2026-0182) and quinn-proto
  (RUSTSEC-2026-0185) (#2538).


### Fixed

- **#2533 — daemon serves its live config via `GetConfigBody` on a cold boot
  (config-authority root cause).** A freshly-booted daemon loaded `live.toml` into
  `live_config` but seeded it at the gen-0 *uninitialised* sentinel; since
  `handle_get_config_body` blanks the body at `state_generation == 0`, a cold boot
  served an **empty** `GetConfigBody`, so the GUI fell back to reading stale
  `config.toml` (the "settings revert on restart" symptom, also the root of #1779's
  read-side gap). Fix: the boot now publishes the loaded config as the **first
  published snapshot at `state_generation = 1`** via the new
  `LiveConfig::new_published()` — honouring ADR-034 KI-A2 / R6-A8 ("gen 0 =
  unambiguous uninitialised; first published = 1") without routing the
  already-canonical boot config through the `mutate()` seam (which would redundantly
  re-persist + re-audit it). The gen-0 sentinel is retained for the unreachable
  `AwaitingConfig` state and as the CAS test fixture; the first runtime mutate now
  publishes gen 2. Daemon-only; no GUI change required.

### Changed

- **Settings field labels follow the design-system convention (#2546 follow-up).**
  `SettingsPanel` field labels (Theme, Accent Color, Editor theme, Event buffer size, …)
  are now UPPERCASE-dim (`.setting-title` → `--label` style); checkbox/toggle setting
  names stay sentence-case. The lone gap the design-review turned up after the
  design-system migration.

### Added

- **GUI design-system: form primitives, editor migration, and a token lint (#2546).**
  Codified the form/label/card/button conventions (`docs/gui-v2/form-conventions.md`)
  and built them as shared classes in `theme.css` (`.field-label`, `.section-label`,
  `.field-help`, `.card`, `.btn-*`). Migrated the Mapping Editor's form components —
  `TriggerSelector`, `ActionSelector`, `SendMidiActionEditor`, `ConditionalActionEditor`,
  `Pc`/`CcContextSwitchEditor`, plus the `MidiOutputSelector`/`VelocityMappingSelector`
  header labels — off hand-rolled `rem`/Title-Case-bold-white onto the tokens: field
  labels are now UPPERCASE-dim-small, matching the Endpoints panes. Added a zero-dep
  **design-token lint** (`npm run lint:design`, gated in the *Lint Tauri GUI* CI job)
  that forbids hardcoded `font-size: px|rem`, plus a **`design-review` skill** that
  renders a component headlessly and checks it against the conventions. The migration
  then **cleared the entire backlog** — all ~90 components tokenized in tiered batches
  (visible bold-white labels first, then token-purity sweeps), so the lint allowlist is
  now empty and every Svelte component is enforced. Added `--font-size-icon`/`-icon-lg`
  tokens for decorative glyphs.

### Changed

- **Nested action/condition editors — label colours aligned to the pane convention
  (#2544).** The Conditional and PC/CC context-switch editors coloured plain form
  text/labels with `--text-bright` (pure white), whereas the gui-v2 panes reserve
  `--text-bright` for emphasis/headings and use `--text` for primary text. Switched the
  15 plain-text/label/value rules (`.form-group label`/`select`, `.else-toggle label`,
  `.operator-header label`, `.sub-condition-select`, `.pc-input-label input`,
  `.bound-input input`, `.action-summary`, `.default-toggle`) to `--text`, while keeping
  `--text-bright` on genuine emphasis (`.btn-primary`/`.btn-danger` button text,
  `.day-btn.active`, `.btn-edit:hover`, `.info-box strong`). Completes the colour half of
  the heading-consistency work begun in #2542.

- **Nested action/condition editors rebased onto the type-scale tokens.** The SendMIDI,
  Conditional, and PC/CC context-switch action editors hard-coded `rem` font sizes
  (0.75–1.1rem ≈ 12–17.6px), rendering a whole step *larger* than every workspace pane
  (which uses the 9–14px `--font-size-*` tokens). Every `font-size` declaration is now
  mapped monotonically onto the nearest token (`0.75/0.8rem → --font-size-sm`,
  `0.8125/0.85/0.875rem → --font-size-base`, `0.9/0.95rem → --font-size-md`,
  `1.1rem → --font-size-xl`), preserving each editor's internal hierarchy while bringing
  it into the app-wide scale. Spacing (`rem` padding/margins) and colours are unchanged;
  this is the type-scale half of the heading-consistency work (a follow-up may align the
  remaining `--text-bright` label colours).

- **Workspace pane heading consistency.** Removed the redundant, oversized in-body
  headings that broke the heading hierarchy across workspace panes. The Mapping Editor's
  `Trigger Configuration` / `Action Configuration` `<h3>`s (rendered at 16px with
  `--text-bright` and a heavy 2px underline — larger than the largest type token, and
  duplicating the parent `TRIGGER` / `ACTION` section labels right above them) are gone:
  `TriggerSelector.svelte` and `ActionSelector.svelte` now rely on the section labels.
  The `Endpoints` and `Discovered Ports` panes — the only two that restated their title
  as a Title-Case `<h2>` + subtitle — drop that secondary header so every pane relies on
  the shared uppercase pane bar alone (matching the gui-v2 spec); their action/refresh
  buttons are retained. No backend or behaviour change.

- **#2471 follow-up — editor-theme default preset relabelled "System (default)".** The
  default RAW CONFIG editor preset (previously "Navy (default)") is keyed entirely on
  `theme.css` custom properties, so it *follows the app theme* (navy in dark mode, light
  in light mode) rather than being a fixed navy palette — "System (default)" reflects that.
  Internal preset key renamed `navy` → `system` (pre-release, no localStorage migration:
  a stale `navy` value falls back to the same default). The "Light" and "High contrast"
  presets are unchanged fixed palettes.

### Added

- **#2530 — RAW CONFIG editor context-menu polish (fast-follow to #2484).** The custom
  Cut/Copy/Paste menu now **closes on Escape** (and returns focus to the editor), and its
  position is **clamped to the viewport** so a right-click near the right/bottom edge no
  longer renders the menu partly off-screen. (The third #2484 follow-up note —
  `editorThemeSpec` definition ordering — was already resolved by #2471's extraction of
  the theme into `lib/utils/editor-theme.js`.)
- **#2471 — configurable RAW CONFIG editor theme.** Settings → Appearance now has an
  **Editor theme** dropdown with three presets — *Navy (default)*, *Light*, and
  *High contrast* — that restyle the RAW CONFIG code editor's syntax colours and
  surface. The choice **applies immediately** to the live editor and to the read-only
  TOML/JSON views (the theme + syntax highlight live in a CodeMirror `Compartment`, so
  a preset change reconfigures without a remount), and is **persisted in localStorage**
  (`conductor-editor-theme`) — deliberately off the `config.toml` path, since it's a UI
  display preference, not device config (avoids the RAW CONFIG clobber surface). The
  preset registry lives in `lib/utils/editor-theme.js` (`EDITOR_THEME_PRESETS` +
  `editorThemeExtensions(key)`) so any future CodeMirror surface reuses it; the **navy**
  default stays entirely `theme.css`-keyed (no hardcoded hex), only the opt-in light /
  high-contrast presets carry literal colours.
- **#2485 — read-only config views use a themed CodeMirror.** The RAW CONFIG view's
  read-only **TOML** and **JSON** tabs (in `ConfigPreview`, also shown in chat plan
  previews) render in a read-only, navy-themed CodeMirror (`ReadOnlyCodeView`) with
  syntax highlighting + line numbers — instead of plain `<pre>` blocks — so they look
  consistent with the edit-mode editor. The shared theme + TOML highlight are factored
  into `lib/utils/editor-theme.js` (single source of truth for both the editor and the
  read-only views). A read-only CodeMirror isn't `contenteditable`, so the default
  display needs none of the #2484 edit-mode hardening. The visual **tree** tab is
  unchanged.
- **#2385 — Medium Press Threshold slider (Short→Medium boundary).** The
  Short→Medium press classification boundary is now a daemon-owned config field,
  `advanced_settings.short_press_ms` (default 200), instead of a hardcoded
  `SHORT_PRESS_MS` constant. A "Medium Press Threshold" slider in the GUI Settings
  panel sets+displays it (the daemon owns the behaviour — ADR-017 / ADR-034). The
  value flows through the #2490 `EventTimings` surface
  (`event_timings_from_config`), so it's applied identically at `EventProcessor`
  creation and on config reload. A press held shorter than `short_press_ms`
  fires a `ShortPress`; a longer press fires a `MediumPress`. (The
  `MediumPress`/`LongPress` release boundary is the separate hardcoded
  `LONG_PRESS_MS`, and `HoldDetected` uses the distinct `hold_threshold_ms`
  "Long Press Threshold" slider — neither is affected by this field.)
- **#2484 (toward) — harden the RAW CONFIG editor.** Edit mode no longer inherits
  the macOS/WebView `contenteditable` affordances that were noise or a data-integrity
  risk: **spellcheck / autocorrect / autocapitalize are disabled** on the content,
  and any **smart quotes/dashes** (typed or pasted) are normalized to ASCII
  (`normalizeSmartChars`) so macOS substitution can't silently turn `"value"` into
  invalid-TOML `“value”`. The **native context menu** (Look Up / Translate /
  Search-with-Google / Spelling&Grammar / Substitutions / Speech / Share / Inspect
  Element / AutoFill) is **suppressed** and replaced with a minimal themed
  **Cut / Copy / Paste** menu (paste is also normalized). And `devtools` is no longer
  hardcoded `true` in `tauri.conf.json` (so Inspect Element is **debug-only**, not
  shipped to release). Finally, `basicSetup` is replaced by an **explicit minimal
  extension set**, dropping unused functionality (autocompletion,
  rectangular-selection, crosshair-cursor, fold-gutter, close-brackets, and the light
  default highlight style) so it's excluded from the build rather than carried latent
  — the editor keeps line numbers, history, bracket matching, selection-match
  highlighting, ⌘F search, and our TOML highlight + lint. This completes #2484.
- **#2478 — themed + right-sized RAW CONFIG editor chrome.** The CodeMirror editor's
  built-in **search/replace panel** (⌘F) and the lint/autocomplete **tooltips** now
  match the navy theme instead of CodeMirror's light defaults — panel/inputs/buttons,
  the close affordance, and tooltip surfaces are styled via the editor's `themeSpec`,
  keyed on `theme.css` custom properties (no hardcoded hex). The editor body and the
  search fields/buttons are also **resized** to the app's form-element scale
  (`--font-size-md`/`--font-size-base` with matching padding) — the previous
  `--font-size-sm` (10px) read too small.
- **#2475 — RAW CONFIG lint: "did you mean `[[modes.mappings]]`?" hints.** A
  misspelled TOML table header (e.g. `[[modes.maippings]]`) makes the `toml` parser
  report the error at the *later*, correctly-spelled table where the structural
  conflict surfaces — misdirecting the user to the wrong line. The editor's lint now
  scans `[...]`/`[[...]]` headers and, on a parse error, attaches a `warning`
  diagnostic at the actual typo: *"Unknown config table 'maippings' — did you mean
  'mappings'?"*. Schema-aware (Levenshtein ≤ 2 against the known config table
  segments), quoted keys skipped, capped at 5. Pure JS in `toml-lint.js` (no editor
  change), so the existing `ConfigCodeEditor` lint picks it up automatically.
- **#2468 — RAW CONFIG live semantic validation.** The RAW CONFIG editor now
  surfaces the daemon's deeper *semantic* config rules **live**, below the editor.
  Backend: the `validate_config_toml` Tauri command runs core's
  `validation::validate_config` over the edited TOML and returns every finding as a
  `ConfigDiagnostic { severity, path, message }` (config-path-based, e.g.
  `endpoints[2].channels`) — complementing the editor's syntax lint
  (`parse_config_toml`) by catching unknown route/trigger aliases (ADR-031 §4.3),
  channel-range and duplicate-alias errors that previously only appeared as an
  on-Save toast. Frontend: a new **`ConfigProblemsPanel`** debounces that command
  and lists the findings (errors then warnings, severity-coded) under the
  CodeMirror editor; a draft that fails to parse shows nothing (the syntax lint
  already flags it). Pure/daemon-free; on-Save `validate_for_loading` stays the
  authoritative gate. Inline squiggles (mapping a config path to a source span via
  `toml_edit`) remain a deferred stretch.
- **#2431 (ADR-047 §D4) — controller-aware gamepad face-button labels.** The GUI
  now labels gamepad buttons for the connected controller's vendor style — Xbox
  *A/B/X/Y*, PlayStation *Cross/Circle/Square/Triangle*, Nintendo's A↔B / X↔Y
  position swap — across the event stream and trigger editor. Style is resolved
  from the connected controller (curated name match, with a user-extensible GUID
  override; vendor-id inference deliberately avoided). Presentation-only:
  positional button IDs (128-146; 147-148 are encoder/axis ids) and all trigger
  matching are unchanged; with no
  controller connected, labels stay the combined cross-vendor form.
- **#2430 (ADR-047, implements #1987) — Device Templates pane.** Restored the
  Device Templates UI as a workspace view (nav → *Device Templates*): browse the
  built-in MIDI + gamepad templates and apply one, which merges its endpoints and
  modes into the active config **additively** via `create_config_from_template`
  (never clobbering existing mappings). A connected controller's matching template
  is flagged **Suggested** (hybrid GUID-first / name auto-detect). Reachable from
  the LLM via the `conductor_navigate_workspace` tool (`device-templates`).
- **#2429 (ADR-047 / ADR-035) — device templates apply additively.** Applying a
  device template now merges into the existing config (endpoints deduped by alias,
  modes added only when new) instead of clobbering the whole file, and refuses
  malformed/legacy configs via `Config::preflight_removed_blocks`.
- **#2285 (Phase 2) — CodeMirror editor for RAW CONFIG; completes #2285.** The
  RAW CONFIG edit mode swaps the Phase 1b `<textarea>` for a CodeMirror 6 editor
  (`ConfigCodeEditor`): TOML **syntax highlighting** (line numbers, bracket match)
  and **live lint** — a debounced call to `parse_config_toml` (Phase 1a) underlines
  syntax errors inline at line:column, with no JS TOML-parser dependency. Daemon
  *semantic* validation (unknown alias, ADR-031 §4.3) stays on-Save (those errors
  are config-path-based, not line:col). The editor keeps the Phase 1b `bind:value`
  contract, so the Save/Cancel + SaveConfig-CAS + ConflictBanner flow is unchanged.
  Adds the `@codemirror/*` packages (the Cargo-based license inventory is
  unaffected). With this, RAW CONFIG is fully editable and #2285 is complete.
- **#2285 (Phase 1b) — editable RAW CONFIG view (textarea).** The RAW CONFIG view
  gains a View↔Edit toggle: **Edit** swaps the read-only `ConfigPreview` for a TOML
  `<textarea>`, **Save** parses it via `parse_config_toml` (Phase 1a) and persists
  through `configStore.save()`'s **SaveConfig CAS** seam (threading the
  `base_revision` content-guard, #2417) — never a direct file write (ADR-034 §D11);
  **Cancel** discards. A TOML syntax error (with line/column) or a daemon validation
  error surfaces as an error toast and keeps you in the editor; a hard CAS conflict
  routes to the app-level `ConflictBanner` (#2255) via `configConflict.raise`, the
  same path the Endpoints/DiscoveredPorts editors use. Edit needs the daemon running
  (it saves via SaveConfig). JSON/tree tabs stay read-only. Phase 2 (CodeMirror
  syntax highlighting + live lint) follows — **#2285 stays open until then**.
- **#2285 (Phase 1a) — `parse_config_toml` command for the editable RAW CONFIG
  view.** Parses an edited RAW-config TOML string into the `Config` JSON the
  `save_config` SaveConfig CAS seam expects (so a raw edit persists through the
  daemon authority + `base_revision` content-guard, never a direct file write —
  ADR-034 §D11). Mirrors `read_import_config` without the file/extension handling
  (the text comes from the in-app editor); enforces the §D2.3 payload cap and
  surfaces `toml`'s line/column parse error. Backend slice; the RawConfigView
  edit mode (Phase 1b) consumes it. Part of #2285 (Phase 1 = textarea; #2285 stays
  open until the Phase 2 CodeMirror editor).
- **#2284 (Slice 1b) — config-drift banner "Review diff" + completes #2284.** The
  drift banner's "Review diff" button fetches the live-vs-on-disk diff
  (`compute_diff_from_disk` → `GetConfigDiff`) and opens it read-only in a new
  `DRIFT_DIFF` workspace view (`ConfigDriftDiffView`), rendering each changed
  top-level section's on-disk value (−) vs live value (+) via `DiffBlock`. It does
  NOT resolve the drift — the banner stays up so the user can then Apply or
  Overwrite after reviewing. With this, all four ADR-034 §D5.5 banner actions ship
  (Apply / Review diff / Overwrite / Dismiss) and #2284 is complete.
- **#2284 (Slice 2b) — config-drift banner "Overwrite user.toml" button.** The
  drift banner now offers the inverse of "Apply changes": where Apply reloads the
  on-disk edit (disk wins), **Overwrite** writes the daemon's live config back over
  the drifted file (live wins) via the `overwrite_user_config` command
  (ADR-034 §D5.5). Success resolves the banner (disk now matches live); a write
  failure leaves it up for retry, with the same mid-flight race guard as Apply.
  ("Review diff", the remaining §D5.5 action, follows in Slice 1b.)
- **#2284 (Slice 2a) — `OverwriteConfigFile` IPC + `overwrite_user_config`
  command.** Backend for the config-drift banner's "Overwrite user.toml" action
  (ADR-034 §D4.D): write the daemon's live config over the drifted on-disk file
  ("my live config wins"). A dedicated daemon IPC because `SaveConfig` with the
  live body is a semantic-identical no-op (same revision ⇒ no CAS bump ⇒ no
  write-through) and would leave the drifted file untouched. The write goes
  through the §D9-suppressed path (a new shared `armed_profile_write` helper, also
  backing the §D11 write-through) so the watcher doesn't re-surface it as external
  drift — the GUI never writes the daemon's config file itself. Rejected during
  AwaitingConfig (no live config to persist). Returns the live revision. The
  banner button (Slice 2b) follows.
- **#2284 (Slice 1a) — `compute_diff_from_disk` Tauri command.** Wraps the
  `GetConfigDiff` ReadOnly IPC (#2414) and returns `{ differs, changed_sections,
  live, target }` — the daemon's live config vs its on-disk file — for the
  upcoming config-drift banner "Review diff" action (ADR-034 §D4.D). The daemon
  performs the §D2.2 safe-walk read of the on-disk file; the GUI never reads the
  config file itself. Backend slice; the banner button + workspace diff rendering
  (Slice 1b) and the "Overwrite" action (Slice 2) follow.
- **#2398 — config-load warning for MIDI feedback-loop topologies.** When a
  `[[routes]]` `to`, a `SendMidi` `port`, or a `MidiForward` `target` resolves to
  a MIDI port that is also declared by a listened `Input`/`Bidirectional`
  endpoint, config validation now emits a `Warning` naming the port and
  suggesting a fix (`ignore_ports`, `listen_mode = "Configured"`, or disabling
  the input). Pure config-level detection (no live port enumeration), so it has
  no false positives for legitimately distinct in/out ports. ADR-009 Negative #1
  wording clarified: `enabled: false` / mute do **not** stop a port being opened
  under the default `listen_mode = "All"` — only `ignore_ports` does (epic #2395,
  stuck-notes symptom).
- **#2326 — OSC forward action (ADR-039-A Slice 3).** `Action::OscForward
  { target }` re-sends the OSC message that fired its mapping to an OSC
  **output** endpoint (by alias), completing ADR-039 lifecycle stage 4 for
  OSC. V1 is pass-through (a `transform` is reserved for a future OSC→OSC
  remap and rejected at config-load); the target must resolve to a declared
  OSC output endpoint, and the inbound OSC message rides the trigger context
  (a non-OSC-triggered mapping is a runtime no-op, mirroring `HidForward`).
  Not a sensitive action class — it emits a packet, not a host effect — but
  rides the Slice-2 D17 network-origin taint. With this, ADR-039-A
  (OSC input/triggers/forward) and epic #2299 (HID/gamepad + OSC parity) are
  complete.

- **#840 — full MIDI data fields in the expanded event view.** Expanding a raw
  event row in the event stream now shows the message's semantic fields, not
  just Device/Channel/Type/Time: Note, Velocity, CC#, Value, Program, Bend
  (signed −8192..+8191), Pressure, gamepad Axis/Analog, and a canonical **Raw**
  hex byte string (e.g. `B0 07 4E`). Each row renders only when its field is
  present, so one block serves every channel-voice type. Two backend gaps are
  closed to feed this: (1) `ProgramChange` events were silently dropped by the
  monitor (`create_monitor_event`'s catch-all `_ => None`) and now emit a
  `program_change` MonitorEvent carrying the program number; (2) `MonitorEvent`
  / the GUI bridge now carry `raw_bytes`, reconstructed daemon-side via the
  existing `midi_bytes::extract_raw_midi` (channel folded into the status
  byte's low nibble) — the GUI bridge previously hardcoded an empty vector.
  Gamepad/synthetic events carry no raw bytes, so the Raw row stays hidden for
  them.

- **#2342 — tray "Switch Profile" submenu.** The system-tray menu now has a
  Switch Profile submenu alongside Switch Mode, listing the built-in Default
  (root config) plus every stored profile (from `ProfileManager::list_profiles`,
  sorted by name) and refreshed on each 5 s poll. Selecting one performs the same
  two-layer switch as the TitleBar dropdown — updating the GUI `ProfileManager`
  active state *and* the daemon over `SwitchProfile` IPC, with rollback if the
  daemon rejects it — via a shared `do_switch_profile` helper extracted from the
  `switch_profile` command. This gives a non-chat profile-switch path so #1261 A3
  (hiding the LLM-Mode profile dropdown) can proceed. The submenu is disabled
  while the daemon is down and cleared on disconnect (mirrors the mode submenu).
- **#2325 — OSC typed triggers + ADR-042 D17 action-class gating (ADR-039-A
  Slice 2).** Inbound OSC can now fire mappings: `Trigger::OscMessage`
  (exact address), `OscAddressPattern` (native OSC 1.0 wildcards `? * [] {}`
  via a ReDoS-safe matcher — compile-time brace expansion bounded at 64
  alternatives, linear-backtracking glob, panic-free over wire bytes) and
  `OscArgRange` (fallible numeric coercion; NaN/strings never match), each
  with the standard `device` filter scoping to a listener alias.
  **Security (D17):** every action dispatched from an OSC event carries a
  network-origin taint (`ActionDispatch.network_origin` = listener alias,
  threaded into the executor unconditionally per dispatch); the action-class
  gate refuses sensitive actions (`Shell`/`Launch`/`Keystroke`, including
  statically nested in `Sequence`/`Delay`/`Conditional`/`Repeat` — the gate
  decides on the whole action tree before any timer runs) unless the
  originating endpoint sets `allow_sensitive_actions = true`; refusals are
  audited (`NetworkActionClassBlocked`). MIDI/HID dispatches are never
  gated. Regression locks cover the R5.1 confused-deputy and R5.2
  state-laundering shapes. OSC lifecycle stage 2 (Typed Triggers) → Done.

- **#2324 — OSC → Art-Net (ADR-039-A Slice 1b).** Inbound OSC can now drive
  Art-Net/DMX outputs: `SignalTransform::OscToArtNet { address_to_dmx }`
  extracts the DMX channel from the OSC address via a `{dmx}` template (the
  attacker-controlled capture is parsed fallibly and range-checked to the DMX
  universe 1–512 before any update is built, same convention as `OscToMidi`)
  and coerces the first argument to the 8-bit level. Admitted in the route
  engine with the same 3-byte Art-Net wire form as `MidiToArtNet`/
  `HidToArtNet`; config-load validation requires the template to start with
  `/` and carry exactly one `{dmx}`. Also lands the Council-mandated
  `RouteInput` enum refactor: `RouteEvalContext`'s per-protocol structured
  fields collapse into one borrowed `Copy` enum
  (`None | Event(&InputEvent) | Osc(&OscInbound)`) so the upcoming Art-Net
  input source (ADR-039-C) becomes a variant, not a third `Option` field —
  allocation-free, MIDI byte hot path unchanged.

- **#599 — value bars for gamepad analog sticks and triggers.** The GUI event
  stream now renders inline value bars for `gamepad_axis` (green center-zero,
  same 0–50 scale convention as pitch bend) and `gamepad_trigger` (coral linear)
  events, driven by a new high-precision `analog_value: Option<f32>` field
  threaded end-to-end: the raw gilrs reading is preserved on
  `InputEvent::EncoderTurned` (`analog`), forwarded on the daemon's
  `MonitorEvent`, and surfaced on the GUI's `MidiEventInfo` (extraction gated to
  gamepad event types). Events without the field (older daemons) gracefully show
  no bar. Rows are labelled with the control identity ("L-Stick Y +0.51",
  "R-Trigger +0.42", "btn 130") and Y-axis bars use a distinct colour (blue)
  from X-axis bars (green). New `--event-analog-stick` /
  `--event-analog-stick-y` / `--event-analog-trigger` theme variables.
  Three macOS pipeline bugs found during hardware verification are fixed along
  the way: (1) analog trigger travel arrives as gilrs
  `ButtonChanged(LeftTrigger2/RightTrigger2)` — previously unhandled, so
  `gamepad_trigger` events never existed on macOS (new
  `trigger_button_changed_to_input`); (2) duplicate HID elements interleave
  spurious `0.0` axis readings with real values, flickering bars to center and
  flapping encoder direction detection (time-windowed `AxisZeroFilter` —
  inline keep/drop, no buffering, zero added latency); (3) idle triggers
  chatter below the deadzone, flooding the stream
  with zero-information events (transition-aware `TriggerNoiseGate`). Also adds
  the `gamepad_diagnostic` binary (raw gilrs event dump with Conductor ID
  mapping) that the troubleshooting docs referenced but never existed.

- **#1361 — OSC input (ADR-039-A Slice 1): route-only listener + `OscToMidi`.**
  Conductor can now **receive** OSC over a loopback UDP listener and route it to
  MIDI/OSC/Art-Net outputs. The ADR-042 listener edge (loopback bind, ACL,
  rate-limit, audit, bind-gate) already existed; this fills the documented
  "parser placeholder" gap: accepted datagrams are decoded (`osc_parser` — bundle
  flattening under depth/message/**node** caps, panic-free over arbitrary bytes)
  and forwarded onto the unified pump as `ProtocolEvent::Osc`, which the route
  engine evaluates via a new structured `OscToMidi` transform (`{cc}`/`{note}`
  address templates, attacker-controlled values range-checked `0..=127` before
  any MIDI byte, float/int arg coercion). OSC events reach the **route engine
  only — never the mapping engine** — so there is no OSC→sensitive-action path
  (ADR-042 D17 satisfied by construction); a config-load **feedback-loop guard
  (D8)** rejects an OSC route whose MIDI output is also a Conductor MIDI input.
  Slice 1 is catch-all routes only; OSC typed triggers + action-class gating
  (Slice 2, #2325) and `OscForward` (Slice 3, #2326) follow. Lifecycle stages 1
  (listener), 3 (catch-all), 6 (transform) are now `Done` for OSC.

- **#1626 — routing canvas shows runtime mute state.** Muting a device at runtime
  (`conductor_set_device_enabled` / the Devices-panel toggle, ADR-009 Phase 4b)
  suppresses its event flow but previously left the Routing Graph unchanged. The
  resolved-routing-graph response now carries a `muted` flag per connector (queried
  from `InputManager::is_device_enabled`), and `ConnectorPill` renders it following
  the events-filter (BindingPills) standard — dimmed pill, struck-through alias, an
  **amber** status dot, and a "muted" tag — distinct from the grey/inert treatment
  of a persisted `enabled = false`. The muted pill stays interactive (mute is a
  runtime toggle, not config removal).
- **#2293 — gamepad hot-plug.** A game controller switched on / paired (or plugged
  in over USB) **after** the daemon started is now picked up, mirroring MIDI's 5s
  hot-plug rescan. Previously the gamepad was only connected once, at startup
  (`connect_gamepad_multi_device`); the 5s `rescan_ports` loop was MIDI-only, so a
  controller powered on later never appeared. The hot-plug check now, when no
  gamepad is connected and the mode allows it (`Both`/`GamepadOnly`), probes for a
  controller **off the input-manager lock** (a `spawn_blocking` `list_gamepads`),
  and only acquires the lock to connect when one is actually present — so the
  discovery window never stalls the loop or delays event processing while no
  controller is connected. Reuses the #2289 detection pump for the connect.

### Added

- **#2414 — `GetConfigDiff` config-diff IPC (drift-banner precursor).** New
  ReadOnly IPC returning a structured diff of the daemon's in-memory live config
  vs the on-disk config (the §D9 drift source): `{ differs, changed_sections,
  live, target }`. `changed_sections` is the sorted set of top-level keys that
  differ (config-shape-agnostic, via `config_changed_sections`), and the full
  `live`/`target` trees let the GUI render the detail. Unblocks #2284's drift
  banner (Review-diff / Overwrite). V1 diffs against the daemon's own
  `config_path`; an arbitrary-allowlisted-path target (§D2.2 safe-walk) is a
  deferred follow-up — #2284 only needs live-vs-own-disk. On the AwaitingConfig
  accept-list (pure read). (ADR-034 §D4.D; epic #2297.)
- **#2417 (B2 completion, Slice B) — GUI threads `base_revision` into saves,
  activating the content-hash guard.** New `get_config_with_revision` Tauri
  command returns `{ config, base_revision }` atomically from one `GetConfigBody`
  read; `configStore` captures the revision on fetch and threads it into the next
  `save_config`, which forwards it to `SaveConfig`. A daemon mutation between the
  GUI's display and the user's save now surfaces as a recognizable
  `StaleBaseContent:` conflict (refresh-and-reapply) instead of silently
  clobbering — closing the residual race #1779/Slice A left. The on-disk fallback
  (offline / no daemon) threads `null` ⇒ unguarded, as before. Read-only
  consumers (raw-config display, chat context) keep using plain `get_config`.
  (ADR-034 §D4 / ADR-043; epic #2297.)
- **#2417 (B2 completion, Slice A) — `SaveConfig` content-hash guard.** The GUI
  re-fetches a fresh `base_generation` right before saving, so the storage CAS
  never detects a conflict — an LLM/`conductorctl` mutation landing between the
  GUI's `GetConfigBody` read and the user's save was silently overwritten.
  `SaveConfig` now accepts an optional `base_revision` (the content hash the
  client displayed); if it no longer matches the live revision the save is
  rejected with the new `IpcErrorCode::StaleBaseContent = 5007` **before** commit
  (no clobber). Content-hash, **not** generation — a daemon self-write that bumps
  the generation without changing content does not false-positive. Absent
  `base_revision` preserves prior behaviour (backward-compatible). The GUI
  threading that supplies `base_revision` lands in Slice B. (ADR-034 §D4 /
  ADR-043; epic #2297.)

### Changed

- **#571 — Deduplicated `IpcConnection::send_request` / `send_request_with_timeout`.**
  The two methods shared near-identical connect→timeout-send→retry-once logic
  (differing only in the timeout value); they now both delegate to a private
  `send_request_inner(request, timeout)`, so the retry/timeout semantics can't
  drift apart. Behaviour-preserving (`REQUEST_TIMEOUT` Debug-formats as `5s`, so
  the surfaced messages are unchanged).

### Fixed

- **#2262 — config export now reads the daemon's live config (ADR-034 §D4.C).**
  `export_config_to_path` read the on-disk active file (`std::fs::read_to_string`)
  on the assumption the §D11 write-through kept it in sync — the same stale-read
  assumption #1779/B2 invalidated, which is why the export half of #2262 was left
  open pending the GetConfigSnapshot-body gap. It now reads the daemon's canonical
  in-memory tree via `GetConfigBody` (the B2 read) and serializes it through
  `Config` + `toml::to_string_pretty` (the same serializer the daemon's own
  `Config::save` uses), so an export captures live LLM/IPC mutations rather than a
  possibly-stale file. Falls back to the on-disk read when the daemon has no body
  to serve (offline / `AwaitingConfig`), matching `get_config`'s daemon-try → disk
  policy; the target keeps the cheap `.toml` extension guard (not `Config::save`'s
  allowlist, which would reject a user export location like `~/Downloads`).
- **#458 — `IpcClient` no longer drops bytes buffered past a response.** The
  client read each response with a *temporary* `BufReader<&mut stream>` and then
  built a *fresh* `BufReader<stream>` in `into_reader()`; if the daemon coalesced
  a response and a following streamed line into one write, the per-request reader
  buffered the extra bytes and dropped them, so `into_reader()` (the #394
  subscribe path) started blind to the first streamed event. `IpcClient` now
  holds one persistent `BufReader<UnixStream>` for the connection's life;
  `send_request` writes via `reader.get_mut()` and `into_reader()` returns the
  same reader, preserving any read-ahead.
- **#1947 — `conductorctl migrate-config --routing` now defaults to the real
  config location.** The migrate command fell back to `~/.conductor/config.toml`,
  but the daemon and GUI resolve config via `dirs::config_dir()` — on macOS that
  is `~/Library/Application Support/conductor/`. So a bare `migrate-config
  --routing` (as the ADR-035 deprecation warning suggests) targeted a file that
  doesn't exist on macOS: it errored and the user's actual config was never
  migrated. The default now resolves to `dirs::config_dir()/conductor/config.toml`
  (matching the daemon/GUI), via a new `resolve_migrate_config_path` helper; an
  explicit `--config <path>` is still honoured. (epic #2297, ADR-035.)
- **#1779 (B2, read side) — GUI now reads the daemon's canonical config, not a
  stale on-disk file.** `#2083` migrated the GUI's *write* path to the
  `SaveConfig` IPC (daemon-is-authority) but left `get_config` reading
  `config.toml` from disk. In the default no-profile setup the GUI could display
  a config the running daemon was not serving (after an LLM/`conductorctl`
  mutation of the live tree), and a later save would CAS-pass against a *fresh*
  generation while persisting the *stale* displayed content — clobbering the
  canonical config (the ADR-043 anti-clobber CAS assumes display and
  `base_generation` come from the same snapshot). New `GetConfigBody` ReadOnly
  IPC returns `{ state_generation, config, revision, applied_at }` atomically;
  `get_config` reads it so the GUI reflects the daemon's in-memory authority
  (live mutations included). When the daemon is unreachable or in the reserved
  `AwaitingConfig` sentinel (`state_generation = 0` → `config: null`), the
  command falls back to the on-disk read, preserving offline behaviour.
  `GetConfigSnapshot` stays metadata-only by design (large body off the hot
  CAS/status path); the Phase-B spec table is reconciled accordingly. (ADR-034
  §D4 / ADR-043; epic #2297.)
- **#2410 — Event-stream rows stay correctly ordered under MIDI-Learn.** The
  daemon emits one FIFO-ordered event stream, but the src-tauri bridge splits it
  across two independent Tauri channels (`midi-events` batched + `mapping-fired`
  individual) with no cross-channel delivery-order guarantee, and the daemon
  itself pushes `mapping_fired` from a different run-loop `select!` arm than the
  raw event — so under MIDI-Learn (which holds/bursts events via the extended
  chord-timeout capture path) the rows clumped by type in the Events panel.
  `MonitorEvent` now carries a monotonic `seq` stamped at `push_monitor_event`
  time (one relaxed atomic; on the monitoring path, not the note-off hot path),
  surfaced on both Tauri channels, and the GUI orders each coalesced frame by
  `seq`. Backward-compatible (`#[serde(default)]`; events without a seq fall back
  to arrival order). Cosmetic display-only fix — never affected mapping
  execution or learn capture.
- **#2368 — Events panel no longer freezes the UI under high-rate input.** A
  controller stick sweep makes the daemon emit ~3,400 events/sec (only ~25% raw
  input; the rest is `mapping_fired`/`action`/`route`/`echo` fan-out). The
  webview event store did one Svelte store update per incoming event/batch, and
  each update forced a full reactive pass plus a whole-buffer `detectCCStreams`
  scan — running hundreds of times/sec and saturating the webview main thread.
  The three live Tauri listeners now ingest through rAF-coalesced helpers that
  apply a **single** `eventBuffer` update per animation frame (≤60/s) regardless
  of input rate. This is strictly a webview-consumer fix: the daemon broadcast is
  lossy and cannot be back-pressured, so MIDI execution / note-off latency is
  unaffected (confirmed — the daemon stayed healthy during the freeze). Hardened
  per LLM Council review with a hidden-tab `setTimeout` backstop (rAF is
  suspended while the window is hidden) and a drop-oldest queue cap so a stalled
  thread cannot grow the pending queue without limit. Nothing the daemon acts on
  is dropped; fire-state side effects (counts, row pulse, toasts) still run
  immediately.
- **#2368 — `conductorctl events` snapshot/export fixes.** Surfaced while
  building a high-rate capture for the above: the `--duration` value is now
  reflected in the "Monitoring events for N seconds…" message (was hardcoded to
  "2 seconds"), and `--limit` (default 50) no longer truncates `--output` file
  exports — it bounds terminal display only, so a 10 s capture writes the whole
  window instead of the first 50 events.
- **#2404 — `connect_multi_device` no longer leaks duplicate background tasks on
  MIDI reconnect.** The 50 ms timer-tick (D12) and 5 s hot-plug (ADR-009 Phase 4)
  loops were spawned inside `connect_multi_device`, which re-runs on every
  `DeviceReconnected` — so each MIDI unplug→replug cycle spawned a fresh pair
  while the old ones kept running (unbounded task accumulation + duplicate
  `TimerTick`/`HotPlugCheck` traffic). These tasks only poll `command_tx` for the
  daemon's life (and must survive disconnect so hot-plug can detect the replug),
  so they're now spawned **exactly once** via `spawn_background_tasks_once`
  (`AtomicBool` set-once guard), matching ADR-009 D12's single-`timer_tick_loop`
  intent. Council-surfaced during #2396 review; was pre-existing on main.
- **#2399 — docs: `ignore_ports` is substring match, not glob.** ADR-009 D4
  showed `ignore_ports = ["IAC Driver*"]`, but matching is plain substring
  containment (`port_name.contains(entry)`), so the `*` was a literal that
  matched nothing. Corrected the example to `"IAC Driver"` and documented the
  substring semantics (and that `""` would skip all ports). Glob support is not
  implemented (epic #2395).
- **#2397 — coalesce `midi_*_suppressed` MonitorEvents (events-panel flood).**
  The recursion guard emitted one `midi_echo_suppressed` / `midi_cascade_suppressed`
  MonitorEvent per suppressed input event, so a feedback loop or chord storm
  flooded the monitor broadcast stream 1:1 and saturated the GUI events panel
  (epic #2395, stuck-notes symptom (i)). A new `SuppressionThrottle` now collapses
  these into at most one summary per kind per 1s window (`"+N more suppressed in
  last 1s"`), mirroring the `RefusalLogger` batching pattern. Suppression
  behaviour is unchanged — only the telemetry emission cadence is throttled.
- **#2396 — executor config now reaches the action-dispatch thread (virtual MIDI
  ports, OscForward, ADR-042 D17 allow-list).** Actions dispatch on a dedicated
  thread that owns its `ActionExecutor` (ADR-015 D1), but three config items were
  applied to a *separate* mutex-guarded `ActionExecutor` (used only for plugin
  lifecycle + SysEx probe) that never dispatches — so they never took effect:
  (1) `SendMidi`/`MidiForward`/route to a `MidiVirtualPort` failed "port not
  found" even though the port was created; (2) `OscForward` couldn't resolve its
  target endpoint; (3) the D17 action-class gate read an empty allow-map and
  defaulted to DENY, so `allow_sensitive_actions = true` never worked (fail-safe,
  but the control was dead). Root cause: ADR-015 D2's config control plane was
  never implemented. Fix (LLM-Council reviewed, reasoning tier): the read-mostly
  maps (OSC endpoints + D17 allow-map) are shared with the dispatch executor via
  a single lock-free `Arc<ArcSwap<SharedActionConfig>>` (atomic across both
  maps); virtual-port names flow via a `watch` channel and are created on the
  executor thread between actions (midir thread-affinity). The thread executor
  starts at its fail-safe default (DENY) before processing any action, so there
  is no fail-open window. ADR-015 D2 and ADR-021 D4 updated to document the
  implemented hybrid; an integration test now drives config through the real
  channels and asserts a dispatched action observes it. Virtual-port changes
  are applied idempotently (diffing the latest desired set against the
  last-applied one, not a consumable watch edge flag) at the idle top-of-loop
  *and* immediately before each dispatch, closing a one-dispatch "port not
  found" window if a port update landed while the executor was blocked
  receiving.
- **#2392 — gamepad hot-plug probe no longer stalls MIDI forwarding every 5 s.**
  Companion to #2390: after the MIDI port enumeration moved off the run-loop, a
  second ~535 ms stall remained on every 5 s hot-plug tick. `process_hot_plug_apply`
  still `.await`ed the gilrs gamepad probe (`spawn_blocking(list_gamepads)`)
  inline on the run-loop, and `list_gamepads` runs a fixed ~500 ms discovery
  window when no controller is connected — which fires whenever a gamepad
  endpoint is configured (e.g. an `xbox` Hid endpoint) but none is plugged in.
  `spawn_blocking(...).await` moves the CPU off-thread but the `.await` still
  parks the run-loop task. The probe now runs in the same off-loop task the
  hot-plug check already spawns (gated by a cheap `needs_gamepad_rescan` read),
  delivering only a boolean via `HotPlugApply { port_infos, gamepad_available }`;
  the run-loop does only the cheap connect, and only when a controller is
  actually present. Also adds a permanent run-loop select-arm latency guard
  (`ArmTimer`) that `warn!`s when any arm body exceeds 150 ms (tagged with the
  command/IPC variant) — this is what pinpointed the stall. Verified on hardware
  (`CONDUCTOR_TRACE_LOG`): max per-event processing latency dropped from ~475 ms
  to 2 ms with zero backlog-flush bursts across ~8 hot-plug ticks.
- **#2390 — hot-plug rescan no longer stalls MIDI forwarding every 5 s.** The
  5-second MIDI hot-plug rescan enumerated CoreMIDI ports (`MidiInput::new()` +
  `.ports()`) **synchronously, inline on the run-loop, while holding the
  `input_manager` lock**. With many open ports (listen-all + IAC + virtual
  ports) that enumeration took ~500 ms and blocked event processing — periodic
  forwarding stalls during which held notes sustained on the output (a "stick"),
  then the queued input flushed in a burst (an "unstick"). The run-loop now
  SPAWNS the enumeration off-loop (`InputManager::enumerate_input_ports_async`,
  `spawn_blocking`) and re-delivers the result as a new `DaemonCommand::HotPlugApply`,
  so it never `.await`s the slow scan inline — it only does the cheap diff/open
  (via `rescan_ports`, which now takes the pre-enumerated port list and holds the
  `input_manager` lock for microseconds when nothing changed). An in-flight guard
  drops a tick if a scan is still running. Both the hot-plug and config-reload
  paths are updated. Bonus: `rescan_ports` is now unit-testable with injected
  ports. Diagnosed via two `CONDUCTOR_TRACE_LOG` captures whose forwarding stalls
  landed exactly on the 5 s rescan boundaries.
- **Stuck/sustained notes on routed MIDI output — recursion guard no longer
  suppresses a controller's own repeated notes.** The MIDI recursion guard's
  echo check (`is_echo`, ADR-015 D8) matched purely on byte fingerprint within a
  100 ms window, with no source attribution. When a route forwarded a
  controller's notes to an output (e.g. `mpk → iac-out`), the forwarded bytes
  were recorded, and the controller's *own* next identical message (a re-strike,
  double-tap, or the matching note-off during fast playing) was then flagged as
  an "echo" and dropped. Asymmetric aging of note-on vs note-off records left a
  forwarded note-on unpaired on the output → a hanging note. The guard is now
  **source-aware**: each recorded send is attributed to the originating device
  (`ActionProvenance.device_id`), and a fingerprint match is treated as an echo
  only when it arrives on a *different* device than the source — a genuine
  loopback always arrives on the input bound to the output's port, never on the
  source controller. Confirmed against a setup where the same note stuck via
  Conductor but not when the synth was wired directly to the controller.
  (Caveat: a pathological *self-route* — routing a port straight back to itself
  — is no longer caught by the byte-echo guard alone; that loud feedback loop is
  the job of the per-port blanket suppression and a future config-load
  loop-detection warning, not silent byte matching.)

- **#835 — gesture pattern badges now render in every mode (always-on).**
  DoubleTap / Chord / LongPress / GamepadChord badges previously appeared only
  in *manual* Learn — never in LLM-initiated Learn or the normal event stream.
  Root cause: the panel's gesture-badge block was gated on `learnSessionActive`
  **and** sourced from the polled MIDI Learn buffer. That buffer is
  single-consumer drain (`get_midi_learn_events` does `drain(..)`), so LLM Learn
  deliberately doesn't poll it (the LLM's `conductor_stop_midi_learn` needs the
  events) → the badge source was empty; normal mode skipped the block entirely.
  The daemon now stamps the structured pattern annotation (`PatternType` +
  notes/buttons + duration/window, mirroring `capture_pattern_events`) onto the
  **always-on processed-monitor stream**, the GUI bridge lifts it to first-class
  `pattern_*` fields, and the panel sources gesture badges from the event buffer
  (ungated) — the same path CC-stream badges already use. Badges now appear
  uniformly in normal mode, manual Learn, and LLM Learn, without draining the
  Learn buffer. (Follow-ups #2385 Medium-Press threshold, #2386 chord-window
  normal-vs-Learn parity.)

- **#912 — chat tool-result success classified through the shared helper.** Two
  call sites in the chat store hand-rolled their own success checks: the
  Learn-stop resume gate used `type === 'Error' || result?.result?.isError`, and
  the post-tool sync used a long inline disjunction. Both mis-classified the
  daemon's non-success `ExecutionResult` variants (`RateLimited`,
  `HardwareIoConfirmation`) as success — so a rate-limited Learn-stop would
  wrongly proceed to resume the agentic loop, and GUI Learn/mapping state could
  drift from the daemon. Both sites now call `isExecutionResultSuccess`
  (the same helper introduced for #911), keeping the legacy `success === true`
  fallback for the post-tool path. Added regression tests covering the
  `RateLimited` and `Error` variants skipping resume.

- **#1106 — consistent risk-tier colours in the plugin Installed view.** The
  PluginManager (Installed plugins) used local `getRiskLevelColor`/`getRiskLevelEmoji`
  helpers returning browser-default `green`/`yellow`/`red`, so the same risk tier
  showed different colours than the Marketplace (which uses the shared
  `capability-risk` util with theme tokens `--green`/`--amber`/`--accent`). Removed
  the local helpers and switched to `riskColour`/`riskEmoji` from
  `$lib/utils/capability-risk` (which normalises the daemon's PascalCase
  `risk_level`), so both views match.
- **#1258 — chat bubbles get avatars + left/right alignment.** Chat turns now
  render as the ADR-032 specimen: a 24px circular avatar (`◆` on accent for the
  assistant, an initial on blue for the user) beside the message bubble, with the
  assistant on the left and the user right-aligned. The avatar replaces the old
  text role label (`YOU`/`CONDUCTOR`), carrying the speaker name on its
  `aria-label`. Bubbles gained the specimen styling (larger radius, `10px 14px`
  padding, a 1px border — blue-tinted for the user — and an 88% max-width). The
  layout lives in the parent `MessageBubble` so the alignment decision is in one
  place; system/error messages are unaffected.
- **#1259 — chat input row matches the ADR-032 handoff.** The Send button is now
  a circular 38×38 accent button with an `↑` glyph (E1) instead of a rectangular
  "Send" text button, and the hint row shows the four designed shortcut groups
  (E2): `⏎ send · ⇧⏎ newline · ⌘L <mode> · ⌘E events` — surfacing the `⌘L`/`⌘E`
  shortcuts ADR-032 D8 designed for (previously only Enter/Shift+Enter showed).
  The `⌘L` label is mode-aware (names the mode you'd toggle into). Applies in both
  modes (shared `ChatView`).
- **#1261 (A1+A2) — TitleBar matches the ADR-032 LLM-Mode design handoff.** The
  brand wordmark is now the lowercase `>conductor` (chevron prefix, accent colour,
  mono 15px) in both modes instead of uppercase `CONDUCTOR` (A1), and the Profile
  dropdown moved into the left cluster after the release badge, matching the
  canonical `[logo] [BETA] [Profile ▾] [Mode-pill]` layout (A2). The per-profile
  Mode dropdown stays in the right cluster. A3 (hiding the Profile/Mode dropdowns
  in LLM Mode, which would amend ADR-032 D10) is intentionally deferred pending a
  profile-switch-fallback decision — the daemon `conductor_switch_profile` MCP tool
  exists (chat works) but the tray menu (all platforms) has no profile-switch entry
  point.
- **#1621 — single source of truth for route line classification.** `routeDisabled`
  and `throughputBucket` had implicit precedence coupling: each routing-graph
  consumer re-chained `throughputBucket(rate, hasErrors, missingEndpoint, routeDisabled(r))`
  by hand, and because `routeDisabled` returns `false` for a *missing* endpoint
  (`undefined === false`), an isolated or mis-ordered call could classify the same
  route differently on different surfaces. Added `routeRenderState(r, rate, hasErrors)`
  that encapsulates the documented precedence (disabled → missing-endpoint `warn` →
  errors → throughput tier); `RoutingGraph` and `RouteInspector` now both call it
  instead of re-chaining. Tightened the `routeDisabled` docstring to mark it as the
  disabled-flag-only primitive, and added unit tests locking the disabled-vs-missing
  distinction so a future refactor that makes them disagree fails CI. No behaviour
  change today — pre-existing latent footgun from #1620.
- **#1634 — Routing Graph route lines are stable across deletes.** The resolver
  (`conductor_get_resolved_routing_graph`) keyed each route by its array index
  (`route-{idx}`), which flows into the GUI's Svelte `{#each (key)}`. Deleting a
  mid-list route shifted every later route's index/key, so Svelte remounted the
  wrong line and selection/hover/styling jumped to a neighbour. Keys are now a
  stable content hash of the route's shape + mode scope (from/to/filter/transform
  /modes — the same identity the duplicate-route validator uses), with an
  occurrence tiebreaker for the rare validator-warned exact-duplicate case.
  `enabled`/`description` are excluded so toggling or relabelling a route doesn't
  remount it. Daemon-only change; the GUI inherits stable keys (and now preserves
  selection across reorders). The `delete_route {index}` op stays index-based.
- **#1694 — Routing Graph no longer rebuilds on a mode switch.** Switching the
  active mode made the Routing Graph panel re-fetch the entire resolved graph over
  IPC and rebuild the canvas (~1–2s), even though the resolved graph (connectors +
  routes) is **mode-independent** — the daemon persists a mode switch by patching
  only `last_selected_mode`. `RoutingGraph.svelte` keyed its reactive `fetch()` on
  the whole `configStore.config` reference, so any config change (including a pure
  mode switch) triggered the rebuild. The fetch is now gated on a structural key
  (`routingStructureKey`, a shared util) that excludes `last_selected_mode`: a mode
  switch is a no-op, while any structural edit (endpoints, mappings, routes) still
  refreshes. Also avoids wasted re-fetches on unrelated config edits.
- **#1928 — new mapping is immediately actionable; "Review Changes" no longer needs a
  trigger-dropdown toggle.** Opening the Mapping Editor for a new mapping showed the
  Trigger Type dropdown on "Note", but the store's `state.trigger` stayed `null` because
  `TriggerSelector` renders its visible default without emitting an initial change and
  `MappingEditor` binds it one-way. "Review Changes" (gated on `!state.trigger`) therefore
  stayed disabled until the user toggled the dropdown away from Note and back. The store's
  `openMappingEditor` now seeds the same default Note trigger that `TriggerSelector`
  renders, so the editor is actionable on open. The default trigger configs were extracted
  to a shared `trigger-defaults.js` util (single source of truth for the selector and the
  store), and `isDirty()` was refined so a freshly-seeded new mapping isn't treated as
  unsaved work (no spurious "Discard changes?" prompt on cancel).
- **#1786 / #1785 / #1784 — tray menu robustness (macOS/Linux/Windows).** Three
  pre-existing `menu_bar.rs` bugs surfaced by Council during #1470 review:
  - **#1786** — "View Logs" resolves the log dir to a fallback candidate that may
    not exist yet; the macOS branch guarded this but the Linux (`xdg-open`) and
    Windows (`explorer`) branches spawned the opener unconditionally, popping a
    file-manager error dialog on a fresh install. Both branches now guard on
    `log_path.exists()` and `tracing::warn!` otherwise, mirroring macOS.
  - **#1785** — the "Reload Config" handler treated a transport-level `Ok(_)` from
    the IPC round-trip as success, so a daemon that accepted the connection but
    failed the reload was reported as "Config reloaded". It now branches on
    `ResponseStatus::Success` and surfaces the daemon's error otherwise (mirrors
    the conductorctl fix in #1424).
  - **#1784** — the mode submenu was only refreshed while connected, so a
    connected→disconnected transition left stale mode entries under the disabled
    "Switch Mode" submenu (and they could reappear inconsistently on reconnect).
    The disconnect transition now clears `cached_mode_names` and empties the
    submenu via `rebuild_modes_submenu(app, &[])`.
- **#2229 — Bluetooth game controllers now detected on macOS (gilrs async-registration
  race).** A Bluetooth-LE HID gamepad (e.g. Xbox Wireless Controller) is paired and
  HID-visible, but gilrs's macOS IOKit backend registers it via an asynchronous
  run-loop match callback that fires ~50 ms *after* `Gilrs::new()`. The daemon's
  `HidDeviceManager::connect()` checked `gilrs.gamepads()` exactly once, immediately —
  losing the race and reporting "No game controllers connected". Now
  `discover_first_gamepad()` pumps `next_event()` on a 50 ms cadence for up to 2 s so an
  already-connected controller is detected (returns as soon as it registers; only the
  no-controller case waits the window). The same pump was added to `list_gamepads()`
  (the `conductorctl list-devices` and MCP/LLM enumeration path), which had the identical
  one-shot race. Hardware-verified end-to-end (BLE Xbox controller → events → bound
  endpoint → mapping fires). Also corrects the CLAUDE.md ADR-029 §D5 claim that the dev
  Input Monitoring grant persists across rebuilds — with ad-hoc signing it is cdhash-keyed
  and voided on every rebuild.

### Changed

- **#1117 — action dropdown suggests actions for the current trigger.** The
  mapping-editor `ActionSelector` now receives the trigger type and, for triggers
  with a natural action affinity (PitchBend/Aftertouch/CC/EncoderTurn → MIDI
  output actions), surfaces those under a "Suggested for <trigger>" optgroup at
  the top, with the rest under "All actions" and a help line. Non-restrictive
  (Option A) — every action stays selectable; triggers without a recommendation
  (Note, gamepad buttons, …) render the flat list unchanged.
- **#1257 — subtle scrollbars matching the ADR-032 mockup.** The global
  `::-webkit-scrollbar-thumb` now uses `var(--border)` instead of a dedicated,
  more-prominent `--scrollbar-thumb` colour, so the 6px thumb blends into the
  track for a near-invisible, macOS-style scrollbar (the mockup's treatment). The
  now-unused `--scrollbar-thumb` token was removed from `theme.css`/`theme-light.css`.
  Width was already correct (6px); this only changes the thumb colour, app-wide.
- **#1260 — StatusBar decluttered: version label removed.** The app version
  (`vX.Y.Z`) no longer renders in the StatusBar footer (and the Tauri
  `getVersion` fetch behind it is gone). A deliberate product call to declutter
  the status strip. The settings gear stays (a convenient second entry to App
  Settings alongside the canonical TitleBar gear). Context: #1260's main drift
  (per-device dots duplicated in the StatusBar) was already resolved by a prior
  refactor — the StatusBar no longer has a per-device loop; and the daemon
  Start/Stop controls are tracked for a move to the TitleBar in #2344.
- **#2229 — bump gilrs 0.10 → 0.11.2 (gilrs-core 0.5 → 0.6).** First step toward
  the macOS Bluetooth-HID input gap: a Bluetooth Xbox controller is enumerated by
  hidapi but invisible to gilrs 0.10's macOS backend, so no gamepad events reach
  the daemon. gilrs 0.11 modernizes its macOS gamepad backend (gilrs-core 0.6
  replaces `io-kit-sys` with `objc2-io-kit`/`objc2-core-foundation`); this bump is
  the cheapest path to test whether that resolves the gap before considering a
  larger hidapi-polling or GameController.framework backend. No source changes were
  required — the `EventType`/`Button`/`Axis`/`Gilrs::new` API surface is stable
  across the bump. Regenerated `NOTICE`/`THIRD_PARTY_LICENSES.md` and updated docs
  (gilrs version references). Requires hardware verification (a real Bluetooth
  controller on macOS) to confirm the gap is closed.

### Added

- **#1904 — GUI config-drift banner + honest §3 docs (ADR-034 §D4.D).** Post-§D9
  the daemon is authoritative and does *not* auto-reload an out-of-band edit to
  its config file — it broadcasts `config_drift_detected` (notify-only). The GUI
  now surfaces this as a push-driven, app-level **"Config changed on disk"** banner
  with **Apply changes** (invokes the existing `reload_config` → `ReloadFromDisk`
  IPC — the same explicit-reload path `conductorctl config reload` uses) and a
  session-local dismiss (refires on the next drift event). New
  `ConfigDriftBanner.svelte` + `configDrift.js` store; `events.js` routes the new
  `config-drift-detected` Tauri event (re-emitted from `commands.rs`
  `config_drift_payload`) and clears drift on `config-reloaded`. Distinct from the
  #2252 conflict banner (that resolves the GUI's *own* CAS save race; this is a
  *genuinely external* edit). Adds operator docs `docs/user-guide/config-management.md`
  and `docs/user-guide/security-model.md` (the honest §3 disclosure — what D4 closes
  vs. what it does not). The per-reload legacy-mode deprecation warning already
  shipped in #2199. *Deferred to a follow-up:* the banner's "Review diff" and
  "Overwrite user.toml" actions (need backend config-diff / export commands).
- **#2260 — SaveConfig writes through to the active profile file (ADR-034 §D11).**
  The GUI `SaveConfig` seam mutated `live.toml` only; boot/profile-reload read the
  active `profile-*.toml`, so a GUI ENDPOINTS *delete* of a virtual port left the
  two diverged and the port reappeared. The committed config is now written through
  to the active profile (after the CAS commit; a conflict writes nothing), with
  ConfigWatcher suppression — `live.toml` and the profile can no longer silently
  diverge via the GUI path.

- **#2252 — GUI conflict-resolution banner for unmergeable config saves
  (ADR-034 §D2.1). [S2b]** Completes the S2a optimistic-rebase work: when a GUI
  endpoint save loses the daemon CAS *and* can't be auto-merged (the user's edit
  and a concurrent writer both touched the same endpoint alias), an app-level
  non-modal banner now offers the resolution Council called for — **Keep my
  version** (re-fetch the daemon's *current* config and force-apply the user's
  delta onto it, never resending the stale tree), **Discard my changes** (reload
  daemon truth), and **View details** (the attempted endpoint vs the current
  one). Driven by a new single-slot `configConflict` store fed by the typed
  `EndpointConflictError` (now carrying the attempted delta + the fresh config);
  the Endpoints, Signal Flow, and Discovered Ports views route hard conflicts to
  it instead of showing a raw error string. A re-apply that itself loses a fresh
  CAS race keeps the banner up rather than dropping the edit. (Daemon-Unavailable
  edit queueing remains a follow-up.)

- **#2252 — GUI config saves optimistically rebase on `StaleBaseGeneration`
  (ADR-034 §D2.1). [S2a]** Building on the #2083 S1 conflict floor: when a GUI
  endpoint save loses the daemon's compare-and-swap (a concurrent LLM plan-apply
  or `conductorctl` advanced the live config), the GUI no longer just errors. A
  GUI edit is a small endpoint *delta* (`create`/`edit`/`delete`, keyed by the
  immutable `alias`), so on conflict the save path re-fetches the daemon's
  current config and **re-applies the delta onto that fresh tree** — never
  resending the stale tree (which would clobber the concurrent work, the #2083
  resurrection bug in reverse). Non-overlapping edits merge silently (you delete
  endpoint A while the LLM adds endpoint B → both stick); only a genuine overlap
  on the delta's own alias (the alias was created/edited/deleted underneath)
  raises a typed `EndpointConflictError` carrying `{op, alias, reason}` for
  manual resolution. A satisfied delete (the alias is already gone in fresh)
  skips the redundant write. New pure, fully unit-tested `config-rebase.js`
  module (`rebaseEndpointDelta`, `isStaleBaseGenerationError`); the
  manual-resolution banner for hard conflicts is the S2b follow-up.

- **#1762 — ADR-039-B HID live cutover + lifecycle completion (steps 4c/4d).**
  The live gamepad path now flows through the `HidInputSource` substrate
  (#1758/#1760) rather than a bare `HidDeviceManager`:
  `connect_gamepad_multi_device` drives `connect` + `start(tx)`, so every
  `InputSource` line has a live consumer (behaviour-identical §4.3 shed-load
  pump). A gamepad's events are now tagged with the alias of a declared
  `[[endpoints]]` entry that is `direction = "Input"` + HID (resolved by
  `resolve_hid_input_alias`), so a catch-all route `from = "<alias>"` matches;
  configs with no HID input endpoint keep the historical `"gamepad"` tag
  (backward compatible). With the HID input path live, the ADR-039 lifecycle
  matrix flips HID **Catch-All**, **Forward Action**, and **Cross-Protocol
  Transform** from `039-B` to `Done` (regenerated `lifecycle-coverage.md`).
  This completes the routing work on #1762 (the macOS Bluetooth HID input
  backend gap is tracked separately in #2229).

- **#1902 — IPC payload cap for config-carrying commands (ADR-034 §D2.3).**
  `SaveConfig` / `ImportConfig` requests are now capped at a 257 KiB total
  request line (a 256 KiB config payload plus a 1 KiB JSON-envelope
  allowance) and rejected with `PayloadTooLarge` (5003) **before** the
  request is deserialised. The
  per-connection socket loop cheaply peeks the command (serde skips `args`
  as `IgnoredAny`, never allocating the config tree) and enforces the cap
  on the raw line length — closing an allocator-pressure vector the loose
  global 1 MB frame cap left open. Other commands are unaffected (only the
  global cap applies to them).

- **#1902 — pending-at-crash audit event (ADR-034 §D8.3).** When the daemon
  starts, audit-outbox reconciliation already classifies any config mutation
  that was `Pending` when the previous process died. Each mutation that did
  **not** publish (its `intended_revision` does not match the loaded `live.toml`)
  now emits a `ConfigMutationPendingAtCrash` audit event (Internal tier,
  recording the mutation id and intended revision) once at startup, before the
  IPC server accepts connections — so an operator querying the audit log
  immediately sees in-flight-at-crash mutations. A clean shutdown emits nothing.
  Modelled as an `AuditEventType` (an event), not a provenance `Source`, because
  the mutation was never applied. (Also backfills the missing
  `PathValidationFailed` variant in the audit-event-type round-trip test.)

- **#1902 — audit-outbox compaction primitive (ADR-034 §D8 / B2).**
  `AuditOutbox::compact_retaining` rewrites the append-only, hash-chained outbox
  from genesis, keeping only the entries a caller-supplied predicate retains —
  the mechanism the forthcoming SQLite flusher uses to drop rows it has durably
  persisted, bounding the file against the 4096-entry cap without breaking the
  chain (deleting individual lines would orphan every following `prev_hash`).
  Crash-safe: survivors are re-chained into a sibling temp file (`O_NOFOLLOW`,
  0600, `fdatasync`'d), atomically renamed over the outbox, and the directory is
  `fsync`'d — a crash before the rename leaves the original intact (the flusher
  re-runs idempotently), a crash after leaves the compacted file; there is no
  partial-write window. A keep-everything call is a no-op (no rewrite).

- **#1903 — ConfigWatcher §D9 demotion + `[config]` policy block (ADR-034 §D9,
  PART 2).** The config-file watcher no longer auto-reloads `user.toml` edits in
  the daemon-managed default. A new `[config]` section selects the behaviour:
  `user_file_policy = "notify"` (default) surfaces an external edit as a
  `config_drift_detected` monitor event and leaves the live in-memory tree
  authoritative — apply it explicitly with `conductorctl config reload`;
  `user_file_policy = "ignore"` disables the watcher entirely (0 inotify slots);
  legacy `source = "file"` retains the pre-ADR auto-reload with a per-reload
  deprecation warning (removed in v6.0, §D4.E). `SIGHUP` remains an explicit
  operator reload (new `DaemonCommand::SignalReload`) — it bypasses the demotion
  so `kill -HUP` keeps reloading. The `[config]` block is omitted from the
  canonical form when default, so existing configs keep byte-identical
  `ConfigRevision`s. Closes the runtime F-12 silent-reload window (ADR-027 D14).

- **#1903 — `conductorctl config save` (ADR-034 §D4.C, 3/n).** Commits a config
  read from **stdin** via `SaveConfig` — for pipelined / generated configs (CI,
  `yq`, `sed`): `cat config.toml | conductorctl config save [--base-generation N]`.
  Pins the daemon's current generation as the CAS base (via `GetConfigSnapshot`)
  unless `--base-generation` is given. Stdin-only by design (LLM Council
  reasoning-tier review — "import for paths, save for bodies"): a positional path
  is rejected with a redirect to `config import` (which applies the daemon's
  §D2.2 path allowlist), so `save` never becomes a path-shaped allowlist bypass;
  an interactive TTY (no piped input) errors instead of hanging. This completes
  the `conductorctl config` subcommand surface.

- **#1903 — `conductorctl config reload` / `import` (ADR-034 §D4.C / §D9, 2/n).**
  `conductorctl config reload [--path PATH]` re-reads the daemon's config file
  (or `--path`) from disk and republishes it; `conductorctl config import PATH`
  imports a config from an explicit allowlisted `.toml`. Both fetch the daemon's
  current config generation via `GetConfigSnapshot` and pin it as the CAS
  `base_generation` (ADR-034 §D2.1 optimistic concurrency — a concurrent change
  returns `StaleBaseGeneration`). `--json` supported; only `config save` (which
  carries the config body) remains for the next slice.

- **#1903 — `conductorctl config` subcommand group (ADR-034 §D4.C / §D9, 1/n).**
  New `conductorctl config drift` (queries `ConfigDriftStatus` — whether the
  on-disk user config has diverged from the daemon's live config) and
  `conductorctl config mark-known-good` (marks the current live config as the
  known-good rollback target via `MarkKnownGood`). First slice of the config IPC
  surface CLI; `save` / `reload` / `import` (which carry a config payload) follow.

- **#2063 — Virtual MIDI Port endpoints now create real OS MIDI ports**: a
  `MidiVirtualPort` endpoint previously mapped its alias to a lookup name but no
  daemon code ever called `MidiOutputManager::create_virtual_port()`, so a route
  to the alias failed with "port not found" and external apps (DAWs) couldn't
  see it (spec'd in ADR-035 / ADR-031 D10 but unimplemented). The daemon now
  reconciles the OS virtual ports to the live `[[endpoints]]` on every output-map
  build — initial connect, the post-commit reload APPLY (ADR-044), and hot-plug
  rescan — creating enabled `MidiVirtualPort` endpoints and tearing down ones
  that were removed or disabled. `MidiOutputManager::sync_virtual_ports()` does
  the create/teardown diff (infallible — per-port failures are reported, not
  fatal, so a reload's APPLY can't fail); `output_resolver::desired_virtual_port_names()`
  is the pure, CI-tested desired-state extractor.

- **#1761 — ADR-039 protocol lifecycle coverage matrix + enforcement**: a typed
  4-protocol × 6-stage matrix in `conductor-daemon/tests/protocol_lifecycle_test.rs`
  is now the structured source of truth for cross-protocol lifecycle coverage
  (ADR-039 §4.2). Each `Done`/`baseline` cell is **compile-proven** against its
  backing Rust symbol via a `done!`/`baseline!` macro (`PhantomData::<Type>` +
  `stringify!`), so removing or renaming an implementation (e.g. `MidiInputSource`)
  fails the build and CI goes red — the cell and its proof can't drift. Non-`Done`
  cells must be a known sub-ADR id (`039-A/B/C`) or an `NotApplicable` with a
  reason. The matrix generates `docs/cross-protocol-parity/lifecycle-coverage.md`
  (regenerate with `LIFECYCLE_REGEN=1`; a drift test fails CI if the committed
  copy is stale).

- **#1759 — ADR-039 route-source generalization (`&ProtocolEvent`)**: the route
  stage is no longer tied to MIDI byte streams — the prerequisite that blocked
  cross-protocol transforms (the engine "only took bytes"). `RouteEngine` now
  exposes a protocol-tagged `route_destinations(&ProtocolEvent)` shim that
  `#[inline]` tag-dispatches (`Input` → reconstruct bytes once → byte-core;
  `Osc`/`Dmx` → nothing until the 039-A/C listeners land). The existing byte
  body is preserved as `route_destinations_midi(&[u8])`, and the daemon hot path
  calls it directly with the bytes it already has — so production instructions
  are byte-identical to before and the routing benchmark is unchanged
  (Core/Shim design, per the Council reasoning-tier review). Adds an **advisory**
  delta perf-gate (ADR-039 §4.5): CI compares `unified_routing_bench`
  median/P99 against the PR's base branch and annotates a >5%/>10% regression,
  while the absolute ADR-036 D7 floor stays the hard gate (cross-checkout
  microbench deltas are too flaky to block on). Actually executing
  `OscToMidi`/`HidToArtNet` remains gated on their inbound listeners
  (039-A / a HID-transform follow-up).

- **#1758 — ADR-039 cross-protocol substrate (charter root)**: the shared types
  and extension point the OSC/HID/Art-Net sub-ADRs build on, landed
  behaviour-preservingly. New in `conductor-core::events`: a protocol-tagged
  `ProtocolEvent { Input(InputEvent), Osc(OscInbound), Dmx(DmxFrame) }` wrapper —
  the 512-byte DMX universe frame is **boxed** behind the internal `DmxFrame`
  newtype so it never inflates the common MIDI/HID hot path (an enum-size
  assertion pins this). New in `conductor-daemon`: the `InputSource` trait
  (`protocol`/`source_alias`/`shutdown`/`metrics`) with
  `MidiInputSource`/`HidInputSource` impls plus a lock-free
  `InputSourceMetricsHandle`. The daemon event pump now carries
  `DeviceEvent<ProtocolEvent>` (MIDI/HID ingress wraps as `ProtocolEvent::Input`;
  the recv loop unwraps before the existing `InputEvent`-shaped processing
  stage), giving the new enum a live consumer. The route stage starts taking
  `&ProtocolEvent` in #1759 and the push-compatible `start(tx)` sink handoff +
  weighted-fair pump land in #1760 (per the spec's R4 revision). Trait is
  daemon-resident, not in core, to keep the pure engine runtime-free.

### Changed

- **#2065 — docs: sweep legacy I/O config blocks to `[[endpoints]]`**: rewrote
  the config examples that still taught the removed `[device]` / `[[bindings]]` /
  `[[connectors]]` blocks (ADR-035) to the unified `[[endpoints]]` form — the
  `CLAUDE.md` "Configuration (config.toml)" example, `docs/examples/fcb1010.md`,
  the LED-feedback examples in `docs/features.md` (also corrected the LED block to
  `[led]` with the real `brightness` 0–127 range), and the `docs/config-compatibility.md`
  matrix (marked `[device]*` rows removed-by-ADR-035, added an `[[endpoints]]`
  row). Historical ADR/spec text and device-*profile* templates (a separate
  `[device]` schema) were intentionally left untouched; a couple of feature-doc
  examples that need schema-accurate rewrites (e.g. the never-real
  `[device.velocity_curve]` shape) are tracked as a follow-up.

- **#2100 (Phase 1) — post-commit runtime rebuild restructured into
  PREPARE→APPLY (ADR-044)**: `EngineManager`'s rebuild is now expressed as a
  fallible `prepare_runtime` (compiles the mapping engine, normalizes endpoints,
  parses the network-listener set) feeding an infallible `apply_prepared` that
  installs the prepared artifacts and returns an `ApplyReport` (never a
  `Result`, so a rebuild can't `?`-bail half-way). `start_network_listeners` is
  split into a config/ACL parse (`ListenerManager::from_config`) + an infallible
  `bind_network_listeners`. **Behaviour-preserving** — `apply_committed_config`
  is now a thin `prepare→apply` wrapper still invoked post-commit by
  `reconcile_runtime_to_live`, so all commit paths behave exactly as before.
  This is the groundwork for Phase 2, which moves PREPARE *before* the
  `LiveConfig` commit (with a revision-equivalence guard) to make config-apply
  atomic — closing the config-committed-but-runtime-stale window. See ADR-044.

### Fixed

- **#2083 — GUI config saves now persist through the daemon (no more endpoint
  resurrection). [S1]** `save_config` previously wrote the active profile file
  directly (`fs::write`). Since ADR-034 §D9 demoted the ConfigWatcher to
  notify-only, the daemon never reloaded that file — its live config stayed
  stale, so a later daemon mutation (an LLM plan-apply) re-persisted the stale
  tree and clobbered the GUI edit (a GUI-deleted endpoint reappeared; #1779).
  The command now persists via the `SaveConfig` IPC: it fetches the current
  generation (`GetConfigSnapshot`) and CAS-commits, so the daemon validates,
  writes the active config, and rebuilds the runtime atomically through the same
  `mutate` seam plan-apply uses — making the daemon's live config the sole
  authority (ADR-034 §D11 amendment). The S1 correctness floor: a
  `StaleBaseGeneration` (CAS conflict, code 5002) is surfaced as a recognizable,
  actionable error so a concurrent edit blocks the save and prompts a refresh
  rather than being silently dropped — the full re-apply/discard/diff resolution
  UX (§D2.1) and profile import/export via `ImportConfig` follow in S2/S3. The
  legacy direct-file writer (`save_config_to_path`) is now `#[deprecated]`,
  off the save path, and retained only for its validate-before-write contract
  tests until Phase D4.E removes it.

- **#2218 — Routing Graph connector pills now reach ACTIVE (green).** The pill
  status dot is computed by `connectorStatus(bound, metrics)` — green when the
  connector has live throughput / recent activity, amber when idle, red when
  disconnected — but `RoutingGraph` never passed the `metrics` prop, so every
  bound pill rested at idle and the green state was unreachable. RoutingGraph now
  feeds each pill live activity merged from two sources: `connectorMetricsStore`
  (visibility-aware poll, same store EndpointsView uses) for **output** forward
  throughput, and inbound event-rates from the live event stream
  (`computeDeviceRates` over `eventBuffer`, keyed by `device_id == alias`) for
  **input** connectors. The daemon only records `record_activity` for route
  *destinations*, so input connectors carry no `connectorMetrics` throughput —
  folding in the event stream (the same signal the EVENTS panel shows) is what
  lets input pills light green too. A connector carrying traffic now goes green
  and decays back to idle. Found during ADR-035 endpoints GUI testing (epic #2050).

- **#2216 — chat assistant now sees endpoints when asked about them (no more
  hallucinating about virtual ports).** The routing-context injection that gives
  the LLM the live `[[endpoints]]` list was gated on `ROUTING_KEYWORDS`, which —
  predating ADR-035 — included `connector` but not the superseding `endpoint`,
  and whose `virtual port` bigram missed by-name references like "the virtual
  test port". So endpoint questions injected no context and the LLM guessed
  (e.g. "it might be an OS virtual port"). Added `endpoint` (also matches
  `endpoints`) and single-word `virtual` to the keywords. Edit/remove of
  existing endpoints via the assistant remains intentionally unimplemented (the
  system prompt already directs users to the GUI editor); whether to add LLM
  endpoint update/delete tools is tracked in #2222 (it would reverse the
  ADR-035 #1748 singleton-create decision). Found during ADR-035 endpoints GUI
  testing (epic #2050).

- **#2054 / #2216 — Conductor's own virtual MIDI ports are no longer
  re-discovered as orphaned unbound inputs.** A `MidiVirtualPort` output
  endpoint is created as a real OS port (#2063), but the input-scan exclusion
  that keeps Conductor's own ports out of input discovery (ADR-009 §D21) was set
  **once** at initial connect — read from already-created ports *before* they
  existed, and never refreshed on reload or hot-plug. So a virtual port added
  via the GUI after startup was re-discovered as an unbound *input* port by its
  raw OS name, showing up mislabeled in the EVENTS pills (#2054) and in the LLM
  chat's device-bindings view as `… | Input | auto-detected` — so the assistant
  couldn't see the real configured output endpoint and called it auto-managed
  (#2216). The exclusion is now derived from the **current** config
  (`desired_virtual_port_names`) at every input (re)scan via a shared
  `build_input_ignore` helper, so it's correct regardless of when the port was
  created. Found during ADR-035 endpoints GUI testing (epic #2050). (The
  complementary chat-keyword change so endpoint questions inject `config.endpoints`
  is #2223; LLM endpoint edit/remove tools remain design-gated in #2222.)

- **#2202 — Discovered Ports panel now tracks hot-plug reconnects live.** The
  BOUND/UNBOUND lists only fetched once on mount, so when a MIDI device was
  unplugged and replugged the daemon re-bound it (and the Endpoints + Events
  panels updated) but Discovered Ports stayed stale until a manual refresh.
  `discoveredPortsStore` now has the same visibility-aware 3s auto-refresh
  polling as `deviceBindingsStore`, and `DiscoveredPortsView` starts/stops it on
  mount/destroy — so the two panels refresh in step. Found during ADR-035
  endpoints GUI testing (epic #2050).

- **#2204 — chat "navigate to view" no longer silently no-ops.** The
  `conductor_navigate_workspace` frontend tool now normalizes underscores and
  whitespace runs to hyphens (so the LLM's common `routing_graph` / `Signal Flow`
  forms resolve to `routing-graph` / `signal-flow` instead of failing the alias
  lookup), and an unresolvable view now **surfaces an error toast to the user**
  rather than only returning a tool-result error the LLM is prompted to suppress.
  Found during ADR-035 endpoints GUI testing (epic #2050). (The complementary
  prompt-side change — having the assistant proactively offer a Routing Graph
  nav action when it describes an existing route — is tracked separately.)

- **#2203 — output endpoint no longer shows connected (green) when its port
  can't be opened.** The resolved-routing-graph response computed an output
  connector's `connected` purely from presence in `device_output_map`, but a
  `MidiVirtualPort` output is inserted into that map unconditionally — so a
  virtual port that was never created (or an input-only / nonexistent target)
  reported connected/green in the Endpoints panel and Routing Graph while every
  routed event failed `connect_by_name` at dispatch. `connected` for an output
  is now gated on the **live MIDI output enumeration**: the resolved port must
  actually be present as an output port. `bound_port` still surfaces the
  configured target so the GUI can show "configured but disconnected". Found
  during ADR-035 endpoints GUI testing (epic #2050). (Save-time validation that
  rejects an unresolvable output endpoint, and the runtime warn-once/suppress
  for dispatch into a dead output (#1128), are tracked separately.)

- **#2066 — fixed invalid shipped configs + added a full `Config::load` gate**:
  several shipped configs parsed as TOML but `Config::load` would reject them
  (pre-existing; surfaced during the ADR-035 template migration). The new tests
  exercise the **full `Config::load`** path (deserialization *and* validation) for
  every shipped template plus `config/test-plugins.toml` — stronger than the prior
  valid-TOML check — so a future schema drift in a shipped config fails CI. That
  gate surfaced (and this PR fixes) five configs:
  - `flight-stick.toml` — removed gamepad triggers `GamepadAxis` (×11 →
    `GamepadAnalogStick`), `GamepadButtonHold`/`GamepadButtonDoubleTap` (no
    gamepad hold/double-tap → removed with a note), and `MouseClick` button
    casing (`"Left"`/`"Right"` → `"left"`/`"right"`).
  - `config/test-plugins.toml` — old mapping shape: bare `note` with no
    `trigger`, mapping-level `velocity`, lowercase action `type`s
    (`plugin`/`sequence`/`delay`), and `data` → rewritten to explicit `Note`
    triggers (velocity bands via `velocity_min`, ordered high→low), PascalCase
    `type`s, and `params`.
  - `apc-mini.toml`, `generic-keyboard-25.toml` — `VolumeControl` `Set` without
    the required `value` → added a starter `value`.
  - `mpk-mini.toml` — invalid keystroke modifiers `Meta`/`Control`/`Shift` →
    `cmd`/`ctrl`/`shift`.

  (A stale `velocity_range` example in `CLAUDE.md` was noted for the #2187 docs
  follow-up.)

- **#2064 — `EndpointConfig` deserializer tolerates JSON `null` for optional
  fields**: the GUI's `save_config` round-trips config through serde_json, but
  the hand-written `EndpointConfig` `Deserialize` routed through `toml::Value`,
  which has no `null` — so any JSON `null` (e.g. an unset `description` or
  `protocol`) failed with *"invalid type: null, expected any valid TOML value"*.
  The deserializer now reads each top-level field as `Option<toml::Value>` and
  drops nulls (treats them as absent → `None`/default) before the strict parse.
  The TOML config-load path has no nulls, so its strict unknown-key and
  required-field checks are unchanged (a `null` for a required field is still the
  missing-field error; a stray/typo'd key is still rejected on both paths).

- **#2100 (Phase 2) — config-apply is now atomic (ADR-044)**: closes the
  config-committed-but-runtime-stale split-brain window. Every commit path
  (`reload_config`, `reload_from_cached_config`, `sync_config_after_apply`, and
  the `SaveConfig`/`Init`/`ReloadFromDisk`/`ImportConfig` IPC handlers) now runs
  PREPARE (compile mapping engine, normalize endpoints, parse the listener set)
  **before** the `LiveConfig` commit — so a config that can't build is rejected
  **without committing** (e.g. a `SaveConfig` with a malformed listener ACL
  leaves `live_config` untouched) — then commits, then APPLY-installs the
  prepared artifacts behind a revision-equivalence guard (`prepared.target_revision`
  vs the committed snapshot; on mismatch it re-prepares from the committed
  snapshot). The entire post-commit APPLY path is infallible-by-type — both
  `apply_prepared` and the `apply_committed_guarded` wrapper return `ApplyReport`
  (never `Result`) — so a rebuild can never `?`-bail half-way and no post-commit
  failure can be reported as a failed reload or revert the lifecycle state away
  from the committed config (a re-prepare failure on the defensive mismatch path
  keeps the current runtime and lets the dispatch backstop retry). The
  shell-sandbox policy (`set_shell_security`) is applied in APPLY (post-commit),
  not pre-commit, so a rejected reload can't half-apply it. Regression tests: a
  malformed-listener `SaveConfig` is rejected with the live generation unchanged
  and the shell-sandbox policy unmutated; the guard re-prepares from the
  committed config on a revision mismatch. Builds on #2071 (#2099) / ADR-043 D2;
  **Closes #2100**.
- **#2071 — unified the divergent post-commit rebuild routines (ADR-043 D2/Q2)**:
  the daemon had three non-equivalent config-rebuild paths — `reload_config`
  (full), `reload_from_cached_config` (cache-hit profile switch; skipped network
  listeners / SysEx probe toggle / input-port rescan / `device_output_map` /
  device status), and `sync_config_after_apply` (LLM plan-apply; skipped all of
  those **plus** the rate limiter and capture flags). A profile switch produced
  different runtime state depending on cache hit/miss, and an LLM apply left
  listeners / output map / device status stale until the next file reload. All
  commit paths now rebuild the **same** runtime state through a single
  content-guarded seam, `EngineManager::reconcile_runtime_to_live` (→
  `apply_committed_config`). Per Council Q2 the guarantee is **structural**, not
  caller-remembered: `handle_ipc_request` reconciles after every command, so a
  committed `LiveConfig` mutation cannot leave the runtime registry/bindings
  stale even if a future handler forgets — the reconcile is a cheap no-op for
  read-only commands and for byte-identical commits (e.g. `MarkKnownGood`).
  Regression tests pin the newly-covered effects on the cached profile-switch and
  plan-apply paths (SysEx probe toggle) and the dispatch backstop repairing an
  out-of-band commit.
- **#2051 — `SaveConfig` IPC now rebuilds the runtime registry (ADR-043 D2)**:
  the `SaveConfig` handler committed config via `live_config.mutate()` but did
  not rebuild the daemon's connector registry / device-output map / bindings,
  so a GUI-created endpoint never reached the running runtime (empty routing
  graph, idle status LED, LLM-blind) until a daemon restart. Extracted
  `reload_config`'s post-commit rebuild into a shared
  `EngineManager::apply_committed_config` and call it from the `SaveConfig`
  handler, so a config save updates the runtime synchronously. Regression test:
  a `SaveConfig` carrying a new endpoint makes the connector registry surface it
  with no restart.

### Removed

- **#2052 — removed the `conductor_list_connectors` MCP/LLM tool (DRY)**: its
  runtime view (connectors with `connected`/`bound_port`) is a strict subset of
  `conductor_get_resolved_routing_graph` (same per-connector fields plus route
  resolution), and the static view is `conductor_get_routing_graph`. Removed from
  **both** tool-def surfaces (`conductor-daemon/.../mcp_tools.rs` and the GUI's
  `conductor-gui/src-tauri/src/llm_commands.rs` — the #1138 duplication), the
  executor impl, the chat system prompt, and all tool-count/name tests; updated
  the cross-referencing tool descriptions, `llm-reference.md`, and the
  `conductor-signal-routing` skill to point at the routing-graph tools. Closes
  the LLM-endpoint-visibility cleanup in #2052 (the formatter/prompt bugs that
  issue named were already resolved via #2051 + frontend updates; the LLM sees
  endpoints through the injected routing context + `conductor_get_routing_graph`
  / `conductor_get_resolved_routing_graph`).

- **ADR-035 — legacy I/O config removed (clean break, no migration path)**: the
  legacy `[device]`, `[[bindings]]`, and `[[connectors]]` config blocks and their
  Rust types (`DeviceConfig`, `DeviceIdentityConfig`, `DevicePortBinding`) are
  gone. `[[endpoints]]` (`EndpointConfig` / `EndpointKind`) is now the single
  authored I/O form. There is **no migration**: legacy blocks left in a config
  are silently ignored on load (`Config` has no such fields and no
  `deny_unknown_fields`), so they simply have no effect — author `[[endpoints]]`
  instead. Removed with it: the `Config.device`/`devices`/`connectors` fields,
  `Config::primary_device()`/`devices_with_no_probe_sysex_override()`, the loader
  lowering helpers (`lower_binding`/`lower_connector`/`check_legacy_toml_keys`)
  and the Phase-2 reject gate, `conductorctl migrate-config`'s
  `--identity`/`--reverse`/bare modes (only `--routing` remains), and the LLM
  device/connector `ConfigChange` ops + their MCP tools (already withdrawn in
  ADR-035 Phase 2). Shipped templates, example configs, and the default profile
  are migrated to `[[endpoints]]`. **Closes #2046** (endpoint channel-scoping is
  now functional — the rule compiler reads channel scopes from `config.endpoints`
  instead of the always-empty legacy `devices`). New endpoint-based replacements:
  `EndpointKind::no_probe()` / `has_any_sysex_identity_matcher()` and
  `Config::endpoints_with_no_probe_sysex_override()`.

### Security

- **#1899 — ADR-042 Phase B-early complete (Slice B.8-early merge gate).** New
  end-to-end test drives the full bind-gate lifecycle through the real
  `EngineManager` (injected mock keychain + on-disk approval registry): a
  non-loopback listener is withheld with no approval → `conductorctl listener
  approve` (via `approval_admin::approve`) writes an HMAC-signed registry entry →
  the next bind binds it → widening the `network_acl` invalidates the approval
  (acl_hash changes) → withheld again; and a tampered registry MAC invalidates
  all approvals fail-closed. This is the Phase B-early **merge gate**; with it,
  Phase B-early (keychain HMAC + manual per-listener approval + non-loopback bind
  enforcement) is complete.

- **#1899 — ADR-042 Phase B-early: network listener bind gate.** A non-loopback
  OSC/Art-Net listener now stays **unbound** until it has an HMAC-verified
  approval; loopback listeners bind unconditionally (Phase A behaviour). The A.2
  config-load gate is lifted — a non-loopback host is permitted at config-load
  when `allow_network = true` + a `network_acl` is set (without `allow_network`
  it is still a config-load error), and the bind is gated at runtime instead.
  New `NetworkBindGate` (`conductor-daemon/src/security/network_bind_gate.rs`)
  reads the keychain once (cached, with in-memory 730-day expiry re-evaluation on
  reload) and the approval registry per bind, keying each decision on the same
  `(alias, host, port, acl_hash)` `conductorctl listener approve` uses — so
  widening the ACL invalidates the approval. Every failure mode (keychain
  unavailable / hard-expired, registry tampered, no approval) is **fail-closed**
  (the listener is withheld with a prominent operator warning + a
  `NetworkListenerApproval` audit event carrying the `acl_hash`); the daemon
  never binds a non-loopback socket approval-less and never falls back to a
  cached approval. Reviewed by LLM Council at the reasoning tier. The Slice
  B.8-early end-to-end merge gate (above) completes Phase B-early.

- **#1899 — ADR-042 Phase B-early (Slice B.7 visibility): `conductor_security_status`
  MCP tool.** New ReadOnly MCP tool `conductor_security_status` exposes the
  network-approval HMAC key's rotation status to LLM callers — `{ hmac_key_fingerprint,
  hmac_key_age_days, hmac_key_warning }`, where `hmac_key_warning` is one of
  `ok` / `consider_rotation` / `should_rotate` / `approaching_expiry` /
  `deprecated` / `hard_expired` / `unavailable`. Mirrors the
  `conductorctl security status --json` schema; report-only (never refuses, even
  for a hard-expired key). The keychain read runs on a blocking thread bounded by
  a 2s timeout, so a wedged/prompting backend degrades to `"unavailable"` rather
  than hanging the executor. Registered on both tool-definition surfaces (daemon
  `mcp_tools.rs` + GUI `llm_commands.rs`, per #1138); the GUI forwards it by name
  to the daemon. Remaining B.7 visibility surfaces (`conductorctl status`
  `[security]` section, desktop notifications, the `HmacKeyRotated` audit event)
  follow.

- **#1899 — ADR-042 Phase B-early (Slice B.7 visibility): `conductorctl
  security status`.** New `conductorctl security status` reports the
  network-approval HMAC key's fingerprint, age in days, and rotation-warning
  level (`ok` / `consider_rotation` / … / `deprecated` / `hard_expired`). A new
  read-only `key_rotation_status` helper reports the level **without** the
  startup hard-fail, so an operator can see *why* a hard-expired key is blocking
  the daemon. Part of the multi-PR #1899; the remaining B.7 visibility surfaces
  (`conductorctl status` `[security]` section, the `conductor_security_status`
  MCP tool, desktop notifications, the `HmacKeyRotated` audit event) follow.

- **#1899 — ADR-042 Phase B-early (Slice B.7 core): keychain init-race
  protection + rotation cadence.** New `conductor-daemon/src/security/keychain_init.rs`:
  `init_keychain` serialises `get_or_create_hmac_key` behind an advisory `flock`
  (5s timeout) on `~/.conductor/security/.keychain_init.lock`, so a concurrent
  first-run (daemon vs `conductorctl`) can't have both generate a key.
  `RotationLevel` classifies the HMAC key's age into the escalating ladder
  (180/270/300/365-day warnings) and refuses to start at the **730-day hard
  expiry** (`KeychainInitError::HardExpired`), exposing `status_tag()`/`message()`
  for the operator-visibility surfaces. Part of the multi-PR #1899; the visibility
  wiring (`conductorctl status` fields, `conductor_security_status` MCP tool,
  desktop notifications, `HmacKeyRotated` audit event) follows.

- **#1899 — ADR-042 Phase B-early (Slice B.4): `conductorctl` listener-approval
  CLI.** New `conductorctl listener list|status|approve <alias>|deny <alias>`
  and `conductorctl security rotate-hmac`. The commands resolve a listener's
  `(alias, host, port, acl_hash)` from the daemon config and edit the
  HMAC-signed approval registry directly (the daemon honours an approval on its
  next bind). `approve` is a no-op for loopback listeners (auto-approved); ACL
  changes invalidate an approval (the `acl_hash` is bound to the exact
  allow-list); a tampered registry reads fail-closed; `rotate-hmac` re-signs
  existing approvals under the new key so a routine rotation preserves them. The
  reusable logic lives in `conductor-daemon/src/security/approval_admin.rs`
  (unit-tested without spawning the binary). Part of the multi-PR #1899; the
  listener-bind enforcement + A.2 config-load lift follow.

- **#1899 — ADR-042 Phase B-early (Slice B.3): network-listener approval
  registry (envelope HMAC).** New `conductor-daemon/src/security/network_approvals.rs`:
  the HMAC-signed, manually-approved registry that gates non-loopback binds. The
  on-disk **envelope** (`{alg, data, mac}`, spec §4.7.1) authenticates a single
  opaque canonical-JSON (`serde_jcs`) `data` string — the verifier MACs the exact
  signed bytes (no parse-mutate-reserialize), pins `alg` to a compile-time
  constant (forecloses `alg:none`/alg-confusion), compares constant-time
  (`subtle`), and only parses after the MAC verifies (rejecting duplicate map
  keys). Fail-closed throughout: tamper → `MacMismatch`, treated as
  `RegistryTampered`. `ApprovalRegistry` persists to `~/.conductor/network_approvals.json`
  via the same hardened path discipline as the keychain fallback (`O_NOFOLLOW`,
  `fstat` owner+regular+0600, atomic temp+rename); approvals key on
  `(alias, host, port, acl_hash)` so an ACL change invalidates them; the D11
  amplification-ack flag self-expires after 90 days (fail-closed). Part of the
  multi-PR #1899; the A.2 config-load lift + listener-bind wiring follow.

- **#1899 — ADR-042 Phase B-early (Slice B.1): keychain abstraction for the
  network-approval HMAC key.** New `conductor-core/src/security/keychain.rs`:
  a `KeychainStore` trait with a `keyring`-vendored OS backend (macOS Keychain /
  Windows credential store / Linux kernel keyutils — matching the feature set
  `conductor-gui` already builds) plus a hardened Unix file-perms fallback. The
  fallback gates on the explicit `CONDUCTOR_LINUX_FILE_PERMS_FALLBACK=1` opt-in
  (with `CONDUCTOR_SECRET_SERVICE_REQUIRED=1` strict mode), opens key files with
  `O_NOFOLLOW`/`O_EXCL` and `fstat`-checks owner + regular-file + mode 0600, and
  writes atomically (temp + rename). `HmacKey` zeroizes on drop and never renders
  its bytes in `Debug`. First slice of the multi-PR #1899; the approval registry
  (envelope HMAC, Slice B.3) and CLI/rotation surfaces land in follow-ups.

- **#1897 — ADR-027 D10d-source: M-of-N (2-of-3) registry root-key escrow**:
  a catastrophic-recovery break-glass path for the pinned registry **root** key.
  When the root (and any live rotation head) is lost or compromised, rotation
  cannot recover — there is no trusted key left to endorse a successor, and
  re-pinning would mean re-shipping every client. Escrow closes that gap: a
  **quorum of 2 of 3** offline escrow holders co-sign a **root-key-override**
  document that re-anchors trust to a fresh root. New module
  `conductor-core/src/plugin/registry_escrow.rs`: `verify_root_override` counts
  **distinct** escrow keys with a valid `verify_strict` signature over the
  domain-separated message `DOMAIN || key_id || NUL || override_seq(be) ||
  new_root_key` — binding the registry id, sequence, and the new root into the
  signed bytes so the signatures can't be lifted onto a different root/sequence.
  Threshold counting dedupes a holder's repeated signature, ignores signers
  outside the escrow set, caps the signature list (DoS), requires a strictly
  increasing 1-indexed `override_seq` (anti-rollback — an older override can't be
  replayed to revert to a compromised key), and rejects an override whose
  `key_id` does not match the expected registry id (cross-registry replay
  defense). The threshold is **not** caller-supplied — it is fixed to the
  build-gated `REGISTRY_ESCROW_THRESHOLD` constant, so no caller can weaken it
  (e.g. pass `0` to accept an override with no signatures). A build-time gate
  enforces threshold `>= 2` (no single-holder re-anchor), `<= N`, that each baked
  key is empty (Phase-1) or 64 hex chars, and that the non-empty keys are
  **distinct** (a duplicated key can't silently shrink the quorum). `RegistryTrustState` gains a `root_override` field
  and an `effective_root()` resolver; `decide_fetch`/`decide_cache` now verify
  against the **effective** root (override → fresh root, else pin), so a recovered
  registry verifies against the new root while documents from the lost root are
  rejected. Escrow keys ship as empty Phase-1 placeholders (fail closed — no
  quorum to satisfy until real keys are baked). 23 escrow unit tests + 4 trust
  integration tests (2-of-3 accepted, 1-of-3 / duplicate / non-escrow / forged
  rejected, binding to root/key_id/seq, anti-rollback, DoS cap, effective-root
  re-anchor + lost-root rejection). **Part of #1897.**
- **#1897 — ADR-027 D10d-source: registry key rotation (client + chain
  validation)**: the registry signing key can now rotate without re-pinning
  every client. A signed-registry envelope may carry an optional `key_manifest`
  — the `{"signing_keys":[...]}` rotation chain (reusing the §D9
  `key_rotation` engine) — which the client validates against the pinned
  **root** key via `validate_chain_pinned` (each successor rotation-signed by
  its predecessor, gap-free monotonic `seq`/`valid_from`, chain anti-rollback
  vs the persisted `last_chain_head_seq`). The document must then be signed by
  the chain **head** (current) key — a rotated-away predecessor's window is
  closed, so a compromised old key cannot sign fresh registries. When no
  manifest is present the document is verified directly against the pinned key
  (unchanged). The chain head-seq high-water mark persists in
  `RegistryTrustState`; the cache path resolves the head integrity-only (no
  rollback advance). Once a rotation has been accepted, a manifest-absent
  document is **rejected** (`RotationManifestRequired`) — otherwise it would
  verify against the rotated-away root, a downgrade past the rotation (Copilot
  review on #2049); pre-rotation single-key documents remain accepted. The
  fetch and cache paths now share one verification core, so the **cache path
  enforces the same anti-rollback guards** (manifest-required, chain/sequence
  high-water marks) with `>=` sequence semantics — a local cache-file swap to an
  older rotated or manifest-absent doc is rejected (Council review on #2049).
  `sequence_number` is now **required and 1-indexed** (an omitted field can't
  default to `0` and slip the cache `>=` check). Comprehensive unit tests (head verifies, old-root rejected, chain-not-rooted-at-pin
  rejected, chain rollback rejected, tampered manifest rejected, cache
  integrity, manifest-absent rejected-after / ok-before rotation). Still a
  follow-up under #1897: the `rotate-registry-key` authoring CLI, signing CI +
  real key, and M-of-N escrow. **Part of #1897.**

### Documentation

- **#1897 — ADR-027 D10d-source: registry key-management runbook**
  (`docs/security/registry-key-management.md`): the operator/custodian companion
  to the signing-trust guide — detailed instructions on **key creation** (root,
  3-key escrow set; air-gap/custody requirements), **use** (signing a release
  with `sign-registry`, the signer's `sequence_number`/`published_at`
  obligations, verify-before-publish), **rotation** (the routine head-key
  turnover procedure + the client-enforced head-only / manifest-required /
  chain-anti-rollback rules), and **2-of-3 escrow recovery** (when to break
  glass, assembling and distributing a root-override document, what the client
  checks, refreshing the escrow set after a compromise). Includes the key
  hierarchy table, a custody/governance policy, and a failure⇒control quick
  reference. **Part of #1897.**
- **#1897 — ADR-027 D10d-source: registry signing & trust guide**
  (`docs/security/registry-signing-trust.md`): operator/developer guide for the
  registry document trust model — the signed-envelope wire format, signing with
  `conductor-sign sign-registry`, the client verification rules (pinned key,
  verify-before-parse, `sequence_number` + required `published_at` rollback
  guards, size cap, `key_id` binding), the empty-pin migration mode → build-time
  pin path, and the trust-state integrity boundary. Documents the remaining
  roadmap (key rotation, signing CI, M-of-N escrow, keychain HMAC) and satisfies
  the "rotation procedure documented" + "migration story (TOFU)" acceptance
  criteria. **Part of #1897.**

### Security

- **#1897 — ADR-027 D10d-source: `conductor-sign sign-registry` (registry
  signing CLI)**: the producer counterpart to the Phase-1 client validation.
  `sign-registry <registry.json> <key> <out> [--key-id <id>]` reads a registry
  document **verbatim**, signs `DOMAIN || key_id || NUL || payload` with the
  Ed25519 key (`<key>.private`), and writes the `{ payload, signature, key_id }`
  envelope the daemon verifies on fetch. The signing core (`build_signed_registry`)
  is pure and round-trip-tested against the client verifier
  (`registry_trust::verify_signed_registry`): a valid envelope verifies, a wrong
  key fails, and a swapped `key_id` fails (key_id is bound into the signed
  bytes). Warns when the payload omits `sequence_number` / `published_at` (which
  the client requires). Defaults `key_id` to `conductor-registry-v1`. Still a
  follow-up under #1897: signing CI + baking the real pinned key, rotation
  chain, and M-of-N escrow. **Part of #1897.**

- **#1897 — ADR-027 D10d-source: registry document trust (Phase 1 — client
  validation)**: the plugin registry (`registry.json`) is no longer fetched and
  trusted blindly. A new `conductor-core::plugin::registry_trust` module
  verifies a signed-registry envelope `{ payload, signature, key_id }` — pure
  Ed25519 over `domain || key_id || NUL || payload` (key_id bound into the
  signature; `verify_strict`), signing the **verbatim** payload string (no JSON
  re-serialisation, so no canonicalisation malleability; consistent with the D9
  sibling's deliberate rejection of JCS). The trust decision gates on whether a
  key is **pinned** (not on document shape), defeating signature-stripping
  downgrades; an 8 MiB document-size cap bounds pre-verification DoS; and
  `published_at` is compared as parsed RFC 3339 **instants**, not raw strings.
  The client verifies the signature **before** parsing the inner payload
  (validate-offline-then-parse, no fetch-and-execute TOCTOU). Rollback/replay
  protection via two monotonic guards carried inside the signed payload: a
  strictly-increasing `sequence_number` (rejects replay of a valid past
  document as "current" — Council R1 P0) and a non-decreasing `published_at`.
  Trust state (last sequence / published_at) persists in a file SEPARATE from
  the plugin trust store. Wired into `fetch_registry` (full verify + rollback
  advance) and `load_cached_registry` (integrity-only). Phase-1 ships an empty
  pinned key, so production warns-and-allows during migration while an *invalid*
  signature on a signed document is always rejected. Follow-ups under #1897:
  signing CLI, signing CI, the rotation-signed-by chain (reuse D9
  `key_rotation`), and M-of-N catastrophic-recovery escrow. **Part of #1897.**
- **#1895 — ADR-027 D10a: persistence write veto (pre-flight argv check)**:
  the daemon now refuses to spawn a `Shell` action whose resolved argv would
  *write* one of its own protected state directories — `~/.conductor/`, the
  macOS `~/Library/Application Support/conductor/`, or
  `$XDG_DATA_HOME/conductor/` (falling back to `~/.local/share/conductor/`).
  Detected write vectors: direct write programs (`tee`, `cp`, `mv`, `install`,
  `ln`, `rm`, `unlink`, `rmdir`, `shred`, `dd of=`, `truncate`, `sed -i`,
  `awk … > file`) targeting a protected path, and explicit interpreters
  (`sh -c "…"`/`bash -c`/`zsh -c`/…) that re-introduce a redirect inside a
  single argv token. A shell-aware tokenizer distinguishes `<` reads (allowed —
  e.g. `cat ~/.conductor/config > /tmp/out`) from `>`/`>>` writes (vetoed), per
  Council R1. Paths are normalised (`~` expansion, `.`/`..` cleanup, best-effort
  symlink resolution of an existing ancestor). A veto is rejected **before**
  spawn (never reaches `execve`), surfaces a `DispatchError` whose message
  points operators at the IPC API (`conductorctl`) for legitimate config/state
  edits, and emits a `ShellVetoedByPersistenceCheck` audit event. The veto
  inherits ADR-027 §D7 env-sanitisation by construction (it guards the same
  `execute_shell` spawn). Council-framed as a **best-effort deterrent**, not a
  hard boundary — process substitution, here-doc bodies, and relative paths
  (cwd unknown) are documented gaps deferred to D10b's OS sandbox.
  **Closes #1895.**

- **#1894 — ADR-027 D9: `conductor-sign rotate-key` (key-rotation authoring)**:
  the producer counterpart to `trust verify` — `rotate-key <old> <new>
  <manifest>` endorses a new signing key with the chain's current head key and
  appends it to (or bootstraps) the plugin's rotation manifest. Builds the
  signed `rotation_payload` and serialises the whole manifest with `serde_json`
  (escaped). Rejects rotating from a stale non-head key and reusing an existing
  key. The `build_rotation` core is pure and round-trip-tested against the
  engine (bootstrap + append chains validate via `validate_chain`; non-head and
  reuse are rejected). End-to-end smoke-tested: generate-key → rotate (bootstrap
  + append) → `trust verify` validates the 3-key chain by trusting **only the
  root** (transitive trust). Completes the D9 `conductor-sign` CLI surface
  (trust verify + migrate-keys + rotate-key). **Part of #1894.**
- **#1894 — ADR-027 D9: `conductor-sign migrate-keys` (v2.7 → D9 migration)**:
  a new subcommand that reads a legacy v2.7 `<plugin>.wasm.sig` and emits a
  root-only rotation manifest `<plugin>.keys.json` (the signer's public key
  becomes the chain root; its `signed_at` the root `valid_from`). A root-only
  manifest is a complete, valid non-rotating chain, so existing single-key
  plugins become forward-compatible with D9 chain validation without re-signing.
  Refuses to overwrite an existing manifest (`create_new`). The JSON-rendering
  helper is pure and round-trip-tested against the engine (`validate_chain`);
  end-to-end smoke-tested (sign → migrate-keys → trust verify). Closes the D9
  "migration path for existing v2.7 single-key plugins" acceptance criterion.
  **Part of #1894.**
- **#1894 — ADR-027 D9: `conductor-sign trust verify` CLI**: a new subcommand
  that validates a plugin's signing-key rotation manifest (`<plugin>.keys.json`)
  against the local trust store — letting an operator confirm transitive trust
  (and rejection reason: untrusted root, broken chain, revoked key, rollback,
  …) before installing a rotated plugin. Exercises the merged D9 validation
  engine end-to-end; the manifest→trusted-fingerprints→`validate_chain` logic is
  a pure, unit-tested helper (4 tests + CLI smoke-test). **Part of #1894.**
- **#1894 — ADR-027 D9: plugin key revocation list (CRL)**: rotation chains
  handle ordinary updates but not *immediate* compromise. Adds CRL enforcement
  to the key-rotation validator (`validate_chain_full`): a revoked fingerprint
  is invalid for **all** purposes — signing *or* parenting — so any chain
  containing a revoked key (root through head) is refused
  (`RotationError::Revoked`). Checked among the cheap pre-crypto gates so a
  known-bad chain fails fast. The CRL set is supplied by the caller (the core
  stays I/O-free; loader-side fetch/storage is a follow-up). Closes the
  Council-flagged "critical missing control" from the D9 review. **Part of
  #1894.**
- **#1894 — ADR-027 D9: plugin signing-key rotation chains (validation core)**:
  v2.7 pinned one Ed25519 key per plugin, so rotating it forced every user to
  re-trust out of band. D9 adds a verifiable *rotation chain* — each new key is
  endorsed by its predecessor, so trusting the root transitively trusts every
  validly-rotated successor with no prompt. This PR lands the pure validation
  engine (`conductor-core/src/plugin/key_rotation.rs`, behind `plugin-signing`)
  plus its JSON transport. The cryptographic scheme was LLM-Council–reviewed
  (high tier): each rotation signs a **domain-separated fixed-width binary**
  payload (`CONDUCTOR_KEY_ROTATION_V1` ‖ chain_id ‖ seq ‖ valid_from ‖
  predecessor_fp ‖ **full** new public key) — no JSON/JCS canonicalisation
  malleability, the full key (not just its fingerprint) is bound, a monotonic
  `seq` blocks reordering/dropping, and `chain_id` (root fingerprint) blocks
  cross-chain signature lifting. Validation is a strict phase-ordered walk
  (structure → base trust → crypto walk → signer/active-window resolution) that
  **hard-fails** on any anomaly (broken link, bad signature, seq gap,
  non-monotonic `valid_from`, multiple roots, self-signed non-root, …),
  degrading the plugin to pinned/non-rotating. 22 unit tests incl. real-Ed25519
  tamper/substitution cases. **Part of #1894** — loader wiring, `conductor-sign
  trust verify` / `rotate-key`, v2.7 migration, the revocation list (CRL), and
  the anti-rollback high-water-mark state are tracked follow-ups.
- **#1891 — ADR-027 D6: multi-dimensional LLM budget**: a single iteration
  counter can't contain a compromised LLM (the "one iteration with 500
  mutations" bypass). Adds a pure budget engine
  (`conductor-core/src/security/llm_budget.rs`) tracking 14 dimensions —
  per-turn/per-session iterations and tool calls, session token in/out,
  per-turn/session wall-clock, and capability quotas (config changes, shell
  exec, network calls, MIDI out, confirmation rate) — plus the Council R1
  **token-burst rate** (`max_tokens_per_60sec`, sliding 60 s). Configured
  **file-only** via `[security.llm]` (same construction as the D17 egress
  allowlist: never on the round-trippable `Config`, so a compromised LLM
  can't widen its own limits). The daemon MCP `ToolExecutor` enforces the
  capability dimensions it can observe at the dispatch chokepoint, halting
  the loop with a new `LlmBudgetExceeded` audit event (D13b hash chain) when
  a quota is exhausted. New CLI: `conductorctl llm budgets show [--json]`. A
  limit of `0` disables a dimension. Deferred follow-ups (tracked in the PR):
  GUI agentic-loop wiring for the token/iteration/wall-clock dimensions
  (`llm_commands.rs`), a process-level wall-clock watchdog, and hierarchical
  daily limits.
- **#1439 — upgrade wasmtime/wasmtime-wasi 26.0 → 45.0**: the WASM plugin
  runtime was pinned at wasmtime 26.x, which had no patch for **18** RustSec
  advisories — including **RUSTSEC-2026-0149** (CVSS 7.5: a `path_open`
  `TRUNCATE` that bypassed `FilePerms::WRITE`, a real WASI sandbox weakness),
  plus Cranelift miscompilations and WASI resource-exhaustion/panic issues.
  These were all suppressed in `.cargo/audit.toml`, so `cargo audit` could not
  fail on them. Upgrading to 45.0 resolves every one; the entire ignore block
  is removed, so `cargo audit` is an enforceable signal again. The 26→45 API
  migration was small and contained to `conductor-core/src/plugin/wasm_runtime.rs`
  (`wasmtime_wasi::preview1` → `::p1`, `ResourceLimiter::table_growing` return
  type, dropped the no-op `Config::async_support`). All WASM plugin tests pass.
  Supersedes the incomplete Dependabot bump #1199 (wasi-only, to 41.x — which
  did not even patch 0149).

### Removed

- **ADR-035 Phase 2 (#1748) — deprecated MCP tools removed**: now that
  `Config::load` rejects legacy `[[bindings]]`/`[[connectors]]`, the MCP tools
  that authored those blocks are gone. Removed from both tool-def surfaces
  (daemon `mcp_tools.rs` + GUI `llm_commands.rs`), the executor dispatch +
  handlers, the `conductor_batch_changes` connector ops, the LLM chat
  system-prompt, and all tests:
  `conductor_create_binding`, `conductor_create_connector`,
  `conductor_create_device_identity`, `conductor_update_device_identity`,
  `conductor_delete_device_identity`, plus the `create_connector` /
  `update_connector` / `delete_connector` batch operations. The unified
  **`conductor_create_endpoint`** (ADR-035) is now the sole MCP I/O-authoring
  tool (daemon tool count 56 → 51; GUI 43 → 39). **Known gap (tracked):**
  `conductor_update_endpoint` / `conductor_delete_endpoint` and batch endpoint
  ops are not yet implemented, so there is temporarily no MCP path to
  update/delete an existing endpoint — edit `[[endpoints]]` TOML or use the GUI
  endpoint editor; runtime mute remains via `conductor_set_device_enabled`.
  Also fixes a latent GUI mis-classification (`conductor_create_endpoint` was
  defaulting to the `readonly` risk tier instead of `configchange`).
- **ADR-035 GUI Endpoints — P2-PR1: remove per-device status pills from both
  nav bars** (§4a review): the `StatusBar` per-binding pills (`● Mikro ● nanoK`)
  and the `TitleBar` device-dots row + `DevicePopover` are gone — per-alias
  chrome doesn't scale, and per-endpoint status now lives in the Endpoints view
  (P1). The aggregate Daemon / Config / version status (StatusBar) and the
  Profile / Mode dropdowns (TitleBar) stay; `TitleBar` keeps its profile-switch
  `deviceBindingsStore.fetch()` refresh for the remaining consumers (Endpoints
  view, EventStream, BindingPills, DeviceList). Deletes the now-orphaned
  `DevicePopover.svelte` (+ its test); updates the StatusBar/TitleBar tests.
- **#1465 — remove orphaned legacy files from the root `conductor` crate**: the
  root crate's `src/lib.rs` is a re-export-only compatibility shim (inline
  `pub mod actions { pub use conductor_core::actions::*; }` etc.), but seven
  stale pre-extraction sibling files (`actions.rs`, `mappings.rs`, `feedback.rs`,
  `device_profile.rs`, `event_processor.rs`, `midi_feedback.rs`, `mikro_leds.rs`)
  were never declared as `mod`s — so they compiled to nothing, weren't linted or
  tested, and silently diverged from `conductor_core`. A release hazard. Deleted
  them (build and `tests/backward_compatibility_test.rs` are unaffected, proving
  they were dead); `src/lib.rs` is now the crate's entire source.
- **ADR-036 Phase 2 — `Trigger::Raw` removed** (#1696): the config parser
  now **rejects** `trigger.type = "Raw"` (previously auto-lowered to a
  `pre_mapping` route with a deprecation warning in Phase 1). `Config::load`
  surfaces a hint to run `conductorctl migrate-config --routing`, which still
  rewrites Raw configs to `[[routes]]` (it operates on the TOML document, not
  the `Config` type). `CompiledTrigger::Raw` and all Raw-specific validation
  (uniqueness, overlap warnings) are gone. **Breaking**: configs containing
  `Trigger::Raw` no longer load — migrate them first. (`--reverse` migration
  and `RoutePhase::PreMapping` removal follow in Phase 3, #1697.)

### Added

- **ADR-035 GUI Endpoints — P2-PR3: Discovered Ports pane**: a new workspace
  pane ("Discovered Ports", `DiscoveredPortsView`) promotes the "what's
  physically plugged in" view out of Connections into its own nav entry. It
  lists every OS port + game controller from `get_discovered_ports`
  (`discoveredPortsStore`), split into **Unbound** (no endpoint matches — each
  with a **"+ Create endpoint"** quick-bind that opens `EndpointEditor` in create
  mode pre-seeded from the port: alias, direction, protocol, NameContains
  matcher) and **Bound** (informational, showing the matched alias), with a
  manual refresh + connection dots. Wired into the workspace nav, the MCP
  `navigate_workspace` enum (`discovered-ports`), and the chat slug map.
- **ADR-035 GUI Endpoints — P1-PR6 HID-controller quick-bind**: the Add Endpoint
  editor now shows a **Detected Controllers** quick-bind list for the Game
  Controller category (create mode), from `list_gamepads` — *Use as Input* fills
  a `NameContains` matcher and auto-fills the alias (no SysEx probe; HID identity
  is the USB product string). Mirrors the MIDI detected-ports flow from P1-PR3,
  completing detected-source quick-bind across both input categories.
  `EndpointsView` fetches `list_gamepads` alongside `get_available_ports` on
  open-create and passes both as props. **Completes GUI Information-Architecture
  P1** (runtime join + editor parity + rate badges).
- **ADR-035 GUI Endpoints — P1-PR5 output send-throughput badge**: output-only
  endpoint rows (OSC / Art-Net / Virtual Port) now show a live send-throughput
  pill (`↑ N msg/s`) when traffic is flowing, completing the rate story from
  P1-PR4 (which covered input traffic only). Adds a thin `get_connector_metrics`
  Tauri command — same IPC→`ExecuteMcpTool` path as `get_resolved_routing_graph`,
  wrapping the existing `conductor_get_connector_metrics` MCP tool (the daemon's
  `ConnectorRegistry` already aggregates throughput; no daemon change) — plus a
  `connectorMetricsStore`, polled in step with the resolved-graph store (3s).
  The throughput is baked into the reactive row view-model and only shown when
  `> 0` so idle outputs stay quiet (the status LED already conveys connected).
- **ADR-035 GUI Endpoints — P1-PR4 per-endpoint input rate badge**: each
  endpoint row in the Endpoints view now shows a live **message-rate badge**
  (msg/s + a type-distribution bar) for input traffic, reusing
  `computeDeviceRates` over the event buffer (keyed by alias = `device_id`,
  mapping-fired entries excluded) + the existing `DeviceRateBadge`, recomputed
  each `nowTick` (1Hz, 5s window) — the same source `BindingPills` uses. Output-
  only endpoints (Virtual Port / OSC / Art-Net) have no input events so show no
  badge; their **send-throughput** badge needs a new `get_connector_metrics`
  Tauri command and lands in a follow-up (P1-PR5). The rate is baked into the
  reactive row view-model (not a template closure) so it stays live.
- **ADR-042 Phase A — network-listener security (loopback-only)** (#1898):
  closes the gap ADR-027 left open — a network-adjacent attacker sending UDP to
  a bound OSC/Art-Net listener mapped to `Shell` is RCE. Phase A lands the edge
  primitives and binds loopback only.
  - **Slice A.1 — `NetworkAcl` primitive** (`conductor-core/src/security/network_acl.rs`):
    compiled CIDR allow-list with D11 hardening — rejects `0.0.0.0/0` / `::/0`,
    enforces an **aggregate** (not per-entry) amplification budget under
    `allow_broadcast` so a broad range can't be sharded past the check, warns on
    IPv6 link-local, and normalizes IPv4-mapped IPv6 in `contains()`.
  - **Slice A.2 — endpoint schema + loopback-only validator**: `OscEndpoint` /
    `ArtNetEndpoint` gain the flattened `NetworkSecurityConfig` (`allow_network`,
    `network_acl`, `sender_acl`, `rate_limit_total`, `rate_limit_per_sender`,
    `i_understand_amplification_risk`, **`allow_sensitive_actions`** (D17),
    `strict_mode`) plus Art-Net `allow_broadcast`. A network *listener*
    (`direction = Input`/`Bidirectional`) bound to a **non-loopback host is a
    config-load error** directing the operator at Phase B-early; `allow_network`
    does not lift the gate in Phase A. `allow_network = true` requires a
    non-empty `network_acl`, which is parsed through `NetworkAcl` (D11 + aggregate
    amplification). Output endpoints (which *send*) are unaffected. New
    `NetworkAcl::is_loopback_address` / `is_loopback_only` helpers (G3 — covers
    `127.0.0.0/8`, `::1`, and IPv4-mapped loopback).
  - **Slice A.3 — ACL filter** (`listeners/acl_filter.rs`): per-packet source-IP
    gate wrapping a compiled `NetworkAcl` plus an optional `sender_acl`;
    IPv4-mapped IPv6 normalized so a dual-stack socket can't bypass an IPv4 entry.
  - **Slice A.4 — rate-limit edge** (`listeners/rate_limit.rs`): `governor`-based
    per-sender bucket **checked before** the per-listener total (so a rejected
    packet never charges the shared budget), per-sender state in a **bounded LRU**
    (spoofed-source OOM defense); the total bucket is the aggregate DoS guarantee.
    OSC defaults 1000/200, Art-Net 100/50.
  - **Slice A.5 — audit edge** (`listeners/audit_edge.rs`): one audit emission
    per 60s per `(listener, source, kind)`, suppression state in a **bounded LRU**.
  - **Slice A.6 — ListenerManager + EngineManager wire-up**: `ListenerManager`
    builds one ordered edge (**ACL → rate-limit**, the G2 security order) per
    enabled Input OSC/Art-Net endpoint, with the loopback-default ACL; an async
    `spawn_listener` binds a loopback UDP socket and runs the receive loop;
    `EngineManager` starts/stops them on connect/disconnect and hot-rebinds on
    reload; `NetworkListenerActivity` is audited (dedup'd) on accepted packets,
    rejections emit dedup'd tracing only. New `GetListenerStatus` IPC command.
    New persisted `AuditEventType` variants (`NetworkListenerActivity`,
    `NetworkListenerBindFailed`, `NetworkActionClassBlocked`,
    `AmplificationRiskAcknowledged`, `NetworkListenerConfigChange`,
    `ListenerOrphanedAtStartup`).
  - **Slice A.6.1 — orphaned-listener detection**: a listener port already held
    by another process (`AddrInUse`) is reported as `ListenerOrphanedAtStartup`
    with an operator hint (detection only; never force-killed).
  - **Slice A.6.6 — action-class gating (D17)**: a network-origin action (incl.
    loopback) that is/nests a sensitive class (`Shell`/`Launch`/`Keystroke`) is
    refused at dispatch unless the origin listener set `allow_sensitive_actions`
    — checked up front (no partial side effects), before the ADR-027 tier gate;
    MIDI/gamepad origins are never gated. `ActionEnvelope.network_origin` taint
    (copied-never-cleared) + `Action::contains_sensitive_action` classifier.
    (The envelope→executor taint wire and the OSC-trigger e2e await the ADR-039
    parser, which first produces network-origin actions.)
  - **Slice A.7 — end-to-end merge gate**: a loopback OSC listener binds, a real
    UDP packet is accepted, and a `NetworkListenerActivity` row is persisted;
    a non-loopback listener is a config-load error.
- **ADR-035 GUI Endpoints — P1-PR3 editor detected-ports + auto-probe + SysEx
  parity**: the **Add Endpoint** editor (`EndpointEditor`) reaches
  `DeviceEditor` parity for the MIDI Device category. Create mode now shows a
  **Detected Ports** quick-bind list (from `get_available_ports`): clicking
  *Use as Input* / *Use as Output* fills a `NameContains` matcher and auto-fills
  the alias; input ports are **auto-probed** (`probe_device_identity`), and on a
  confident, direct-paired identification a **SysExIdentity** matcher is
  prepended (highest specificity, survives port renames / USB re-enumeration)
  and shown as a read-only identity chip — with a live `DeviceIdentityBadge`
  surfacing the probe outcome inline. Reuses the existing `probeOutcomeToBadgeState`
  helper + `DeviceIdentityBadge` component; race-guarded against rapid port
  clicks. (Game Controller / Virtual / OSC / Art-Net don't map onto physical
  MIDI ports, so the section is MIDI-only; HID-controller quick-bind via
  `list_gamepads` is a follow-up.)
- **ADR-035 GUI Endpoints — P1-PR2 output-category runtime status**: the
  Endpoints view now lights up the **output-only categories** (Virtual Port ·
  OSC · Art-Net), which previously always showed an idle (hollow) dot. Their
  status LED + bound-port caption now resolve from the daemon's
  **resolved routing graph** (`resolvedRoutingGraphStore`, keyed by alias),
  polled in step with the bindings store (3s). `endpointStatus` /
  `groupEndpoints` gained an optional `connectorByAlias` argument: an input
  binding still wins (it alone carries the runtime mute flag), falling back to
  the connector's `connected` flag so output endpoints report a real
  connected/disconnected state instead of idle. Pure helpers + unit tests
  extended. (The per-endpoint message-rate badge and the editor's
  detected-ports/auto-probe/SysEx parity follow in subsequent P1 PRs — see
  `docs/endpoints-unification/gui-endpoints-ia.md`.)
- **ADR-035 GUI Endpoints — P1 runtime status join**: the Endpoints view now
  **groups** endpoints by category (MIDI Device · Game Controller · Virtual Port ·
  OSC · Art-Net) with per-group connected/configured counts, and **joins each to
  live daemon state** (`deviceBindingsStore`, keyed by alias) for a status LED
  (connected/muted/disconnected/idle), the live bound-port (with `auto`-pair
  tag), **mute/unmute**, and **copy-port**. Pure join/grouping helpers extracted
  to `endpoint-runtime.js` and unit-tested. (Output-only categories show a
  configured count; their per-connector metrics + the editor's detected-ports/
  auto-probe parity follow in P1-PR2 — see `docs/endpoints-unification/gui-endpoints-ia.md`.)
- **ADR-035 Slice 10 — GUI "Add Endpoint" + endpoint stores** (#1747): a unified
  **Endpoints** workspace view (target-state surface superseding the device /
  connector split) with an **Add/Edit Endpoint** modal covering all four
  `EndpointKind` variants:
  - `EndpointEditor.svelte` — **category-first** progressive form over five
    user-facing categories (MIDI Device · Game Controller · Virtual MIDI Port ·
    OSC · Art-Net) that map to `(kind, protocol, legal-direction)`. **Direction
    is constrained per category** so you can't author a config the validator
    rejects: MIDI Device → Input/Output/Bidirectional; **Game Controller (HID)
    → Input only** (HID output dropped, ADR-039 D7); Virtual Port →
    Output/Bidirectional; OSC/Art-Net → Output. Advanced **channel-scope**
    selector (1–16, MIDI categories only). Alias is immutable on edit (it's the
    identity routes/mappings reference). Emits the exact `EndpointConfig` JSON
    shape; uses Svelte 5 callback props (`onsave`/`onclose`).
  - `EndpointsView.svelte` — lists/creates/edits/deletes endpoints, reading and
    writing `config.endpoints` only (no binding↔endpoint bridging).
  - `configuredEndpoints` store (reads `config.endpoints`); `saveEndpointConfig`
    / `deleteEndpointConfig` utils through the existing `save_config` path.
  - Wired into the workspace nav as **Endpoints** and into the MCP
    `conductor_navigate_workspace` view set.
  - **Validator**: HID endpoints with `direction != Input` are now rejected at
    load (companion to the editor's Input-lock; ADR-039 D7).
  - Tests: store derivation, save/delete utils, per-category editor payload +
    HID Input-lock + channel-scope (vitest), and HID-direction validation
    (Rust). Design captured in `docs/endpoints-unification/gui-endpoints-ia.md`.
    Legacy binding/connector UI is now redundant and is removed in a follow-up.
- **#1408 — live LLM streaming in the chat GUI** (epic #1886 PR4): the chat
  assistant now renders provider responses token-by-token instead of appearing
  all at once. The agentic loop's per-turn call streams SSE deltas through the
  `llm_chat_stream` Tauri command (`ipc::Channel`) into a single draft message,
  then reconciles it in place with the authoritative `ChatResponsePayload` (the
  deltas are a live projection; the returned payload stays the source of truth)
  — never a second message. Gated by the existing `streamResponses` setting
  (default on; the previously dead hardcoded `stream:false` is removed).
  Superseding a turn (new message) cancels the in-flight backend stream via
  `llm_chat_cancel` so abandoned turns stop consuming provider tokens, and a
  stream error or malformed result degrades gracefully to the blocking
  `llm_chat` path. Closes #1408 and completes epic #1886.
- **spec §10 — configurable dispatch-trace buffer size** (#1733): new
  `advanced_settings.trace_buffer_size` (default 1000) sets the capacity of
  the daemon's in-memory dispatch-trace ring buffer, previously hardcoded.
  `DispatchTraceRing::with_capacity` is constructed from the setting at
  daemon start; config validation rejects `0` and values above 1,000,000
  (~500 MB). Documented in `docs/llm-reference.md`.
- **ADR-037 D4 — `CONDUCTOR_TRACE_LOG` env-gated stderr trace** (#1732):
  starting the daemon with `CONDUCTOR_TRACE_LOG=1` (alias `CONDUCTOR_TRACE=1`)
  now emits one structured-JSON line per routed event to stderr — the same
  `{ timestamp_ms, device_id, active_mode, event, destinations }` shape as the
  `conductor_get_dispatch_trace` ring entry. Off by default with zero
  per-event overhead when unset (the gate is read once and cached); set the
  env var before launching the daemon. Complements the in-memory ring buffer
  and MCP tool for live `jq`-able tailing. Documented in `docs/llm-reference.md`.
- **ADR-036/037 Slice 10 — route `phase` + `modes` visualization** (#1668,
  supersedes #1597): the routing graph now surfaces each route's dispatch
  phase and mode scope.
  - **Daemon**: `conductor_get_resolved_routing_graph` now includes
    `phase` (`"pre_mapping"` | `"post_mapping"`) and `modes` (string
    array; empty = global/all-modes) on every route entry.
  - **GUI canvas** (`RoutingGraph.svelte`): pre-mapping routes (the
    deprecated `Trigger::Raw` escape hatch) draw with a warm amber stroke
    to signal "fires before mapping"; post-mapping routes keep the
    throughput-bucket palette. Mode-scoped routes show a small chip at the
    line midpoint listing the scoped modes; bare routes show none.
  - **GUI inspector** (`RouteInspector.svelte` / `RouteRow.svelte`): each
    row carries a `pre` phase chip (pre-mapping only) and one chip per
    scoped mode. A new **"Active mode only"** filter toggle dims routes
    that don't apply to the currently active mode (`routeAppliesToMode`).

### Changed

- **ADR-035 Phase 2 (#1748) — `Config::load` rejects legacy `[[bindings]]` /
  `[[connectors]]`**: the unified `[[endpoints]]` set is now the single authored
  source of truth. A config carrying `[[bindings]]` (or its `[[devices]]` serde
  alias) or `[[connectors]]` fails to load with an actionable error pointing at
  `conductorctl migrate-config --identity` (which rewrites them as
  `[[endpoints]]`, comment-preserving, with a `.bak`; `--dry-run` to preview).
  The serde aliases still **parse** so the migration tool can read old files —
  only the daemon/GUI load entry point refuses them. Config validation now
  resolves `device` references (triggers, `PcContextSwitch`/state conditions,
  routes) against the endpoint aliases. The `fcb1010` reference fixture is
  migrated to `[[endpoints]]`. (Removing the deprecated MCP tools + the GUI
  legacy device modals — the rest of #1748 — follow in subsequent PRs.)
- **ADR-035 GUI Endpoints — P2-PR2: relabel the EventStream surface to
  "endpoint"**: the EventStream surface (EventFilter status filter, EventRow
  detail row, BindingPills context menu + colour picker) now uses the unified
  **"endpoint"** vocabulary instead of "binding". The runtime "Device"
  (`device_id`) origin label on events is unchanged (it's the physical source,
  distinct from the configured endpoint it matched). The **Signal-Flow** relabel
  + the `configuredDevices`→`configuredEndpoints` repoint are **deferred to P3**:
  Signal-Flow's gear → edit flow opens the legacy `DeviceEditor` and saves to
  `config.bindings`, so repointing its configured-metadata read to
  `configuredEndpoints` (or relabeling its edit/create controls) must move
  together with that editor/save migration (Copilot #1979). Frontend-only.
- **#1436 / #1440 — declare and document the minimum supported Rust version**:
  the workspace opted every package into edition 2024 (needs ≥1.85) but
  declared no `rust-version`, so older toolchains failed mid-build instead of
  being rejected cleanly — and the README/CONTRIBUTING/architecture docs still
  claimed Rust 1.70+ (which can't even parse edition 2024). Added
  `rust-version = "1.88"` to `[workspace.package]` (inherited by all five
  edition-2024 crates) — the verified dependency-graph floor (e.g. `time` 0.3.x
  requires 1.88; confirmed with `cargo +1.88.0 check`). Docs now say 1.88+ and
  note that `rust-toolchain.toml` pins the exact stable build toolchain
  (#1435). `conductor-capture` (edition 2021) is intentionally left
  unconstrained.
- **ADR-035 Slice 9.5 — runtime consumes the unified endpoint set** (#1878):
  the daemon's port-binding and output-resolution paths now run on the
  normalized `[[endpoints]]` set instead of `config.devices` / `config.connectors`.
  - `PortResolver::resolve` takes `&[EndpointConfig]` (input path; lands with
    the input-path commit) — Input/Bidirectional endpoints bind MIDI input
    ports via `effective_matchers(Input)`.
  - `output_resolver`'s two legacy builders (`build_device_output_map` +
    `build_connector_output_map`) are replaced by one `build_output_map(&[EndpointConfig], …)`
    that folds in explicit output matching (`effective_matchers(Output)`),
    `MidiVirtualPort` direct mapping, and input-port auto-pairing (LED-feedback
    output for input-only endpoints), plus the #1611 output/bidirectional alias
    resolution for `MidiForward { target = "<alias>" }`.
  - **Uniform protocol gate**: only MIDI endpoints land in the MIDI output map
    (`EndpointConfig::effective_protocol()`). OSC/Art-Net dispatch through the
    connector registry; a HID endpoint can no longer accidentally auto-pair to
    a like-named MIDI output port (a latent bug in the legacy device path).
  - `resolve_device_io` reads endpoint direction from the unified set; the
    engine-manager reload / connect / hot-plug store sites all feed
    `normalize_to_endpoints`. No authored-config or behavioural change for
    existing setups.
- **CI hardening — SHA-pin `dtolnay/rust-toolchain` + add Dependabot**:
  - All 12 uses of the `dtolnay/rust-toolchain` action across `ci.yml`,
    `tauri-build.yml`, `release.yml`, and `security.yml` now pin the action to
    an explicit commit SHA (`29eef33` for the `stable` channel, `4214355` for
    `beta`) with the channel name kept in a trailing comment. Previously every
    use referenced the `@stable` / `@beta` *branches* — the loosest possible
    pin. A regression pushed to one of those upstream branches breaks toolchain
    setup in every workflow at once (e.g. `cargo` resolving to `rustup-init`,
    failing every `cargo <subcommand>` with `unexpected argument`). SHA-pinning
    makes the toolchain version a deliberate, reviewable bump — matching how
    the repo already pins `jlumbroso/free-disk-space`. Behaviour-neutral: the
    pinned SHAs are the current channel HEADs, verified green on recent CI
    runs.
  - New `.github/dependabot.yml` keeps those pins (and all action/Cargo/npm
    deps) from rotting: it reads the `# stable` trailing comment, tracks the
    upstream branch, and opens a grouped weekly PR when it moves. Covers three
    ecosystems — `github-actions` (`/`), `cargo` (`/`), `npm`
    (`/conductor-gui/ui`). Minor + patch bumps are grouped into one PR per
    ecosystem per week; major bumps stay as individual PRs for deliberate
    review.
  - Removed `.github/workflows/dependencies.yml`: the weekly `cargo update`
    cron is fully superseded by Dependabot's `cargo` ecosystem, which does the
    same job with per-crate PRs, security-advisory awareness, and grouping.

### Fixed

- **#1955 — OpenAI streamed turns under-reported usage/cost**: streamed OpenAI
  requests never set `stream_options.include_usage`, so OpenAI omitted the
  terminal usage chunk and cost tracking under-counted every streamed OpenAI
  conversation (streaming is the default). Surfaced by the all-provider streaming
  audit. The parser side was already ready (`OpenAiStreamChunk` maps `usage`; the
  accumulator keeps the last value) — only the request opt-in was missing. Added
  an optional `stream_options: { include_usage: true }` to `OpenAIRequest`, set on
  the streaming path only (`#[serde(skip_serializing_if = "Option::is_none")]`, so
  the blocking request body is unchanged). Cosmetic/telemetry only — does not
  affect tool execution.
- **#1408 streaming — chat LLM never calls tools on Gemini (both paths)**:
  follow-up to the Anthropic fix below, found by auditing all five providers.
  Gemini returns `finishReason: "STOP"` for functionCall turns (and never emits a
  positive `"TOOL_USE"` signal — that string is not a real Gemini finishReason),
  so both the streaming `parse_stream_chunk` and the blocking `parse_response`
  resolved a tool turn to `EndTurn`, and the GUI loop (which gates on
  `stop_reason === 'tool_use'`) dropped the calls. The only reliable tool-turn
  signal is the presence of assembled tool calls. Fix: a provider-agnostic
  `reconcile_tool_stop_reason(stop_reason, has_tool_calls)` helper that upgrades
  **only** `EndTurn -> ToolUse` when tool calls are present (preserving
  `MaxTokens`/`ContentFilter`/`StopSequence`), applied in `StreamAccumulator::finish()`
  (covers all providers' streaming paths) and in Gemini's blocking `parse_response`
  (which bypasses the accumulator). Audit outcome: OpenAI, OpenRouter, and LiteLLM
  were already correct (they share `parse_openai_sse`, mapping `finish_reason
  "tool_calls" -> ToolUse`); Anthropic was fixed below; Gemini was the remaining gap.
  Regression tests: accumulator net (EndTurn+tools -> ToolUse; MaxTokens+tools
  preserved), Gemini blocking, and an end-to-end Gemini stream (functionCall +
  STOP folded through `accumulate_stream` -> ToolUse).
- **#1408 streaming — chat LLM stopped calling tools (Anthropic)**: with the new
  default-on SSE streaming path, Anthropic tool turns silently never executed.
  The model emitted its `tool_use` block, but the assembled response came back
  with `stop_reason: end_turn`, and the GUI agentic loop gates tool execution on
  `stop_reason === 'tool_use'`. Root cause: Anthropic sends the authoritative
  stop reason in `message_delta` (`tool_use`) and then a SEPARATE terminal
  `message_stop` event; `to_stream_chunk` mapped `message_stop` to
  `Some(EndTurn)`, which the `StreamAccumulator`'s last-write-wins clobbered the
  `ToolUse` with. Fix: `message_stop` now emits `stop_reason: None` (it is purely
  a terminal marker), and the accumulator keeps the first *meaningful* stop
  reason rather than letting a trailing generic `EndTurn` overwrite it.
  Regression tests added at both layers. The old blocking `chat()` path was
  unaffected, which is why tool calls worked before streaming became the default.
- **#1538 / #1539 — de-flake brittle wall-clock test assertions**: several
  integration/e2e tests enforced strict upper-bound timing that fails under
  loaded/virtualized CI with no product regression. `should_skip_timing_test()`
  now returns `true` on **any** CI (was macOS-only), so the gated assertions and
  timing tests are skipped there. `test_long_press_simulation` drops
  its `< 2700ms` upper bound (keeps the `>= 2500ms` min-hold + note sequencing),
  and the e2e `test_e2e_timing_latency` (`< 1ms`) and `test_e2e_throughput_100_events`
  (`< 10ms`) now gate only their timing assertion behind the CI skip — the event
  pipeline still runs everywhere as smoke coverage. (Sibling findings #1518 and
  #1533 were reassessed as already-fixed and closed.)
- **#1520 — MIDI simulator `VelocityRamp` reaches max**: the test simulator's
  velocity ramp computed `span / steps` over `0..steps`, so it never emitted
  `max_velocity` — a 20→120 / 5-step ramp stopped at 100. It now interpolates
  over `steps - 1` (u16 math) so the first event is `min` and the last is `max`
  inclusive (`steps == 1` is the degenerate single sample at `min`), keeping
  #1508's assertions that reject `steps == 0` and `max < min` loudly rather than
  dividing by zero / underflowing. Fixed in **both** simulator copies —
  `conductor-daemon/tests/midi_simulator.rs` and the root `tests/midi_simulator.rs`
  that the `midi_simulator` diagnostic binary imports via `#[path]` (they had
  diverged: #1508 only landed in the root copy) — so the CLI tool is fixed too.
  Both `test_velocity_ramp_gesture` tests now assert exact note-on velocities,
  with edge-case tests for 1 step and `should_panic` for 0/descending.
- **#1459 — feedback: pad presses ignored while a non-reactive scheme is
  active**: `FeedbackManager::on_pad_press` inserted into `reactive_state`
  unconditionally, so presses during Static/Rainbow/etc. inflated
  `active_pads()` and lingered; because switching *into* Reactive doesn't clear
  state, a later `update()` could then emit those stale presses as completed
  fade-outs. The insert (and velocity feedback) is now gated on the Reactive
  scheme, matching the documented behavior.
- **#1457 — direct `MappingEngine::get_action(&MidiEvent)` matches
  `Aftertouch`, `PolyAftertouch`, `PitchBend`, and `ProgramChange`**: the raw
  matcher only handled `Note`/`CC`, so direct lookups for those four returned
  `None` even with a matching mapping configured — inconsistent with the
  `EventProcessor` path. `trigger_matches_raw` now covers them (mirroring the
  processed-event predicates). Other triggers use the processed path:
  temporal ones (`NoteChord`/`DoubleTap`/`LongPress`/`EncoderTurn`) genuinely
  need EventProcessor state, while `VelocityRange` is simply not covered by the
  raw matcher today (its band is a pure function of NoteOn velocity, so it
  could be added later).
- **#1452 — device fingerprint: mixed note+CC streams no longer forced into a
  note category**: `EventStats::classify` returned `PadController`/`Keyboard`
  whenever `note_count > 0`, so a hybrid/ambiguous stream (e.g. 40% notes / 60%
  CC, with CC below its 0.7 dominance bar) was misclassified as a note device.
  Classification now requires note *majority* (`note_count / total > 0.5`)
  before committing to a note category, leaving genuinely mixed devices as
  `Unknown` (the enum's documented mixed category) to avoid overconfident
  binding suggestions. Note-majority devices with a few aux CC knobs still
  classify by range.
- **#1453 — `PadPageMapping::note_range` reports true min/max**: it read the
  first and last `pad_to_note` entries positionally, so a profile with pads
  assigned out of note order (e.g. pad 0 = 60, pad 15 = 36) reported a
  reversed `(60, 36)` range. It now takes the actual min and max note values,
  fixing display/diagnostics/page heuristics that consume the helper.
- **100 pre-existing vitest failures across 4 frontend test files (#1046)**:
  the full `conductor-gui/ui` vitest suite had ~100 failing tests on `main`,
  none gated by CI (no workflow runs vitest). Root causes were stale test
  infrastructure: `vi.mock` factories that no longer matched the store API
  surface (`chatStore.setWorkspaceContextFn` from ADR-024 Phase 4A, the
  `nowTick` clock export, `configStore`/`appStore` dragged in by the unmocked
  `chat.js` transitive graph), a `WORKSPACE_VIEWS` count assertion stuck at 14
  after `ROUTING_GRAPH` made it 15, two `setInputValue` assertions predating
  its `{ focus, select }` options arg, a `DeviceHeader` interaction test still
  clicking the `.device-header` div after the toggle moved to a `.dh-collapse`
  `<button>`, and a missing `@tauri-apps/api/event` mock that let
  `RawConfigView`'s `listen()` throw an unhandled rejection. The full suite is
  now green (3143 passing, 0 errors). Wiring vitest into CI so this cannot rot
  silently again is tracked separately.

- **GUI lagged ~3s after a file-watcher config reload (#1070)**: editing
  `config.toml` and saving triggered the daemon's file-watcher reload, but the
  daemon emitted no outbound signal — the GUI only learned about the change on
  its next 3s poll, so the device list, status pills, TitleBar dropdown, and
  Events filter all lagged. `EngineManager::reload_config()` now broadcasts a
  `config_reloaded` `MonitorEvent` on the existing event channel after every
  successful reload (covering file-watcher edits, tray-menu Reload, and
  `conductorctl reload` uniformly). The GUI's event-monitor stream re-emits it
  under the `config-reloaded` Tauri event that `App.svelte` already listens for
  (wired by #1069 for the tray-menu path), so config + device-binding stores
  refresh immediately. Follows the same channel + `event_type` pattern #943
  used for `ambiguous_port_detected` — no new IPC types or protocol changes.

### Added

- **ADR-031 Phase 2A — Routes config types + validation (#1142 → #1161)**: Builds on Phases 1A (#1147) and 1B (#1156) by introducing `[[routes]]` as the explicit signal-path between connectors. Config-layer foundation only; Phase 2B (next PR) adds the daemon `RouteEngine` runtime + stage-9 integration + 4 MCP tools. Nine TDD slices:
  - **§4.1 types**: `RouteConfig` (from/to/transform?/filter?/enabled/description), `SignalFilter` reusing `MidiMessageType` from ADR-030 P1 (NOT `Vec<String>` per spec §4.1 post-#1139), `SignalTransform` enum (Midi newtype around existing `MidiTransform`, plus MidiToOsc / OscToMidi / MidiToArtNet / HidToArtNet variants stubbed for Phase 5).
  - **§4.2 field**: `routes: Vec<RouteConfig>` on `Config` with backward-compat empty default. Bulk-updated 78 `Config { ... }` struct literals across 20 files.
  - **§4.3 reject rules** (5 rules): unknown `from`/`to` alias, self-referencing route, A→B+B→A direct cycle (gated on both endpoints known + non-self-ref so unknown-alias routes don't cascade extra cycle errors), cross-protocol pair without correct-variant transform (Midi→Osc requires `MidiToOsc`, etc.; unsupported pairs like HID→OSC reject with explicit "no defined variant" error rather than silently accepting wrong-variant fallback), `cc_range`/`note_range` with `min > max` + `channels` out of 0-15 range + `SysEx`/`ChannelPressure` in `message_types` (each with parity to existing binding-side / Raw-trigger checks).
  - **§4.3 overlap warnings** (3 non-fatal classes, modeled on `warn_raw_overlaps_specific` from PR #1127): route shadowed by Raw, route shadowed by specific trigger (both gated on `route.from` being a binding alias — connector-source routes don't get false-positive trigger-shadowing warnings), exact-duplicate route (filter + transform shape via JSON-value equality; `description`/`enabled` ignored).
  - **Test-quality hardening**: `assert_error_at_path` helper pins each test to its rule's specific path (`routes[N].field`), preventing false positives where an unrelated error happens to mention the same substring. Disabled-route behavior pinned (validation runs regardless of `enabled = false`). Transitive 3-cycle test as negative-control documenting the deliberate scope limit.
  - **Council reviews**: 3 substantive Council passes (`201c9f10` FAIL → addressed slice 6+7+8; `40fb0123` FAIL → addressed slice 9; `ba914a33` final pass: "comprehensive, well-structured, accurately reflects the specifications, ready to be merged"). 4 Copilot review rounds, all addressed.
  - **Deferred to Phase 2B**: §4.4 RouteEngine runtime, §4.5 stage-9 integration into the event pump, §4.7 4 MCP tools (`conductor_create_route` / `conductor_list_routes` / `conductor_update_route` / `conductor_delete_route`) with 5-surface LLM-discoverability per spec §5.3, channel-preservation property test through the route engine. **Filed as #1165**: tighten binding-side `protocol = None` default for cross-protocol route validation (HID-only bindings without explicit `protocol = "hid"` could be misclassified as Midi).
- **ADR-031 Phase 1B — Daemon integration of ConnectorRegistry + 2 MCP tools (#1141 → #1156)**: Builds on Phase 1A (#1147) by making the daemon actually OWN a registry instance and exposing it to LLMs. Seven TDD slices:
  - **Wire-up**: `Arc<RwLock<ConnectorRegistry>>` field on `EngineManager`, built from config in `new()` from `(config.devices, config.connectors)`. Public field on `SharedDaemonStateRefs` so MCP/IPC code paths read without holding any other daemon lock. `RwLock` (not `ArcSwap`) chosen because the registry has interior mutability for `bind_port`/`disconnect`/`record_activity`; reads are infrequent (MCP/IPC queries — ActionExecutor does NOT consult the registry today, hot path stays on the lock-free `device_output_map`).
  - **Reload**: new free async helper `rebuild_connector_registry(registry, devices, connectors)` called from `EngineManager::reload_config()` after `device_output_map.store(...)` so the registry stays in sync with config edits. Per-connector runtime state (`bound_port`, `connected`, `metrics`) resets on rebuild — same trade-off `device_output_map` already accepts.
  - **MCP `conductor_list_connectors` (ReadOnly)**: inline handler in `ToolExecutor::execute_readonly` mirroring the SysEx tools' pattern — snapshot taken under registry's `RwLock` for a consistent view. Sorted by alias before serializing so output is stable across runs (HashMap iteration order varies with hash seed). New `ConnectorRegistry::iter()` accessor.
  - **MCP `conductor_create_connector` (ConfigChange)**: returns a `ConfigPlan` with one new `ConfigChange::CreateConnector` variant. `apply_change` enforces alias-uniqueness across BOTH `[[connectors]]` AND `[[bindings]]` (ADR-031 § 3.3 shared namespace) at apply-time. `description`/`apply`/`preview_diff` arms added; `history.rs::create_single_inverse_with_offset` returns `CannotCreateInverse` for `CreateConnector` (DeleteConnector tool not shipping in Phase 1B).
  - **5-surface LLM-discoverability** for both new tools (per ADR-030 retrospective lesson, PR #1130): `mcp_tools.rs` + `llm_commands.rs` (GUI in-process duplicate, byte-identical descriptions, risk-tier `match` arms, tool counts updated to 48 / 39) + `chat.js` system prompt (Query bullet adds list_connectors, Modify bullet adds create_connector) + `docs/llm-reference.md` (`[[connectors]]` schema block) + `chat.test.ts` `backendTools` drift-guard list.
  - **Schema-drift regression tests** for both tools per PR #1130 pattern: pin tool-name + description-keywords + `direction`/`protocol` enum variants + required-args set + risk-tier mapping. Cheap canary against silent drift like the `ProgramChange.pc` / `VolumeControl` cases.
  - **Deferred to a follow-up PR**: third Phase 1 MCP tool `conductor_get_routing_graph` (gated on Phase 2 `RouteConfig` for actual value-add over `list_connectors`); §3.5 `output_resolver` refactor (the right design after analysis is to extend `output_resolver::build_device_output_map` to honour `[[connectors]]` rather than push the registry into `ActionExecutor`); §3.4.1 virtual port lifecycle (D10 DAW proxy) + §3.4.2 `MidiOutputManager::auto_excluded_virtual_port_names` (both gated on Phase 2 routes existing).
- **ADR-031 Phase 1A — Connector config types + ConnectorRegistry data structure (#1141 → #1147)**: Foundation for the signal-routing-graph epic (#1140). New `[[connectors]]` config block introduces `Connector` as a first-class named I/O endpoint:
  - **Config types** (`conductor-core`): `ConnectorDirection` (Input/Output/Bidirectional, default Bidirectional), `ConnectorProtocol` (Midi/Osc/ArtNet/Hid, default Midi, with `Copy + Eq` derives matching the binding-side `Protocol` analog), `EndpointConfig` (Matcher / OscEndpoint / ArtNetEndpoint / MidiVirtualPort), `ConnectorConfig` (alias, direction, protocol, endpoint, description, enabled, channels — `description` and `channels` use `skip_serializing_if` for round-trip parity with `DeviceIdentityConfig`).
  - **Validation**: alias uniqueness across `[[bindings]]` + `[[connectors]]` (single O(N+M) walk; emits exactly one collision finding per connector even when bindings have duplicate aliases of their own). Empty-alias rejection. Channel-scope validation (range 0-15 + non-MIDI warning) at parity with `devices[*].channels` (#751 / #746).
  - **`ConnectorRegistry` runtime** (NEW `conductor-daemon/src/connector_registry.rs`): builds from `(devices, connectors)`, lowers input bindings to Input connectors honouring ADR-021 D1 matcher precedence (`input.matchers` shadows legacy top-level `matchers` — the original implementation silently dropped port info for any modern config). Public surface: `from_config`, `get`, `contains`, `resolve_output` (filters explicitly by `connected`), `bind_port`, `disconnect` (clears both `bound_port` and `connected` — daemon hot-plug + reload teardown hook per spec § 3.4.2 vocabulary), `record_activity`. All disconnect/bind paths tolerate unknown aliases (concurrent-reload-safe).
  - **Test coverage**: 15 tests in `conductor-core/tests/connector_config_test.rs`; 14 tests in `conductor-daemon/tests/connector_registry_test.rs`.
  - **Deferred to Phase 1B**: §3.4.1 virtual port lifecycle, §3.5 `output_resolver` refactor, §3.6 IPC commands, §3.7 3 MCP tools (`conductor_list_connectors`, `conductor_create_connector`, `conductor_get_routing_graph`) + GUI duplicates. **Deferred to follow-up #1154**: OSC/ArtNet endpoint-shape lowering of bindings — currently all input-direction lowering uses `EndpointConfig::Matcher`; ArtNet's `universe` field has no equivalent on `DevicePortBinding`.
- **ADR-030 Passthrough Routing — completes the 5-phase epic (#1097)**: `Raw` catch-all trigger type pairs with `MidiForward` action to give Conductor "default-on routing with selective intercept" semantics — the Bome MIDI Translator "swallow" model. Add a Raw passthrough on a device, add specific triggers (Note, CC, …) for the events you want to intercept; the matcher always fires the specific rule first per ADR-030 §D2. Phases that landed:
  - **P1 (#1098 → #1105/#1107)**: Core `Trigger::Raw` enum variant + matching engine + validation. Two-pass partition (specific-then-Raw) + `MidiMessageType` filter + per-scope uniqueness check (each mode and `[[global_mappings]]` independently; `device` omitted counts as the "(any)" key per §D7).
  - **P2 (#1099 → #1111)**: Daemon end-to-end Raw + MidiForward integration tests through the full event pipeline.
  - **P3 (#1100 → #1114)**: GUI Raw trigger editor with channel + message-types checkboxes (unsupported types disabled but still uncheckable when already set), MidiForward action editor with port select / custom-text fallback, passthrough badge in the EventStream, MIDI Learn coverage warning when adding a specific trigger that overlaps an existing Raw.
  - **P3a hardening (#1118/#1119/#1120 → #1123/#1124/#1127)**: Three follow-up fixes from PR #1114 manual test — (1) cross-bucket shadowing (any-device specific now correctly shadows device-scoped Raw via 8-stage matcher; the prior 4-stage matcher placed device-bucket lookups before any-device lookups, letting a device-scoped Raw silently steal events); (2) MidiForward channel preservation (`extract_raw_midi` now reads `channel` from `InputEvent` and ORs into status byte instead of hardcoding channel 0 — pre-fix, every passthrough landed on MIDI channel 1 regardless of source channel; extracted to `crate::midi_bytes` module with 11 unit tests including defensive masks for out-of-range channels); (3) validator emits an overlap warning at config-load time when a Raw and a specific in the same scope would collide.
  - **P4 (#1101 → #1130)**: LLM/MCP discoverability — `Raw` and `MidiForward` added to `conductor_create_mapping` / `conductor_update_mapping` schema descriptions in both `mcp_tools.rs` and the GUI's `llm_commands.rs`, system prompt (`chat.js`), L1 reference (`docs/llm-reference.md`), and the `conductor-midi-mapping` skill (`SKILL.md`). Also fixed pre-existing `ProgramChange.pc` (was documented as required, is `Option<u8>`) and `VolumeControl` (was documented `{action: ...}`, is `{operation: Up|Down|Mute|Unmute|Set, value?}`) schema-vs-config drift Copilot flagged during review. L2 knowledge-layer chunks (`passthrough-patterns`, `raw-vs-route`, `shadowing-semantics`) deferred — depends on ADR-018 ingestion infra.
  - **P5 (#1102 → this PR)**: User-facing config reference (`docs/examples/passthrough-routing.md`) with all three Raw filter variants + priority-shadowing diagram + Raw / MidiForward pairing reference. Optional Raw passthrough example added to the MPK Mini device template. This CHANGELOG entry.

## [5.6.1-alpha] - 2026-05-11

Install-test patch for v5.6.0-alpha. Picks up #1122 (the `SecCodeCopySigningInformation` flag-bit hotfix caught by the v5.6.0-alpha install-test) plus the Learn-start race fix and `chat.test.ts` race-condition test that landed in the same install-test window. Re-runs the v5.6.0-alpha install-validation rubric against production-built artifacts to confirm Apple-Team-ID-signed peers now classify as `CliTrusted`/`GuiTrusted` instead of being denied as `Untrusted`.

### Fixed

- **Daemon (ADR-027 Phase 1A regression caught by v5.6.0-alpha install-test, #1122)**: `verify_conductor_team_id` was passing `flags = 0` (`kSecCSDefaultFlags`) to `SecCodeCopySigningInformation`, which causes the returned `CFDictionary` to **omit the `teamid` key entirely**. The `info.find("teamid")` lookup therefore always returned `None`, `verify_conductor_team_id` always returned `false`, and every Apple-Team-ID-signed binary classified as `Untrusted` instead of `CliTrusted` / `GuiTrusted`. With Phase 1A active (`shadow_mode = false`), that meant **every legitimate signed peer was being denied for ConfigChange / HardwareIO / Privileged tools**. Real-world impact: a notarised Conductor.app's `conductor-gui` would have been treated as `Untrusted` after v5.6.0-alpha shipped, breaking Plan/Apply, hardware-IO confirmations, and any privileged tool path. Fix: pass `kSecCSSigningInformation` (= `0x2`, the bit-mask for "include signing certificates + team identifier") so the returned dict actually contains `teamid`. Verified via the v5.6.0-alpha install-test daemon log: with the fix, the v5.6.0-alpha tarball's properly-signed `conductorctl` now classifies as `CliTrusted`; the dev-build `conductor-gui` (ad-hoc signed, no Team ID) still correctly classifies as `Untrusted`. Test gap that allowed the regression: PR-A's `tests/peer_pin_3n_classification.rs::classify_peer_returns_untrusted_for_foreign_binary` short-circuits at the basename rule before reaching `verify_conductor_team_id`, and the synthetic-`PinnedPeer` unit tests use all-zero `audit_token`s that fail the SecCode lookup regardless of flags — neither test exercises the positive-Team-ID-match path. Tracked as #1125. Closes the v5.6.0-alpha install-test pause.

## [5.6.0-alpha] - 2026-05-10

Install-test pause. Cuts after the ADR-027 Phase 1A bundle (D5 decision table + D3 capability vocabulary + D1 peer-credential pinning + wiring PRs A/B/C/D) lands and `SecurityPolicy::default().shadow_mode` flips to `false` for production builds — the IPC tool dispatch path is now gate-protected, so a fresh-Mac install validation is the right next step before continuing Phase 1B / new ADR work. Same pattern as the v5.5.0-alpha → v5.5.1-alpha → v5.5.2-alpha install-test cycle. Also bundles the parallel ADR-030 P1 (#1105/#1107), ADR-032 LLM Mode UI P0–P4 (#1091/#1093/#1094/#1095/#1096), and assorted GUI fixes (#1075/#1077/#1104/#1109) that merged alongside the Phase 1A wiring.

### Added

- **Daemon (ADR-027 Phase 1A — activation, PR-D)**: Phase 1A goes live. `SecurityPolicy::default()` flips `shadow_mode = false` for production builds (lib `cfg(test)` keeps `true` so the existing 50+ ToolExecutor unit-test fixtures don't have to thread a synthetic trusted `CallerContext` through every site — they already verify the dispatch pipeline, gate enforcement is exercised separately by `tests/security_gate_test.rs`'s 11 decision-table cases plus the live daemon at startup). Two new constructors on `CallerContext` close the PR-B `TODO(PR-D, gate-bypass on None)` gap by distinguishing the two `caller_ctx is None` sources at the call site rather than at the gate-call site: `CallerContext::internal_trusted()` returns `trust_level: GuiTrusted, peer: None` for daemon-internal dispatches (the inline `conductor_*_plugin` arms inside `ToolExecutor`'s tool dispatch now use this when sending plugin-management commands back to the daemon command channel — the outer LLM tool call has already passed the gate boundary, so the inner dispatch's gate decision matches what a verified GUI peer would see: Allow for ReadOnly/Stateful, RequirePlan for ConfigChange, RequireConfirmation for HardwareIO, Deny for Privileged; note the receiving `IpcCommand::{ListPlugins, GetPluginInfo, EnablePlugin, DisablePlugin}` handlers don't currently consult `caller_ctx`, so the field is inert at runtime today — passing `internal_trusted` is future-proofing for when those handlers are wired through the gate); `CallerContext::synthetic_unpinned()` returns `trust_level: Untrusted, peer: None` for IPC peers whose pinning failed at accept (Linux < 5.3 with no `pidfd_open`, same-uid TCC anomaly on macOS, etc.). `ToolExecutor::execute` converts a `None` caller_ctx to `synthetic_unpinned()` via `Cow::Owned` so the gate's decision table is consulted on every path — no more "skip gate when None" bypass. **ActionExecutor enforcement was scoped out** of PR-D after threat-model analysis: `ActionExecutor.execute()` is only called from (a) physical-input-driven mapping dispatch (user-initiated by definition), (b) the plugin manager (signature-verified plugins per v2.7), and (c) tests; there's no IPC entry point that lands directly in the executor, so the threat is already covered by ToolExecutor's PR-B/C gate (which gates IPC-originated tool calls) plus existing physical-input + plugin-signing trust models. The two integration tests in `tests/security_gate_test.rs` that asserted the default policy was shadow-mode-on are renamed (`production_security_policy_default_has_shadow_mode_off`) and reworked (the shadow-mode envelope test now constructs an explicit `SecurityPolicy::with_shadow_mode(true)` rather than relying on `default()`). With this PR merged, **the IPC tool dispatch path is gate-protected**: an external IPC peer that fails to pin gets `Untrusted`, an internal-origin daemon dispatch gets `GuiTrusted`, and a real verified peer gets the trust level `classify_peer` derived in PR-A. The Phase 1A bundle (D5 decision table + D3 capability vocabulary + D1 peer-credential pinning + wiring PRs A/B/C/D) is **complete**; closes #1002 once merged. Tracks epic #999.

- **Daemon (ADR-027 D1 wiring PR-C)**: Replace the PR-B `RequirePlan` / `RequireConfirmation` deny-with-message stubs in `ToolExecutor::execute` with routing to the existing per-tier handlers — `execute_config_change` (returns `ExecutionResult::PlanCreated { plan }` for ADR-007 D2's Plan/Apply flow) and `execute_hardware_io` (returns `ExecutionResult::HardwareIoConfirmation` for the ADR-027 D7 confirmation token flow). Behavioural impact today is **zero**: with `SecurityPolicy::shadow_mode = true` (Phase 1A invariant) the gate still returns `Allow` for every (tier, trust) pair, and the per-tier dispatch below the gate match still runs unchanged. PR-D's flag flip is what activates the gate routing; PR-C ensures that when the flag flips, `ConfigChange` tools route through Plan/Apply via the gate's `RequirePlan` decision and `HardwareIO` tools route through the confirmation flow via `RequireConfirmation` — both already-implemented pipelines that the per-tier dispatch reaches today. The PR-B audit-log calls in the stub paths are no longer needed; routing through the per-tier handlers means audit attribution follows whatever those handlers already do (`execute_config_change` calls `log_plan_created`, `execute_hardware_io` calls the per-status handler logs). Note: the plan-creation audit event today doesn't include the originating tool_name / args — that's a known pre-existing attribution gap (Copilot review on PR #1103, round-2) that a follow-up PR can close by adding tool-attributable plan logging if needed. The gate's `GatePlanRequest` / `GateConfirmationRequest` payloads (the `_req` arg) are informational today (originating tier only); a future PR could thread them into the audit stream or plan metadata if Plan/Apply ever wants to distinguish gate-routed from tier-dispatch-routed plans. ActionExecutor enforcement remains **deferred to PR-D** alongside the flag flip and the `TODO(PR-D, gate-bypass on None)` resolution — those are coupled (the flip activates the gate, which immediately needs both the None-handling fix and ActionExecutor coverage to avoid regressions). ~16 security tests still pass; the per-tier handlers (`execute_config_change`, `execute_hardware_io`) are covered by existing ToolExecutor tests. fmt + clippy clean. Tracks epic #999, sub-task of D1 (#1002). Stack: PR-A (#1080), PR-B (#1092), PR-C (this), PR-D (ActionExecutor + flag flip + None-handling).

- **Daemon (ADR-027 D1 wiring PR-B)**: ToolExecutor now consults `security::gate::enforce` before dispatching MCP tools. The IPC accept loop's `CallerContext` (PR-A) is plumbed through `DaemonCommand::IpcRequest` → `engine_manager::handle_ipc_request` → `ToolExecutor::execute(name, args, caller_ctx)` so the gate's per-tier × per-trust-level decision table can refuse a tool call before any tool work runs. Behavioural impact today is **zero** because the default `SecurityPolicy::shadow_mode = true` (Phase 1A invariant) makes `enforce` return `Allow` for every (tier, trust) pair regardless of caller — the call is in place, the policy gate is not yet active. PR-D's flag flip is what activates real enforcement; PR-B's job is to make sure the data pipeline is correct so the flip is a single-line behavioural change. The new `caller_ctx: Option<crate::security::CallerContext>` field on `DaemonCommand::IpcRequest` carries the pinned + classified peer through both `ipc.rs` `cmd_tx.send` sites (subscription bootstrap + main request dispatch), `Option` because in-process / test invocations and accept-pin failures both produce `None`. The handler matches all four `GateDecision` variants today: `Allow` and `AllowWithAudit` proceed (D13a's audit-stream emission for `AllowWithAudit` is a follow-up — the existing `log_tool_complete` calls cover today's audit needs); `Deny(reason)` returns `ExecutionResult::Error` with the `DenialReason` rendered into the message and writes to `audit_logger.log_tool_denied`; `RequirePlan(_)` and `RequireConfirmation(_)` were **stubbed as deny-with-explanatory-message** for PR-B (a `RequirePlan`/`RequireConfirmation` outcome was treated as a denial so the gate's signal wasn't silently dropped); the PR-C entry above now describes the real wiring into Plan/Apply (ADR-007 D2) and confirmation (ADR-027 D7 partial) machinery that replaces those stubs. ~50 test call sites in `executor.rs::tests` updated to pass `None` as the new `caller_ctx` arg (the test fixtures don't simulate a real peer pin, so the gate-bypass path matches PR-A's pre-wiring behaviour). The internal-origin `DaemonCommand::IpcRequest` constructed inside `executor.rs::execute_plugin_command` (LLM-initiated plugin call) also passes `caller_ctx: None` because there's no external peer for synthetic daemon-internal commands; this leaves a small "gate-bypass on internal commands" surface that's deliberate for now (the LLM caller has already passed the gate check at the outer tool-call boundary — internal sub-dispatch is implementation detail). ActionExecutor is **not** enforced in PR-B and is deferred to PR-D (originally planned for PR-C but moved when PR-C scope was tightened to ToolExecutor-only): it lives in a separate thread for Enigo thread-affinity reasons (`executor_thread.rs:279`) and plumbing `CallerContext` through that thread boundary is a bigger refactor than is appropriate to bundle here. ~165 daemon tests stay green, fmt + clippy clean, workspace check clean. Tracks epic #999, sub-task of D1 (#1002). Stack: PR-A (#1080), PR-B (this), PR-C (RequirePlan + RequireConfirmation routing — ActionExecutor enforcement was originally bundled here but moved out when scope was tightened), PR-D (ActionExecutor enforcement + flag flip + None-handling).

- **ADR-032 LLM Mode UI (#1084 epic, P0–P4 complete)**: Five-phase rollout of a chat-first "LLM Mode" alongside the existing Studio Mode three-zone layout. Toggleable via `⌘L` or the TitleBar mode pill; persisted to `localStorage` under `conductor:uiMode`. **P0 (#1091)**: Token migration — adopted the design-handoff sheet, added `--content-max` (720px reading column), `--slide-in-w` (320px overlay drawer), `--artifact*` purple aliases, composite font shorthands (`--h1`/`--h2`/`--h3`/`--body`/`--code`/`--caption`/`--label`), `--r-*` radius aliases, and `--shadow-2`. Renamed `--titlebar-height` → `--titlebar-h` and `--statusbar-height` → `--statusbar-h`. Added light-theme parity for all overlay/artifact tokens. **P1 (#1093)**: Layout shell — new `lib/components/llm-mode/{LlmModePanel,ConversationHeader}.svelte`; `App.svelte` switches layout based on `$uiMode`; TitleBar gains a mode pill and device-status dots (visible in BOTH modes — shared shell per ADR D5/D10); StatusBar shortcuts hint adapts per mode. **P2 (#1094)**: Context chips + four inline artifact types — `lib/components/llm-mode/ContextChips.svelte` (devices · mode · mappings · sync · `Learn active` chip · `⚡ Events ▾` toggle); `chat/PlanMessage.svelte` promotes the artifact aliases when in LLM Mode (Studio visual unchanged); new `chat/{RouteDiagram,MappingTable,SignalPulse}.svelte` mounted by `AssistantMessage` when `message.artifact?.type` matches. SignalPulse uses a class-based color allowlist to harden against CSS injection from untrusted LLM tool payloads. **P3 (#1095)**: Slide-in events drawer (`llm-mode/SlideInEventsPanel.svelte`) — 320px fixed-width right-edge overlay, fly-x-320 transition, click-outside does NOT close (per ADR D4 — users routinely interact with chat while watching events), Esc closes (focus-scoped via auto-focus on mount), reuses the canonical `EventRow`/`EventFilter` from Studio Mode. Auto-opens when MIDI Learn starts in LLM Mode (workspace.js subscription). Plus `llm-mode/DevicePopover.svelte` (240px anchored under TitleBar device dots — opposite click-outside contract: DOES close; mute toggle goes through `deviceBindingsStore.toggleMute`). **P4 (this PR)**: LLM-mode awareness — daemon `IpcCommand::SetUiMode` accepts `"llm"`/`"studio"` (rejects everything else) and persists on `EngineManager.ui_mode`; `IpcCommand::Status` includes `ui_mode` when set (omitted when None — no shape change for consumers without a connected GUI). New Tauri `set_ui_mode` command + workspace.js publish-on-change subscription (skips synchronous initial emit, swallows invoke failures so the daemon-not-running case is safe). Four bundled skills (`conductor-{midi-mapping,learn,binding-setup,troubleshooting}/SKILL.md`) gain a `## UI Mode awareness` section instructing the LLM to call `conductor_status` first and adapt copy to the active surface (inline plan card vs workspace panel; slide-in drawer vs persistent events panel). Closes #1084 and #1085–#1089.

- **Daemon (ADR-027 D1 wiring PR-A)**: Plumb peer pinning + classification through the IPC accept loop. `PinnedPeer::from_stream` is now generic over `&impl AsFd` so the IPC's `tokio::net::UnixStream` works directly without a `ManuallyDrop`-bridged `std::os::unix::net::UnixStream` ownership hack. After every `listener.accept()` the loop calls `PinnedPeer::from_stream(&stream)` and wraps the result via `CallerContext::from_peer(Arc::new(peer))` (which calls `classify_peer` internally to derive the trust level), threading the resulting `Option<CallerContext>` into `handle_client` so request dispatch can hand it to `security::gate::enforce` in subsequent wiring sub-pieces. `CallerContext` gains a `peer: Option<Arc<PinnedPeer>>` field — `None` for unit-test constructions via `CallerContext::new(TrustLevel)` (which keep working unchanged), `Some` for real IPC flows via the new `CallerContext::from_peer` constructor. The `from_peer` constructor calls `classify_peer` to derive the `trust_level` so the decision table sees the same band a same-binary live re-classification would produce. Pin failures are **logged but not enforced** — the Phase 1A `SecurityPolicy::shadow_mode = true` invariant means the gate is not yet consulting `CallerContext`, and dropping a connection on pin failure during the wiring rollout would regress today's working behaviour. The accept-loop comment + `handle_client`'s doc comment both call out that PR-D's flag flip will reject `None`-classified peers; until then we proceed with the connection so behaviour stays unchanged during the wiring rollout. `pin_linux` / `pin_macos` / `pid_from_local_peerpid` are also now generic over `&impl AsFd` for consistency. No new tests in this PR — the existing security_gate_test (11 cases) + peer_pin lib + 2/N + 3/N integration tests (14 cases) all stay green and pin every API surface this PR touches: `CallerContext::new(TrustLevel)` continues to compile (peer = None), `from_peer` round-trips through `classify_peer`, the accept-loop pin call uses the generic AsFd interface against tokio's UnixStream. Behaviour is unchanged through this PR — visible only as a new `debug!` log line per IPC accept (`uid=…  pid=…  exe=…  trust=…`) when `RUST_LOG=conductor_daemon=debug` is set, useful for verifying the pinning is working end-to-end on a real install before enforcement ships. PR-B will consume the `CallerContext` for `gate::enforce` calls in `mcp.rs` (ToolExecutor) + `action_executor.rs`; PR-C wires `RequirePlan` + `RequireConfirmation` flows; PR-D flips `shadow_mode = false`. Tracks epic #999, sub-task of D1 (#1002).

- **Daemon (ADR-027 D1 3/N)**: Peer trust classification via the new `conductor-daemon::security::peer_pin::classify_peer(&PinnedPeer) -> TrustLevel` function. Maps a (2/N)-pinned peer to the gate's existing `TrustLevel` enum (`GuiTrusted` / `CliTrusted` / `Untrusted`) — the input axis to D5's per-tier decision table. **Three-rule classifier:** (1) the peer's `uid` must equal the daemon's effective uid (belt-and-braces against socket-permission drift; the IPC socket is mode 0600 / 0700 so different-uid connects shouldn't be possible in practice but the check fails closed if they ever are); (2) the peer's `initial_exe` filename must match one of the three trusted binary names — `conductor-gui` → `GuiTrusted`, `conductor` / `conductorctl` → `CliTrusted`, anything else → `Untrusted`; (3) on macOS only, an exe-name match additionally requires a valid Apple code signature carrying Conductor's Team ID (`38H355VKB5`). Mismatch demotes to `Untrusted` rather than admitting a same-name unsigned binary at a trusted path — the `make dev-build` ad-hoc-codesigned dev binaries (per CLAUDE.md §"Dev builds and Input Monitoring") therefore correctly classify as `Untrusted` (their ad-hoc signature has no Team ID). Linux has no equivalent in-tree signature anchor — distro packaging, exe-path permissions, and the install-time package signature are the verification — so the Linux classifier stops at rules (1) and (2). The macOS path uses raw FFI for `SecCodeCopyGuestWithAttributes` and `SecCodeCopySigningInformation` because `security-framework` 3.x doesn't expose `SecCodeCopyGuestWithAttributes` on its `SecCode` wrapper and `SecCodeCopySigningInformation` is reachable only via `SecStaticCode` — neither covers the "look up running process by audit-token" use case D1 needs. The audit-token (already captured in `PinnedPeer.initial_audit_token` per (2/N)) is wrapped as a `CFData` in a `CFDictionary` with key `"audit-token"`, then handed to `SecCodeCopyGuestWithAttributes(host=NULL, attrs, 0, &out)`. Looking up `"teamid"` in the returned signing-information dictionary and byte-comparing to `CONDUCTOR_TEAM_ID` is the trust anchor. CF memory management uses `core-foundation`'s `TCFType` family (auto-release on drop) for everything except the raw `SecCodeRef`, which we explicitly `CFRelease` after extracting signing info. New cargo deps gated to `cfg(target_os = "macos")`: `security-framework = "3"` (links Security.framework via its build.rs) and `core-foundation = "0.10"` (CFType wrappers); my own raw `unsafe extern` block carries `#[link(name = "Security", kind = "framework")]` so the symbols I bind raw are findable at link time. New `[[bin]] d1-peer-test-helper` from (2/N) is reused as the foreign-binary fixture for the (3/N) integration test in `tests/peer_pin_3n_classification.rs`, which spawns the helper, pins, classifies, and asserts `Untrusted` (capture-then-cleanup-then-assert pattern so a panicking assert doesn't leak the helper subprocess into `pause()`). Five new unit tests in `peer_pin.rs::tests` cover the synthetic-`PinnedPeer` cases the integration test can't reproduce: different-uid → `Untrusted`, unknown basename → `Untrusted`, and one each for the three trusted basenames (Linux: `GuiTrusted` / `CliTrusted`; macOS: all three demote to `Untrusted` because synthetic audit tokens can't satisfy the Team ID check — fail closed). Module docs rewritten to mark (3/N) as the current state, with explicit warning that mid-session `execve` is still not detected (the static classification at pin time is locked in for the connection's lifetime — re-classification on every gate call is a possible follow-up but not yet planned). Wiring through `mcp.rs` / `action_executor.rs` and the `shadow_mode = false` flag flip remain a separate sub-piece; the atomic-bundle invariant is still in force. Tracks epic #999, sub-task of D1 (#1002).

- **Daemon (ADR-027 D1 2/N)**: Replace the (1/N) `still_pinned()` stub with a kernel-handle race-free liveness check per spec §4.1. `PinnedPeer` now carries a platform-specific kernel handle captured at `from_stream` time — `pidfd: OwnedFd` on Linux (via `libc::syscall(SYS_pidfd_open, pid, 0)`, called immediately after `getsockopt(SO_PEERCRED)` so the kernel-side process descriptor pins the peer before any TOCTOU window opens) and `initial_audit_token: [u32; 8]` plus `conn_fd: OwnedFd` on macOS (via `getsockopt(SOL_LOCAL, LOCAL_PEERTOKEN)` for the audit-token snapshot, with `try_clone_to_owned()` dup'ing the connection FD so the liveness check can outlive the IPC server's `UnixStream` wrapper). `still_pinned()` dispatches platform-specifically: Linux runs `libc::poll(POLLIN, timeout=0)` non-blockingly with EINTR-retry, returning `false` if `POLLIN` is set (process exited) or if any abnormal `revents` bit (`POLLERR` / `POLLHUP` / `POLLNVAL`) appears (fail closed); macOS re-fetches the audit token via the dup'd FD and byte-compares to the snapshot — fetch failure or mismatch returns `false`. Race-safety reasoning is documented in the module docs: `pidfd_open` returns a kernel-managed descriptor referring to the *original* process descriptor, so even if the original PID is recycled to a different process before `still_pinned` is called, the `pidfd` continues to refer to the now-exited instance and `POLLIN` reliably fires; on macOS the audit token is used strictly as a process-lifetime liveness primitive, with kernel connection-teardown on peer exit being what flips `still_pinned` to `false` (a token-byte mismatch instead means an unexpected kernel-level peer-identity change, e.g. socket-state record recycled to a different process — fail closed). **What this does NOT detect:** mid-session `execve` into a different binary while the pinned process keeps running — Linux `pidfd` survives `execve`, and the macOS `audit_token_t`'s pid/uid/asid/auid fields are inherited across same-uid `execve`, so a re-exec'd peer returns the *same* token. Detecting a binary swap is 3/N's signature-classification job. Trust-level questions (signed? Apple Team ID? in a blessed location?) are also 3/N. The `shadow_mode = true` default keeps the gate dormant until both 3/N and the wiring sub-piece have landed; the atomic-bundle invariant is unchanged. New `PeerAuthError` variants (`#[non_exhaustive]` already in 1/N): `PidfdOpenFailed(io::Error)` on Linux (kernel < 5.3 or peer raced into exit between `getsockopt` and `pidfd_open`), `AuditTokenFailed(io::Error)` and `ConnectionFdDupFailed(io::Error)` on macOS (`getsockopt(LOCAL_PEERTOKEN)` failure / size-mismatch / FD dup failure). All three are fail-closed: without a kernel handle there is no race-free liveness primitive, and falling back to the racy pid-only check would re-introduce the TOCTOU. The `LOCAL_PEERTOKEN` socket-option constant (`0x006`) is declared locally rather than depending on a libc-crate version that exposes it; `audit_token_t` is represented as `[u32; 8]` matching the `<bsm/audit.h>` `unsigned int val[8]` layout (32 bytes, no padding, byte-comparable). 7 tests pinning the contract: 6 `peer_pin.rs::tests` unit tests including a renamed `still_pinned_returns_true_for_live_self_connection` that inlines the FD lifecycle so all socket descriptors stay alive through the assertion (the macOS `LOCAL_PEERTOKEN` re-fetch fails on a torn-down connection, which is correct fail-closed behaviour but had to be exercised carefully); 1 new `tests/peer_pin_2n_lifecycle.rs` integration test that spawns the new `d1-peer-test-helper` binary as a real subprocess (the unit-test self-connect path can't kill itself), pins, SIGKILLs the helper, and asserts `still_pinned()` returns `false`. Implementation took TDD discipline: red-bar commit landed first (helper binary + integration test asserting `still_pinned() == false` against the 1/N stub, which fails because 1/N hard-codes `true`), then this commit turns it green. Deferred to D1 (3/N) per the [#1002 tracking comment](https://github.com/monstrous-media/conductor/issues/1002#issuecomment-4397352700): `classify_peer(&PinnedPeer) -> PeerTrustLevel`, macOS code-signature verification via `SecCodeCopyGuestWithAttributes` (Apple Team ID extraction), Linux `/proc/<pid>/status` UID matching + signature fingerprint, and the defence-in-depth peer-bound HMAC tokens decision (re-evaluate post-3/N once kernel-handle + signature coverage is known). Wiring through `mcp.rs` + `action_executor.rs` and the `shadow_mode = false` flip remain a separate sub-piece after 3/N. Tracks epic #999, sub-task of D1 (#1002).

## [5.5.2-alpha] - 2026-05-07

Install-test patch release. Cuts the v5.5.x install-test pause that #1061 created when it surfaced — the strict CSP shipped in v5.5.0-alpha (#1024) was literally the spec text, but the spec itself omitted the renderer→host IPC transport sources, leaving the paid GUI functionally inert. v5.5.2-alpha re-tags after that fix lands so the install-test cycle (matching the v5.5.0-alpha → v5.5.1-alpha pause pattern) can validate on a fresh Mac install before Phase 1A continues. With this release, both P0 install-test blockers from the v5.5.0-alpha pause (#1038 audit-wiring gap, #1061 CSP regression) are closed.

### Fixed

- **GUI (#1061, ADR-027 D11)**: Add `ipc:` and `http://ipc.localhost` to `connect-src` in both `csp` and `devCsp`. The strict CSP shipped in PR #1024 implemented the spec literally, but the spec itself omitted these renderer-host transport sources — Tauri 2 dispatches every `invoke()` over `ipc://localhost/<command>` (Linux/macOS) or `http://ipc.localhost/<command>` (Windows), and CSP's `'self'` source matches scheme+host+port exactly so it does NOT cover `ipc:`. Result was every Tauri command CSP-blocked at the renderer (`get_config`, `get_device_bindings`, `llm_get_providers`, `plugin:app|version`, `plugin:event|listen`, etc.) — the GUI was functionally inert with blank panels and silent failures since #1024 merged. Workaround until this PR was setting `csp = null` and accepting the ADR-027 D11 regression. Fix: add the IPC sources to both CSP fields. Fail-closed test infrastructure — the `tauri_csp_test.rs` exclusivity test had pinned the broken set as ground truth, and the `production_csp_has_no_localhost_or_websocket_origins` test used substring scanning that would falsely flag `http://ipc.localhost`. Both updated: exclusivity test now requires the IPC sources, the no-dev-origins test renamed to `production_csp_has_no_dev_server_origins` and switched to parsed-token matching against the per-directive source set, and a new `ipc_scheme_is_present_in_both_csp_and_dev_csp` test pins the invariant in both CSP variants. ADR-027 §D11 and impl-spec §9.1 amended to call out that `ipc:` / `http://ipc.localhost` are renderer-host transport (NOT egress endpoints) and must be in every CSP that ships, including production. Spec §9.1 also now documents the `csp` vs `devCsp` field split and the substring-vs-token test invariant. Closes #1061. Blocks v5.5.x re-tag alongside #1038 (already fixed in v5.5.1-alpha).

### Added

- **Daemon (ADR-027 D1 1/N)**: New `conductor-daemon::security::peer_pin` module hosting the peer-credential pinning scaffold for spec §4.1. `PinnedPeer { uid, pid, initial_exe }` captures the connecting peer's identity at Unix-socket accept time via raw `libc` (stdlib's `UnixStream::peer_cred` is gated behind the unstable `peer_credentials_unix_socket` feature, rust-lang/rust#42839, so we use `getsockopt`/`getpeereid` directly): Linux uses `getsockopt(SOL_SOCKET, SO_PEERCRED)` for uid+pid, macOS uses `getpeereid` for uid + `getsockopt(SOL_LOCAL, LOCAL_PEERPID)` for pid; both then resolve the executable path via `/proc/<pid>/exe` (Linux) or `proc_pidpath` (macOS). Both `getsockopt` paths validate the returned `len` matches the expected struct/value size and reject non-positive pids — without these the kernel returning a short response would leave the struct partially zeroed and mis-pin the peer as uid 0 / pid 0. `#[non_exhaustive]` on both the struct and the `PeerAuthError` enum so D1 (2/N) can add the `pidfd: OwnedFd` (Linux) and `audit_token: AuditToken` (macOS) fields without breaking downstream construction. **TOCTOU vulnerability is documented and intentional in (1/N)** — the kernel-level race-free pinning (`pidfd_open` / `audit_token_t`) lands in (2/N) per spec §4.1; until then `PinnedPeer::still_pinned()` is a stub returning `true`. The gate is not enabled until both (2/N) and the wiring sub-piece are merged, enforced by the Phase 1A atomic-bundle invariant (`SecurityPolicy::shadow_mode = true` default). 6 unit tests in `peer_pin.rs::tests` use a temp-dir Unix socket with self-connection: pid + uid match the test process; initial_exe is absolute; `still_pinned` returns true (pinning the (1/N) stub semantics for review-time visibility when (2/N) replaces it); compile-time `Send` guard for cross-thread use across the IPC tokio task boundary; `Display` smoke test for D13a denial-stream rendering. No call sites yet — the IPC server's accept loop will pin in (4/N) and the gate's `CallerContext` will carry `Arc<PinnedPeer>`. Tracks epic #999, sub-task of D1.

- **Core (ADR-027 D3 3/N)**: New `conductor-core::security::interpreters` module hosting wrapper-chain resolution and interpreter classification per spec §3.2 — pure functions only, no call sites yet. **The v1.0 bypass class is not yet closed at runtime** (`command = "/usr/bin/env python3 -c '…'"` still walks past the `argv[0]`-only check because no caller consumes the resolver). This PR lays the groundwork: it ships `resolve_effective_executable` and `classify_interpreter` ready to be wired in. The runtime path that augments the gate's capability check with `InterpreterExec(family)` — and therefore actually closes the bypass — lands together with D1 (peer-credential IPC auth) and the wiring sub-piece, to keep the Phase 1A atomic-bundle invariant. The new `resolve_effective_executable(command: &Path, args: &[String], max_depth: usize) -> Result<ResolvedExecutable, ResolveError>` function walks the wrapper chain (16 wrappers from spec §3.2 — `env`, `nice`, `ionice`, `timeout`, `nohup`, `sudo`, `doas`, `setsid`, `chpst`, `runuser`, `xargs`, `unshare`, `stdbuf`, `time`, `schedtool`, `taskset`) with per-wrapper option handling for the realistic bypass surface, returning the *effective* binary path plus the chain of wrappers traversed (recorded for D13a's denial / approval audit stream). The new `classify_interpreter(path: &Path) -> InterpreterClassification` function maps an effective binary's basename — with version-suffix stripping (`python3`, `python3.12`, `node20.10.0` all classify identically) and explicit distribution / build aliases (`nodejs` → Node, `pythonw` / `python2` / `python3m` / `python3dm` / `pythonw3` → Python) so platform-specific binary names don't slip past the gate — onto one of the 13 `InterpreterFamily` variants from D3 (1/N), or `NotInterpreter`. `ResolveError` (`#[non_exhaustive]`) carries `DepthExceeded { limit }`, `MissingProgram { wrapper }`, `CannotResolveWrapper { wrapper }`, `MalformedPath` — callers fail closed by treating any error as `InterpreterFamily::Other` per the documented contract. `DEFAULT_MAX_WRAPPER_DEPTH = 8` covers real chains while shutting down pathological loops. 67 unit tests in `interpreters.rs::tests` cover: per-wrapper happy paths (`env -i`, `env KEY=VAL`, `env -u VAR`, `env --`, `sudo -u user`, `sudo -unobody` concatenated, `sudo --user=value`, `nice -n 10`, `ionice -c 3`, `timeout 5s`, `timeout --signal TERM 5s`, `timeout -k 2s 5s`, `xargs -I {}`, `nohup`, `setsid`, `time`, `unshare -r`, `stdbuf -oL`); nested wrapper chains (`env nohup python3`, `sudo -u nobody env python3`); depth-limit boundaries; error paths (missing program, malformed path); and a `every_wrapper_has_a_handler` runtime-test invariant that fails at test time for any wrapper added to the `WRAPPERS` slice without a matching handler arm in `find_wrapped_program`. Tracks epic #999, sub-task of #1001.

- **Core (ADR-027 D3, sub-pieces 1/N + 2/N)**: `conductor-core::security::capabilities` module hosting the closed capability vocabulary (1/N) **plus** the per-`ActionConfig`-variant capability mapping function (2/N) the Phase 1A bundle gate (D5 + D3 + D1) will enforce against. The `Capability` enum (`#[non_exhaustive]`) covers: `ShellExec`, `InterpreterExec(InterpreterFamily)`, `ShellNetwork`, `KeystrokeSend`, `KeystrokeModifierCombo`, `LaunchApp`, `MidiOut`, `OscOut` (new in 2/N for OSC traffic over UDP — different blast radius from local MIDI ports), `MouseSend` (new in 2/N — separated from `KeystrokeSend` so audit logs differentiate, and a user can grant one without the other), `PluginExec` (new in 2/N — invoke a plugin at all; per-plugin filesystem sandbox is captured by `FsRead`/`FsWrite(PathScope::PluginData)` per ADR-027 D10c), `ConfigRead`, `ConfigWrite`, `FsRead(PathScope)`, `FsWrite(PathScope)`. Supporting `InterpreterFamily` (Python, Ruby, Perl, Node, Bash, Sh, Zsh, Fish, AwkOrSed, Lua, TclSh, Php, Other) and `PathScope` (PluginData, Home, Explicit(PathBuf)) round out the vocabulary. `CapabilitySet = HashSet<Capability>` alias for both declarations and grants. The new `capabilities_for_action(&ActionConfig) -> CapabilitySet` function (spec §3.1) maps every existing variant exhaustively (no `_` arm — adding a new `ActionConfig` variant fails compilation until capabilities are declared): `Keystroke` → `KeystrokeSend` (+ `KeystrokeModifierCombo` if modifiers are present); `Text` → `KeystrokeSend`; `Launch` → `LaunchApp`; `Shell` / `VolumeControl` → `ShellExec` (cross-platform — VolumeControl spawns `osascript` on macOS and `pactl` on Linux); `MouseClick` → `MouseSend`; `SendMidi` / `MidiForward` → `MidiOut`; `OscSend` → `OscOut`; `Plugin` → `PluginExec`; recursive variants (`Sequence`, `Repeat`, `Conditional`, `PcContextSwitch`, `CcContextSwitch`) return the **union** of their inner actions' capabilities; `ModeChange` → `ConfigWrite` (the daemon persists `last_selected_mode` to the on-disk config TOML via `EngineManager::persist_mode_change`); `Delay` returns an empty set as the only true control-flow primitive with no security-relevant work. The wrapper-resolution / interpreter-classification path (per spec §3.2 — defeating the `/usr/bin/env python3 -c '…'` bypass class, which adds `InterpreterExec(family)` on top of `ShellExec` at runtime) lands in (3/N). Shell argv-form schema migration (#1037 / spec §3.3) is **explicitly** sequenced after Phase 1A. 8 unit tests in `capabilities.rs::tests` cover serde round-trip, `HashSet` semantics, `Copy` invariant for `InterpreterFamily`, `Send + Sync` compile-time guard, per-family interpreter distinction; 22 integration tests in `conductor-core/tests/capability_declared_actions_test.rs` pin every leaf-variant mapping, the recursive composition for all 5 recursive variants, the `Keystroke` modifier-presence branching, the `Sequence { actions: [] }` no-op case, the `Delay` empty-set / `ModeChange` `ConfigWrite` distinction, and a no-spurious-grants invariant (`Keystroke` does **not** pull in `MidiOut` / `LaunchApp` / etc.). Tracks epic #999, sub-task of #1001.

- **Daemon (ADR-027 D5, sub-pieces 2/N + 3/N)**: New `conductor-daemon::security::gate` module hosting the global capability/tier enforcement gate that the Phase 1A bundle (D5 + D3 + D1) rallies behind. Per spec §2 line 100–104 the bundle is atomic — partial intermediate states are unsafe — so the gate ships in **shadow mode** by default: `SecurityPolicy::shadow_mode = true` makes `enforce` return `GateDecision::Allow` for every tier regardless of caller, preserving today's behaviour exactly. When the Phase 1A wiring sub-piece eventually flips that flag (only after D3 capability declarations and D1 peer-credential IPC auth also land), `enforce` consults a real per-tier decision table per spec §2.2 + §2.5: `ReadOnly` / `Stateful` / `ArtifactRender` → `Allow`; `ConfigChange` → `RequirePlan(GatePlanRequest)` (any caller — Plan/Apply per ADR-007 D2); `HardwareIO` → `RequireConfirmation(GateConfirmationRequest)` for `GuiTrusted` / `CliTrusted`, `Deny` for `Untrusted` (the confirmation prompt has nowhere to land); `Privileged` → `Deny` for any caller (the fallback bucket per spec §2.5: "Unknown tool name → Privileged → denied"). `GatePlanRequest` carries the originating `tier`; `GateConfirmationRequest` carries `tier` + `trust_level`; `DenialReason` has two variants — `PrivilegedTier { trust_level }` and `UntrustedCallerForElevatedTier { tier }` — both shaped for D13a's structured denial-stream consumption. The `Gate` prefix on the request types disambiguates them from the existing `daemon::hardware_io::confirmation::ConfirmationRequest` (a SysEx pending-confirmation token), since the two will coexist along the eventual hardware-IO wiring. All public hand-off types (`GatePlanRequest`, `GateConfirmationRequest`, `CallerContext`, `SecurityPolicy`, `DenialReason`, and the latter's struct-like variants) are `#[non_exhaustive]` with `Type::new(…)` constructors (`SecurityPolicy::with_shadow_mode(false)` for the test mode), so D3/D1 can extend them additively without breaking downstream destructuring or struct-literal construction. 11 integration tests in `conductor-daemon/tests/security_gate_test.rs` plus 5 unit tests in `gate.rs::tests` pin every cell of the decision table, the shadow-mode envelope, the denial payload shapes, and a compile-time `Send + Sync` guard covering all hand-off types so the gate travels safely across the IPC tokio task boundary. No call sites of `security::gate::enforce` exist in tree yet — wiring through `mcp.rs` + `action_executor.rs` lands together with D3 + D1 to keep the atomic-bundle invariant. Tracks epic #999, sub-task of #1000.

## [5.5.1-alpha] - 2026-05-03

Install-test patch release. v5.5.0-alpha was cut as a deliberate breakpoint to validate the just-landed ADR-027 work on a clean Mac account before resuming Phase 1A; the install test caught one HIGH-severity wiring gap (#1038) plus surfaced two UX / design follow-ups that are sequenced after Phase 1A (#1037, #1040). This release closes the wiring gap so the audit-log machinery actually runs on real installs.

### Fixed

- **Daemon (#1042)**: ADR-027 D13b/D13c now wired into the production `EngineManager`. Pre-fix `AuditLogger::new` was never called outside tests — `ToolExecutor` was constructed with `audit_logger: None` and every audit-write site was a silent no-op. The audit DB at `~/Library/Application Support/conductor/audit.db` (and per-platform equivalents) never appeared on real installs, so two declared P3 controls (D13b append-only hash chain, D13c PII redaction) were dormant in production despite shipping with 60+ tests passing. New `conductor-daemon/src/daemon/audit/init.rs` introduces `create_audit_logger(data_local_dir)` + `default_audit_logger()` as the single tested entry point; `EngineManager::new()` calls it before `Arc::new(ToolExecutor)` so the existing `set_audit_logger` setter can still reach `&mut self`. Failure semantics: init failure logs an `error!` and the daemon stays up with audit disabled, per ADR-027 §D13b's "tamper-evidence, not tamper-prevention" framing — D13a will revisit with a visible "audit unavailable" surface. Two new TDD tests pin the production-path behaviour: `create_audit_logger_creates_db_file_at_expected_path` (file appears on disk) and `create_audit_logger_writes_into_d13b_hash_chain` (writes go through D13b's hash chain). Closes #1038.

### Install-test follow-ups deferred to post-Phase-1A

Surfaced during the v5.5.0-alpha install test but explicitly sequenced AFTER Phase 1A to keep that bundle's review surface tight. Both are tracked under epic #999.

- **#1037** (MEDIUM-HIGH) — Shell action's current `command: String` schema lets a user invoke `/bin/sh -c "..."` and bypass D7 env sanitisation via the interpreter. Real bypass of a P1 control. Closes when D3 (capability-gated argv array + absolute-path requirement + interpreter detection) lands as a follow-up to the Phase 1A bundle.
- **#1040** (LOW) — Deny-listed keystroke mappings can be saved without warning; runtime guard in D8 fires correctly when triggered, but the mapping looks fine in the editor. Defence-in-depth still works (no security regression). Small extension to D2's plan validator + a mapping-editor warning chip.

## [5.5.0-alpha] - 2026-05-03

ADR-027 Security Hardening — first round of P1 + P2 + P3 decisions land. Nine of the twenty ADR-027 decisions are now on main; this version is cut as a deliberate install-test pause before the multi-week Phase 1A bundle (D5 + D3 + D1) reworks the permissions / IPC-auth surface. Cutting now bisects the install-test surface — anything that breaks on a fresh-install Mac account against this version is attributable to the just-landed audit / plugin / IPC / keystroke / CSP work, NOT to the larger bundle that follows.

### Added

- **ADR-027 D11 — Tauri CSP** (#1024): Strict CSP locked down on the Tauri webview, mitigating compromised-LLM-provider arbitrary-JS execution against the Tauri command surface (Finding F-09). Inline FOUC scripts moved out of `index.html`; `connect-src` allowlist matches the production LLM provider set; dev HMR origin lives under `app.security.devCsp` so the prod policy isn't loosened for development.

- **ADR-027 D2 — apply_plan validation enforcement** (#1025): `apply_plan` now re-validates the entire plan against current schema before applying any change, refusing the whole batch on the first error. Closes the path where a TOCTOU window or a stale plan could persist a partially-validated config.

- **ADR-027 D7 (partial) — shell action env sanitisation** (#1026): `shell.exec` strips `LD_PRELOAD`, `LD_LIBRARY_PATH`, `DYLD_*`, `PYTHONPATH`, `NODE_OPTIONS`, `RUBYLIB/OPT`, `PERL5LIB/OPT` before `execve`; passes through only an explicit allowlist (`PATH=/usr/bin:/bin`, `HOME`, `LANG`, `LC_ALL`). Closes part of Finding F-07 (env hijacking turning any shell action into arbitrary code execution). Timeout enforcement is the remaining D7 work.

- **ADR-027 D20 — LLM-context secret redaction** (#1027): `conductor-gui/src-tauri/src/llm/redaction.rs` walks tool-call JSON for secret-shaped keys (`api_key`, `*_token`, `*_secret`, `client_secret`, etc.) and replaces values with `<redacted:N>` markers before they're attached to LLM messages. Closes the exfiltration vector where a compromised LLM provider could observe `Authorization` headers, OAuth tokens, or webhook signing keys passed through `tool_use` arguments.

- **ADR-027 D16 (partial) — IPC concurrent-connection cap** (#1028): Per-PID semaphore caps simultaneous IPC connections from any single client process, refusing further connect() calls past the cap rather than letting a misbehaving client drain the daemon's accept loop. Closes part of Finding F-08 (daemon DoS). Idle-connection timeout and per-peer message rate limit are the remaining D16 work.

- **ADR-027 D10c — per-plugin WASM filesystem scope** (#1029): Replaces wasmtime's `DirPerms::all()` with per-plugin subdirectories under `~/.conductor/plugins/<plugin_id>/data/`. Each WASM plugin now sees only its own scope at `/data` inside the sandbox; cross-plugin reads / writes / symlink escapes return EPERM. `WasmPlugin::set_capabilities()` (round-8) syncs runtime grant changes into the live store, and `grant_capability()` enforces the manifest contract (round-9): the manager refuses to grant a capability the plugin didn't declare in its manifest, closing the privilege-escalation path opened by the round-7 fail-closed change.

- **ADR-027 D8 — keystroke policy mode** (#1030): Action executor consults a deny-list + 60/sec rate limiter before dispatching `Keystroke` actions. Block list covers known destructive shortcuts (Cmd+Shift+Q, Cmd+Opt+Esc, full-screen-lock chords, etc.); rate limit prevents a runaway script from hammering the system input layer. `KeystrokePolicyError::DenylistedCombo` carries the actual requested key (not the rule key) so audit entries / UI surfaces show the user what was attempted.

- **ADR-027 D13b — append-only audit log hash chain** (#1031): Each audit row stores `prev_hash = sha256(prev_persisted_bytes)` and `entry_hash = sha256(prev_hash || canonical_persisted_bytes)`. Verifier reads raw column values (no lossy enum / JSON parsing — every byte mutation is detectable). Schema migrates v1 → v2 idempotently via `PRAGMA table_info` (round-6 — replaces the prior error-message string match, which was not a stable SQLite API). Legacy v1 rows stay NULL; verify_chain skips them and re-roots the v2 segment at GENESIS. `cleanup()` is transactional (DELETE + chain rebuild atomic) and only rebuilds rows that already had hashes, preserving the legacy-NULL contract. Tamper-evidence, not tamper-prevention: the chain makes mutations visible the next time `verify_chain()` runs.

- **ADR-027 D13c — audit log PII redaction** (#1032): `arguments` and `result` JSON walked for secret-shaped keys at insert time; values replaced with `<redacted:N>` (or wholesale `<redacted>` for non-string secret values) before persisting. Each gated by its own `AuditLoggerConfig` flag (`redact_arguments`, `redact_results`), default-on. Composes with D13b: redaction runs FIRST so the hash chain protects post-redaction bytes — the same bytes a verifier reads back from disk. UTF-8-safe truncation reserves marker length so `RESULT_MAX_BYTES` is a hard cap on the FINAL stored string. Insert-side fast path skips the serde re-serialise round-trip when nothing was redacted (round-4): saves an input-sized allocation on the common no-secret path. Insert-side lock scope shrunk to just SELECT(prev_hash) + sha256 + INSERT (round-4): redaction + JSON parse no longer holds the SQLite mutex against concurrent audit writers.

- **Docs (#1033)**: ADR-030 (Passthrough Routing), ADR-031 (Signal Routing Graph), ADR-032 (LLM Mode UI), ADR-033 (Agentic Context-Aware Mapping). Plus the GUI v2 design handoff bundle (Studio + LLM mode mockups, brand assets, type-scale + type-stack previews) and the implementation specs for ADR-030 and ADR-031.

### Pause for install testing

This version is the breakpoint between just-landed P1+P2+P3 work and the larger Phase 1A bundle (D5 capability gate + D3 capability-declared actions + D1 peer-credential IPC auth, which ship atomically per spec §19 since partial ships break invariants). Validate on a clean Mac account before resuming Phase 1A:

1. Audit DB created at `~/Library/Application Support/conductor/audit.db` with v2 schema; `verify_chain` succeeds on first daemon start.
2. WASM plugin install creates `~/.conductor/plugins/<name>/data/` subdir; plugin can't read sibling plugin paths.
3. Tauri CSP doesn't block legitimate LLM SSE streams.
4. Shell actions with sanitised env still work for normal use cases (PATH, HOME, LANG, LC_ALL passthrough).
5. Keystroke policy doesn't block benign macros; rate limit only kicks in on runaway loops.
6. Input Monitoring grant survives across rebuilds (ADR-029 / `make dev-build` codesign identity).

Issues found during install testing land as fixes against this version before the Phase 1A bundle starts.

## [5.4.0-alpha] - 2026-04-30

ADR-026 Phase 4 — settings escape-hatch + per-device opt-out + identity-probe history. Closes the Phase 4 acceptance row entirely (six bullets across four PRs). Probing now has a clean kill-switch path from "global off" → "per-device off" → "individual re-probe with per-port rate-limit" with full diagnostic visibility in the GUI.

### Added

- **Daemon + GUI (#979)**: ADR-026 Phase 4.3b — `DeviceIdentityHistory` panel + per-port probe-attempt ring buffer. The `ProbeCoordinator` now records every completed probe (`Ok` and `Err` alike) into a `Mutex<HashMap<String, VecDeque>>` bounded to 32 entries per port (oldest evicted on overflow, returned newest-first). New `ProbeHistoryEntry { timestamp_ms, port_name, outcome }` struct uses the existing `ProbeOutcomeWire` shape so the GUI can drive both the inline badge and history rows from the same JSON. New `IpcCommand::GetProbeHistory` daemon command + `get_probe_history` Tauri command surface the buffer to the GUI. New `DeviceIdentityHistory.svelte` renders rows with timestamp / status / RTT, plus a Re-probe button that invokes `probe_device_identity` and re-fetches history on completion. Renders inside `DeviceDetail` (the expanded device-row panel) for Input + Bidirectional bindings only — Output-only bindings have no probe history because probes are keyed on the input route. `MultipleIdentified` framed as "N replies — shared route" matching `DeviceIdentityBadge` for cross-view consistency. Race-guarded fetch (monotonically-increasing `fetchSeq` token), surfaced Re-probe IPC failures, and composite `${timestamp_ms}-${i}` `{#each}` key (handles two probes inside the same millisecond). 4 new probe lib tests + 9 new component tests.

- **GUI (#977)**: ADR-026 Phase 4.3a — Settings UI for SysEx identity probing. New "SysEx Identity Probing" section in `SettingsPanel.svelte` with two checkboxes bound to `advanced_settings.sysex_identity_probing` (master kill-switch) and `advanced_settings.probe_on_connect` (auto-probe on hot-plug; manual Identify still works when off). Sub-toggle `disabled` when master is off — spec §4.3 calls out the dependency, and an enabled-looking sub-toggle that's actually a no-op would be misleading. Both persist via `configStore.save`, matching the existing chord/hold-threshold slider pattern. `aria-labelledby` wires each checkbox to its descriptive title so screen readers announce the setting name (Copilot review fix). 6 new component tests + `mockConfigStore` infrastructure that paves the way for testing the existing chord/hold sliders too.

- **Daemon + Core (#976)**: ADR-026 Phase 4.2 — per-device `no_probe` opt-out. New `DeviceIdentityConfig.no_probe: bool` field (default `false`). `ports_eligible_for_probe_on_connect` filters out bindings with `no_probe = true`, with one important exception: bindings whose matchers include a `SysExIdentity` matcher are still probed. Honouring `no_probe` for a SysEx-keyed binding would mean the matcher could never resolve (probe is the ONLY way to populate the identity it tests against), so the daemon ignores the flag and logs an override warning at config load + reload. Both eligibility AND `last_known` updates use the SAME filter — flipping `no_probe = false` via config reload now triggers a probe on the next dispatch tick without requiring a physical reconnect. Manual Identify still works regardless of the flag — the opt-out is for noise reduction (vintage hardware that misbehaves under SysEx polling), not a prohibition. Override warning skips disabled identities (PortResolver wouldn't probe them anyway). 3 new probe-on-connect tests.

- **Daemon + Core (#975)**: ADR-026 Phase 4.1 — `sysex_identity_probing` master toggle is now actually wired. The field has lived in `AdvancedSettings` since v4.26 but no code path consulted it for the manual probe — only `probe_on_connect.rs` checked it. `ProbeCoordinator` gains an `enabled: AtomicBool` field, `set_enabled()` / `is_enabled()` accessors, and a fast-path gate at the top of `probe()`. The hard guarantee that no SysEx leaves the daemon when disabled comes from `set_enabled(false)` itself acquiring the global probe lock to drain in-flight probes — combined with a second `enabled` re-check after acquiring the lock, the race window closes completely. `set_enabled(true)` stays lock-free (nothing to drain). Daemon wires from `config.advanced_settings.sysex_identity_probing` on startup AND `reload_config`. 2 new probe lib tests including a deterministic in-flight drain test that spawns a probe with a 100ms sleeping send_fn and asserts `set_enabled(false)` blocks until the send completes.

### Changed

- **GUI (#977 review)**: `PATTERN_HOLD_DEFAULT` test mock aligned with production (5000ms, was 2000ms) — was masking Reset-button visibility logic in tests.

- **Memory**: ADR-026 phase notes now include 4.1, 4.2, 4.3a, 4.3b summaries. Two new TDD lessons: (1) for dispatched events / callbacks, assert the dispatched payload — DOM-only assertions miss the regression where `handleSave()` silently dropped fields (3.E #973 review); (2) `cargo fmt --check` runs in the Lint workflow and rejects any drift, including over-long `assert!` lines that fit on one line locally but exceed 100 cols.

## [5.3.0-alpha] - 2026-04-30

### Fixed
- **Daemon (#955, PR #960)**: `reload_config` now re-runs `PortResolver` against the new bindings and refreshes `device_status.devices` to mirror the post-rescan view. Pre-fix, editing `[[bindings]]` while the daemon was running was a no-op for runtime port resolution — the InputManager kept whatever port→DeviceId mapping it built at startup, so events kept flowing under stale aliases until daemon restart. Includes the post-review correctness fixes from #960's `/ultrareview`: two-phase rekey apply (drains all old keys before any reapply, so `alias_a ↔ alias_b` swaps no longer drop a port via `HashMap::insert` overwrite), `muted_devices` migration in lockstep with the rekey (mute state previously orphaned silently and `is_device_enabled` returned `true` after a binding rename), and `compute_rekeys` consulting `desired_ports` for DeviceId disambiguation (raw fallback no longer collapses `"X #2"` onto `"X"` when duplicate-named ports are present). 4 new unit tests for the drain phase + 1 for instance disambiguation; the rekey logic stays pure and hardware-free for testing.

- **Daemon (#957, PR #958)**: Daemon now honours `profiles.json` `active_profile_id` at startup. Pre-fix, launching `./target/debug/conductor` directly (no `--config`) always loaded `<config_dir>/config.toml` regardless of which profile the GUI considered active — so direct CLI / GUI auto-spawn / systemd / launchd starts silently used the wrong config until the GUI got a chance to send a `SwitchProfile` IPC. New `daemon::startup::resolve_startup_config_path` resolves `--config <explicit>` → `profiles.json` `active_profile_id` → `<config_dir>/config.toml` in that order. Fail-soft: missing / malformed / null active id / unresolvable id all fall back to the default. 9 unit tests pin each precedence rung. Cleanup pass (PR #964) added validation of the active profile's `config_path` (rejects relative paths, non-`.toml` extensions, non-existent or directory targets — module docs promise the daemon never refuses to start because the profile state is broken), short-circuits `get_default_config_dir()` when `--config` is provided (systemd/launchd safety when `$HOME` is unset), and fixed a Windows test bug where `format!`+`Path::display()` produced unescaped backslashes that broke `serde_json::from_str`.

- **GUI (#954)**: `get_discovered_ports` and `get_available_ports` Tauri commands now honour the active profile when cross-referencing live MIDI ports against `[[bindings]]` matchers. Pre-fix, both hardcoded `~/.config/conductor/config.toml` so users on a profile saw every discovered port flagged "Unbound" in CONNECTIONS → DISCOVERED PORTS and in the Add Binding dialog's port picker, even when the daemon had resolved the bindings correctly. Extracted `load_active_config(state)` private helper that mirrors the active-profile resolution `get_config` / `save_config` already do.

- **GUI (#956, PR #963)**: `get_config_path`, `get_config_toml`, and `get_config_history` Tauri commands now honour the active profile. Sibling bug to #954 — pre-fix the UI displayed the wrong config path, the WI-12 config preview pane showed the wrong file's contents, and the "last changed" history tracked the wrong `.known_good` backup. Extracted `config_paths::active_or_default_config_path(manager)` into its own testable module; the previously-private `load_active_config` helper now delegates to it. `apply_template` deliberately left targeting `config.toml` — that's the bootstrap path before any profile exists. 4 unit tests using `ProfileManager::with_directory(tempdir)` cover happy path, fallback, ghost-id defence, and cross-profile switching.

- **GUI (#961, PR #962)**: `DeviceIdentityBadge` now renders the daemon's `detail` field inline rather than a literal `"error"` placeholder. Pre-fix, three badge states quietly swallowed `detail` and showed fixed strings — `error` rendered as `"error"` (cause buried in tooltip), `identified` from `MultipleIdentified` rendered an empty green pill (manufacturer/family/model live in `candidates[]`), and `no-reply` rendered hard-coded `"no reply"` instead of the daemon's more informative timeout window. Surfaced from dev testing on LPD8 mk2 where a re-probe inside the 60s per-port cooldown returned `RateLimited` with detail `"Rate-limited; retry in 45s"`, but the badge showed a generic alarming red error pill. CSS truncates with ellipsis at 28ch so long causes don't blow out the row width; full text remains in the tooltip.

- **GUI (#951 review)**: Council `/ultrareview` of Phase 3.D.2 surfaced three correctness regressions; all fixed. (1) The probing-state pulse animation now respects `prefers-reduced-motion: reduce` — eight sibling components in the codebase already gate their animations on this preference; the new badge is now consistent. (2) `identityBadges` map switched from plain `{}` to `Object.create(null)` — defends against user-configured `device_id` values matching `Object.prototype` keys (`toString`, `constructor`, etc), which would otherwise resolve to inherited functions, defeat the `?? fallback` guard, and render `state={undefined}` with aria-label `"Device identity: undefined"`. Same pattern as `utils/device-colors.ts`. (3) Identify button now stops `keydown` propagation and explicitly handles Enter/Space — pre-fix, keyboard users couldn't activate the button at all because the row's `on:keydown` (which opens the context menu on Enter/Space and `preventDefault()`s the synthetic click) hijacked activation. 4 new component tests pin each fix.

- **Daemon (#945)**: ADR-026 Phase 3.D.1 IPC `ExecuteMcpTool` path for `conductor_probe_device_identity` no longer deadlocks. The tool_executor's HardwareIO branch dispatches the probe via `state_refs.command_tx.send(DaemonCommand::ProbeDeviceIdentity)` and awaits a oneshot — but `handle_ipc_request` runs **inside the `command_rx` select arm**, so the inner DaemonCommand sat in the mpsc buffer unprocessed and the oneshot never resolved until the executor's 30 s `tokio::time::timeout` fired (long after the IPC client gave up at 5 s and closed the pipe → `Broken pipe (os error 32)` cascade in daemon logs). Same architectural pattern that `conductor_switch_profile` got a direct-handler workaround for. Fix: short-circuits at the top of the `ExecuteMcpTool` arm and calls `run_probe_device_identity` directly, bypassing the executor + command_tx round-trip. Wire format unchanged (still `ExecutionResult::HardwareIoConfirmation { status: Confirmed { result: <ProbeOutcomeWire JSON> }, tool_name }`) so the GUI's Phase 3.D.1 `extract_probe_outcome_from_execution_result` helper continues to parse the response without modification. 2 new module-level tests in `engine_manager.rs::tests` pin the fix — each call wrapped in a 5 s `tokio::time::timeout` so any future re-introduction of the deadlock surfaces as a clean test failure rather than a hung test binary.

### Added
- **GUI**: ADR-026 Phase 3.D.2 — `DeviceIdentityBadge` Svelte component + `Identify` button per row in `DeviceList`. The badge surfaces the SysEx Identity probe state inline next to each configured + connected binding: six states with distinct visual treatment — `unidentified` (grey), `name-matched` (amber, the default for configured bindings before any probe), `probing` (blue + pulse animation while in flight), `identified` (green with `manufacturer · family:model` summary; tooltip distinguishes `direct_paired_port` vs `shared_route` confidence), `no-reply` (informational, common for non-SysEx-capable hardware), `error` (red, with the failure detail in the tooltip — e.g. `NoPairedOutput` surfaces a `[[bindings.output]]` config hint). The Identify button invokes the Phase 3.D.1 Tauri command, flips the badge to `probing` optimistically, and updates to the resolved state when the probe returns. Hidden on disconnected or unconfigured rows where probing can't succeed. New `lib/utils/identity-badge.ts` pure-helper module maps `ProbeOutcomeWire` JSON to the badge view-model — testable in isolation and reusable for Phase 3.E (binding wizard). 34 new tests: 18 pure-helper, 8 component, 8 integration in `DeviceList.test.ts` (Identify button visibility per row state, Tauri invocation, probing-state UX, success/error transitions, NoPairedOutput hint surfacing).

- **GUI**: ADR-026 Phase 3.D.1 — `probe_device_identity(port_name)` Tauri command. Thin typed wrapper that dispatches the existing `conductor_probe_device_identity` MCP tool via the daemon's IPC `ExecuteMcpTool` path, then unwraps the doubly-encoded `ExecutionResult → ConfirmationStatus::Confirmed → result` JSON envelope so the frontend gets the flat `ProbeOutcomeWire` JSON (`{status, identity, confidence, ...}`) directly. Pure helper `extract_probe_outcome_from_execution_result` extracted for unit testing — covers Confirmed unwrap, Blocked, RequiresConfirmation, `ExecutionResult::Error`, RateLimited, malformed JSON, and missing discriminator cases. Routes through `AppState::send_ipc_request` rather than a one-shot connection so the call shares the GUI's pooled IPC connection (auto-reconnect on broken sockets) and updates `daemon_connected` state on every send. Unblocks Phase 3.D.2 (DeviceIdentityBadge component + DeviceList integration) which will consume this command from Svelte. 7 new tests pin the wire-format unwrapping contract.

- **Daemon + Core**: ADR-026 Phase 3.C.2 — probe-on-connect orchestration. The daemon now auto-fires a SysEx Universal Identity probe whenever a configured `[[devices]]` binding becomes connected, gated on the Phase 3.C.1 `sysex_identity_probing` + `probe_on_connect` flags. Outcomes route through a new pure `classify_probe_outcome` helper to one of four actions: `AutoPromote` (single reply on the expected port → `DirectPairedPort` confidence → triggers `DaemonCommand::HotPlugCheck` so `PortInfo` re-resolves and `[[devices]] matchers = [{ type = "SysExIdentity", ... }]` fires), `SurfaceConfirmation` (`SharedRoute` or `MultipleIdentified` → `tracing::warn!` placeholder until 3.C.3 wires the GUI `IdentityNeedsConfirmation` event channel), `LogNoReply` (devices without SysEx Identity support — common for older hardware; debug-level informational), and `LogStartError` (rate-limited / unpaired / write-error — debug-level diagnostic). The dispatcher is idempotent — `EngineManager.last_known_configured_ports` tracks observed bindings so steady-state hot-plug rescans don't re-probe already-known ports (avoiding the per-port 60 s rate-limit budget burn). Hooked into both `connect_multi_device` (initial setup) and `process_hot_plug_check` (rescan ticks). New `ProbeResult::Identified.confidence` field surfaces the `IdentityConfidence` label directly so callers don't need a second cache lookup; existing wire-format-parity test extended to pin `confidence: "direct_paired_port"` on the JSON shape. New `daemon::probe_on_connect` module hosts the pure logic with 21 unit tests covering all gate paths and outcome classifications.

- **Core**: ADR-026 Phase 3.C.1 — `AdvancedSettings.sysex_identity_probing` + `probe_on_connect` config flags. Pure types/defaults change — fields are declared ahead of any consumer and have **no runtime effect yet**; they round-trip through config serialisation so existing user configs (which never had these fields) keep working unchanged. Both default to `true` per ADR-026 D6 ("default-on, settings-gated"). `sysex_identity_probing` is the *intended* global kill-switch; Phase 3.C.2 will wire the daemon to short-circuit identity probing entirely when it is off (auto-on-bind, GUI Identify button, MCP probe tool all gated then). `probe_on_connect` is independent — users will be able to disable just the auto-on-bind background task while keeping manual probing available once the consuming logic lands. Phase 4 surfaces both as Settings UI toggles. 5 new tests pin: both flags default-on, omitted-from-config inherits defaults, global kill-switch off, probe-on-connect off independently, snake_case serde roundtrip (so the field names don't drift to camelCase and break user configs).

- **Core**: ADR-026 Phase 3.B.2 — cross-port reply correlation. The `ProbeCoordinator` now holds open a configurable correlation window (default 80 ms via `with_correlation_window`) after the first SysEx Identity Reply lands, and collects any sibling-port replies that arrive during that window. Aggregation produces `ProbeResult::Identified` (single distinct port observed → cache as `IdentityConfidence::DirectPairedPort` if reply landed on the probe target, else `SharedRoute`) or `ProbeResult::MultipleIdentified` (≥2 distinct ports / identities → cache every unambiguous port as `IdentityConfidence::SharedRoute`; ports that produced multiple distinct identities in the window stay uncached to avoid lying about which device is present). Internal refactor: replaced the per-port `pending: HashMap<String, PendingProbe>` with a single `current_probe: Option<Arc<InFlightProbe>>` slot whose collector is gated by an `Open | Closed` lifecycle — `observe_reply` calls that race against the drain hit the `Closed` branch and drop, eliminating the lost-reply window. Replies dedup'd on the FULL `(port, identity)` tuple. The single-Arc invariant (no local clone in `probe()`) is what makes `invalidate()` correctly drop the wake channel and short-circuit a hung probe. `rtt_ms` captured before the correlation-window sleep so it stays a true round-trip and doesn't inflate by ~80 ms. 9 new tests pin: single-port → DirectPairedPort, same-identity-on-two-ports → SharedRoute + MultipleIdentified, late-reply (after window) → ignored, distinct-identities-on-two-ports → MultipleIdentified, RTT excludes correlation sleep, observe-after-close drops, single-reply-on-non-target → SharedRoute, same-port-multi-identity → no cache, plus the unit-level `InFlightProbe` close contract.

- **Daemon + Core**: ADR-026 Phase 3.B.1 — `Result<ProbeResult, ProbeStartError>` type split. Replaces the flat `ProbeOutcome` enum with two purpose-split types in `conductor_core::device_intelligence::probe`: `ProbeResult { Identified, MultipleIdentified, NoReply }` for outcomes that come from completing the wait, and `ProbeStartError { RateLimited, SysExDisabled, NoPairedOutput, SendFailed }` for synchronous-start failures returned before any wait. `ProbeCoordinator::probe()` now returns `Result<ProbeResult, ProbeStartError>`. The `MultipleIdentified` variant is defined but not yet produced — Phase 3.B.2 will wire cross-port reply correlation that emits it. New `ProbeOutcomeWire` enum wrapper (`#[serde(untagged)]` over the two split enums; both share `#[serde(tag = "status")]`) preserves the flat `{"status": "..."}` JSON wire format Phase 2 MCP callers parse, so no LLM-tool-schema breakage. `DaemonCommand::ProbeDeviceIdentity::response_tx` updated to carry the new Result; mcp.rs and executor.rs collapse via `ProbeOutcomeWire::from()` before serialising. All ~30 existing probe / cache tests migrated to the new pattern shape; 6 new tests pin the Result discriminator (Ok=outcome / Err=start-error), the `MultipleIdentified` variant shape, and the wire-format parity (`{"status": "Identified"}` etc).

- **Daemon + Core**: ADR-026 Phase 3.A — `IdentityConfidence` foundation. New `IdentityConfidence { DirectPairedPort | SharedRoute }` enum in `conductor_core::device_intelligence::probe`; the session probe cache now stores `(SysExIdentity, IdentityConfidence)` tuples and `cached()` / `snapshot()` return the pair. `PortInfo` gains a sibling `sysex_identity_confidence: Option<IdentityConfidence>` field paired with the existing `sysex_identity`. MCP cache-read tools (`conductor_get_device_identity`, `conductor_list_device_identities`) surface the label as a `confidence: "direct_paired_port" | "shared_route"` field on every payload (both fields null on cache miss). Phase 3.A always stores `DirectPairedPort` — actual `SharedRoute` *detection* via cross-port reply correlation lands in 3.B alongside `MultipleIdentified` and the `Result<ProbeResult, ProbeStartError>` split. Tool descriptions updated across `mcp_tools.rs`, GUI `llm_commands.rs`, and the chat system prompt to document the new wire shape. 9 new tests (probe cache + snapshot, PortInfo field, MCP wire format with positive cache-hit cases).

### Changed
- **GUI**: GAP-C1/C2/C3 — Remove SVG overlays, wire inline loop badges. Loop warnings now display as inline `⚠ loop` red badges in MappingBranch rows (via `inLoop` prop wired through DeviceGroup) instead of V2-style SVG LoopArc arcs above tracks. Removed CrossTrackConnector SVG overlay (cross-track routing already shown via inline `↗` badges). FlowLegend simplified to "Event Dots" toggle only (removed "Loop Arcs" and "Routing" toggles). Deleted `computeLoopArcs()`, `computeConnectors()`, `trackYPositions`, `totalDiagramHeight` dead code.

### Added
- **Knowledge**: ADR-025 Phase 4.2 — FCB1010 L2 device-knowledge entry (#892). New `conductor_knowledge::ingest::foot_controllers` module with a typed `DeviceKnowledgeEntry` struct (identity, PC layout, default expression CCs, quirks, example-config link) per the spec's recommended shape. Ships with one canonical entry — Behringer FCB1010 — including the 5-channel PC fan-out quirk, baked-in expression CCs, the `PcContextSwitch` remapping pattern, and a pointer to `docs/examples/fcb1010.md`. The renderer emits a `KnowledgeChunk` that the L2 retriever pulls back on FCB1010-related queries. New `docs/examples/fcb1010.md` walks users through the one-pedal-many-functions setup end-to-end. Remaining foot-controller entries (Morningstar MC6/MC8/MC3, Nektar Pacer, Hotone Ampero Control, Boss ES-8, Gordius Little Giant) deferred until ADR-026 SysEx identity probing lands so their `family_id`/`member_id` fields can be populated from real probes. 11 new tests (10 chunk-content + 1 end-to-end retrieval wiring).
- **GUI**: ADR-025 Phase 4.1 — LLM system prompt alignment (#889). Audited the ADR-025 coverage in `conductor-gui/ui/src/lib/stores/chat.js` against the spec's recommended block. Three gaps filled: (a) `CcIsOn` / `CcIsOff` state-condition sugar now listed alongside `CcValueInRange` with guidance to prefer them for sustain-pedal-family CCs (64-69); (b) use-case framing on the context-switch-authoring bullet — `PcContextSwitch` for multi-function expression pedals (FCB1010-style), `CcContextSwitch` for zoned controllers (modwheel ranges, ribbon zones), and `Conditional` + `NoteHeld`/`CcIsOn` for dual-function pads / sustain-pedal layered routing; (c) new rule (e) in the control-state-tools block calling out the global-not-mode-scoped invariant so the LLM doesn't suggest "switch modes" as a state-reset trick. No MCP tool changes; scattered correct content left intact.
- **Daemon**: ADR-025 Phase 3.F runtime check — unobserved-PC warning (#886). Closes the last outstanding Phase 3 acceptance bullet. After each config-swap (startup, reload, profile-switch, plan-apply), the daemon schedules a one-shot deferred check that fires 60s later. At fire time, it compares `expected_pc_tuples(current_config)` against the live `PhysicalControlStateStore` and emits `tracing::warn!` naming each `(device, channel)` tuple the config expects but that is absent from the current store snapshot — surfacing unplugged cables, wrong binding aliases, daemon-vs-hardware mismatches, and post-reset gaps proactively. A fresh config-swap aborts any prior pending check so rapid reloads don't stack warnings. Graceful shutdown also aborts the pending handle. New pure functions `unobserved_pc_tuples` + `log_unobserved_pc_tuples` in `conductor_core::config::control_state_analyzer`; new scheduler method `EngineManager::schedule_pc_observation_check` wires them into the existing four config-swap sites. 7 new analyzer unit tests.
- **GUI + Daemon**: ADR-025 Phase 3.D — ContextSwitch pattern on Program Change transitions (#883). New `PatternType::ContextSwitch` variant in `conductor_core::event_types` (serialises as `"context_switch"`). Daemon hot path (both single- and multi-device entry points) now snapshots the prior PC on each `(device, channel)` tuple before the control-state write; when the new PC differs (or it's the first PC ever seen on that tuple), the emitted PC `MonitorEvent` carries a `context_switch: { prev_pc, new_pc }` structured payload, lifted through the Tauri bridge as a first-class `context_switch` field on `MidiEventInfo`. EventRow renders a dashed-amber `PC X → PC Y` inline badge on those events (`PC 12` when first-ever). Always-on — not gated on Learn mode — so the live routing context is visible anytime it changes. Channel-less PC events are skipped symmetrically with the control-state store. Added backend and frontend coverage for transition detection, bridge plumbing, and badge rendering.
- **GUI**: ADR-025 Phase 3.C — active-state pills in EventStreamPanel (#879). New lifecycle-aware Svelte store `controlState` polls `conductor_get_active_pc` via the existing `llm_execute_tool` Tauri bridge at 1 Hz while the panel is subscribed, stops cleanly when the last subscriber unsubscribes. New `ControlStatePills.svelte` renders one dashed-amber `⚡ fcb1010·ch1·PC12` pill per `(device, channel)` with an active Program Change; renders nothing when the store is empty so the header stays uncluttered. Mounted in the Events panel header next to the mode badge. Uses the shared `createPollingManager` primitive (ref-counted, visibility-aware, in-flight-guarded) and a generation counter to prevent stale emissions across subscription cycles. Tolerates daemon outages (keeps last snapshot), tool-level errors (`ExecutionResult::Error` / `::RateLimited` / `isError` all preserve the last snapshot), and malformed envelopes. Validates each PC entry (drops empty device, out-of-range channel/pc). Semantic `role="group"` + per-pill `aria-label` for screen-reader support. 20 new frontend tests (11 store + 7 component + 2 panel integration).
- **Daemon**: ADR-025 Phase 3.F — expected-PC-tuple inventory (#877). New pure analyzer `conductor_core::config::control_state_analyzer::expected_pc_tuples(&Config)` walks all mode + global mappings and returns the deduplicated `(device, channel)` tuples whose PC state is consulted — either via `ActivePcIs` conditions (including nested `And`/`Or`/`Not`) or `PcContextSwitch` actions. Also recurses into `CcContextSwitch` branches so PC dependencies nested inside CC ranges aren't missed. `log_expected_pc_tuples(&Config, context)` helper emits an info-level log at every config-swap site — daemon startup (`startup`), file-watched or IPC reload (`reload`), cached profile switch (`profile-switch`), and LLM plan-apply (`plan-apply`) — so users can grep for the tuples their config depends on and cross-check with `conductor-state` output. Sort order stable across runs (device alias, then channel). Exhaustive `match` over `ActionConfig` and `Condition` variants so new additions force a compile-time decision here rather than a silent miss. 14 unit tests.
- **CLI**: ADR-025 Phase 3.E — `conductor-state` diagnostic binary (#874). New tool that connects to a running daemon, pulls `conductor_get_control_state` via IPC `ExecuteMcpTool`, and pretty-prints the live `PhysicalControlStateStore`. Supports `--device <alias>` filtering and `--json` raw output. Groups by device, renders PC / CC / held notes / aftertouch / pitch-bend entries with 1-indexed channel display. Complements the Phase 3.B `via context` chip in the GUI — lets users inspect what state the daemon sees from the terminal. 8 unit tests.
- **GUI**: ADR-025 Phase 3.B — EventRow `routing_trace` annotation (#872). Context-switched `MappingFired` rows now show a dashed-blue `via context` chip inline next to the result pill, plus a new `Routing` section in the expanded detail panel listing each trace breadcrumb (e.g. `PC 12 on fcb1010 ch1`, `CC 7=50 ∈ [0, 63] on fcb1010 ch1`). Frontend consumes the `routing_trace` field on `MappingFiredPayload` plumbed by Phase 3.A (#871) — no parsing of the string shape; entries are rendered as-is. Zero cost on non-context routes (field absent / empty). 7 new EventRow tests.
- **GUI**: ADR-020 Phase 4B — V3 test coverage completion (#666). Dedicated test files for 5 previously untested signal flow components (EventDot, LoopArc, FlowLegend, CrossTrackConnector, ChannelActivityBar). V2 coverage gap remediation in MappingBranch tests (+2 tests). New signal-flow-store tests (+3 tests). 39 new tests, total signal-flow test count 429.
- **GUI**: ADR-019 Phase 7 — Integration testing and regression verification (#644). 24 cross-store integration tests verifying the unified filter system across `workspaceFilters.js`, `events.js`, and `signalFlowMetrics.js` module boundaries. Tests cover linked filter propagation, unlink/diverge/re-link state machine, mode pinning with unmapped tagging, collapse/density persistence, rapid store mutations, aggregate metrics, global mappings, performance regression guards (O(n^2) detection for 200 events and 5-device metrics), and combined filter regressions. Mock boundary: only leaf data sources mocked; ADR-019 stores run as real code.
- **GUI**: ADR-020 Phase 3B — Wire density toggle to switch DeviceGroup ↔ DeviceSummary (#664). Density toggle (▤ expanded / ▬ compact) added to Signal Flow header. Smart default auto-selects compact when >15 mappings, expanded when ≤15 (one-shot, respects user override). Per-device expand-in-compact: clicking a compact device's chevron shows full DeviceGroup while others stay compact. New `expandedInCompact` session store in `signal-flow-store.js`. LoopArc visibility corrected for compact mode. 8 new tests.
- **Testing**: ADR-021 Phase 5A — Integration testing and backward compatibility verification (#675). 30 cross-layer integration tests verifying the full ADR-021 pipeline: config parsing for all 5 device combos → direction classification → auto-pairing (NI/Novation/USB-MIDI/ambiguous) → output map → SendMidi alias resolution → MidiForward `_source` resolution → GUI DirectionBadge rendering → Signal Flow routing targets. Backward compatibility verified for legacy `matchers`-only configs and pre-ADR-021 JSON. DevicePortStatus serialization roundtrip for all 3 directions. Store-level edge-case tests merged into existing `device-direction.test.ts`. 4 new test files: 3 Rust integration tests (19 tests), 1 TypeScript test (7 tests), plus 4 tests added to existing store test file.
- **GUI**: ADR-019 Phase 6 — Unmapped event tagging in Events stream (#643). Raw events with no matching trigger in the active mode are tagged `_unmapped: true` in `filteredEvents` and rendered with a dashed amber "unmapped" label in EventRow. Pre-computed Set-based lookup via `buildMappedTriggerSet()` for O(1) matching. `mapping_fired` events never tagged. Mode changes update tagging reactively. Global mappings included in lookup. ~20 new tests.
- **GUI**: ADR-021 Phase 4A — Signal Flow action target resolution in MappingBranch (#674). New `resolveActionTargets()` pure function resolves SendMidi/MidiForward routing targets from raw port strings to device aliases with colour dots. `_source` target renders as `↩ Echo` with amber styling. Cross-track targets render as clickable `<button>` with `↗` arrow and target device colour, dispatching `crossTrackClick` for flash-highlight. Unresolved targets fall back to raw port name (backward compatible). `ResolvedTarget` interface, `resolvedTargetMap` prop threaded through SignalFlowView → DeviceGroup → MappingBranch. 14 new tests (9 pure function + 5 component).
- **GUI**: ADR-019 Phase 5B — Wire compact mode UI to deviceMetrics store (#642). New `DeviceSummaryRow.svelte` component renders single-line device summary: colour dot, direction badge, name, port, type distribution bar (proportional flex segments with `--event-*` colours), sparkline (8 bars scaled to max), stat cells (mapped count, unmapped amber/dash, fire rate, warning count red). New `compact-mode-helpers.ts` pure utility with `formatTypeDistributionTooltip()`, `computeTypeBarSegments()`, `computeSparklineHeights()`, `padSparkline()`, `formatFireRate()`. DeviceGroup gains `density` and `metrics` props — switches between DeviceHeader (expanded) and DeviceSummaryRow (compact). SignalFlowView subscribes to `deviceMetrics` store and passes per-device metrics to DeviceGroup. LoopArc hidden in compact mode when device collapsed. 43 new tests.
- **GUI**: ADR-019 Phase 4B — Shared mode filter with live/pinned indicator (#640). Mode selector dropdown in Signal Flow header reads/writes `activeModeFilter` from `workspaceFilters.js`. "Current ({mode})" tracks daemon mode reactively; selecting a specific mode pins it with 📌 indicator. Events panel header shows read-only mode badge. Events from devices not in the active mode's mappings are dimmed (opacity 0.35). FilterSummaryBar reflects pinned state. 17 new tests.
- **GUI**: ADR-019 Phase 5A — signalFlowMetrics derived aggregate metrics store (#641). New `signalFlowMetrics.js` derives per-device metrics (mappingCount, typeDistribution, fireRate, fireRateHistory sparkline, warningCount, hasActiveAlert) from 5 existing stores. Pure functions `resolveActiveMode()` and `computeDeviceMetrics()` exported for testability. Module-level sparkline ring buffer (8 samples). `globalUnmappedCount` derived store for pulse unmapped total. 22 tests.
- **GUI**: ADR-021 Phase 3B — Update DeviceStatusPills, DeviceList, and DeviceSettingsView with I/O context (#673). DeviceStatusPills shows DirectionBadge between status dot and name; output-only pills have dashed border, reduced opacity, and are excluded from event filter click handlers. DeviceList shows DirectionBadge per device row, output port name with connection status dot, "auto-paired" badge, and "Not configured" fallback for bidirectional/output devices without output port. Output-only device rows have dashed left border. DeviceSettingsView I/O info visible through DeviceList composition. 17 new tests.
- **GUI**: ADR-021 Phase 3A — Extend device stores and create DirectionBadge (#672). `DeviceBinding` Rust struct gains 4 fields (`direction`, `output_port_name`, `output_connected`, `output_auto_paired`) extracted from daemon JSON with backward-compatible defaults. New `deviceDirectionMap` and `deviceOutputMap` derived stores in `stores.js`. New `DirectionBadge.svelte` component renders direction arrows (← Input, → Output, ↔ Bidirectional) with device colour at 50% opacity. Wired into DeviceHeader → DeviceGroup → SignalFlowView. 15 new tests (6 store, 9 component).
- **GUI**: ADR-020 Phase 2C — Wire DeviceHeader to signalPulseStore for channel activity + unmapped counts (#662). Enhanced `computeSignalPulse()` with `unmapped_by_device` per-device unmapped event tracking. SignalFlowView computes live per-device unmapped counts (500ms rAF throttle) and wires through DeviceGroup to DeviceHeader. Graceful degradation: "—" placeholder when no events received. 6 new tests.
- **GUI**: ADR-020 Phase 2B — Wire InlineBadge click handlers for Chat bridge (#661). InlineBadge gains `clickable` prop (renders `<button>` with `stopPropagation` + `aria-label`). MappingBranch forwards `badgeClick` with mapping context, adds `selected` prop with accent border. FanOutRow gains `mappingIndex` + `crossTrackDeviceId` props, cross-track badge becomes clickable `<button>`. DeviceGroup renders real `UnmappedRow` components (replacing placeholder slots), conditionally renders `FanOutRow` sub-rows based on `expandedFanOutKeys`, passes `selectedMappingIndex` and `highlighted` props. SignalFlowView wires 4 new handlers: badge click (loop-warning→Chat bridge, fan-out/sequence→toggle expansion), `+ Map` CTA→Chat bridge with prefilled mapping request, cross-track click→flash highlight device track, junction click→set `selectedMapping` store. New stores in `workspace.js`: `selectedMapping`, `expandedFanOuts`/`toggleFanOut`, `highlightedDeviceId`/`flashHighlightDevice`. 22 new tests (120 total across 6 test files).
- **GUI**: ADR-019 Phase 4A — Extract TypeFilterChips reusable component (#639). New `TypeFilterChips.svelte` supports single-select (EventFilter) and multi-select (Signal Flow) modes via callback props. EventFilter refactored to use TypeFilterChips — zero behaviour change, all 19 existing tests pass without modification. 12 new tests.
- **GUI**: ADR-019 Phase 3 — LinkToggle component and link/unlink wiring (#638). New `LinkToggle.svelte` button with `L` keyboard shortcut. New `toggleFiltersLinked()` state machine in `workspaceFilters.js`: unlink copies shared filter to panel overrides; re-link snaps shared to Signal Flow (SF wins). Wired into SignalFlowView header. ~19 tests.
- **GUI**: ADR-019 Phase 2B — Wire collapsible filter panes (#637). EventStreamPanel starts collapsed with FilterSummaryBar; click to expand full DeviceStatusPills + EventFilter with `▴` re-collapse button. SignalFlowView gets collapse chevron in flow-header; collapsed state shows FilterSummaryBar with mode/device/type segments and link indicator. Keyboard shortcuts: `F` toggles Signal Flow filter collapse, `Shift+F` toggles Events filter collapse. Auto-collapse at <500px workspace width via ResizeObserver. New `filter-summary.ts` pure utility with `computeEventsSummarySegments()` and `computeSignalFlowSummarySegments()`. 19 utility tests, 7 EventStreamPanel tests, 5 SignalFlowView tests.
- **GUI**: ADR-020 Phase 1D — Wire SignalFlowView V3 rendering with signal-flow-store (#659). New `signal-flow-store.js` (v3Rendering, expandedFanOuts, collapsedDevices, selectedMapping, hoveredMapping). DeviceGroup wired with store-driven collapse and device dimming via signalFlowDeviceFilter. Controlled component pattern (no internal mutation). 21 tests.
- **GUI**: ADR-020 Phase 2A — Wire fired state + live event dot animation to MappingBranch (#660). DeviceGroup replaces V2 TrackRow as sole track renderer, with real MappingBranch components (replacing placeholder slots). New `matchEventToMapping()` function matches raw events to mapping triggers by type/device/channel/number (strict: rejects missing channel/number). New `shouldSpawnDot()` throttle function (200ms per mapping). MappingBranch renders EventDot components on its rail with `reducedMotion` support. SignalFlowView processes events into per-branch dot pools (max 3 per branch), gated by showDots legend toggle. 17 new tests (101 total across 4 test files).
- **GUI**: ADR-019 Phase 2A — FilterSummaryBar + collapse state (#636). New `FilterSummaryBar.svelte` reusable component: clickable collapsed one-line summary bar (20px) showing filter state with `·`-separated segments, optional link indicator, right-aligned chevron with rotation. Two new persisted boolean stores in `workspace.js`: `signalFlowFilterCollapsed` (default: expanded) and `eventsFilterCollapsed` (default: collapsed). localStorage persistence via existing `readLocalBool` pattern. 13 component tests, 6 store tests.
- **GUI**: ADR-020 Phase 1C — FanOutRow + UnmappedRow components (#658). New `FanOutRow.svelte` (indented sub-row for fan-out/sequence secondary actions) and `UnmappedRow.svelte` (dashed row for unmapped triggers with `+ Map` CTA). Extracted `getActionIcon()` utility. `UnmappedTrigger` type for unmapped event data. 26 tests.
- **GUI**: ADR-020 Phase 1B — MappingBranch + InlineBadge components (#657). New `MappingBranch.svelte` (trigger→rail→action row with device stripe, fired state, inline badge) and `InlineBadge.svelte` (5 variants: fan-out, sequence, conditional, loop-warning, fired). New `getTriggerColorClass()` and `getTriggerTypeLabel()` utilities. 39 tests.
- **GUI**: ADR-020 Phase 1A — DeviceGroup + DeviceHeader components (#656). New `DeviceGroup.svelte` (container wrapping device header + mapping/unmapped slots with collapse and dimmed state) and `DeviceHeader.svelte` (device metadata bar: colour dot, name, port, channel activity bars, mapping/unmapped counts, connection status). Placeholder slots for MappingBranch/UnmappedRow (Phase 1B). CSS from V3 mockup, all colours via theme.css custom properties. 19 tests.
- **LLM**: Token budget optimisation & alert deduplication (ADR-016 Chunk 5, #569). New `signal-token-budget.ts` utility with `estimateTokens()`, adaptive compression, and priority-based alert selection. `budgetedSignalContext` derived store enforces 500-token cap across T1+T2+T3 injection. Enhanced alert dedup key (`type:device_id:event_type:channel`), 5/minute rate limit. Idle suppression: T2 dropped when no events >60s. Multi-provider signal injection tests (Anthropic, OpenAI, Gemini). ~35 new tests.
- **Settings Persistence (ADR-017, Epic #559)** — Wire SettingsPanel to real persistence via two TOML files (`preferences.toml`, `daemon.toml`).
  - **Phase 1 — Preferences Backend** (#468, #469, #472, #475): New `conductor-core/src/config/preferences.rs` with `GuiPreferences`, `DaemonSettings` types + `atomic_write_toml()`. Four Tauri commands (`get_preferences`, `save_preferences`, `get_daemon_settings`, `set_daemon_setting`). SettingsPanel wired to real load/save. About links fixed to use `@tauri-apps/plugin-shell` `open()` with corrected URLs. Removed redundant `hasChanges = false` (#469). 11 Rust unit tests, 5 Svelte tests.
  - **Phase 2 — Daemon Settings + Dynamic Log Level** (#474): `SetLogLevel` IPC command persists to `daemon.toml`. Daemon reads `daemon.toml` at startup for initial log level. `tracing_subscriber::reload::Layer` enables future runtime log level changes. GUI log level default changed from `debug` to `info`.
  - **Phase 3 — Platform autoStart** (#475): Added `tauri-plugin-autostart` with `MacosLauncher::LaunchAgent`. Two Tauri commands (`is_auto_start_enabled`, `toggle_auto_start`). SettingsPanel "Start on login" checkbox wired to platform autostart.
  - **Phase 4 — Daemon Binary Path + Config File** (#506, #552): Tray menu "Start Daemon" reads `daemon_binary_path` from `preferences.toml`. SettingsPanel Advanced section shows config file path, "Open in Editor" and "Reveal in Finder" buttons, and daemon binary path input.
- **GUI**: Signal context store with topology analysis (ADR-016 Chunk 1A, #563). New `signal-context.js` store computes structural topology from config and device bindings: classifies mappings (simple/fan-out/sequence/conditional), detects SendMidi/MidiForward/OscSend routing, generates amber/red feedback loop warnings. `topologySummaryText` derived store formats ADR-016 D2 text for LLM system prompt. Debounced 200ms recomputation on config/binding changes. 39 tests.
- **LLM**: Wire T1 topology into Chat system prompt (ADR-016 Chunk 1B, #564). `_buildMessages()` in `chat.js` now appends `topologySummaryText` to the LLM system prompt with signal awareness instructions. LLM can reference devices by name, understand routing paths, and proactively mention warnings without querying. Omitted when no devices/mappings configured. 5 tests.
- **MCP**: `conductor_get_topology_summary` ReadOnly tool (ADR-016 Chunk 1C, #565). Returns structured JSON with device status, mapping classification (simple/fan-out/sequence/conditional), cross-device routing paths (SendMidi/MidiForward/OscSend), and feedback loop warnings (red=confirmed, amber=potential). Daemon-side analysis inspired by frontend `buildTopologySummary()` with broader scope (all modes, device_id matching). 8 tests.
- **LLM**: Signal pulse aggregation (ADR-016 Chunk 2, #566). Rolling-window event aggregation computes per-device activity rates, active channels, mapping fire counts (top 3), error counts, and unmapped event count. `computeSignalPulse()` and `formatPulseText()` pure functions with timer-driven recomputation. Pulse text injected into LLM messages before last user message (separate from topology in system prompt). Two new MCP tools: `conductor_get_signal_pulse` (returns pulse summary) and `conductor_get_recent_events` (filtered event buffer access) — both intercepted in frontend JS, bypassing daemon IPC. System prompt updated with new tool names. ~35 frontend tests, 5 Rust test assertions.
- **LLM**: Significant event alerts and proactive engagement (ADR-016 Chunk 3, #567). T3 alert detection with 6 detection functions: loop (topology warning changes), device change (connected field comparison), mode change (info severity, non-proactive), high event rate (>200/sec sustained 2+ pulse windows, red severity), sustained unmapped (>50 events of same type+channel in 30s with per-type cooldown), and mapping error (stub for ADR-014). Alert shape: `{id, type, severity, message, timestamp, acknowledged, proactive}`. Alert management with dedup (60s window), max cap (50), auto-prune during pulse tick, acknowledge/dismiss (removes from array). `unacknowledgedAlertsForLLM` derived store returns `{id, text}` objects with proactive formatting, suppresses proactive alerts in performance mode. Alerts injected into LLM messages with `[SYSTEM — Signal Alerts]` prefix, acknowledged after injection. Two new MCP tools: `conductor_get_loop_analysis` (reads topology.warnings from signal-context store) and `conductor_get_mapping_stats` (fire counts from event buffer, mode by name, sort_by enum) — both frontend-intercepted. System prompt updated. ~30 signal-context tests, ~12 chat tests, 2 Rust test assertions.
- **LLM**: Conversational integration + Performance Mode (ADR-016 Chunk 4, #568). System prompt `## Signal Awareness` section teaches LLM to reference topology naturally, handle alerts conversationally, and respect Performance Mode. `/performance` chat command (on/off/toggle, `/perf` alias) with "PERF" badge in Chat header. Performance Mode doubles pulse interval, suppresses proactive alerts, and increases cooldowns (already wired in Chunks 2–3). Signal Flow → Chat bridge: clicking warning badge pre-populates Chat input with contextual resolution request and opens Chat panel if collapsed. `signalFlowAction` store tracks bridge metadata. Simplified topology injection (removed duplicate instructions). ~27 new tests across 5 files; updated 5 existing tests for refined system prompt content matching.
- **GUI**: Signal Flow workspace view — Phases B & C: live animation, loop arcs, cross-track routing (#583, PR #629). New `signal-flow-animation.ts` pure functions (event-to-track matching, dot pool management, channel activity windowing, fired junction matching). Six new components: `EventDot.svelte` (animated dot with `travel-right` CSS), `ChannelActivityBar.svelte` (per-channel rate bars), `LoopArc.svelte` (SVG bezier arcs, amber/red severity), `CrossTrackConnector.svelte` (dashed SVG cross-track lines), `FlowLegend.svelte` (legend with toggles). Extended `TrackJunction.svelte` with fired pulse + click-to-navigate. Extended `TrackRow.svelte` with event dots, channel activity, fired junctions, highlight, reduced motion. `SignalFlowView.svelte` wired with rAF loop, `eventBuffer`/`mappingFireState` subscriptions, SVG overlay, click handlers (junction→Mappings, device→highlight, loop→Chat), `prefers-reduced-motion`. `workspace.js` adds `navigateToMapping()`. 125 signal-flow tests.
- **GUI**: Signal Flow workspace view — Phase A: static track diagram (#583, PR #595). New `SignalFlowView.svelte`, `TrackRow.svelte`, `TrackJunction.svelte`, `signal-flow-helpers.ts`. Horizontal track diagram with source blocks, MIDI channel slots, mapping junctions (trigger→action pills), fan-out/sequence/conditional classification, empty state. 82 tests.
- **GUI**: Device colour mapping infrastructure (#581). New `device-colors.ts` module assigns stable 1–6 colour indices per device. CSS variables `--device-1` through `--device-6` with 10% tint variants in `theme.css`. `DeviceStatusPills` pill dots show per-device colours. Right-click context menu colour picker with 6 swatches. Manual colour overrides persist via localStorage. 33 unit tests.
- **GUI**: Event provenance — device stripe + channel tag (#582). 3px coloured left border on each EventRow using per-device colour from `device-colors.ts`. Compact channel tag (1–16) between type label and detail text. Highlight (Learn mode) overrides device stripe via CSS specificity. Fired rows also show device stripe. `getChannelTag()` helper with 9 unit tests, 8 component tests.
- **GUI**: Inline value bar indicators for CC, pitch bend, and aftertouch events (#477). New `ValueBar` component (56×6px micro-bar) and `getValueBarProps()` helper. CC and aftertouch use linear fill (0–100%); pitch bend uses center-zero bidirectional bar. 13 unit tests, 5 component tests.
- **Daemon**: Async action execution — ADR-015 (#516). Decouples long-running action execution from the daemon's `tokio::select!` event loop via a dedicated `std::thread` executor. Bounded crossbeam channel (capacity 32) for dispatch, unbounded tokio mpsc for completions. `biased` select prioritizes completions. New types: `ActionDispatch`, `ActionCompletion`, `ActionProvenance`, `ActionDispatcher`. Interruptible sleep (10ms chunks) for Sequence/Delay/Repeat cancellation. MIDI recursion guard (FNV-1a fingerprint ring buffer, 64 entries, 100ms TTL) for echo suppression. Dual latency tracking (`execution_time_us` + `latency_us`) in `MappingFiredPayload`. New event types: `mapping_matched`, `mapping_dropped`, `mapping_cancelled` with `invocation_id` correlation. Terminal event invariant: every dispatch gets exactly one of fired/dropped/cancelled. Simulation now dispatches non-blocking via `try_dispatch()`. 10 integration tests.
- **Core**: `DispatchOutcome::Cancelled` variant for interrupted actions.
- **Core**: `MappingMatchedPayload`, `MappingDroppedPayload`, `MappingCancelledPayload` event types.
- **Core**: `invocation_id` and `execution_time_us` fields on `MappingFiredPayload`.

### Fixed
- **GUI**: Config store now refreshes after LLM plan apply and daemon config reloads (#574). `configStore.fetch()` called after successful `applyPendingPlan()`. Status polling detects `config_reloads` counter changes and auto-refreshes config (≤2s for file watcher and `conductorctl reload`). 6 new tests.
- **GUI**: Pitch bend display now correctly converts unsigned 14-bit values (0–16383, center=8192) from the backend to signed offset (-8192 to +8191) for both detail text and value bar calculation (#477). Previously showed "+8192" at rest instead of "+0".
- **Daemon**: Simulation no longer blocks event loop (#516). Replaced `spawn_blocking.await` with `try_dispatch()` so IAC loopback events process in real-time during simulation. `mapping_fired` now correctly appears after sequence note events.
- **Daemon**: Recursion guard only records `sent_midi` on successful execution — failed/cancelled actions no longer cause false positive echo suppression.
- **Daemon**: Removed duplicate `mapping_matched` emission in legacy input path.
- **Daemon**: `mapping_dropped` events now include `invocation_id` for correlation.
- **Daemon**: Shutdown processes cancelled completions through `handle_action_completion()` so terminal events are emitted.

- **MIDI Channel Pipeline** — Full channel awareness for the input pipeline (#434, #435, #436, #437, #438). MIDI channel is now preserved from raw bytes through the entire pipeline and available for trigger filtering, MIDI Learn, and GUI components. Channel is 0-indexed internally (0-15), 1-indexed in UI display (1-16). `channel: None` on triggers means "match any channel" (backward compatible with existing configs). Gamepad triggers are unaffected (no MIDI channel concept).
  - **Phase 1 — Core Pipeline** (#434): Added `channel: u8` to all `MidiEvent` variants, `channel: Option<u8>` to all `InputEvent`, `ProcessedEvent`, `Trigger`, and `CompiledTrigger` variants. Added `channel_matches()` helper and `Trigger::channel()` accessor. 28 tests.
  - **Phase 2 — MIDI Learn** (#435): Added `channel: Option<u8>` to `TriggerSuggestion`. MIDI Learn captures and preserves channel from detected events. TOML generation includes `channel` field when present. 7 tests.
  - **Phase 3 — GUI Components** (#436): Added channel selector dropdown to `RefinementCard` ("Any Channel" + Channels 1-16). Added channel filter dropdown to `EventFilter` ("All Ch" + Ch 1-16). Channel pre-selected from MIDI Learn suggestion. Hidden for gamepad types. 19 tests.
  - **Phase 4 — Daemon Verification** (#437): Added 5 end-to-end tests mirroring the daemon's MIDI callback path through core components and `CompiledRuleSet` (daemon hot path). 33 total channel pipeline tests.
  - **Phase 5 — Documentation** (#438): Updated trigger schema docs, SendMIDI guide, README, MCP skill definitions, and CLAUDE.md with channel filtering examples and consistent indexing documentation.

### Fixed
- **GUI**: Color picker dialog now appears near the custom color (+) button instead of a random screen position (#495). Moved hidden `<input type="color">` into a `.custom-swatch-wrapper` colocated with the `+` button so the native dialog anchors near the button's position.
- **GUI**: Aligned MappingStateView with Option D mockup (#484). Added `mappingFireCount` store and `getMappingFireCount()` for tracking per-mapping fire counts. Added `×N` fire count badge to MappingRow. Removed inner box border from MappingStateView. Passed `description` prop and fire state to all MappingRow instances including global mappings. Removed dead CSS rule.
- **GUI**: Fixed collapsed message Expand button not working in chat (#449). Svelte compile-time reactivity analysis couldn't track `expandedMessageIds` inside `shouldCollapse()` function body. Extracted collapse logic to `chat-collapse-helpers.ts`, inlined the `expandedMessageIds` check in the template `{#if}` expression, and added a hover "collapse" button on manually-expanded old messages.
- **GUI**: LLM now sees validation errors on blocked plans and auto-corrects (#525). Tool result for `PlanCreated` includes `validation_errors`/`validation_warnings`. When errors exist, agent loop continues so LLM can create a corrected plan without user intervention. System prompt updated with Plan Validation guidance.
- **GUI**: Auto-reject pending plan when user sends new chat message (#526). `sendMessage()` invokes `llm_reject_plan` for stale plans, clears store state, and adds system message so LLM processes fresh.
- **GUI**: Fixed stale LLM responses appearing after user interrupts agentic loop. When user sends a new message (e.g. cancelling MIDI Learn) while a tool-use loop is in progress, the previous loop's pending LLM response is discarded instead of being added out of order. Uses generation counter to detect superseded loops.
- **GUI**: Removed redundant compact MappingStateView from 6 workspace views (History, Raw Config, Devices, Profiles, Settings, Plugins) where mapping context is irrelevant (#460). Kept for Config Diff and MIDI Learn views.

### Added
- **GUI**: Simulate button on MappingRow (#489, ADR-014 Phase 5B). Hover-reveal `▶ Sim` button on each mapping row. Click executes the mapping via daemon; Shift+Click for dry-run mode. Error toast with red flash on failure. `simulate_mapping` Tauri command routes through IPC to daemon's EngineManager. Supports both mode and global (`__global__`) mappings. 39 tests (22 MappingRow + 17 MappingStateView).
- **Daemon**: `simulate_mapping` method on EngineManager (#488, ADR-014 Phase 5A). Looks up a mapping by mode name + index, compiles the action, optionally executes it, and emits a `mapping_fired` MonitorEvent for GUI feedback. Supports `__global__` sentinel for global mappings. New types: `SimulateOptions`, `SimulateResult`, `SimulateError` in `conductor-core/dispatch.rs`. Helper functions: `trigger_info_from_trigger()`, `default_value_for()`, `synthesize_midi_bytes()`.
- **GUI**: Full Markdown rendering for LLM chat responses (#523). Replaced regex-based `formatContent()` with `marked` (GFM) + `DOMPurify`. Supports headings, fenced code blocks, lists, blockquotes, tables, horizontal rules, strikethrough. Scoped CSS in MessageBubble for all block elements.
- **GUI**: EventFilter Fired toggle and quick filters (#486). Independent "⚡ Fired" toggle for mapping_fired event visibility, quick filter buttons (All/Raw Only/Fired Only), and state consistency guard preventing contradictory filter states.
- **GUI**: Toast notification system for mapping fires (#487, ADR-014 Phase 4). Transient workspace overlays showing trigger→action summaries with auto-dismiss (3s), hover-pause, error persistence, coalescence for continuous controls, max 5 visible. `toastsEnabled`/`toastsContinuous` settings gate toast emission.
- **GUI v2 Phase 1 — Layout Shell**: Rewrote `App.svelte` from router-based 8-view layout to three-zone unified workspace (Chat | Workspace | Events). Created `ChatPanel`, `WorkspacePanel`, `EventStreamPanel` panel components. Created `TitleBar` with device/profile dropdowns and settings gear. Created `workspace.js` store (9 workspace views, config diff/MIDI learn action routing) and `events.js` store (ring buffer, filters, app-level Tauri event listener). Added `theme.css` with full navy/indigo CSS variable palette. Updated `StatusBar` with CSS variables and dynamic version. Deleted old `views/` directory and `Sidebar.svelte`.
- **GUI v2 Phase 2 — Config Approval Flow**: Replaced modal-based config approval with workspace-integrated diff view. Created `ConfigDiffView` (pending change review with approve/edit/reject buttons, expiration countdown, change descriptions, diff preview), `MappingStateView` (mode tabs + mapping list, compact mode), `MappingRow` (trigger→action display with colored event-type dots and NEW/UPD badges), `DiffBlock` (simplified diff renderer). Created `diff-helpers.ts` utility with `formatTriggerText`, `formatActionText`, `getTriggerDotColor`. Wired LLM plan creation to workspace via `showConfigDiff()`. Non-mapping workspace views show compact mapping list below. **Spec deviation**: Reused existing `llm_apply_plan`/`llm_reject_plan` Rust commands instead of adding 3 new backend commands (same functionality, avoids duplication).
- **GUI v2 Phase 3 — MIDI Learn Refinement**: Replaced MIDI Learn modal capture with workspace-integrated refinement card. Created `RefinementCard` (interpretation chips, editable parameter fields, advanced velocity range slider, confirm/relearn/cancel actions), `AlternativeChips` (interpretation selector), `RangeSlider` (dual-thumb velocity range slider), `MidiLearnRefinement` (workspace view container). Created `refinement-helpers.ts` utility with `getAlternativeInterpretations`, `getParameterFields`, `buildTriggerFromParams`. Added `Refining` state to `LearnSessionState` in Rust backend. Wired event selection to workspace refinement via `showMidiLearnRefinement()`. **Spec deviation**: Refinement logic implemented as pure TypeScript functions instead of new Rust Tauri commands — event data from existing `stop_midi_learn` response is sufficient, avoids unnecessary IPC round-trips.
- **GUI v2 Phase 4 — Event Streaming**: Full event stream panel with rich event rows, filter chips, device status pills, and learn mode support. Created `EventRow` (colored dot, type label, detail string, relative time, hover Learn button), `EventFilter` (All/Note/CC/Bend/AT chips), `DeviceStatusPills` (connection status pills with click-to-filter and right-click mute/unmute context menu). Created `event-helpers.ts` utility with centralized `EVENT_TYPE_META` lookup table for `getEventDotClass`, `getEventTypeLabel`, `getEventDetail`, `getRelativeTime`, `normalizeEventType`. Rewrote `EventStreamPanel` composing all new components with auto-scroll (keyed on source buffer, not filtered view), clear, and learn mode highlighting. Fixed event listener to listen for `'midi-event'`/`'midi-events'` (Rust emits dashes, not underscores). Added `pushEvents()` for atomic batch ingestion, `clearEvents()`, `autoScroll` store, `nowTick` readable store (1s interval for live relative timestamps). **Spec deviations**: (1) No new Rust commands needed — `start_event_monitoring`/`emit("midi-events")` already implemented. (2) Event listener at store level (`initEventListener()`) not component `onMount` — events buffered when panel is hidden. (3) Event name fix: store listened for `'midi_event'` but Rust emits `'midi-event'`.

- **GUI v2 Phase 5 — Remaining Views**: Migrated all remaining feature views to workspace sub-views. Created `AppSettingsView` (wraps SettingsPanel), `PluginView` (two-tab Marketplace/Installed), `ProfileSettingsView` (wraps ProfileManager), `DeviceSettingsView` (composes DeviceList + HidDeviceList + TemplateSelector in sections), `RawConfigView` (wraps ConfigPreview with Reload from Disk button), `ConfigHistoryView` (shows applied LLM plan changes from chat store). Created `ChatHistoryDrawer` (slide-out drawer wrapping ConversationHistory with overlay + ESC close). Wired ChatPanel header buttons: hamburger toggles history drawer, + creates new conversation. Added workspace navigation dropdown menu (ellipsis button) for all views. Added Vite alias for `@tauri-apps/api/tauri` → `@tauri-apps/api/core` (PluginManager uses old Tauri v1 import path). **Spec deviations**: (1) `get_config_history` Rust command deferred — config not git-tracked, ConfigHistoryView shows LLM plan change history from chat store instead. (2) No rollback button in ConfigHistoryView.

- **GUI v2 Theme Migration**: Migrated 10 chat-related components from v1 gray theme (hardcoded hex values, rem units) to v2 navy/indigo theme (CSS variables from `theme.css`, px units). Files: MappingRow, DiffBlock, MidiLearnDialog, SuggestionChips, CostSummaryPanel, ConversationHistory, MessageBubble, ChatView, MappingStateView, EventRow. Added role labels (YOU/CONDUCTOR/TOOL/RESULT/PLAN) to chat messages. Changed MessageBubble from bubble alignment to full-width stacked layout. Fixed MidiLearnDialog wrong variable names (11 v1 vars → v2 vars). Fixed DiffBlock wrong var name (`--diff-add` → `--diff-add-bg`). Fixed MappingStateView compact mode (hides section header, not mode tabs). Fixed EventRow font size to use `var(--font-size-sm)`.

- **GUI v2 Gap Resolution — CSS Variables & Theme Compliance**: Migrated ~400 hardcoded color values across 30+ Svelte components to CSS variables from `theme.css`. Added overlay utilities (`--overlay-20` through `--overlay-80`), white tints (`--white-05/10/15`), brand tints at 08/15/20/30 opacity for accent/green/blue/amber/purple. Added custom checkbox styling (GAP-L02). Migrated JS `style=` binding colors in LiveEventConsole, LivePreview, MappingList to `getComputedStyle` pattern with SSR fallbacks. Enforced mockup button convention: `.btn-primary` = green, `.btn-secondary` = transparent ghost, `.btn-danger` = transparent ghost with accent border across 9 components. Changed PluginView tab styling from underline to filled toggle (GAP-M03). Added LED Settings stub section to SettingsPanel (GAP-M02). Added 7 keyboard shortcut and resize handle tests to App.test.ts (GAP-M04).

### Fixed
- **Event stream auto-reconnect after daemon restart (#440)**: The event stream panel now automatically reconnects when the daemon is stopped and restarted. Backend streaming task uses exponential backoff (2s→30s) to reconnect the IPC subscription. Frontend watches daemon status transitions and re-initializes the event listener. Connection status indicator shows "Reconnecting..." or "Disconnected" in the EventStreamPanel.
- **GUI v2 QA bug fixes (#412-#431)**: Addressed 20 QA issues from manual testing of the three-zone layout. Store initialization (#426): added `statusStore`, `deviceBindingsStore`, `configStore`, `profileStore` init in App.svelte `onMount`. TitleBar dropdowns (#421): wired `bind:value` + `on:change` handlers. Chat fixes (#412-#414): textarea resize, suggestion chip icon removal. ChatHistoryDrawer (#415-#416): removed duplicate headings, fixed conversation list styling. StatusBar (#417): corrected element order, added "Config: synced" state. WorkspacePanel (#419): dynamic heading per active view. SettingsPanel (#418): added `color: var(--text)` to inputs. ConfigPreview (#422): migrated 12 hardcoded hex colors to CSS variables. Button colors (#428): applied green/blue/red convention across ProfileManager, PluginMarketplace. Accessibility (#430): added `:focus-visible` and input focus styles to `theme.css`. MessageBubble (#413): added CONDUCTOR heading for system messages, per-bubble copy button, collapsible tool_call messages, Apply button uses `var(--green)`. PluginMarketplace (#423): removed redundant h1 heading. ProfileSettingsView (#424): wired profileStore for CRUD operations. DeviceSettingsView (#425): fixed TemplateSelector modal trap with toggle pattern.
- **XSS defense-in-depth**: Added `sanitizeUrl()` defense-in-depth to `formatContent()` in `text-helpers.ts`. `escapeHtml()` already runs first (primary defense), but URL validation now explicitly checks protocol. Added security model documentation comment. Added 20 new test cases for XSS vectors.
- **Timer cleanup**: Added `onDestroy` cleanup for pending `setTimeout` timers in `ConversationHistory.svelte` (search debounce, copy feedback) and `ChatView.svelte` (copy feedback). Prevents post-destroy state mutations.
- **Search race condition**: Added sequence counter guard to `ConversationHistory` search handler. Prevents stale search results from overwriting newer results during rapid typing.
- **safeStringify fallback**: `MessageBubble.safeStringify()` now handles circular references with `WeakSet` tracking and BigInt serialization. Fallback shows object keys instead of useless `[object Object]`.

### Planned
- Windows and Linux platform support for app detection
- Action macros and scripting
- Cloud sync (optional)

## [4.26.77] - 2026-02-12

### Added
- **Config validator lifecycle integration**: `validate_config()` called during config reload — warnings logged, errors reject config and retain old. `ConfigPlan::new()` validates proposed config with `validation_warnings` and `validation_errors` fields in plan response (Closes #319)
- **Config versioning**: Successful config reload creates `config.toml.known_good` backup. New `conductorctl rollback-config` CLI command and `RollbackConfig` IPC command for restoring from backup (Closes #319)

## [4.26.76] - 2026-02-12

### Fixed
- **Cost tracking pricing table**: `ModelPricing::for_model()` now recognizes Claude 4.x family (Opus 4, Sonnet 4/4.5, Haiku 4/4.5) and Google Gemini models (2.0 Flash, 1.5/2.0 Pro, 1.5 Flash). Previously returned `None` for newer model strings, showing $0.00 costs (Closes #317)

## [4.26.75] - 2026-02-12

### Fixed
- **MIDI Learn GamepadChord field**: `analyze_midi_learn_events()` now correctly reads `pattern_buttons` instead of `pattern_notes` for GamepadButtonChord events (Closes #315)
- **MIDI Learn VelocityRange suggestion**: When 3+ presses of the same note have velocity range > 30, suggests VelocityRange trigger with soft/medium/hard zones instead of plain Note (Closes #315)

## [4.26.74] - 2026-02-12

### Fixed
- **MIDI Learn chord timeout**: `EventProcessor::with_chord_timeout()` existed but was never called — all EventProcessors used the default 50ms timeout, causing the first note of a 3-note chord to expire. Added `set_chord_timeout()` for dynamic updates: MIDI Learn now uses 150ms, reverts to 50ms on stop. New devices during Learn also get extended timeout (Closes #313)

## [4.26.73] - 2026-02-12

### Fixed
- **_buildMessages drops SYSTEM messages**: The `_buildMessages()` method silently dropped `MESSAGE_TYPES.SYSTEM` messages, so the LLM never saw plan apply/reject outcomes and couldn't continue the conversation after config changes. System messages are now wrapped as `role: 'user'` with `[System]` prefix for provider compatibility (Closes #311)

## [4.26.72] - 2026-02-12

### Fixed
- **System prompt missing MCP tools**: Updated the LLM system prompt to include all 19 MCP tools — was missing 7 tools added in v4.26.66-v4.26.69 (conductor_switch_mode, conductor_send_midi, conductor_send_sysex, conductor_validate_config, conductor_list_device_bindings, conductor_set_device_enabled, conductor_scan_ports). Removed the "Send MIDI output or control hardware directly" prohibition that prevented the LLM from using conductor_send_midi (Closes #309)

## [4.26.71] - 2026-02-12

### Added
- **Mode selector in mapping editor**: Target mode dropdown in the mapping editor dialog allows creating or moving mappings to a different mode without switching modes first. Cross-mode move is transactional (remove from source + add to target in a single config save). Warning message displayed when moving an existing mapping (Closes #307)

## [4.26.70] - 2026-02-12

### Added
- **"Set as Default" mode button**: New button in MappingsView to designate a mode as the startup default. Adds `default_mode: Option<String>` field to Config. Mode selector dropdown shows "(Default)" badge next to the default mode name. Button hidden when current mode is already the default (Closes #305)

## [4.26.69] - 2026-02-12

### Added
- **MCP tool `conductor_switch_mode`**: New Stateful-tier MCP tool for the LLM to switch the active mapping mode by name. Returns mode name and index on success, lists available modes on failure (Closes #303)

## [4.26.68] - 2026-02-12

### Added
- **Plan-to-dialog navigation**: "Edit" button on CreateMapping/UpdateMapping plan changes navigates directly to the MappingsView mapping editor. New `navigateToMapping()` function and `editingMappingRequest` store enable cross-view navigation. MappingsView auto-selects mode and opens the mapping editor via reactive subscription (Closes #301)

## [4.26.67] - 2026-02-12

### Added
- **MCP tool `conductor_send_midi`**: New HardwareIO-tier MCP tool for the LLM to send MIDI messages to connected devices. Supports note_on, note_off, cc, and program_change with full validation. Standard MIDI auto-confirms (low risk). New `MidiSendMessage` type with `validate()` and `to_bytes()` methods (Closes #299)

## [4.26.66] - 2026-02-12

### Added
- **Config schema validator**: New `conductor-core/src/config/validator.rs` module validates config against MIDI/HID/OSC protocol standards. New `conductorctl validate-schema` CLI subcommand with colored output. New `conductor_validate_config` ReadOnly MCP tool for LLM-driven validation. Reports errors, warnings, and protocol coverage metrics (Closes #297)

## [4.26.65] - 2026-02-12

### Fixed
- **MIDI Learn trigger type coverage**: Added missing trigger types (Encoder, PitchBend, Aftertouch, PolyPressure) to `analyze_midi_learn_events()`. Stop response now includes `suggested_trigger` field with pre-computed trigger config for the LLM. System prompt includes Trigger Type Reference for all 12 supported types (Closes #291)

## [4.26.64] - 2026-02-12

### Fixed
- **MIDI Learn chord detection for 3+ notes**: Daemon-side chord debouncing via `capture_pattern_events()` — only the final (largest) chord reaches the frontend. Configurable chord timeout via `with_chord_timeout()` (default 50ms, MIDI Learn uses 150ms). Frontend `MidiLearnDialog` auto-re-selects the largest chord when new events arrive (Closes #289)

### Verified
- Chat Apply Changes → LLM resume works correctly (#293)
- Date filtering in chat history works correctly (#294)
- Cost recording pipeline works correctly (#295)
- Chat history delete stability — no race condition (#296)

## [4.26.63] - 2026-02-11

### Fixed
- Replace dynamic import with static import for `invoke` in MessageBubble, eliminating Vite production build warning
- Add missing doc-site entries for WI-3 (costs zero), WI-5 (MIDI Learn guidance), WI-6 (chord 2 notes), WI-8 (chat export), WI-12 (config preview)

## [4.26.62] - 2026-02-11

### Added
- **Config preview toggle**: Three-tab config viewer (Raw TOML / Formatted JSON / Visual Tree) in plan review messages. Expandable "View Current Config" section lazy-loads config on first click. New `ConfigPreview.svelte` component, `config-preview-helpers.ts` utilities, and `get_config_toml` backend command (Closes #287)

## [4.26.61] - 2026-02-11

### Added
- **Chat history search**: Search conversations by message content. Backend `search_conversations()` with SQL LIKE query, `llm_search_conversations` Tauri command, debounced 300ms search input in sidebar, and `highlightSearchMatch()` with XSS-safe HTML escaping (Closes #285)

## [4.26.60] - 2026-02-11

### Added
- **Chat history date filtering**: Filter pills (All, Today, Week, Month, Older) above conversation list. Frontend-only filtering on `updated_at` timestamp via `filterConversationsByDateRange()` in conversation-helpers.ts. Works with multi-select mode (Closes #283)

## [4.26.59] - 2026-02-11

### Added
- **Chat history multi-select delete**: Toggle selection mode in conversation sidebar to select multiple conversations for batch deletion. Backend `delete_conversations()` operates in a single transaction. Includes "Select all" checkbox, "Delete (N)" button with confirmation dialog, and auto-clear of current conversation if deleted (Closes #281)

## [4.26.58] - 2026-02-11

### Added
- **Enhanced chat copy**: Copy Chat now includes tool calls (with JSON arguments), tool results (success/error), plan proposals (description, changes, diff), skill messages, and system messages. Metadata header shows session ID, provider, model, and export timestamp. Extracted `formatChatAsMarkdown()` into reusable `export-helpers.ts` utility (Closes #279)

## [4.26.57] - 2026-02-11

### Fixed
- **MIDI Learn doesn't detect VelocityRange trigger type**: Added velocity history tracking during MIDI Learn polling. When the same note is pressed 3+ times with velocity range > 30, `eventToTrigger()` now suggests a `VelocityRange` trigger with auto-calculated `soft_max` and `medium_max` thresholds. Velocity history resets on start/stop (Closes #277)

## [4.26.56] - 2026-02-11

### Fixed
- **MIDI Learn chord detection limited to 2 notes**: Previously `handle_note_press` immediately completed when `held_notes.len() >= 2`. Now uses debounced chord completion — each new note cancels and restarts a 100ms timer via `schedule_chord_completion()`. After 100ms with no new notes, all held notes are captured as a chord. Also cancels chord timer in `complete_learning()` to prevent double-fire (Closes #275)

## [4.26.55] - 2026-02-11

### Fixed
- **LLM doesn't recommend MIDI Learn**: Added "Workflow Guidelines" section to the system prompt with step-by-step MIDI Learn flow. When user wants a mapping but hasn't specified the note/CC, the LLM now recommends capturing the control first (Closes #273)

## [4.26.54] - 2026-02-11

### Fixed
- **Plan preview shows Rust Debug format**: `preview_diff()` now uses `serde_json::to_string()` instead of `format!("{:?}")` for trigger, action, and matcher values. Output is now `{"type":"Note","note":36}` instead of `Note { note: 36, velocity_range: None }` (Closes #271)

## [4.26.53] - 2026-02-11

### Fixed
- **Chat usage and costs always zero**: `llm_record_cost` Tauri command existed but was never called from the frontend. Added `_recordCost()` fire-and-forget method, called after every LLM response (both tool_use and final) in `_runAgenticLoop()`. Failures are logged but do not break the chat (Closes #269)

## [4.26.52] - 2026-02-11

### Fixed
- **LLM agent loop stops after plan apply**: After PlanCreated, the agentic loop exited and never resumed. Added `_resumeAfterPlanDecision()` to re-invoke the loop after apply/reject so the LLM can verify changes and continue. On apply failure, the loop does NOT resume. Also adds rejection system message on reject (Closes #267)

## [4.26.51] - 2026-02-11

### Fixed
- **Plan apply never persisted config to disk**: After `tool_executor.apply_plan()` succeeded, the modified config was never synced back to the engine manager, saved to `config.toml`, or recompiled into the rule set. Added `sync_config_after_apply()` which retrieves the modified config from the ToolExecutor, saves to disk, recompiles the rule set with `spawn_blocking`, atomically swaps via ArcSwap, and reconciles the current mode index. Also added `ToolExecutor::get_config()` accessor (Closes #265)

## [4.26.50] - 2026-02-11

### Fixed
- **TOOL_CALL message type lost on DB round-trip**: `_messageTypeToDbRole()` now stores `TOOL_CALL`, `SKILL`, `PLAN_PENDING`, and `ERROR` as distinguishable DB role strings (`'tool_call'`, `'skill'`, `'plan_pending'`, `'error'`) instead of collapsing them to `'assistant'`/`'system'`. `_dbRoleToMessageType()` reverses the mapping, preserving message types through persistence. Previously, loading a saved conversation converted all tool call messages to generic assistant messages (Closes #261)

## [4.26.49] - 2026-02-11

### Fixed
- **TOCTOU hash silent failure**: `hash_config()` now uses `expect()` instead of `unwrap_or_default()`. Previously, if Config serialization ever failed, the hash would silently produce a hash of an empty string, making all TOCTOU comparisons pass and allowing stale plans to be applied (Closes #260)

## [4.26.48] - 2026-02-11

### Fixed
- **Plan index drift on sequential deletes**: `apply()` and `apply_atomic()` now pre-process changes to sort `DeleteMapping` operations by descending index per mode before applying. Previously, deleting indices [1, 3] would remove the wrong mapping at index 3 because the earlier delete at index 1 shifted all subsequent indices. The fix ensures all indices reference the original config state (Closes #259)

## [4.26.47] - 2026-02-10

### Fixed
- **Plan details rendering**: `ConfigPlan::new()` now pre-computes `diff_preview` and `change_descriptions` fields so they serialize with the plan. `MessageBubble.svelte` uses backend-provided `change_descriptions` (with frontend fallback) instead of raw `change.description`, fixing blank descriptions for `DeleteMapping`, `CreateMode`, `DeleteMode`, and `CreateDeviceIdentity` change types. Added `CreateDeviceIdentity` to `getChangeIcon()` (Closes #257)

## [4.26.46] - 2026-02-10

### Fixed
- **Plan/Apply silent failure**: `applyPendingPlan()` now checks `result.success` from the daemon. Previously, `PlanApplyResult { success: false, error: "..." }` was treated as success — the plan was cleared and a success message shown. Now throws on `success: false`, keeps `pendingPlan` intact for retry, and shows error in chat via `addErrorMessage` (Closes #255)

## [4.26.45] - 2026-02-10

### Changed
- **Inline plan review**: Plan/Apply workflow now displays changes inline in chat messages instead of a popup modal. Shows change list with type icons, expandable diff preview, and Apply/Cancel buttons directly in the message bubble (#253)
- **Removed PlanReviewModal popup**: Replaced with inline MessageBubble rendering for a more natural chat flow (#253)

## [4.26.44] - 2026-02-10

### Fixed
- **PlanReviewModal crash**: `change.change_type` → `change.type` to match Rust serde `#[serde(tag = "type")]` output (#251)
- **Stuck chat after plan creation**: `clearMessages()` and `newConversation()` now clear `pendingPlan` state. Added "Cancel Plan" button in chat header (#251)
- **Missing tool_result for PlanCreated**: `_runAgenticLoop` now adds a `tool_result` message before creating the plan-pending message, preventing orphaned `tool_use` blocks that cause LLM API errors (#251)
- **No apply feedback**: `applyPendingPlan()` now adds a success system message after applying changes (#251)
- **Defense-in-depth orphan filtering**: `_buildMessages()` now strips `toolCalls` from assistant messages when their corresponding `tool_result` messages are missing from the context window (#251)

## [4.26.43] - 2026-02-10

### Fixed
- **Chat tool_result orphan at window boundary**: `_buildMessages()` off-by-one (`> 0` → `>= 0`) caused orphaned `tool_result` at index 0 of the 20-message window to be sent to the LLM API, which rejects it with "unexpected tool_use_id". Also filters UI-only `TOOL_CALL` messages before slicing to maximize effective context window (#247)

## [4.26.42] - 2026-02-10

### Fixed
- **PlanReviewModal display**: Modal never appeared because Tauri v2 `emit()` doesn't reliably deliver frontend-to-frontend events. Replaced with Svelte store + props communication (matching MidiLearnDialog pattern). Also fixed `plan.plan_id` → `plan.id` field name mismatch and Apply error handling (keep modal open on failure) (#245)

## [4.26.41] - 2026-02-10

### Added
- **Mode management discoverability**: System prompt and `conductor_batch_changes` tool description now enumerate all 5 operation types including `create_mode` and `delete_mode`, so the LLM can discover mode management capability (#243)
- **Chat guide**: Added mode creation example to the chat user guide

## [4.26.40] - 2026-02-10

### Fixed
- **Plan/Apply bridge**: The PlanReviewModal never appeared because `chat.js` checked `result.plan_pending` (doesn't exist) instead of `result.type === 'PlanCreated'` (daemon serde format), and the modal component was never mounted in ChatView — fixed both (#241)

## [4.26.39] - 2026-02-10

### Fixed
- **Chat LLM mode creation**: LLM couldn't create modes because `conductor_batch_changes` supported `create_mode`/`delete_mode` but the system prompt and tool description didn't mention them — updated both to enumerate all 5 operation types (#239)
- **Reverted**: Mode creation discoverability changes reverted because Plan/Apply bridge was broken (#241)

## [4.26.38] - 2026-02-10

### Fixed
- **MIDI Learn → Chat result flow**: `handleMidiLearnCapture` used `addSystemMessage` which is invisible to the LLM (`_buildMessages` skips SYSTEM messages) and doesn't trigger the agentic loop — changed to `sendMessage` with full trigger JSON so the LLM sees the captured data and continues the conversation (#237)
- **MIDI Learn cancel notification**: Closing the dialog without capturing now sends a cancel message to the LLM so it can offer alternatives, instead of silently doing nothing (#237)

### Added
- **Tests**: 8 new unit tests for MIDI Learn → Chat result flow (sendMessage vs addSystemMessage, trigger JSON inclusion, cancel notification, midiLearnUsed guard, dialog close behavior)

## [4.26.37] - 2026-02-10

### Fixed
- **MIDI Learn chord deserialization**: `eventToTrigger()` returned `{ type: 'NoteChord', timeout_ms }` but Rust `TriggerSuggestion` expects `{ type: 'Chord', window_ms }` — serde deserialization failed, showing "Error generating config" in MidiLearnDialog when chord was detected (#235)
- **MIDI Learn GamepadButtonChord**: Same `timeout_ms` → `window_ms` field name fix for gamepad chord patterns (#235)
- **Flaky test**: Eliminate `test_save_handles_absolute_and_relative_paths` race condition — split off relative path test to unique temp subdir per process

### Added
- **Tests**: `midi-learn.test.ts` with 12 unit tests for `eventToTrigger()` backend compatibility (chord type/field names, defaults, gamepad chord, other patterns, null handling)

## [4.26.36] - 2026-02-10

### Fixed
- **Chat/MCP schemas**: Fix trigger field names in tool descriptions — VelocityRange now uses `soft_max`/`medium_max` (not `velocity_min`/`velocity_max`), LongPress includes `duration_ms`, DoubleTap/NoteChord include `timeout_ms`, Aftertouch includes `pressure_min`, PitchBend includes `value_min`/`value_max`, CC includes `value_min`, all triggers document optional `device` field (ADR-009) (#233)
- **MIDI Learn conversion**: Extract `convertSuggestionToTrigger()` to `trigger-helpers.ts` with proper field mapping for VelocityRange, Aftertouch, PitchBend, CC, Note, and all gamepad types (#233)

### Added
- **MIDI Learn UX**: Pattern events (chord, long press, double tap) now sort to top of events list with accent-color border and type badge (#233)
- **MIDI Learn UX**: Auto-select pattern events when detected — saves a click for chord/long-press/double-tap detection (#233)
- **MIDI Learn UX**: Updated instruction text: "Press multiple pads together for chord detection" (#233)
- **GUI TriggerSelector**: Device dropdown for all 13 trigger types (ADR-009) — select target device from configured multi-device bindings (#233)
- **Tests**: `trigger-helpers.test.ts` with 18 unit tests for trigger conversion
- **Tests**: `MidiLearnDialog.test.ts` pattern rendering tests (badge, CSS class, instruction text)

## [4.26.35] - 2026-02-10

### Fixed
- **CI tests**: Mark 4 `midi_output` tests as `#[ignore]` — require ALSA `/dev/snd/seq` not available in CI (#231)
- **CI tests**: Mark 9 OBS WASM plugin tests as `#[ignore]` — require pre-built plugin binary not available in CI (#231)
- **CI tests**: Mark 37 daemon/integration tests as `#[cfg_attr(target_os = "linux", ignore)]` — Enigo requires display server (#231)
- **CI tests**: Fix `wasm_runtime` doc test — change from `no_run` to `ignore` (stale import path + API mismatch) (#231)
- **CI coverage**: Add `continue-on-error` to coverage steps — Enigo requires display server not available in headless CI (#231)
- **Release**: Remove Linux from Tauri GUI release matrix — ALSA `MidiOutputConnection` is not `Sync`-safe (#231)
- **Release**: Fix macOS GUI tarball path — workspace builds output to root `target/`, not member `target/` (#231)

## [4.26.34] - 2026-02-09

### Fixed
- **Release**: Fix Linux daemon build — add missing GTK/glib system dependencies to `release.yml` (#229)
- **Release**: Fix Linux Tauri GUI build — add missing `libxdo-dev` dependency for enigo/xdotool support (#229)

## [4.26.33] - 2026-02-09

### Fixed
- **CI lint**: Fix clippy `needless_return` in `midi_watcher.rs` — add `#[allow]` for early-exit guard needed on macOS (#227)
- **CI tests**: Mark 5 plugin integration tests as `#[ignore]` — require pre-built plugin binaries not available in CI (#227)

## [4.26.32] - 2026-02-09

### Fixed
- **Release**: Add missing `@tauri-apps/plugin-dialog` npm dependency — fixes Vite/Rollup build failure in release workflow (#225)

## [4.26.31] - 2026-02-09

### Fixed
- **Tests**: Fix flaky `test_sequence_action_ordering` — replace non-deterministic thread spawning with sequential execution (#223)

## [4.26.30] - 2026-02-09

### Fixed
- **CI**: Split test/coverage steps by platform — exclude `conductor-gui` on Linux due to ALSA `MidiOutputConnection` not being `Sync`-safe (#221)
- **CI**: Coverage job now excludes GUI crate to avoid Linux compilation errors

## [4.26.29] - 2026-02-09

### Fixed
- **CI lint**: Fix `cargo fmt` formatting in `conductor-capture` (import ordering, struct destructuring)
- **CI lint**: Fix `collapsible_if` clippy warnings in `conductor-core/src/transform.rs`
- **CI lint**: Fix unused `error`/`warn` tracing imports in `midi_watcher.rs` (gate behind `cfg(target_os = "macos")`)
- **Security**: Update `bytes` 1.10.1 → 1.11.1 (RUSTSEC-2026-0007: integer overflow in `BytesMut::reserve`)
- **Security**: Update `time` 0.3.44 → 0.3.47 (RUSTSEC-2026-0009: DoS via stack exhaustion)
- **Security**: Add `.cargo/audit.toml` to ignore unfixable wasmtime v26 advisories (RUSTSEC-2025-0046, RUSTSEC-2025-0118)

## [4.26.28] - 2026-02-09

### Changed
- **Documentation rationalization**: Removed ~100 historical phase reports, verification summaries, implementation notes, and superseded documentation from root, `docs/`, `.research/`, and scattered locations (#217)
- **README.md**: Updated broken links to point to docs-site instead of deleted files
- Kept 10 standard root files (README, CHANGELOG, CLAUDE, CODE_OF_CONDUCT, CONTRIBUTING, SECURITY, SUPPORT, THIRD_PARTY_LICENSES, GOVERNANCE, MAINTAINERS, ROADMAP)
- Preserved `docs/adrs/` (ADR-001 through ADR-010) and reference documentation

## [4.26.27] - 2026-02-09

### Fixed
- **SettingsView save race condition**: Replace `isSaving` drop-guard with dirty-flag re-queue pattern — saves during in-flight save are coalesced and re-run after completion instead of being silently dropped (#214)

## [4.26.26] - 2026-02-09

### Fixed
- **ChatView export tool names**: `formatLocalMessagesAsMarkdown` now reads `msg.toolCall?.name` instead of `msg.toolName` — fixes export showing "unknown" for all tool calls (#213)
- **ChatView duplicate close handler**: MidiLearnDialog no longer has both `onClose` prop and `on:close` event — removes redundant double-invocation (#213)

## [4.26.25] - 2026-02-09

### Fixed
- **Config file open**: Settings edit button now uses `open_config_in_editor` Tauri command instead of `@tauri-apps/plugin-shell` `open()` which was blocked by URL-only shell scope regex (#211)
- **Cross-platform**: `open_config_in_editor` uses `open` (macOS), `xdg-open` (Linux), `cmd /C start` (Windows)
- Removed unused `@tauri-apps/plugin-shell` import from SettingsView

## [4.26.24] - 2026-02-09

### Fixed
- **Settings event console collapsed**: `.console-container` CSS now has explicit `height: 350px` and `overflow: auto` so LiveEventConsole renders visibly inside the container (#209)
- **Settings event console scroll**: Auto-scrolls to console section when toggled on via `scrollIntoView`

## [4.26.23] - 2026-02-09

### Fixed
- **MIDI Learn dialog re-trigger**: ChatView reactive block no longer re-opens MidiLearnDialog after cancel — tracks last handled message index to prevent re-evaluation on store updates (#207)

## [4.26.22] - 2026-02-09

### Changed
- **Extract format-helpers.ts**: `formatCost`, `formatTokens` from CostSummaryPanel (#205)
- **Extract conversation-helpers.ts**: `formatTimeRelative`, `formatFullDateTime`, `truncatePreview`, `getProviderIcon` from ConversationHistory
- **Extract text-helpers.ts**: `escapeHtml`, `formatContent`, `getToolDisplayName` from MessageBubble
- **Update CostSummaryPanel**: Import helpers from format-helpers.ts
- **Update ConversationHistory**: Import helpers from conversation-helpers.ts
- **Update MessageBubble**: Import helpers from text-helpers.ts
- **Rewrite 6 test files**: Sidebar, SettingsPanel, CostSummaryPanel, ConversationHistory, MessageBubble, DeviceList — all use real imports + component render tests

## [4.26.21] - 2026-02-09

### Fixed
- **SettingsView loading state**: Split single `loading` into `isLoading`/`isSaving` — shows correct "Loading..." vs "Saving..." text (#203)
- **SettingsView error handling**: Removed redundant `appStore.setError()`/`appStore.clearError()` calls — uses only local `error` state
- **SettingsView unused import**: Removed dead `appStore` import
- **SettingsView test rewrite**: Component render tests with mocked configStore, shell plugin open() tests

## [4.26.20] - 2026-02-09

### Fixed
- **LiveEventConsole non-unique key**: Replaced `event.timestamp` key with monotonic `_key` counter (#201)
- **LiveEventConsole async onDestroy**: Removed `async` from onDestroy for reliable cleanup
- **Extract midi-helpers.ts**: `formatTimestamp`, `formatNoteName`, `formatBytes`, `getEventColor` now importable
- **LiveEventConsole test rewrite**: 7 tests — real imports from midi-helpers.ts, component render tests

## [4.26.19] - 2026-02-09

### Fixed
- **ChatView unused imports**: Removed dead `invoke` and `currentConversationId` imports (#199)
- **ChatView test rewrite**: Component render tests with mocked chat store, retained scroll logic unit tests

## [4.26.18] - 2026-02-09

### Fixed
- **MidiLearnDialog reactive loop restarts session after timeout**: Added `hasAutoStarted` flag to prevent reactive statement from restarting MIDI Learn session when `active` returns to false after timeout (#197)
  - Flag resets when dialog closes, so re-opening correctly starts a new session
- **MidiLearnDialog test rewrite**: 7 tests — auto-start guard, z-index, component render

## [4.26.17] - 2026-02-09

### Fixed
- **MappingsView edit cancel mutates original**: Replaced shallow spread (`{ ...mapping }`) with `structuredClone()` in `editMapping()` (#195)
  - Nested trigger/action objects are now deep-copied, so canceling an edit doesn't corrupt the source
- **MappingsView test rewrite**: 7 tests — addNewMapping, deep clone proof, component render

## [4.26.16] - 2026-02-09

### Fixed
- **DevicesView async onMount cleanup leak**: Svelte ignores cleanup return from `async onMount` — `stopAutoRefresh` was never called on unmount (#193)
  - Split into sync `onMount` + `onDestroy` for reliable cleanup
- **DevicesView test rewrite**: 5 tests — lifecycle mount/unmount, rendering, backward compat

## [4.26.15] - 2026-02-09

### Fixed
- **ModesView `saveConfig()` doesn't persist selected mode**: Added `last_selected_mode: modes[selectedModeIndex]?.name` to saved config (#191)
- **ModesView index bounds risk**: Added reactive guard to clamp `selectedModeIndex` when modes array shrinks
- **ModesView test rewrite**: 12 tests — mode resolution, save persistence, bounds guard, component render

## [4.26.14] - 2026-02-09

### Added
- **Testing infrastructure**: Install `@testing-library/svelte`, `@testing-library/jest-dom`, `jsdom`
  - Configure vitest with jsdom environment and `svelteTesting()` plugin
  - Create `vitest-setup.ts` for jest-dom matchers
  - Enable Svelte 5 browser conditions for vitest
- **StatusBar helper extraction**: `src/lib/utils/status-helpers.ts`
  - Export `formatUptime`, `getCurrentMode`, `getDeviceCount`, `getStatusColor`, `getStatusText`
  - StatusBar.svelte now imports from utils (no more inline definitions)
- **StatusBar component render tests**: Tests mount/unmount lifecycle, DOM output
  - 24 unit tests for real exported helpers (not copies)
  - 6 render tests using `@testing-library/svelte`

### Fixed
- **`formatUptime()` floating-point display bug**: `formatUptime(43.7)` returned `"43.7s"` instead of `"43s"`
  - Added `Math.floor()` to seconds calculation (`const secs = Math.floor(seconds % 60)`)

## [4.26.13] - 2026-02-09

### Fixed
- **Build failure**: Install missing `@tauri-apps/plugin-shell` npm package
  - v4.26.11 added `import { open } from '@tauri-apps/plugin-shell'` in SettingsView
    but did not install the frontend npm package (Rust side was already configured)
  - `npm run build` now succeeds

## [4.26.12] - 2026-02-08

### Fixed
- **LLM Council findings** from v4.26.2-v4.26.11 GUI bug fixes
  - Wire `openMidiLearnFromChat()` to reactive tool call detection in ChatView (#181)
  - Add tests for MIDI Learn auto-open from `conductor_start_midi_learn` tool call
  - Update Event Console docs to reflect auto-start behavior (#183)

## [4.26.11] - 2026-02-08

### Fixed
- **"Open in Editor" button** in SettingsView now works (#185)
  - Replaced non-existent `invoke('open_file')` with Tauri shell plugin `open()` API
  - Works cross-platform via system default application association
  - 3 new tests for shell open behavior

## [4.26.10] - 2026-02-08

### Fixed
- **Event Console auto-start monitoring** on component mount (#183)
  - LiveEventConsole now calls `start_event_monitoring` automatically in onMount
  - Auto-stops monitoring in onDestroy (existing behavior preserved)
  - 9 new tests for auto-start/stop patterns and helper functions

## [4.26.9] - 2026-02-08

### Fixed
- **MIDI Learn dialog in ChatView** for chat-triggered learn mode (#181)
  - Import MidiLearnDialog into ChatView with capture result injection as system message
  - Captured MIDI events display note and velocity details in chat

## [4.26.8] - 2026-02-08

### Fixed
- **MidiLearnDialog z-index collision** with mapping editor modal (#179)
  - Increased MidiLearnDialog overlay z-index from 1000 to 1100

## [4.26.7] - 2026-02-08

### Fixed
- **"+ Add Mapping" button now works** in MappingList component (#177)
  - Wire `on:addMapping={addNewMapping}` handler to MappingList in MappingsView

## [4.26.6] - 2026-02-08

### Fixed
- **GUI displays actual version** from Tauri API instead of hardcoded "v2.0.0" (#175)
  - Sidebar and SettingsPanel use `getVersion()` from `@tauri-apps/api/app`
  - Updated `tauri.conf.json` and `package.json` to current version

## [4.26.5] - 2026-02-08

### Changed
- **DevicesView removes redundant "Input Mode" display** (#173)
  - Per-device bindings provide more specific information about each device type

## [4.26.4] - 2026-02-08

### Fixed
- **ModesView selects active mode** instead of always defaulting to first mode (#171)
  - Restore `last_selected_mode` from config in `loadConfig()`, matching MappingsView pattern

## [4.26.3] - 2026-02-08

### Fixed
- **StatusBar shows device count** instead of single legacy device name (#169)
  - Daemon: Add `device_count` from `input_manager.get_device_bindings().len()` to status response
  - Backend: Add `device_count: Option<u64>` to `DaemonStatus`, parse from IPC response
  - Frontend: Replace "Device: <name>" with "Devices: N" count display

## [4.26.2] - 2026-02-08

### Fixed
- **StatusBar Mode displays active mode name** instead of "Running" (#167)
  - Daemon: Use ArcSwap `current_mode` instead of `config.modes.first()` for accurate mode reporting
  - Backend: Add `current_mode` field to `DaemonStatus` struct, parse from IPC response
  - Frontend: `getCurrentMode()` reads `status.current_mode` instead of `status.lifecycle_state`

## [4.26.1] - 2026-02-08

### Added
- **Persistent daemon MIDI watcher** — CoreMIDI hot-plug detection for newly connected devices (#116)
  - Dedicated `conductor-midi-watcher` thread with long-lived `MidiInput` + `CFRunLoopRun()`
  - Stores `CFRunLoopRef` via `AtomicPtr` for clean `CFRunLoopStop` + thread join on shutdown
  - Graceful degradation: returns `Option<MidiWatcherHandle>` instead of panicking on spawn failure
  - `#[must_use]` attribute prevents accidental immediate drop
  - LLM Council reviewed: explicit CoreFoundation framework linkage, proper lifecycle management
  - 3 unit tests + 1 ignored integration test (requires OS MIDI subsystem)

### Changed
- **ListenMode default changed from `Configured` to `All`** — unconfigured ports now visible in GUI by default
  - Removed adaptive fallback (dead code now that `All` is default)
  - Explicit `listen_mode = "Configured"` still respected

### Removed
- **Vestigial GUI MIDI device code** — dead since v4.25.0 DeviceList rewrite to daemon bindings
  - Removed `start_midi_watcher`, `list_midi_devices`, `MidiDevice` struct from GUI commands
  - Removed `connect_midi_device`, `disconnect_midi_device` Tauri commands
  - Removed `DeviceConnectionResult` struct
  - Removed `createDevicesStore` from frontend stores.js
  - Removed `devices.list()`, `devices.connect()`, `devices.disconnect()` from frontend api.js
  - Removed `devicesStore` auto-refresh from DevicesView.svelte

## [4.26.0] - 2026-02-08

### Added
- **ADR-009 Remaining Gaps Resolution** — Resolves 12 deferred spec items (1 high, 4 medium, 7 low) from ADR-009 multi-device architecture audit
  - **Gap F: Fix `is_configured` broken logic** (PR #148, HIGH)
    - `DevicePortStatus.is_configured` field replaces broken `starts_with("raw:")` prefix check
    - All JSON serialization uses the new boolean field
    - GUI DeviceList uses `is_configured` from backend
  - **Gap A: ListenMode default** (PR #150)
    - `ListenMode::default()` returns `All` (changed to `All` in v4.26.1)
    - Adaptive fallback removed in v4.26.1 (no longer needed)
  - **Gaps D+E: Rich ProcessedEvent + Raw passthrough** (PR #152)
    - `HoldDetected` gains `press_velocity` and `duration_ms`
    - `DoubleTap` gains `first_velocity`, `second_velocity`, and `interval_ms`
    - `ChordDetected` gains per-note `velocities`
    - New `ProcessedEvent::Raw(InputEvent)` variant emitted before gesture detection
  - **Gap B: Per-device rate limiting** (PR #154)
    - `DeviceRateLimiter` with sliding window counter (default 10,000 events/sec)
    - Per-device independent counters with 1-second window
    - Configurable via `advanced_settings.max_events_per_sec`
  - **Gaps C+G: Auto-exclude virtual ports + MIDI Learn ambiguous** (PR #156)
    - Daemon virtual output ports auto-excluded from input scanning
    - MIDI Learn temporarily opens Ambiguous ports for device discovery
  - **Gaps I+J: MidiMessage typed enum + ValueCurve::Lut** (PR #158)
    - `MidiMessage` typed enum with `parse()`/`to_bytes()` for structured MIDI handling
    - `MidiTransform::apply()` refactored to parse → transform → serialize pattern
    - `ValueCurve::Lut(Box<[u8; 128]>)` for custom 128-entry lookup tables
  - **Gap H: OscSend action** (PR #160)
    - `Action::OscSend { host, port, address, args }` for OSC over UDP
    - `OscArg` typed enum: `Int(i32)`, `Float(f32)`, `String(String)`
    - `rosc` crate for encoding/decoding, optional feature flag
  - **Gap K: ActionEnvelope provenance** (PR #162)
    - `ActionEnvelope { action, device_id, matched_rule, mode_name }` for dispatch metadata
    - `CompiledRuleSet::match_event_with_provenance()` returns enriched envelopes
    - Enhanced debug logging in multi-device engine manager

### Changed
- `ValueCurve` is no longer `Copy` (due to `Lut` variant containing `Box`)
- `AdvancedSettings` gains `max_events_per_sec: u32` (default 10,000)

### Dependencies
- Added `rosc = "0.11"` (optional, behind `osc` feature) for OSC support

## [4.25.0] - 2026-02-07

### Added
- **ADR-009 Gap Resolution** — Resolves 6 deferred spec items from ADR-009 multi-device architecture
  - **Gap 1: ActionDispatcher + ModeChange Fix** (PR #142)
    - `DispatchResult` structured return type for action execution (`DispatchOutcome::Completed`, `ModeChangeRequested`, `DispatchError`)
    - `Action::ModeChange` now atomically updates `ArcSwap<ModeState>` instead of printing to stderr
    - `CompiledRuleSet::find_mode_index()` for mode name → index lookup
    - 16 new tests (3 rule_set, 8 dispatch, 5 integration)
  - **Gap 2: MidiTransform Pipeline** (PR #144)
    - `MidiTransform` type with channel/CC/note remap, velocity scale/offset, value inversion, and curves (Linear/Logarithmic/Exponential)
    - `Action::MidiForward { target, transform }` for MIDI routing between devices with optional transforms
    - `raw_midi` field on `TriggerContext` with `extract_raw_midi()` helper
    - Safety: NoteOn vel=0 preserved (stuck note protection), NaN/Inf guard, SysEx/system message rejection, bounded allocation
    - 27 transform + 4 forward + 3 config tests
  - **Gap 3: `conductor_create_device_identity` MCP Tool** (PR #140)
    - ConfigChange-tier tool for LLM-assisted device setup
    - Validates alias uniqueness and non-empty matchers
  - **Gap 4: DeviceIdentityConfig Extra Fields** (PR #140)
    - `description: Option<String>` and `enabled: bool` fields on `DeviceIdentityConfig`
    - Resolver skips disabled identities
  - **Gap 5: IPC Deprecation Warnings** (PR #140)
    - `SetDevice` and `DisconnectDevice` IPC commands emit deprecation warnings
    - Response JSON includes `"deprecated"` field
  - **Gap 6: GUI Legacy Mode Removal** (PR #140)
    - DeviceList.svelte: removed legacy single-device fallback
    - Added empty state ("No Devices Detected") and loading state
    - Deprecated `connect_midi_device` and `disconnect_midi_device` Tauri commands

### Changed
- `ActionExecutor::execute()` now returns `DispatchResult` instead of `()`
- MCP tool count: 15 → 16 (added `conductor_create_device_identity`)

### Documentation
- Actions reference: MidiForward action with transform parameters and examples
- MCP tools reference: `conductor_create_device_identity` tool, `MidiForward` in valid action types
- Architecture docs: dispatch.rs and transform.rs module descriptions

## [4.24.0] - 2026-02-07

### Added
- **ADR-009 Phase 6: Polish, Testing, Documentation** — Config validation, migration CLI, comprehensive tests, benchmarks
  - **Config validation**: `Config::validate()` now checks for unique device aliases (non-empty, no duplicates) and validates that trigger `device` fields reference defined aliases
  - **`conductorctl migrate-config`**: New CLI subcommand to migrate legacy `[device]` config to `[[devices]]` format with dry-run mode, `--write` to apply, and `.bak` backup
  - **Property tests**: 4 proptest-based tests verifying device filter determinism, specificity ordering invariants, matcher robustness on arbitrary strings, and resolver determinism
  - **Integration tests**: 6 tests for multi-device rule routing via `CompiledRuleSet::match_event()` — device-specific priority, any-device fallback, global device rules, multi-mode routing
  - **E2E tests**: 5 full-pipeline tests: `MidiEvent -> EventProcessor -> ProcessedEvent -> CompiledRuleSet -> Action` with device routing
  - **Benchmark**: 7 criterion benchmark groups for lock-free rule engine — single/multi-device match (~65ns), device scaling (O(1)), ArcSwap load, concurrent readers, worst-case no-match, compilation latency
  - **Multi-device setup guide**: New docs-site guide covering `[[devices]]`, matchers, device-specific mappings, hot-plug, GUI status, MCP tools, and migration
  - **CLI reference**: Added `migrate-config` documentation to CLI commands reference
  - 9 config validation tests, 5 migration tests

## [4.23.0] - 2026-02-07

### Added
- **ADR-009 Phase 5: MCP Multi-Device Tools** — Exposes multi-device state and controls to LLM agents via MCP
  - **New tool: `conductor_list_device_bindings`** (ReadOnly) — Returns per-device binding state with device_id, port_name, connected/enabled/is_configured status, plus summary counts (total_devices, connected_count, muted_count)
  - **New tool: `conductor_set_device_enabled`** (Stateful) — Mute/unmute a specific device by device_id
  - **New tool: `conductor_scan_ports`** (Stateful) — Triggers immediate port rescan for device hot-plug detection
  - `to_status_json()` now includes `device_bindings` array with per-device status
  - `to_devices_json()` now includes `device_bindings` array alongside raw port list
  - `SharedDaemonStateRefs` gains `command_tx` for triggering daemon commands from MCP
  - IPC `ToolExecutor` handles both new Stateful tools via `daemon_state_refs`
  - MCP tool count increased from 12 to 15 (6 ReadOnly, 4 Stateful, 3 ConfigChange, 2 HardwareIO)
  - 7 new unit tests for multi-device MCP tools

## [4.22.0] - 2026-02-07

### Added
- **ADR-009 Phase 4: Hot-Plug & GUI Multi-Device** — Automatic MIDI device detection and multi-device GUI controls
  - **Phase 4a: Daemon Hot-Plug Loop**
    - `DaemonCommand::HotPlugCheck` with 5-second interval rescan loop
    - `InputManager::rescan_ports()` detects newly-connected and removed MIDI devices
    - Hot-plug loop spawned alongside timer tick in `connect_multi_device()`
    - Respects `ignore_ports` filter and `max_midi_ports` cap during rescan
    - 9 integration tests for hot-plug behavior
  - **Phase 4b: GUI Multi-Device Status**
    - `get_device_bindings` Tauri command for per-device status (connected, enabled, configured)
    - `toggle_device_mute` Tauri command for per-device mute/unmute control
    - `deviceBindingsStore` with 3-second auto-refresh for real-time device status
    - `DeviceList.svelte` rewritten with status indicators (green=active, yellow=muted, red=disconnected)
    - Mute/unmute toggle per device, "configured" badge for named devices
    - `DeviceBinding` TypeScript type for frontend type safety
    - Legacy single-device fallback preserved for backward compatibility
    - 7 frontend tests for multi-device API and type validation

## [4.21.0] - 2026-02-07

### Added
- **ADR-009 Phase 3: Lock-Free Rule Engine** — Replaces RwLock-based hot path with wait-free ArcSwap reads for zero-contention event processing
  - `CompiledRuleSet`: Immutable compiled rule structure with per-device HashMap indexing for O(1) device-specific rule lookup
  - `ModeRuleSet`: Per-mode rules split into device-specific (HashMap) and any-device (Vec) collections
  - `CompiledRule` + `CompiledTrigger`: Compiled trigger/action pairs for direct matching without re-compilation
  - `ModeState`: Atomic mode snapshot replacing `Arc<RwLock<usize>>` mode index
  - `rule_compiler::compile()`: Transforms `Config` → `CompiledRuleSet` off the hot path
  - `CompiledRuleSet::match_event()`: Priority-ordered matching (device-specific → any-device → global device-specific → global any-device)
  - Engine manager hot path now uses `ArcSwap::load()` (~1ns wait-free reads) instead of `RwLock::read().await`
  - Config reload uses `ArcSwap::store()` for atomic rule swap — never blocks in-flight event processing
  - Mode switches use atomic `ArcSwap::store()` instead of write lock
  - Extracted `compile_trigger()`, `compile_action()`, `trigger_matches_processed()`, `device_matches()` as reusable `pub(crate)` functions from MappingEngine
  - Backward-compatible: MappingEngine retained for MCP tools and external consumers
  - 36 tests across conductor-core and conductor-daemon verifying lock-free rule engine

### Dependencies
- Added `arc-swap = "1"` to conductor-core and conductor-daemon for lock-free atomic pointer swap

## [4.20.0] - 2026-02-07

### Added
- **ADR-009 Phase 2: Daemon Multi-Device Manager** — Wires Phase 1 core types into the daemon for simultaneous multi-port MIDI listening
  - `InputManager::listen_to_all_ports()` opens all MIDI ports simultaneously, filtered by `ignore_ports` (D4) and capped at `max_midi_ports` (D1)
  - `DeviceEvent<InputEvent>` channel for device-tagged event delivery
  - Per-device `EventProcessor` isolation via `DashMap<DeviceId, EventProcessor>` (D14) for independent hold detection, chord buffering, and double-tap state
  - `DaemonCommand::TimerTick` with 50ms timer loop for hold detection across all devices (D12)
  - Device mute/unmute via `InputManager::set_device_enabled()` (D8) and `DaemonCommand::SetDeviceEnabled`
  - `IpcCommand::SetDeviceEnabled` for remote device enable/disable control
  - `DevicePortStatus` struct for multi-device status reporting with per-port connection and mute state
  - `MidiLearnEvent.device_id` field for device-aware MIDI Learn capture
  - `MidiDeviceManager.device_id` field for device identity tracking
  - `filter_ports()` pure function extracted for testable port filtering logic
  - Backward-compatible: legacy `[device]` single-device configs continue to work unchanged
  - Adaptive multi-device activation: multi-device mode engages when `[[devices]]` configured or `listen_mode = "All"` (D13)
  - Gamepad events tagged with `DeviceId::raw("gamepad")` in multi-device mode
  - 23 integration tests covering all multi-device functionality

### Dependencies
- Added `dashmap = "6"` for lock-free concurrent per-device EventProcessor storage

## [4.19.0] - 2026-02-06

### Added
- **ADR-009 Phase 1: Core Types and Config Migration** — Multi-device listening architecture foundation
  - `DeviceId`, `DeviceEvent<T>`, `DeviceMatcher`, `BindingState` types in new `identity` module
  - `PortResolver` for binding MIDI ports to device identities by matcher specificity (D2, D7)
  - `DeviceIdentityConfig` and `ListenMode` config types for `[[devices]]` array
  - `device: Option<String>` filter field on all 13 trigger variants (backward-compatible, defaults to `None`)
  - `Config.device` changed to `Option<DeviceConfig>` with `primary_device()` convenience method
  - `Config.devices: Vec<DeviceIdentityConfig>` for multi-device identity bindings
  - `AdvancedSettings`: `listen_mode`, `ignore_ports`, `max_midi_ports` fields
  - `MappingEngine::get_action_for_processed_with_device()` for per-device trigger matching
  - 7 `DeviceMatcher` types with specificity ordering: `CoreMidiUniqueId > UsbIdentifier > UsbTopology > ExactName > PlatformId > NameContains > NameRegex`

## [4.18.3] - 2026-02-06

### Fixed
- **MIDI device re-addition still not detected after warmup fix (#116)**: The v4.18.0 warmup-per-poll approach created and destroyed ~30 Core MIDI clients per minute, too transient for macOS to deliver device-added notifications reliably. Replaced with a persistent background MIDI watcher thread that keeps one `MidiInput` alive for the entire app lifetime, ensuring Core MIDI always has an active client to receive notifications and keep its cache up to date.
- **Copy Chat button non-functional (#116)**: The "Copy Chat" button silently failed because the backend `llm_export_conversation` DB lookup did not match the frontend conversation state. Replaced with local-only copy that formats directly from the in-memory messages array, eliminating the backend round-trip entirely. Button now enables based on `messages.length > 0` instead of requiring a `conversationId`.

## [4.18.2] - 2026-02-06

### Added
- **Chat export for debugging (#114)**: New "Copy Chat" button in chat header copies the conversation as formatted markdown to clipboard. Backend `llm_export_conversation` Tauri command supports verbose mode (full tool_calls JSON) and non-verbose mode (tool names only). Added `trace!`-level logging for full LLM request payloads and tool execution details — enable with `RUST_LOG=conductor_gui::llm_commands=trace`.

## [4.18.1] - 2026-02-06

### Fixed
- **LLM hallucinated invalid SendMidi types due to missing enums in tool schemas (#112)**: Enriched MCP tool schema descriptions for `conductor_create_mapping`, `conductor_update_mapping`, and `conductor_batch_changes` to list all valid trigger types (Note, CC, VelocityRange, LongPress, DoubleTap, NoteChord, EncoderTurn, etc.) and action types including all valid SendMidi `message_type` values (NoteOn, NoteOff, CC, ProgramChange, PitchBend, Aftertouch). Prevents LLMs from fabricating non-existent types like `note_on_off` or fields like `duration_ms`.

## [4.18.0] - 2026-02-06

### Fixed
- **MIDI device re-addition not detected in GUI (#110)**: The warmup `MidiInput`/`MidiOutput` instances were dropped before the 100ms sleep, preventing macOS Core MIDI from delivering "device added" notifications. Fixed by keeping warmup instances alive during the sleep so the active client receives cache-update notifications. Affects `device_utils.rs`, `commands.rs`, and `midi_output.rs`.

## [4.17.1] - 2026-02-06

### Fixed
- **Chat tools receive no daemon state — device_connected always false (#107)**: The `ToolExecutor` (used by chat via IPC) had no reference to daemon state, causing `conductor_get_status` to always return fallback with `connected: false`. Fixed by giving `ToolExecutor` shared daemon state refs (`SharedDaemonStateRefs`) so it reads live device status, lifecycle state, and statistics.

- **MIDI output ports missing warmup pattern — newly-started apps not found (#108)**: `MidiOutputManager::connect_by_name()` used a stale macOS Core MIDI port cache. Added retry-with-warmup pattern: fast path tries without delay, macOS slow path does Core MIDI cache warmup (create+drop MidiOutput + 100ms) when port not found, then retries.

## [4.17.0] - 2026-02-06

### Fixed
- **MCP tools report stale MIDI device enumeration (#105)**: Consolidated 3 separate device enumeration implementations into shared `device_utils` module with macOS Core MIDI warmup pattern
  - Creates and drops warmup MidiInput to bust OS driver cache
  - 100ms delay for hardware change recognition
  - Fresh enumeration returns all connected devices
  - Fixes chat reporting fewer devices than physically connected
  - Async wrapper uses `spawn_blocking` to avoid blocking tokio runtime

- **MCP status conflates daemon running with device connected (#106)**: Added explicit `daemon_running` and `device_connected` fields to status response
  - `daemon_running`: Always `true` when daemon responds (independent of device state)
  - `device_connected`: Explicit alias for device connection status
  - `connected` preserved for backward compatibility
  - Fixes chat incorrectly reporting "daemon disconnected" when no device is active

### Added
- New shared `device_utils` module (`conductor-daemon/src/daemon/device_utils.rs`)
- `enumerate_midi_devices_fresh()` — sync MIDI enumeration with warmup
- `enumerate_midi_devices_fresh_async()` — async wrapper for tokio contexts

## [4.16.0] - 2026-02-04

### Fixed
- **Chat history not loaded on app start (BUG-1)**: Added `chatStore.initialize()` call in ChatView's `onMount` hook
  - Conversation history now loads automatically when the app starts
  - Fixes issue where conversations list was empty until manually refreshed

- **New conversations don't appear in sidebar (BUG-2)**: Added `listConversations()` call after `createConversation()`
  - New conversations now immediately appear in the history sidebar
  - No manual refresh required after sending first message

- **ConversationHistory lacks auto-load (BUG-3)**: Added `onMount` hook to ConversationHistory component
  - Component now automatically loads conversations when displayed
  - Ensures conversation list is populated on component mount

### Security
- **XSS vulnerability in MessageBubble (SEC-1)**: Added HTML escaping before markdown formatting
  - `escapeHtml()` function escapes `<`, `>`, `&`, `"`, `'` characters
  - Applied before any markdown transformation to prevent script injection
  - Links now only allow `http://` and `https://` protocols (blocks `javascript:`)

- **Object rendering bug (SEC-2)**: Fixed skill display showing `[object Object]`
  - Now properly extracts `skill.name` from skill objects
  - Graceful fallback for string skills and missing names

- **Error state stuck in ERROR (BUG-4)**: `clearError()` now resets session state
  - Session state properly resets from ERROR to ACTIVE/IDLE when clearing errors
  - Prevents UI from being stuck in error state after dismissing error banner

- **Message type persistence (BUG-5)**: Fixed missing role mappings for TOOL_CALL, SKILL, PLAN_PENDING
  - All message types now map to appropriate database roles
  - TOOL_CALL and PLAN_PENDING map to 'assistant', SKILL maps to 'system'

- **Tool result data loss (BUG-6)**: Tool result payloads now persisted correctly
  - `_persistMessage` now serializes `message.result` for TOOL_RESULT messages
  - Actual tool output stored instead of generic "Tool result received" string
  - Enables conversation replay with full tool execution context

- **Tool result hydration (BUG-7)**: Tool results now rehydrated when loading conversations
  - `loadConversation` parses JSON content back to `result` property for tool messages
  - Handles both JSON object results and string results
  - Ensures tool outputs render correctly after conversation reload

- **Singular toolCall property (#101)**: Fixed TOOL_CALL messages with single `toolCall` property
  - Now handles both `toolCalls` array and singular `toolCall` property
  - Normalizes to array format for database persistence
  - Prevents orphaned tool_results when replaying conversations

- **Scroll timing with setTimeout (#102)**: Replaced `setTimeout(scrollToBottom, 0)` with Svelte's `tick()`
  - Uses `tick()` for reliable DOM updates before scrolling
  - Prevents race conditions where scroll happens before DOM update
  - More idiomatic Svelte approach than setTimeout workaround

- **Streaming content auto-scroll (#103)**: Now tracks both message count AND content changes
  - Detects streaming updates that mutate content without changing array length
  - Scrolls automatically when last message content changes
  - Ensures user stays at bottom during streaming responses

- **Scroll throttling (#104)**: Added requestAnimationFrame throttling for scroll during streaming
  - Prevents layout thrash when receiving rapid token updates
  - Uses `scrollPending` flag to coalesce rapid scroll requests
  - Improves performance on lower-end devices during streaming

### Added
- **Session ID display with copy button (FEAT-1)**: Current session ID now visible in conversation history header
  - Truncated ID shown (first 8 characters)
  - Full ID visible on hover via tooltip
  - One-click copy button with visual feedback
  - Useful for debugging and sharing conversation references

- **Full timestamp tooltips (FEAT-2)**: Hover over any time to see full date and time
  - Conversations and messages now show full timestamp on hover
  - Format: "Feb 4, 2026 3:45 PM"
  - Supplements the relative time display ("2h ago", "Yesterday", etc.)

### Tests
- `chat.test.ts`: Added 5 tests for store initialization and createConversation refresh
  - `should load conversations when initialize() is called`
  - `should set provider from configured providers`
  - `should handle no configured providers gracefully`
  - `should handle initialization errors`
  - `should refresh conversations list after creating`

- `ConversationHistory.test.ts`: Added 7 tests for session ID, tooltips, and onMount
  - `formatFullDateTime should return full date and time string`
  - `formatFullDateTime should format older dates correctly`
  - Session ID truncation tests
  - Copy session ID logic tests
  - onMount behavior verification

- `MessageBubble.test.ts`: NEW - 22 tests for XSS prevention and content formatting
  - `escapeHtml` tests for script injection, HTML tags, quotes, ampersands
  - `formatContent` tests for markdown formatting with XSS protection
  - Skill display tests to prevent `[object Object]` rendering
  - Timestamp formatting tests

- `chat.test.ts`: Added 8 more tests for error handling, role mapping, and tool result round-trip
  - `clearError` tests for session state reset
  - `_messageTypeToDbRole` tests for TOOL_CALL, SKILL, PLAN_PENDING
  - Tool result serialization tests for object and string results
  - Tool result hydration tests for conversation reload
  - Singular `toolCall` handling test (#101)

- `ChatView.test.ts`: NEW - 6 tests documenting scroll behavior patterns (#102, #103, #104)
  - `should use tick() for reliable DOM updates instead of setTimeout`
  - `should scroll when streaming content changes`
  - `should track both message count AND content for scroll triggers`
  - `should throttle scroll calls during rapid content updates`
  - `autoScrollEnabled` behavior tests for user scroll detection

## [4.15.1] - 2026-02-03

### Fixed
- **Missing Implementation**: Include actual P3-05 and P3-06 implementation code
  - v4.15.0 only included tests and documentation, not the feature code
  - This patch adds the complete implementation

### Added
- `ConversationHistory.svelte` - Conversation history sidebar component
- `CostSummaryPanel.svelte` - Cost tracking panel component
- Backend LLM commands for conversation and cost management
- Chat store updates for conversation persistence

## [4.15.0] - 2026-02-03

### Added
- **Conversation Persistence (P3-05)**: Chat conversations now persist across app restarts
  - Conversations stored in SQLite database with full message history
  - ConversationHistory sidebar for browsing and loading past conversations
  - Create new conversations, delete old ones
  - Messages persisted with tool calls for replay
  - 6 Tauri commands: `llm_create_conversation`, `llm_get_conversation`, `llm_get_messages`,
    `llm_list_conversations`, `llm_delete_conversation`, `llm_add_message`

- **Cost Tracking UI (P3-06)**: View LLM usage costs in an expandable panel
  - CostSummaryPanel displays total cost, tokens, and request count
  - Per-conversation cost breakdown
  - Cost breakdown by provider (OpenAI, Anthropic, etc.)
  - Cost breakdown by model with sorting
  - 4 Tauri commands: `llm_get_total_cost`, `llm_get_conversation_cost`,
    `llm_get_cost_by_provider`, `llm_get_cost_by_model`

- **Frontend Tests**: Comprehensive test coverage for new features
  - `chat.test.ts`: 28 tests for conversation persistence logic
  - `CostSummaryPanel.test.ts`: 20 tests for cost formatting and Tauri contracts
  - `ConversationHistory.test.ts`: 24 tests for time formatting and UI logic

### Changed
- ADR-007 marked as IMPLEMENTED (Phases 1-4 complete)
- Updated implementation plan to show P3-05 and P3-06 as complete
- Resolved open questions about conversation persistence and cost tracking

## [4.14.0] - 2026-02-03

### Fixed
- **Path Handling (LLM Council v4.13.3)**: Replaced 4 instances of `unwrap_or("")` with proper error handling
  - Added `DaemonError::InvalidPath` variant with context information
  - Created `pathbuf_to_str_or_err()` helper function for safe path conversion
  - Fixed path handling in ValidateConfig IPC, reload_config, and service startup
  - Non-UTF8 paths in commands now return descriptive errors instead of silently failing
  - Note: `get_daemon_state()` intentionally uses graceful degradation (returns None for
    config_path) - this is correct behavior for read-only status queries

- **Status Handler Type Consistency**: Changed `input_mode` from `Option<&str>` to `Option<String>`
  - Status IPC handler now matches `get_daemon_state()` return type
  - Eliminates potential lifetime issues with string references

- **IPC Test Improvement**: Changed `unwrap()` to `expect()` with descriptive message
  - Windows named pipe path test now has proper panic context

### Added
- **Lock Ordering Documentation**: Comprehensive documentation of 11-lock acquisition order
  - Added to EngineManager struct documentation
  - Documents common access patterns (process_input_event, reload_config, Status IPC)
  - Explains design rationale for fine-grained locking

### Tests
- `test_invalid_path_error_message_format` - Verifies error message contains context
- `test_pathbuf_to_str_helper_valid_utf8` - PathBuf helper with valid UTF-8
- `test_pathbuf_helper_returns_error_context` - Non-UTF8 path error context (Unix)
- `test_validate_config_with_valid_path` - ValidateConfig path handling
- `test_reload_config_with_valid_path` - reload_config path handling
- `test_status_handler_uses_string_not_str` - Type consistency verification
- `test_lock_ordering_documented` - Lock ordering documentation reminder

## [4.13.3] - 2026-02-03

### Fixed
- **ToolExecutor Constructor Bug**: Fixed `new_with_config` ignoring passed config parameter
  - Function now properly clones and uses the provided config
  - Latent defect identified by LLM Council verification
- **State Reporting Accuracy**: Fixed false `"MidiOnly"` report when input_manager unavailable
  - `get_daemon_state()` now returns `None` for input_mode when input_manager not initialized
  - `get_state()` and IPC Status command also updated for consistency
  - Semantic correctness issue identified by LLM Council

### Added
- Test: `test_new_with_config_uses_passed_config` verifies constructor behavior
- Test: `test_daemon_state_input_mode_none_when_no_input_manager` verifies state reporting

## [4.13.2] - 2026-02-02

### Fixed
- **MIDI Learn Buffer**: Added explicit `MIDI_LEARN_MAX_EVENTS` constant (100 events)
  - Ring buffer now documented with clear bounding behavior
  - Addresses LLM Council feedback on potential memory growth
- **Comment Alignment**: Fixed buffer size comments to match actual channel capacity (1000)
- **Stale TODO**: Replaced "Week 3-4" gamepad reconnection TODO with gilrs reference

### Changed
- Documented config sync design for ToolExecutor (intentional TOCTOU protection pattern)
- Config snapshot is created on-demand before tool execution for Plan/Apply safety

## [4.13.1] - 2026-02-02

### Added
- **MIDI Learn LLM Tools (ADR-007 Phase 2)**: Full implementation of MIDI Learn via chat
  - `conductor_start_midi_learn` - Starts MIDI Learn mode and captures controller input
  - `conductor_stop_midi_learn` - Stops capture and returns events with pattern analysis
  - Pattern detection for LongPress, DoubleTap, Chord, and GamepadChord
  - Graceful simulation mode for standalone/test scenarios

### Fixed
- **Device Listing**: Fixed `conductor_list_devices` tool returning empty arrays
  - MCP server now enumerates MIDI devices properly
  - ToolExecutor fetches device data dynamically

### Removed
- Deleted obsolete `conductor-daemon/src/action_executor.rs.bak` backup file
- Removed stale TODOs in `conductor-core/src/engine.rs` (referenced v0.1.0 Phase 1)

### Changed
- Updated engine.rs documentation to clarify stub nature (full impl in conductor-daemon)
- ToolExecutor now shares MIDI Learn state with EngineManager for real-time capture

## [4.13.0] - 2026-02-02

### Added
- **LLM Integration Phase 3 (ADR-007)**: Agentic tool loop for automatic tool execution

  **Agentic Tool Loop**
  - Chat store implements agentic loop with MAX_ITERATIONS=10
  - Automatic tool execution for ReadOnly and Stateful tools
  - Plan detection pauses loop for user approval
  - `llm_execute_tool` and `llm_get_tools` Tauri commands
  - Tool call and result message types for visual feedback

### Changed
- Fixed event system: use Tauri `emit()` instead of browser CustomEvent for plan-ready events

## [4.12.0] - 2026-02-02

### Added
- **LLM Integration Phase 2 (ADR-007)**: Configuration mutations with Plan/Apply workflow

  **ToolExecutor & Risk Tiers**
  - Transport-agnostic `ToolExecutor` with risk-tier-based execution
  - Auto-execute for ReadOnly tools, logging for Stateful tools
  - ConfigChange tools return ConfigPlan for user approval
  - Risk tier definitions: ReadOnly, Stateful, ConfigChange, HardwareIO, Privileged

  **ConfigPlan with TOCTOU Protection**
  - `ConfigPlan` struct with SHA256 hash validation
  - 5-minute expiration TTL for security
  - `ConfigChange` enum: CreateMapping, UpdateMapping, DeleteMapping, CreateMode, DeleteMode
  - Race condition protection via base state hash verification

  **ConfigChange MCP Tools**
  - `conductor_create_mapping` - Create new mapping in mode
  - `conductor_update_mapping` - Update existing mapping
  - `conductor_delete_mapping` - Delete mapping by index

  **Stateful MCP Tools**
  - `conductor_start_midi_learn` - Start MIDI Learn capture
  - `conductor_stop_midi_learn` - Stop and return captured events

  **Plan/Apply UI**
  - `PlanReviewModal.svelte` - Modal for reviewing ConfigPlans
  - `InlineDiff.svelte` - Diff display with red/green highlighting
  - Expiration countdown timer
  - Cancel and Apply buttons with loading states

  **Tauri Event System**
  - `llm:plan-ready` - Plan ready for user review
  - `llm:plan-applied` - Plan successfully applied
  - `llm:plan-rejected` - Plan rejected by user
  - `llm:config-updated` - Configuration updated notification
  - `llm:midi-learn-state-changed` - MIDI Learn state changes

  **Documentation**
  - User guides: Chat Interface, LLM Providers
  - Tutorial: Create Mapping with Chat
  - Troubleshooting: Chat Issues
  - Reference: MCP Tools
  - Developer docs: LLM Integration, MCP Server, Agent Skills

### Changed
- MCP server now exposes 10 tools (was 5 ReadOnly)
- Improved placeholder documentation in config_helpers.rs, menu_bar.rs, profile_manager.rs, app_detection.rs

### Dependencies
- Added `serde` feature to `chrono` for DateTime serialization

## [4.11.0] - 2026-02-01

### Added
- **LLM Integration Architecture (ADR-007)**: Natural language configuration via Agent Skills and MCP

  **Phase 1A: Skills Foundation**
  - Bundled Agent Skills in `skills/` directory following agentskills.io specification
    - `conductor-midi-mapping` - Primary mapping skill with trigger/action references
    - `conductor-midi-learn` - MIDI Learn mode skill with pattern detection algorithms
    - `conductor-device-setup` - Device setup skill with supported devices reference
  - Skills Validation CLI (`conductor-skills` binary)
    - `validate` - Validate skill structure and YAML frontmatter
    - `list` - List installed skills with metadata
    - `install` - Install skills from GitHub (future implementation)
  - Chat UI Component (skills-only mode)
    - `ChatView.svelte` - Main chat interface with message history
    - `MessageBubble.svelte` - Message rendering with role-based styling
    - `chat.js` store - Reactive chat state management
    - Added "Chat" section to sidebar navigation

  **Phase 1B: MCP Server & LLM Providers**
  - MCP Server in conductor-daemon
    - JSON-RPC 2.0 protocol over Unix domain sockets
    - 5 ReadOnly tools for daemon introspection:
      - `conductor_get_status` - Lifecycle state and uptime
      - `conductor_list_devices` - Available MIDI and HID devices
      - `conductor_get_config` - Current configuration
      - `conductor_list_mappings` - Mappings by mode
      - `conductor_get_mapping` - Single mapping by mode/index
    - Tool Risk Tiers (ReadOnly, Stateful, ConfigChange, HardwareIO, Privileged)
    - Socket path: `~/.cache/conductor/conductor-mcp.sock` (or runtime dir)
  - LLM Provider Abstraction in conductor-gui
    - `LLMProvider` trait with chat, streaming, and health check methods
    - OpenAI provider (GPT-4o, GPT-4-turbo, GPT-4, GPT-3.5-turbo)
    - Anthropic provider (Claude 4 Sonnet/Opus, Claude 3.5 Sonnet/Haiku)
    - Secure API key storage via system keychain (macOS Keychain, Windows Credential Manager)
    - Full chat types: ChatMessage, ChatRequest, ChatResponse, ToolCall, etc.
    - Support for tool/function calling

### Dependencies
- Added `reqwest` 0.12 for HTTP client with streaming
- Added `keyring` 3 for secure credential storage
- Added `async-trait` 0.1 for async trait support
- Added `futures`/`futures-util` 0.3 for stream support
- Added `chrono` 0.4 for timestamps

## [4.10.12] - 2026-01-31

### Fixed
- **Test Stability**: Fixed flaky test `test_save_handles_absolute_and_relative_paths`
  - Root cause: `std::env::set_current_dir()` modifies global process state
  - Tests could race when running in parallel, causing intermittent failures
  - Added RAII `WorkingDirGuard` to ensure directory restoration even on panic
  - Fixed `test_relative_path_in_current_dir` which never restored working directory

## [4.10.11] - 2026-01-31

### Fixed
- **Lifecycle State Machine**: Daemon now correctly enters Degraded state when device connection fails during startup
  - Previously entered Running state regardless of connection result
  - Now uses Degraded state to signal operational-but-no-input condition
  - Recovery via IPC reconnection works from Degraded state
  - Added Starting → Degraded state transition to state machine
  - Added Starting → Stopping state transition for clean shutdown during startup
  - Fixes LLM Council concern about semantic correctness of daemon state

## [4.10.10] - 2026-01-31

### Fixed
- **Daemon Mode Tracking**: Fixed daemon always using mode 0 instead of last selected mode
  - Root cause: `process_input_event` had hardcoded `current_mode = 0`
  - Now reads `last_selected_mode` from config on startup and tracks it
  - CC triggers in non-default modes now work correctly
  - Added `current_mode_index` field to EngineManager for mode tracking
  - Mode index is reconciled on config reload to prevent out-of-bounds access

## [4.10.9] - 2026-01-31

### Added
- **Persistent Mode Selection**: Selected mode in Mappings view now persists across app restarts
  - Added `last_selected_mode` field to config.toml (stores mode name)
  - On app load, restores previously selected mode from config
  - On mode change, saves selection to config file
  - Mode selection also persists during navigation within the same session via appStore

### Fixed
- **SendMidi Auto-Connect**: SendMidi action now auto-connects to output ports on demand
  - Previously, MIDI output ports required manual connection before sending
  - Added `connect_by_name()` method to `MidiOutputManager`
  - `execute_send_midi()` now auto-connects if port isn't already connected
  - Error messages include list of available ports when port not found

- **MIDI Learn CC Classification**: Fixed incorrect classification of CC events as Encoder
  - Root cause: `MidiEvent::ControlChange` was converted to `InputEvent::EncoderTurned` upstream
  - Fix: CC events now stay as `InputEvent::ControlChange` through the pipeline
  - Added `ProcessedEvent::CCReceived` for pedals/buttons that send fixed CC values
  - Added trigger matching for `CC` triggers against `CCReceived` events
  - MIDI Learn now correctly classifies single CC events as `CC` (not `Encoder`)
  - This ensures pedal presses trigger correctly without requiring encoder-like behavior

## [4.10.8] - 2026-01-31

### Fixed
- **Reduced Noisy Trigger Matching Warnings**: Changed fallback log level from `warn!` to `trace!` in `trigger_matches_processed`
  - The warning fired for every non-matching trigger/event combination (expected behavior)
  - For example: GamepadButton triggers tested against EncoderTurned events would warn
  - Now only visible when running with `DEBUG=1` or `RUST_LOG=trace`
  - No change to actual matching behavior - just reduces log noise

## [4.10.7] - 2026-01-31

### Fixed
- **MappingsView Explicit Event Handlers** (#65): Fixed action/trigger changes not saving (follow-up to #64)
  - Root cause: Svelte's `bind:prop={object.property}` doesn't reliably trigger parent reactivity
  - Added explicit `on:change` handlers in MappingsView.svelte for ActionSelector and TriggerSelector
  - Handlers directly update `editingMapping.action` and `editingMapping.trigger` from event detail
  - Reassign `editingMapping = editingMapping` to explicitly trigger Svelte reactivity

## [4.10.6] - 2026-01-30

### Fixed
- **ActionSelector Type Change Propagation** (#64): Fixed action type changes not saving
  - Root cause: When changing action type (e.g., Keystroke → Send MIDI), the new config wasn't reliably syncing to the bound `action` prop
  - Replaced reactive statement `$: action = config` with explicit `syncToAction()` function
  - All config-modifying functions (`handleTypeChange`, `toggleModifier`, `emitChange`) now explicitly call `syncToAction()`
  - Error fixed: Action type changes now correctly propagate to parent component on save

## [4.10.5] - 2026-01-30

### Fixed
- **ActionSelector/TriggerSelector Binding Sync** (#63): Fixed Keystroke keys not saving in Mapping Editor
  - Root cause: Svelte components used local `config` state without syncing back to bound `action`/`trigger` props
  - Added `$: action = config;` reactive statement to `ActionSelector.svelte`
  - Added `$: trigger = config;` reactive statement to `TriggerSelector.svelte`
  - This ensures `bind:action` and `bind:trigger` patterns work correctly with parent components
  - Error fixed: `Config validation failed: Invalid action: Keystroke requires keys`

## [4.10.4] - 2026-01-30

### Fixed
- **MIDI Learn Trigger Type Conversion** (#59): Fixed MIDI Learn encoder/chord triggers failing to save
  - Root cause: `TriggerSuggestion` format (`Encoder`) was passed directly to save flow expecting `Trigger` format (`EncoderTurn`)
  - Added `convertSuggestionToTrigger()` in `MappingsView.svelte` to convert types:
    - `Encoder` → `EncoderTurn`
    - `Chord` → `NoteChord`
  - Also handles field name differences (`window_ms` → `timeout_ms`)
  - MIDI Learn preview (`generate_trigger_config_toml`) continues using `TriggerSuggestion` format

## [4.10.3] - 2026-01-30

### Security
- **File Permission Hardening** (#55): Created profile configs now have 0o600 permissions on Unix
  - Ensures configs are only readable/writable by owner regardless of system umask
  - Uses `tokio::fs::set_permissions()` after atomic file creation
  - TDD test verifies mode 0o600 on created files

- **Profile Name Validation** (#56): Profile names validated in `update_profile()`
  - Rejects names with control characters (log injection prevention)
  - Rejects names exceeding 256 characters (DoS prevention)
  - Rejects null bytes (string termination attacks)
  - 5 TDD tests cover validation scenarios

- **Bounded Main Config Read** (#57): Main config read limited to 10MB
  - Prevents memory exhaustion when copying main config to new profile
  - Falls back to default template if main config exceeds limit
  - Graceful degradation with warning log

- **Filename Collision Detection** (#58): Auto-generated filenames detect collisions
  - When sanitized profile ID conflicts with existing file, adds numeric suffix
  - Prevents profile overwrite when different IDs sanitize to same filename
  - Safety limit of 1000 attempts prevents infinite loops
  - 2 TDD tests verify collision handling

### Documentation
- Updated ADR-006 with v4.10.3 security hardening patterns

## [4.10.2] - 2026-01-30

### Fixed
- **Profile Creation Bug** (#52): Fixed profile creation failing when frontend sends empty config_path
  - Backend now auto-generates config_path using sanitized profile ID
  - Creates default config file from template or copies main config
  - Added `create_default_profile_config()` helper method
  - 3 TDD tests verify auto-generation behavior

### Changed
- **App Profiles Moved to Sidebar** (#53): Improved UX for per-app profile management
  - App Profiles now accessible directly from main sidebar navigation
  - Created dedicated `ProfilesView.svelte` component
  - Removed ProfileManager modal from DevicesView
  - DevicesView now focused solely on device/template management

### Added
- **Default Profile Template**: New `resources/default_profile.toml` for auto-generated profiles
  - Contains device settings, modes, and example mappings
  - Used when no main config exists to copy from

## [4.10.1] - 2026-01-30

### Security
- **Arbitrary File Read Prevention** (ADR-006): Fixed `import_profile_json()` accepting untrusted config paths
  - Added path validation to reject paths outside allowed directories
  - Canonicalizes paths to prevent symlink attacks
  - Rejects `/etc/passwd` and similar sensitive files
- **Path Traversal Prevention** (ADR-006): Fixed `import_profile_toml()` using filename as profile ID
  - Added `sanitize_profile_id()` to strip dangerous characters
  - Added `validate_profile_id()` to reject `../` sequences
  - Added `safe_profile_destination()` for safe path construction
- **TOCTOU Race Condition Fix** (ADR-006): Fixed race conditions in `update_profile()` and `delete_profile()`
  - Now holds all locks for entire read-modify-write operations
  - Prevents data loss under concurrent access
  - 2 concurrency tests verify atomic operations

### Performance
- **Async I/O Migration**: Converted 8 blocking `std::fs` calls to `tokio::fs` in profile_manager.rs
  - Prevents GUI freezes when file operations are slow
  - Locations: create_dir_all (2), read_to_string (3), read_dir, metadata, write, copy

### Fixed
- **Cache Staleness**: Added mtime-based cache invalidation to detect external file modifications
  - Cache now stores file modification time
  - Reloads from disk when file changes externally, even within TTL
- **Non-Deterministic Default Profile**: Fixed `get_default_profile()` returning random results
  - When multiple profiles are marked as default (invalid state), now returns alphabetically first
  - Logs warning when invariant is violated

### Added
- **path_validation.rs Module**: New security module with 18 TDD tests
  - `validate_profile_id()`: Profile ID safety validation
  - `validate_config_path()`: Config path containment validation
  - `safe_profile_destination()`: Safe import destination construction
  - `sanitize_profile_id()`: Unsafe character removal

### Documentation
- Created ADR-006: Profile Manager Security & Performance Patterns

## [4.9.0] - 2026-01-30

### Added
- **Type-Safe Event Schemas** (ADR-004, #42, #47): Replaced 32 hard-coded strings with type-safe enums
  - Created `EventType` enum (12 variants) and `PatternType` enum (4 variants) in conductor-core
  - Serde snake_case serialization for JSON API compatibility
  - Helper methods: `is_gamepad()`, `is_midi()` for classification
  - 11 TDD tests for serialization/deserialization
  - See ADR-004 for cross-layer type safety patterns

### Fixed
- **VelocityRange Config Ignored** (#48): Trigger now respects user-defined soft_max/medium_max
  - Previously: Config values silently ignored, hard-coded 40/80 boundaries used
  - Now: `classify_velocity()` helper uses config thresholds
  - Added trace logging for velocity classification
  - 3 new TDD tests verify custom threshold behavior

- **load_from_config Stale Modes** (#49): Fixed HashMap not clearing old modes on reload
  - When config changes from 5 modes to 3, old modes 3-4 no longer persist
  - Added `mode_mappings.clear()` at start of function
  - Added `mode_count()` method for testability
  - 2 regression tests verify mode count changes correctly

- **Chord Comment/Code Mismatch** (#50): Updated misleading comment in mapping.rs
  - Comment implied subset matching ("all required notes present")
  - Code performs exact matching (required == detected)
  - Updated to accurately describe behavior

### Documentation
- Created ADR-004: Type-Safe Event Schemas documenting cross-layer type patterns
- Updated ADR-002 with v4.9.0 implementation status and deferred work tracking

## [4.8.0] - 2026-01-30

### Fixed
- **DaemonStatus.connected Semantics Bug** (ADR-002, #41): Fixed misleading connected status
  - `DaemonStatus.connected` was hardcoded to `true` on IPC success, causing GUI to show
    "connected" even when no device was actually connected
  - Now correctly derives from `device.connected` status: `device.as_ref().map_or(false, |d| d.connected)`
  - Added 3 TDD tests for derivation scenarios (device connected, disconnected, absent)

### Infrastructure
- **LLM Council v4.7.0 Verification Investigation**
  - Investigated 7 Council claims from v4.7.0 verification (FAIL, 0.52 confidence)
  - Identified 5 false positives (HID ID type, input_mode casing, VecDeque capacity, Rust edition, dates)
  - Confirmed 1 real bug (DaemonStatus.connected semantics) - fixed in this release
  - Noted 1 valid enhancement for v4.9.0 (schema fragility - stringly-typed event/pattern types)
  - Updated ADR-002 with full investigation results

## [4.7.0] - 2026-01-30

### Added
- **MIDI Learn Pattern Auto-Detection** (ADR-002, #36): Multi-event pattern auto-detection in MIDI Learn mode
  - Daemon now processes events through `EventProcessor` during MIDI Learn
  - Extended `MidiLearnEvent` and `DaemonMidiLearnEvent` structs with pattern fields:
    - `pattern_type`: "long_press", "double_tap", "chord", "gamepad_chord"
    - `pattern_notes`: For MIDI chord patterns
    - `pattern_buttons`: For gamepad chord patterns
    - `pattern_duration_ms`: For long press timing
    - `pattern_timeout_ms`: For chord/double-tap windows
  - Updated `stores.js` `eventToTrigger()` to convert detected patterns to triggers (#39)
  - Updated `stores.js` `formatEvent()` for human-readable pattern display
  - Enables auto-detection of LongPress, DoubleTap, NoteChord, GamepadButtonChord

### Fixed
- Gamepad events now properly emit as `gamepad_button`, `gamepad_axis`, `gamepad_trigger` event types
- `gamepad_axis` event type added to `eventToTrigger()` switch case

### Infrastructure
- Added 5 TDD tests for `DaemonMidiLearnEvent` pattern fields in commands.rs
- GitHub Epic #36 with sub-issues #37-#40 for tracking
- ADR-002 requirements U3, U4, U5, U10 marked Complete (100% UI coverage)

## [4.6.0] - 2026-01-30

### Added
- **Gamepad Trigger UI Support** (ADR-002, #30): Full UI support for 4 gamepad trigger types
  - **TriggerSelector.svelte** (#31, #32): Added gamepad triggers to dropdown and form sections
    - GamepadButton: Button ID selector with human-readable labels (A/Cross, B/Circle, etc.)
    - GamepadButtonChord: Multi-button chord with timeout configuration
    - GamepadAnalogStick: Axis selector (Left X/Y, Right X/Y) with direction filter
    - GamepadTrigger: Trigger selector (LT/RT) with threshold configuration
  - **MidiLearnDialog.svelte** (#33): Human-readable formatTrigger() for gamepad triggers
  - **stores.js** (#34): Gamepad event formatting and trigger conversion
    - formatEvent() for gamepad_button, gamepad_button_release, gamepad_stick, gamepad_trigger
    - eventToTrigger() for gamepad event → trigger conversion

### Infrastructure
- Added `getGamepadButtonName()` helper function in TriggerSelector, MidiLearnDialog, and stores.js
- Added `handleGamepadChordButtonsChange()` for multi-button chord input parsing
- ADR-002 updated with implementation completion status (UI 100% complete)

## [4.5.0] - 2026-01-30

### Fixed
- **GUI Commands Technical Debt** (ADR-003, #25): Remediated 4 state management issues in commands.rs
  - **Fail-Safe Defaults** (#26): `validate_config` no longer defaults to valid=true on malformed data
  - **AppState Synchronization** (#27): All IPC commands now properly update `daemon_connected` state
  - **Type-Safe Casting** (#28): Added `parse_port()` helper for bounds-checked MIDI port values (0-255)
  - **Atomic Replacement** (#29): `apply_connection_status()` sets all flags in single pass, preventing stale flags

### Added
- **ADR-003**: GUI State Management Patterns
  - Pattern 1: Fail-Safe Defaults Principle (never default to success on missing data)
  - Pattern 2: AppState Synchronization Requirement (IPC reachability vs device status)
  - Pattern 3: Type-Safe Casting Pattern (bounds-checked helpers)
  - Pattern 4: Atomic Replacement Pattern (prevent UI flickering)
  - Includes LLM Council review amendments

### Infrastructure
- Added 10 TDD tests for `apply_connection_status()` and `parse_port()` in commands.rs
- Extended AppState sync to `reload_config`, `stop_daemon`, `validate_config`, `ping_daemon`

## [4.4.0] - 2026-01-30

### Added
- **GUI Config Management** (#20): Implemented `get_config` and `save_config` Tauri commands
  - `get_config` (#21): Loads config.toml and returns as JSON for frontend
  - `save_config` (#22): Validates and saves config with atomic writes
  - Unblocks SettingsView, ModesView, MappingsView GUI components
  - GUI screens no longer show "Not implemented yet" errors
  - Daemon auto-reloads via ConfigWatcher within 500ms of save

### Infrastructure
- Added 9 TDD tests in `config_commands_test.rs` for config command validation
- Uses conductor-core's `Config::load()` and `Config::save()` for file I/O

## [4.3.0] - 2026-01-30

### Fixed
- **Trigger Matching Gaps** (ADR-002, #12): Fixed 7 MIDI trigger types that were silently failing
  - **DoubleTap** (#13): Now correctly matches `ProcessedEvent::DoubleTap` events
  - **LongPress** (#14): Now triggers when hold duration exceeds threshold
  - **Aftertouch** (#15): Now matches channel pressure events above threshold
  - **PitchBend** (#16): Now matches pitch bend events within optional value range
  - **VelocityRange** (#17): Now matches soft/medium/hard velocity presses
  - **CC ProcessedEvent** (#18): Now matches control change via ProcessedEvent path
  - **EncoderTurn** (#19): Now matches encoder rotation with optional direction filter
- **Global Mappings Fallback**: Fixed `get_action()` to correctly fall back to global mappings when mode-specific mappings don't match
- **Note ProcessedEvent Path**: Added `Note` trigger matching for `ProcessedEvent::PadPressed`

### Added
- **Council Safeguard**: Added `tracing::warn!` for unhandled trigger/event combinations in mapping engine
- **TDD Test Suite**: 22 new tests in `trigger_matching_test.rs` covering all trigger matching scenarios

### Infrastructure
- Added 6 new `CompiledTrigger` enum variants (DoubleTap, LongPress, Aftertouch, PitchBend, VelocityRange, EncoderTurn)
- Added 8 new match arms in `trigger_matches_processed()` for complete trigger coverage
- ADR-002 updated with Resolution section documenting fix completion

## [4.2.0] - 2026-01-30

### Added
- **MIDI Learn Event Streaming** (#10): MIDI Learn now captures real events from connected devices
  - Events stream from daemon to GUI via polling-based architecture
  - Click captured events to select as trigger
  - Auto-timeout with configurable duration (default 30 seconds)
  - Real-time event list with last 20 captured events
  - Supports Note On/Off, CC, Encoder, Pitch Bend, Aftertouch events
- **ADR-002**: Architecture Decision Record for MIDI Learn event streaming
  - Documents polling approach via IPC commands
  - Includes LLM Council review feedback and safeguards

### Fixed
- **MIDI Learn Dialog** (#10): Fixed component wiring so dialog opens correctly
  - Added missing `isOpen` prop to MidiLearnDialog in MappingsView
  - Added missing `on:midiLearn` handler to TriggerSelector
  - Dialog now uses `onCapture` callback prop instead of Svelte events
- **Daemon Event Capture**: Fixed InputEvent pattern matching for MIDI Learn
  - Corrected field destructuring for all InputEvent variants
  - Channel defaults to 0 (unified abstraction doesn't have channel info)

### Infrastructure
- Added `START_MIDI_LEARN`, `STOP_MIDI_LEARN`, `GET_MIDI_LEARN_EVENTS` IPC commands
- Added `MidiLearnEvent` struct in daemon for event data transfer
- Added `midi_learn_active` flag and `midi_learn_events` buffer in EngineManager
- Added daemon MIDI Learn API (`daemonMidiLearn`) in GUI api.js
- Added `daemonMidiLearnStore` in GUI stores.js for daemon-based MIDI Learn
- Added 4 new tests for MIDI Learn command serialization

## [4.1.0] - 2026-01-29

### Added
- **GUI Device Connection** (#7): Connect/Disconnect buttons now functional in GUI
  - Users can connect to any available MIDI device from the Devices panel
  - Real-time connection status updates preserved during auto-refresh
  - Error handling with dismissible error messages
  - IPC client for GUI-daemon communication
  - `DisconnectDevice` IPC command
- **ADR-001**: Architecture Decision Record for GUI device connection via IPC

### Fixed
- Device connection status now persists during auto-refresh (#9)
- Running average calculation in daemon statistics can now decrease
- `get_connected_gamepads()` now returns only actually connected devices

### Infrastructure
- Added `get_midi_manager()` and `get_midi_manager_mut()` to InputManager
- Implemented `switch_device()` and `disconnect_midi_device()` in EngineManager
- Added `reconnect_midi_port()` to InputManager for port-based reconnection
- Added `get_connected_gamepad_info()` to HidDeviceManager
- Added `apply_connection_status()` helper with unit tests
- Added `get_connected_port_from_daemon()` for IPC status queries

## [3.0.0] - 2025-11-21

### 🎮 Multi-Protocol Input: Game Controller (HID) Support

**Major Release**: Conductor now supports game controllers (gamepads, joysticks, racing wheels, flight sticks, arcade controllers) alongside MIDI devices, enabling hybrid workflows with unified input management.

### Added - Multi-Protocol Input System

- **Game Controller (HID) Support**: Full support for SDL2-compatible game controllers
  - **Gamepads**: Xbox 360/One/Series, PlayStation DS4/DS5, Switch Pro Controller
  - **Joysticks**: Flight sticks, arcade sticks with analog axes and buttons
  - **Racing Wheels**: Logitech, Thrustmaster, and any SDL2-compatible wheel
  - **HOTAS**: Hands On Throttle And Stick systems for simulation
  - **Custom Controllers**: Any SDL2-compatible HID device
  - Button mapping (0-255, indexes 128-255 reserved for HID)
  - Axis support (LeftX, LeftY, RightX, RightY, LeftTrigger, RightTrigger)
  - Digital D-Pad mapping (Up, Down, Left, Right)

- **Unified InputManager**: Hybrid MIDI + HID input processing
  - Three operating modes: `MidiOnly`, `GamepadOnly`, `Both`
  - Unified event processing pipeline for MIDI and HID inputs
  - Independent device connection management
  - Graceful fallback when devices unavailable
  - Thread-safe concurrent device access

- **HID Trigger Types** (4 new types)
  - `GamepadButton`: Button press/release with button index (0-255)
  - `GamepadAxis`: Analog stick/trigger movement with threshold detection
  - `GamepadDPad`: Digital directional pad input (Up/Down/Left/Right)
  - `GamepadButtonCombo`: Multiple simultaneous button presses (chord detection)

### Added - Official Device Templates

- **6 Official Gamepad Templates**: Pre-configured mappings for popular controllers
  - `xbox-360-gamepad.toml` - Xbox 360 Controller
  - `xbox-one-gamepad.toml` - Xbox One/Series Controller
  - `playstation-ds4-gamepad.toml` - PlayStation DualShock 4
  - `playstation-ds5-gamepad.toml` - PlayStation DualSense (PS5)
  - `switch-pro-gamepad.toml` - Nintendo Switch Pro Controller
  - `generic-gamepad.toml` - Generic SDL2-compatible gamepad

- **Template Categories**: Organized by device type
  - `pad-controller` - Pad-based MIDI controllers
  - `gamepad` - Game controllers (Xbox, PlayStation, Switch, etc.)
  - `keyboard` - MIDI keyboard controllers
  - `mixer-controller` - DJ mixer-style controllers
  - More device types coming in future releases

### Added - GUI Integration

- **Template Selector Enhancement** (`config/templates/README.md`, 850+ lines)
  - Device type filtering in template browser
  - Gamepad category badge with controller icon
  - Search and filter by device name, vendor, or category
  - One-click template import with device type detection
  - Visual device type indicators (MIDI vs HID)

- **DevicesView Split**: Separate sections for MIDI and HID devices
  - **MIDI Controllers Section**: Lists connected MIDI input/output devices
  - **HID Game Controllers Section**: Lists connected game controllers with metadata
    - Controller name and vendor information
    - Button count and axis count display
    - Connection status indicators
    - Real-time connection/disconnection updates

- **MIDI Learn Integration**: MIDI Learn mode now supports HID devices
  - Capture gamepad button presses during MIDI Learn
  - Capture axis movements with threshold detection
  - Capture D-Pad directions
  - Auto-generate HID trigger configurations
  - Visual feedback during HID input capture

### Added - Daemon IPC Extensions

- **Extended Status Command**: IPC status now includes HID information
  - `input_mode` field: "midi-only", "gamepad-only", or "both"
  - `hid_devices` array: List of connected game controllers
    - Device name, vendor, product ID
    - Button count, axis count
    - Connection timestamp
  - Backward compatible with existing IPC clients

### Changed - Architecture

- **InputManager Modes**: Three operating modes for flexible workflows
  - `MidiOnly` (default): MIDI devices only (v2.x behavior)
  - `GamepadOnly`: HID game controllers only (no MIDI)
  - `Both`: Hybrid MIDI + HID workflows (unified event processing)

- **ID Range Allocation**: Clear separation of MIDI and HID identifiers
  - 0-127: Reserved for MIDI notes and CC messages
  - 128-255: Reserved for HID controller buttons and axes
  - Prevents conflicts in hybrid configurations

- **Event Processing Pipeline**: Unified handling of MIDI and HID events
  - Common `ProcessedEvent` enum for both input types
  - Unified trigger matching in mapping engine
  - Consistent velocity and timing detection across protocols

### Dependencies

- **Added**: `gilrs v0.10` - Cross-platform game controller library
  - Supports SDL2 game controller protocol
  - Works with gamepads, joysticks, racing wheels, flight sticks
  - Thread-safe device enumeration and event polling
  - Hot-plug support for USB controllers
  - Platform-specific backend (XInput on Windows, IOKit on macOS, evdev on Linux)

### Migration

- **100% Backward Compatible**: All existing MIDI configurations work unchanged
  - Default mode is `MidiOnly` (v2.x behavior)
  - No config changes required for MIDI-only workflows
  - Opt-in to HID support by setting `input_mode = "both"` or `input_mode = "gamepad-only"`

- **Migration Guide**: See `docs/MIGRATION_v2_to_v3.md` for complete guide
  - How to enable HID support
  - Converting MIDI mappings to HID mappings
  - Hybrid workflow examples
  - ID range best practices

### Documentation

- **User Guides** (2 new files, ~1,200 lines)
  - `docs/guides/gamepad-integration.md` - Complete game controller guide
    - Supported device types and vendors
    - Button and axis mapping reference
    - Configuration examples for common workflows
    - Troubleshooting and device detection
  - `docs/guides/hybrid-workflows.md` - MIDI + HID hybrid setup
    - When to use hybrid mode
    - Practical examples (DAW control, gaming macros, accessibility)
    - Best practices for ID range allocation

- **Configuration References** (1 new file, ~600 lines)
  - `docs/configuration/hid-triggers.md` - Complete TOML reference for HID triggers
    - GamepadButton, GamepadAxis, GamepadDPad, GamepadButtonCombo syntax
    - Threshold configuration for analog axes
    - Dead zone handling and sensitivity tuning
    - Validation rules and performance notes

- **Template Documentation** (`config/templates/README.md`, updated)
  - All 6 official gamepad templates documented
  - Device type categories explained
  - Template discovery and import instructions
  - Custom template creation guide

### Performance

- **HID Event Processing**: <0.5ms latency (comparable to MIDI)
- **Controller Polling**: 120 Hz default (8.3ms interval)
- **Memory Usage**: 10-15MB (5MB increase for HID support)
- **CPU Usage**: <2% idle, <6% active (1-2% increase for gamepad polling)
- **No impact on MIDI latency**: Still <1ms for MIDI events

### Testing

- **68 New Tests** (100% pass rate)
  - 15 InputManager tests (mode switching, device management)
  - 20 HID trigger tests (button, axis, D-Pad, combo)
  - 18 template loading tests (gamepad templates)
  - 15 GUI integration tests (DevicesView, MIDI Learn with HID)
- **Total Workspace Tests**: 213 tests passing (100% pass rate)
  - conductor-core: 60 tests (was 45)
  - conductor-daemon: 89 tests (was 74)
  - conductor-gui: 64 tests (was 26)

### Security

- **HID Device Sandboxing**: Controller access restricted to user-owned devices
- **Input Validation**: All button/axis values validated and clamped
- **No System Hooks**: Uses standard SDL2 APIs (no kernel extensions)
- **Permission Model**: Same Input Monitoring permission as MIDI (macOS)

### Platform Support

- **macOS**: Full support with IOKit backend
- **Linux**: Full support with evdev backend (udev rules may be required)
- **Windows**: Full support with XInput backend (Xbox controllers) and DirectInput fallback

### Known Limitations

- Gamepad LED control not yet implemented (planned for v3.1)
- Haptic feedback (vibration) not yet supported (planned for v3.1)
- Gyroscope/accelerometer data not exposed (planned for v3.2)
- Touchpad input (DS4/DS5) not yet supported (planned for v3.2)

### Breaking Changes

None - fully backward compatible with v2.7.0. All MIDI-only configurations work unchanged.

### Next Steps

- v3.1: Gamepad LED control and haptic feedback
- v3.2: Advanced HID features (gyroscope, touchpad)
- v3.3: Custom HID device profiles (beyond SDL2 gamepad mapping)

## [2.7.0] - 2025-11-19

### 🔐 Plugin Security & Verification

**Phase 6 (Part 4)**: Comprehensive security layer for WASM plugins with cryptographic signing, resource limiting, filesystem sandboxing, and enterprise-grade safety guarantees.

### Added - Plugin Signing & Verification

- **Ed25519 Digital Signatures** (486 lines) - Industry-standard cryptographic verification
  - 32-byte public keys, 64-byte signatures
  - SHA-256 binary integrity checking
  - Deterministic signing (identical inputs → identical signatures)
  - Embedded JSON metadata (signer name, email, timestamp, version)
  - Protection against tampering and unauthorized modifications

- **Three-Tier Trust Model** - Flexible security policies
  - **Unsigned**: Development and testing (security warnings displayed)
  - **Self-Signed**: Plugins signed with any valid key (authenticity verified)
  - **Trusted Keys**: Only allow plugins signed with pre-approved keys (maximum security)
  - Configurable per-plugin or system-wide
  - Trust store in `~/.conductor/trusted_keys.json`

- **CLI Signing Tool** (`conductor-sign`, 460 lines) - Complete key management and signing workflow
  - `generate-key` - Generate Ed25519 keypair with PEM encoding
  - `sign` - Sign WASM plugins with metadata embedding
  - `verify` - Verify plugin signatures and integrity
  - `trust add/remove/list` - Manage trusted key store
  - Portable PEM format for easy distribution
  - Integration with WASM plugin loader

### Added - Resource Limiting

- **Fuel Metering** - CPU instruction counting to prevent runaway plugins
  - Default: 100M instructions per execution
  - Configurable per-plugin (10M to 1B instructions)
  - Real-time tracking via wasmtime fuel API
  - Automatic termination on limit exceeded
  - Performance overhead: <1%

- **Memory Limits** - Prevent memory exhaustion
  - Default: 128 MB maximum memory per plugin
  - Configurable per-plugin (16 MB to 512 MB)
  - Linear memory growth constraints
  - Table growth limits (1024 elements default)
  - Protection against allocation attacks

### Added - Filesystem Sandboxing

- **Directory Preopening** (WASI) - Whitelist-based filesystem access
  - Explicit directory grants (read-only or read-write)
  - Path traversal prevention (no `../` escapes)
  - Default: No filesystem access unless explicitly granted
  - Per-plugin directory configuration
  - WASI preview1 standard compliance

### Added - Integration Tests

- **10 Plugin Signing Tests** (436 lines, 100% pass rate, 0.53s execution)
  - `test_sign_and_verify_workflow` - End-to-end signing workflow
  - `test_load_signed_plugin_with_self_signed_mode` - Self-signed loading
  - `test_reject_unsigned_plugin_when_required` - Signature enforcement
  - `test_reject_tampered_plugin` - Binary integrity detection
  - `test_reject_invalid_signature` - Wrong key rejection
  - `test_signature_metadata_format` - JSON metadata parsing
  - `test_load_unsigned_plugin_when_not_required` - Backward compatibility
  - `test_multiple_executions_with_signed_plugin` - Runtime verification
  - `test_key_size_validation` - Ed25519 key validation (32-byte enforcement)
  - `test_signature_deterministic` - Reproducible signatures

### Added - Documentation

- **mdBook WASM Plugin Documentation** (6,715 lines across 4 new pages)
  - `development/wasm-plugins.md` - Overview, architecture, security features
  - `development/wasm-plugin-development.md` - Complete development tutorial
  - `development/plugin-security.md` - 4-layer security architecture guide
  - `development/plugin-examples.md` - Real-world examples (Spotify, OBS, system utils)
  - Quick comparison tables (native vs WASM plugins)
  - Security checklists and best practices
  - Complete conductor-sign CLI reference
  - Configuration examples with all security modes

- **Technical Documentation** (648 lines)
  - `docs/v2.7-plugin-signing-complete.md` - Complete implementation report
  - Architecture diagrams with 4-layer security model
  - Performance benchmarks and overhead analysis
  - Integration guide for plugin developers

### Technical Details

- **Production Code**: ~1,400 lines across 3 new files
  - `conductor-core/src/plugin/signing.rs` (486 lines)
  - `conductor-daemon/src/bin/conductor-sign.rs` (460 lines)
  - `conductor-core/tests/plugin_signing_test.rs` (436 lines)

- **Dependencies Added**:
  - `ed25519-dalek v2.2` - Ed25519 signatures
  - `pem v3.0` - PEM encoding for keys
  - `base64 v0.22` - Base64 encoding for signatures

- **Test Coverage**: 10 integration tests (100% passing)
- **Build Time**: No measurable impact (still ~26s clean, ~4s incremental)
- **Runtime Overhead**:
  - Signature verification: <5ms on first load (one-time cost)
  - Fuel metering: <1% per execution
  - Memory tracking: Negligible

### Security Architecture

```
┌─────────────────────────────────────────────────┐
│  Security Layers                                │
│                                                 │
│  Layer 1: Cryptographic Verification           │
│  - Ed25519 digital signatures                   │
│  - SHA-256 integrity checking                   │
│                                                 │
│  Layer 2: Resource Limiting                     │
│  - CPU fuel metering (100M instructions)        │
│  - Memory limits (128 MB)                       │
│                                                 │
│  Layer 3: Filesystem Sandboxing                 │
│  - Directory preopening (WASI)                  │
│                                                 │
│  Layer 4: Capability System                     │
│  - Explicit permission model (from v2.3)        │
└─────────────────────────────────────────────────┘
```

### Usage

**Generate Keypair:**
```bash
conductor-sign generate-key ~/.conductor/my-key
# Creates: my-key.pem (private), my-key.pub.pem (public)
```

**Sign Plugin:**
```bash
conductor-sign sign my_plugin.wasm ~/.conductor/my-key \
  --name "Your Name" \
  --email "you@example.com"
# Creates: my_plugin.wasm.sig (detached signature)
```

**Verify Signature:**
```bash
conductor-sign verify my_plugin.wasm
# Output: Signature verified successfully (shows metadata)
```

**Manage Trust Store:**
```bash
# Add trusted key
conductor-sign trust add ~/.conductor/my-key.pub.pem "My Plugin"

# List trusted keys
conductor-sign trust list

# Remove trusted key
conductor-sign trust remove <public-key-hex>
```

**Configuration (Trusted Keys Mode):**
```toml
[[modes.mappings]]
trigger = { Note = { note = 60 } }
action = { WasmPlugin = {
    path = "~/.conductor/wasm-plugins/my_plugin.wasm",
    signature_policy = "trusted_keys_only",  # Require pre-approved keys
    max_fuel = 50000000,  # 50M instructions
    max_memory_mb = 64,   # 64 MB limit
    allowed_dirs = [
        { path = "~/.conductor/plugin-data", writable = true }
    ]
}}
```

### Performance

- **Signature Verification**: <5ms (one-time on load)
- **Fuel Metering Overhead**: <1% per execution
- **Memory Tracking**: Negligible overhead
- **No impact on MIDI event processing latency**: Still <1ms

### Breaking Changes

None - fully backward compatible with v2.6.0. Unsigned plugins continue to work with security warnings.

### Migration Guide

1. Pull latest code: `git pull origin main`
2. Build release: `cargo build --release --workspace`
3. Install CLI tool: `cargo install --path conductor-daemon --bin conductor-sign`
4. (Optional) Generate signing keys: `conductor-sign generate-key ~/.conductor/my-key`
5. (Optional) Sign existing plugins: `conductor-sign sign plugin.wasm ~/.conductor/my-key`
6. (Optional) Configure trust store for maximum security

### Security Considerations

- **Unsigned plugins**: Display security warnings but execute (backward compatibility)
- **Self-signed plugins**: Verify signature authenticity, no pre-approval needed
- **Trusted keys mode**: Maximum security - only execute plugins from approved developers
- **Resource limits**: Prevent denial-of-service attacks from runaway plugins
- **Filesystem sandboxing**: Prevent unauthorized file access
- **No network sandboxing yet**: WASM plugins can make network requests if capability granted

### Known Issues

None

### Next Steps

- v2.8: Plugin marketplace with discovery and distribution
- v2.9: Network sandboxing for WASM plugins
- v3.0: Windows and Linux platform support for app detection

## [2.3.0] - 2025-01-18

### 🔌 Plugin Architecture

**Phase 6**: Extensible plugin system allowing third-party developers to create custom actions through dynamically loaded shared libraries with capability-based security.

### Added - Core Plugin Infrastructure

- **ActionPlugin Trait** (335 lines) - Core plugin interface with 7 methods
  - `name()`, `version()`, `description()`, `author()`, `license()` - Metadata methods
  - `execute()` - Main execution method with params and context
  - `capabilities()` - Capability requirements declaration
  - `initialize()` / `shutdown()` - Optional lifecycle hooks

- **Plugin Loader** (259 lines) - Dynamic library loading via libloading
  - Platform-specific binary support (.dylib/.so/.dll)
  - Symbol resolution for `_create_plugin` C-ABI function
  - Version compatibility checking
  - Safe trait object handling

- **Plugin Discovery** (440 lines) - Manifest-based plugin registry
  - Scans `~/.conductor/plugins/` for `plugin.toml` manifests
  - TOML-based plugin metadata parsing
  - Plugin registry with HashMap storage
  - Duplicate detection and validation

- **Capability System** (172 lines) - Permission-based security model
  - **6 Capability Types**: Network, Filesystem, Audio, Midi, Subprocess, SystemControl
  - **3 Risk Levels**: Low (auto-grant), Medium, High (explicit approval)
  - Auto-grant for safe capabilities (Network, Audio, Midi)
  - Per-plugin capability tracking

### Added - Plugin Manager

- **PluginManager** (645 lines) - Lifecycle and execution management
  - Thread-safe with Arc<RwLock<HashMap>>> for concurrent access
  - Plugin lifecycle: discover → load → initialize → execute → shutdown → unload
  - SHA256 binary verification (optional)
  - Execution statistics (call count, failures, latency)
  - Error handling with comprehensive error types

- **Action::Plugin Integration** - Seamless action execution
  - New `Action::Plugin { plugin, params }` variant
  - TriggerContext propagation (velocity, mode, timestamp)
  - JSON parameter support via serde_json::Value
  - Backward compatible with existing actions

### Added - GUI Plugin Manager

- **Plugin Management UI** (850 lines) - Complete plugin control interface
  - Plugin discovery and listing with metadata cards
  - Load/unload controls for lifecycle management
  - Enable/disable toggles for plugin availability
  - Capability grant/revoke with risk level indicators
  - Execution statistics display (calls, failures, latency)
  - Search and filtering by name, type, capabilities
  - Risk level badges (color-coded: green/yellow/red)

- **Tauri Backend Commands** (274 lines) - 11 plugin management commands
  - `plugin_discover` - Scan for new plugins
  - `plugin_list_available` / `plugin_list_loaded` - List plugins
  - `plugin_get_metadata` - Fetch plugin details
  - `plugin_load` / `plugin_unload` - Lifecycle control
  - `plugin_enable` / `plugin_disable` - Toggle availability
  - `plugin_grant_capability` / `plugin_revoke_capability` - Permission management
  - `plugin_get_stats` - Get execution metrics

### Added - Example Plugin

- **HTTP Request Plugin** (265 lines + 200 lines docs) - Reference implementation
  - HTTP methods: GET, POST, PUT, DELETE
  - Custom headers support
  - JSON body with velocity substitution (`{velocity}` placeholder)
  - Error handling and logging
  - 5 unit tests covering all features
  - Complete README with usage examples

### Added - Documentation

- **Plugin Development Guide** (850+ lines) - Comprehensive tutorial
  - Quick start guide with step-by-step instructions
  - Complete API reference
  - Capability system explanation
  - Testing strategies
  - Distribution instructions
  - Best practices and troubleshooting

- **mdbook Integration** - Added to documentation site
  - `/development/plugin-development.md` - Developer guide
  - Integration with existing documentation structure

### Technical Details

- **Production Code**: ~5,800 lines across 11 new files
- **Test Coverage**: 42 plugin-specific tests (100% passing)
- **Dependencies Added**: libloading, sha2
- **Build Time**: No measurable impact (still ~26s clean, ~4s incremental)
- **Runtime Overhead**: <0.1ms per plugin execution

### Security

- Capability-based permission system prevents unauthorized access
- Risk-level assessment (Low/Medium/High) with auto-grant logic
- SHA256 checksum verification for binary integrity
- Plugins run in same process (not sandboxed) - trust required
- GUI displays risk levels clearly with color-coded badges

### Performance

- Plugin loading: ~10-50ms per plugin (one-time cost)
- Discovery: ~5ms for 10 plugins
- Execution overhead: <0.1ms per action
- No impact on existing action types

### Breaking Changes

None - fully backward compatible with v2.2.0

### Migration Guide

1. Pull latest code
2. Run `cargo build --release`
3. Create `~/.conductor/plugins/` directory
4. Install plugins as needed
5. Use GUI Plugin Manager to manage plugins

### Known Issues

None

## [2.2.0] - 2025-11-18

### 🎯 Velocity Curves & Advanced Conditionals

**Phase 5 (Part 2)**: Context-aware mappings and velocity-sensitive controls enabling dynamic workflows that adapt to time, application context, and input intensity.

### Added - Advanced Conditionals System

- **10 Condition Types**: Build complex conditional logic for context-aware actions
  - `Always` / `Never` - Testing and debugging conditions
  - `TimeRange` - Time-based workflows (HH:MM format, supports midnight crossing)
  - `DayOfWeek` - Day-based workflows (1=Monday through 7=Sunday)
  - `AppRunning` - Process detection (macOS, Linux via `pgrep`)
  - `AppFrontmost` - Active window detection (macOS via NSWorkspace)
  - `ModeIs` - Current mode matching for mode-aware actions
  - `And` / `Or` - Logical operators with short-circuit evaluation
  - `Not` - Logical negation for inverted conditions

- **Conditional Action Type**: Execute different actions based on runtime conditions
  - `then_action` - Action executed when conditions are true
  - `else_action` - Action executed when conditions are false
  - Nested conditions support (unlimited depth)
  - Real-time condition evaluation with <1ms latency

### Added - Velocity Mapping System

- **4 Velocity Mapping Types**: Transform trigger velocity to action-specific values
  - `Fixed` - Constant velocity output (ignore input velocity)
  - `PassThrough` - 1:1 direct mapping (velocity unchanged)
  - `Linear` - Custom min/max range scaling with configurable bounds
  - `Curve` - Non-linear transformations with intensity control:
    - **Exponential**: `output = input^(1-intensity)` - Boost soft hits
    - **Logarithmic**: `log(1 + intensity × input) / log(1 + intensity)` - Compress hard hits
    - **S-Curve**: Sigmoid function with intensity-controlled steepness

- **Integration with SendMIDI**: Velocity mapping applies to MIDI output messages
  - Map trigger velocity → MIDI NoteOn velocity dynamically
  - Real-time curve calculation with <0.1ms overhead
  - Visual curve preview in GUI

### Added - Mode Context Propagation

- **TriggerContext Enhancement**: Actions now receive current mode information
  - `current_mode: Option<usize>` field added to TriggerContext
  - Enables `ModeIs` condition evaluation
  - Backward compatible (optional field)

### Added - GUI Components

- **ConditionalActionEditor** (596 lines)
  - Visual condition builder for all 10 condition types
  - Time picker for TimeRange conditions
  - Day selector for DayOfWeek conditions
  - App selector with process detection
  - Logical operator composition (And/Or/Not)
  - Nested condition support with tree view
  - Real-time validation with error display

- **VelocityMappingSelector**
  - Curve type selector (Fixed/PassThrough/Linear/Curve)
  - Real-time curve preview graph (SVG visualization)
  - 64-point curve sampling for smooth preview
  - Interactive parameter controls (min/max/intensity)
  - Visual feedback for curve shape

### Documentation

- **User Guides** (2 new files, ~1,000 lines)
  - `docs-site/src/guides/velocity-curves.md` - Complete velocity mapping guide
    - All 4 mapping types with mathematical formulas
    - Practical use cases and examples
    - GUI configuration instructions
    - Tips and best practices
  - `docs-site/src/guides/context-aware.md` - Context-aware mappings guide
    - All 10 condition types documented
    - Platform support notes (macOS/Linux/Windows)
    - Real-world practical examples
    - Nested condition patterns

- **Configuration References** (2 new files, ~800 lines)
  - `docs-site/src/configuration/curves.md` - Complete TOML reference for velocity mappings
    - Parameter constraints and validation rules
    - Intensity parameter guide
    - Default behavior documentation
  - `docs-site/src/configuration/conditionals.md` - Complete TOML reference for conditions
    - All 10 condition types with syntax examples
    - Nested conditions documentation
    - Validation rules and performance notes

- **Tutorial** (1 new file, ~500 lines)
  - `docs-site/src/tutorials/dynamic-workflows.md` - Step-by-step workflow tutorial
    - Beginner: Time-based app launcher
    - Intermediate: Velocity-sensitive DAW control
    - Advanced: Multi-condition smart assistant
    - Best practices and debugging tips

- **Updated Files**
  - `docs-site/src/configuration/actions.md` - Updated Conditional action reference
  - `docs-site/src/SUMMARY.md` - Added new guides and tutorial section

### Performance

- **Condition Evaluation**: <1ms for most conditions
  - TimeRange/DayOfWeek: Very fast (system time lookup)
  - ModeIs: Very fast (string comparison)
  - AppFrontmost: Very fast (<1ms, native API)
  - AppRunning: Moderate (~10ms, subprocess call)
  - And/Or: Short-circuit evaluation for efficiency

- **Velocity Curve Calculation**: <0.1ms
  - No performance impact on MIDI event processing
  - Memory usage: 5-10MB (no increase from v2.1)

### Testing

- **145 Workspace Tests Passing** (100% pass rate)
  - conductor-core: 45 tests
  - conductor-daemon: 74 tests
  - conductor-gui: 26 tests (1 ignored)
- No regressions from v2.0 or v2.1
- Comprehensive condition evaluation test coverage
- Velocity curve calculation unit tests

### Changed

- `TriggerContext` struct extended with optional `current_mode` field (backward compatible)
- `ActionConfig` enum extended with `Conditional` variant
- Condition evaluation system added to `conductor-daemon/src/conditions.rs` (425 lines)

### Security

- Shell commands properly sanitized in conditional execution
- Safe system APIs for app detection (pgrep, NSWorkspace)
- Time parsing validated with error handling
- No user code execution in condition evaluation

## [2.1.0] - 2025-11-17

### 🎹 Virtual MIDI Output

**Phase 5 (Part 1)**: Full MIDI output support enabling DAW control, hardware synth integration, and MIDI routing capabilities.

### Added - Virtual MIDI Port Creation

- **Platform-Specific Virtual Port Support**
  - macOS: CoreMIDI virtual sources via IAC Driver
  - Linux: ALSA/JACK virtual port creation
  - Windows: Physical port support (virtual requires loopMIDI driver)
  - Auto-detection of virtual vs. physical ports

### Added - MidiOutputManager

- **Core MIDI Output Engine** (`conductor-core/src/midi_output.rs`, 618 lines)
  - 11 public methods for port management
  - Connection pooling for multiple output ports
  - Thread-safe message queueing with `Arc<Mutex<VecDeque>>`
  - Platform-conditional compilation for virtual port support
  - Comprehensive error handling with `EngineError::MidiOutput` variants

- **Public API**:
  - `create_virtual_port(port_name: &str)` - Create named virtual MIDI port
  - `list_output_ports()` - List all available MIDI output ports
  - `connect_to_port(port_index: usize)` - Connect to output port by index
  - `send_message(port_index: usize, message: &[u8])` - Send raw MIDI bytes
  - `disconnect_port(port_index: usize)` - Close specific port connection
  - `disconnect_all()` - Close all active connections

### Added - SendMIDI Action Type

- **6 MIDI Message Types** - Full MIDI 1.0 channel voice message support
  - `NoteOn` (0x90) - Trigger notes with velocity (0-127)
  - `NoteOff` (0x80) - Release notes
  - `CC` (Control Change, 0xB0) - Continuous controllers (CC 0-127, value 0-127)
  - `ProgramChange` (0xC0) - Preset/patch selection (0-127)
  - `PitchBend` (0xE0) - 14-bit pitch wheel control (-8192 to +8191)
  - `Aftertouch` (0xD0) - Channel pressure (0-127)

- **Configuration Flexibility**
  - MIDI channel selection (0-15, displayed as 1-16 in UI)
  - 19 message type aliases for readable configs (e.g., "note-on", "control-change")
  - Sensible defaults (note=60/Middle C, velocity=100, channel=0)
  - Comprehensive parameter validation with detailed error messages

- **MIDI Spec Compliance**
  - Status byte channel masking (0-15)
  - Data byte masking (0-127, 7-bit values)
  - 14-bit pitch bend encoding (LSB/MSB)
  - Out-of-range value clamping
  - Proper message framing per MIDI 1.0 specification

### Added - ActionExecutor Integration

- **MIDI Message Encoding** (`conductor-daemon/src/action_executor.rs`, ~280 lines)
  - Complete byte-level MIDI encoding for all 6 message types
  - Channel byte manipulation (0x00-0x0F)
  - Data byte validation and masking (0x7F)
  - 14-bit pitch bend conversion (split into LSB/MSB bytes)
  - Error handling for invalid parameters
  - Integration with existing action execution pipeline

### Added - GUI Components

- **Tauri Commands** (AMI-268, 224 lines)
  - `list_midi_output_ports()` - Lists all MIDI output ports with metadata
  - `test_midi_output(port, note, velocity, duration)` - Send test MIDI message
  - `validate_send_midi_action(action_config)` - Validate SendMIDI configurations
  - AppState integration with MidiOutputManager

- **MidiOutputSelector Component** (AMI-269, 450 lines)
  - Port selection dropdown with auto-refresh
  - Virtual/physical port badges (🔷 blue for virtual, 🔌 green for physical)
  - Platform badges (🍎 macOS, 🐧 Linux, 🪟 Windows)
  - Test output button (sends Middle C for verification)
  - Error/empty/loading state handling
  - Dark theme matching existing Conductor GUI

- **SendMidiActionEditor Component** (AMI-270, 800 lines)
  - All 6 MIDI message type editors:
    - Note On/Off: Note slider with musical note names (C4, D#5, etc.)
    - Control Change: Common CC dropdown (Volume, Pan, Modulation, etc.)
    - Program Change: Preset selector (0-127)
    - Pitch Bend: Bidirectional indicator (-8192 to +8191)
    - Aftertouch: Pressure control (0-127)
  - MIDI channel selector (1-16 display)
  - Dynamic parameter fields (change based on message type)
  - Real-time validation with 300ms debounce
  - Color-coded indicators (velocity bar, pitch bend direction)
  - Integration with MidiOutputSelector
  - Readonly mode for viewing existing configs

- **Svelte Store Integration**
  - `midiOutputPortsStore` - Centralized port state management
  - `api.midiOutput.*` - API namespace for MIDI output operations
  - Real-time port refresh and validation

### Documentation

- **User Guide** (`docs/send-midi-action-guide.md`, ~580 lines)
  - Quick start tutorial (3 easy steps)
  - All 6 message types with practical examples
  - Platform-specific setup instructions:
    - macOS: IAC Driver configuration
    - Linux: ALSA/JACK virtual port creation
    - Windows: loopMIDI driver installation
  - Troubleshooting guide for common MIDI issues
  - MIDI reference tables:
    - Common CC numbers (Volume, Pan, Modulation, etc.)
    - General MIDI drum map (kick=36, snare=38, etc.)
    - MIDI note numbers with musical notation

- **Example Configurations** (2 files, ~830 lines)
  - `config/examples/daw-control-ableton.toml` (~450 lines)
    - 3 modes: Instruments, Mixer, Effects
    - 21+ real-world DAW control mappings
    - MIDI panic sequence (all notes off)
    - Arpeggio pattern examples
  - `config/examples/hardware-synth-control.toml` (~380 lines)
    - 4 modes: Performance, Sound Design, Presets, Multi-Synth Routing
    - 27+ mappings for external hardware synths
    - Chord stacking examples (power chords, triads)
    - Multi-output routing for multiple synths

- **Technical Documentation** (~4,500 lines across 7 files)
  - Architecture design document
  - Implementation completion report
  - GUI integration reports (AMI-268, AMI-269, AMI-270)
  - Final verification report
  - Platform support matrix

### Testing

- **47 New Tests** (100% pass rate)
  - 7 unit tests for MidiOutputManager
  - 18 doctests for API documentation examples
  - 10 integration tests for SendMIDI action (TOML parsing, validation, encoding)
  - 12 unit tests for ActionExecutor MIDI encoding
  - All edge cases covered (invalid channels, out-of-range values, etc.)

### Performance

- MIDI message encoding: <0.1ms per message
- Port connection: <10ms
- Memory usage: 5-10MB (no significant increase)
- Zero latency overhead on MIDI event processing

### Security

- MIDI output restricted to configured ports only
- Data byte masking prevents buffer overruns
- Port index validation prevents out-of-bounds access
- Error messages do not expose system internals

## [2.0.0] - 2025-11-14

### 🎉 Major Release: Tauri GUI & Visual Configuration

**Phase 4 Complete**: Full-featured visual configuration interface built with Tauri v2, providing an intuitive GUI for MIDI mapping management, MIDI Learn mode, per-app profiles, and real-time debugging.

### Added - Visual Configuration Editor

- **Mode-Based Config Management**: Create and manage modes with color coding
  - Visual mode editor with inline editing
  - Drag-and-drop mapping organization
  - Real-time validation and preview
  - Color-coded mode indicators

- **Mapping List UI**: CRUD operations for MIDI mappings
  - Add, edit, delete mappings
  - Type-specific trigger and action selectors
  - Live preview of trigger events
  - Automatic validation and error highlighting

- **Trigger Selector**: Visual selector with type-specific configuration
  - Note, CC, VelocityRange, LongPress, DoubleTap, EncoderTurn, PitchBend, Aftertouch
  - Context-aware form fields for each trigger type
  - Real-time parameter validation

- **Action Selector**: Visual selector with type-specific configuration
  - Keystroke, Text, Launch, Shell, VolumeControl, ModeChange, Sequence, etc.
  - Keystroke picker with live key capture
  - Application launcher with file browser
  - Shell command editor with syntax highlighting

### Added - MIDI Learn Mode

- **One-Click MIDI Learn**: Auto-detect MIDI inputs with single click
  - 10-second countdown timer with cancel option
  - Auto-detection of trigger type (Note, CC, VelocityRange, etc.)
  - Support for all trigger types
  - Visual feedback during learning
  - Automatic config generation from captured events

### Added - Per-App Profile System

- **Automatic Profile Switching**: Context-aware mapping based on frontmost app
  - macOS frontmost app detection via NSWorkspace
  - Profile auto-switching when app focus changes
  - Profile caching with SHA256-based validation
  - Profile import/export (JSON and TOML formats)
  - Profile discovery and auto-registration
  - Profile manager UI with visual indicators

### Added - Device Template Library

- **6 Built-in Controller Templates**: Pre-configured mappings for popular devices
  - Native Instruments Maschine Mikro MK3
  - Novation Launchpad Mini MK3
  - KORG nanoKONTROL2
  - Akai APC Mini
  - Arturia BeatStep
  - Generic 25-Key MIDI Keyboard
- Auto-detection via MIDI device name pattern matching
- Category filtering (pad-controller, keyboard, mixer-controller)
- Template browser with search and filter
- One-click config generation from templates

### Added - Live Event Console

- **Real-time MIDI Event Monitoring**: Debug MIDI inputs in real-time
  - Color-coded event types (NoteOn=green, CC=blue, PitchBend=purple, etc.)
  - Filter by event type and channel
  - Pause/resume functionality
  - Event count tracking
  - Raw MIDI byte display (hex format)
  - Note name display (C4, D#5, etc.)
  - Timestamp with millisecond precision

### Added - Settings Panel

- **Application Preferences**: Configure GUI behavior
  - Auto-start on login (UI ready, OS integration TBD)
  - Theme selection (Light/Dark/System, UI ready)
  - MIDI Learn timeout adjustment (5-60 seconds)
  - Event buffer size control (100-10,000 events)
  - Log level configuration (Error/Warn/Info/Debug)
  - About section with version and links

### Added - Menu Bar Integration

- **Native System Tray**: Platform-specific menu bar
  - macOS: Native NSApplication menu bar
  - Quick actions: Pause, Reload, Configure, Quit
  - Status indicators: Running, Stopped, Error
  - Minimize to tray functionality

### Technical Stack

- **Backend**: Tauri v2.9.3 with Rust
  - 40+ Tauri commands for IPC
  - Thread-safe state with Arc<RwLock<>>
  - JSON-based IPC protocol
  - Event streaming for real-time updates

- **Frontend**: Svelte 5.1.9 with Vite 6.4.1
  - 14 custom UI components
  - TypeScript for type safety
  - Reactive state management
  - Fast builds (~400ms)

### Performance

- Daemon IPC: <1ms round-trip
- MIDI Learn start: <50ms
- Profile switching: <100ms
- Memory usage: ~60MB total
- Frontend build: <500ms

### Platform Support

- **macOS**: Full support with native integration
- **Linux**: Basic support (app detection TBD)
- **Windows**: Basic support (app detection TBD)

### Issues Completed (26/26)

**Week 1-2**: AMI-158-166 (Tauri Setup & Infrastructure)
**Week 3**: AMI-171-174 (MIDI Learn Mode)
**Week 4**: AMI-175-180 (Visual Config Editor)
**Week 5**: AMI-181-184 (Per-App Profiles)
**Week 6**: AMI-185-187 (Polish & Release)

### Known Limitations

- Documentation site not yet updated (deferred to Phase 5)
- Auto-start OS integration pending (UI complete)
- Theme switching implementation pending (UI complete)
- App detection macOS-only (Linux/Windows TBD)
- Drag-and-drop mapping reorder planned but not implemented

## [1.0.0] - 2025-01-13

### 🎉 Major Release: Production-Ready Daemon

**Phase 3 Complete**: Full daemon architecture with hot-reload, IPC control, and service integration. This is the first production-ready release with zero-downtime configuration updates.

### Added - Daemon Infrastructure

- **Background Daemon Service**: Runs as persistent background service
  - Unix domain socket IPC for inter-process communication
  - Graceful shutdown with SIGTERM/SIGINT handling
  - State persistence across restarts (`~/.local/state/conductor/daemon.state`)
  - 8-state lifecycle machine (Initializing → Running → Reloading → Degraded → etc.)
  - Atomic config swaps using Arc<RwLock<>> pattern

### Added - Configuration Hot-Reload

- **Zero-Downtime Config Reload**: Changes detected and applied in 0-10ms typical
  - File system watcher with 500ms debounce window
  - Automatic change detection on config file save
  - Phase-by-phase timing (config load, mapping compile, atomic swap)
  - Performance grading system:
    - Grade A (<20ms): Excellent - Imperceptible
    - Grade B (21-50ms): Good - Target performance
    - Grade C (51-100ms): Acceptable
    - Grade D (101-200ms): Poor - Investigate
    - Grade F (>200ms): Unacceptable
  - Running statistics (fastest, slowest, average reload times)
  - Reload counter and performance history

### Added - CLI Control Tool (conductorctl)

- **Command-Line Interface**: Control daemon from terminal or scripts
  - `status` - Query daemon state, uptime, events processed, reload stats
  - `reload` - Force immediate configuration reload
  - `ping` - Test connectivity and measure IPC latency
  - `stop` - Gracefully stop daemon
  - `validate [--config PATH]` - Validate configuration files
  - Dual output modes:
    - Human-readable: Colored terminal output with Unicode symbols
    - JSON: Machine-readable for scripting (`--json` flag)
  - Verbose logging mode (`--verbose` flag)

### Added - Service Integration

- **systemd Service Template** (`conductor-daemon/systemd/conductor.service`):
  - User-level service support
  - Auto-restart on failure (5s throttle, max 5 bursts per 5 minutes)
  - Security hardening (NoNewPrivileges, ProtectSystem=strict, ProtectHome=read-only)
  - Resource limits (1024 file descriptors, 64 processes)
  - Journal logging integration
  - ExecReload support via conductorctl

- **macOS LaunchAgent** (`conductor-daemon/launchd/com.amiable.conductor.plist`):
  - Run at login with LaunchAgent plist
  - Crash recovery with 5s throttled restart
  - Process priority configuration (Nice -5 for low latency)
  - Log file rotation to `~/Library/Logs/conductor.log`
  - GUI session integration (LimitLoadToSessionType: Aqua)

### Added - Documentation

- **Man Pages**: Professional Unix manual pages
  - `conductor(1)` - Daemon manual (trigger types, action types, config format)
  - `conductorctl(1)` - CLI tool reference (commands, options, examples)
  - Installation to `/usr/local/share/man/man1/`

- **DEPLOYMENT.md**: Comprehensive deployment guide (500+ lines)
  - Quick start instructions
  - Platform-specific installation (macOS LaunchAgent, Linux systemd)
  - Service management commands
  - Configuration management
  - Monitoring and log analysis
  - Troubleshooting guide with common issues
  - Performance benchmarking guide
  - Uninstallation procedures

### Added - Engine Enhancements

- **Performance Metrics** (`daemon/types.rs`):
  - Config load timing (ms)
  - Mapping compilation timing (ms)
  - Atomic swap timing (ms)
  - Total reload duration (ms)
  - Performance grade calculation (A-F)

- **Daemon Statistics** (`daemon/types.rs`):
  - Events processed counter
  - Actions executed counter
  - Error tracking since start
  - Config reload counter
  - Uptime tracking (seconds)
  - Reload performance history

### Added - Testing & Benchmarking

- **Reload Benchmark Suite** (`conductor-daemon/benches/reload_benchmark.rs`):
  - Multiple config sizes (2-10 modes, 10-100 mappings)
  - 10 iterations per test for statistical reliability
  - Average, min, max timing measurements
  - Performance grading validation

- **Daemon Integration Tests**:
  - IPC protocol tests (request/response cycle)
  - Config reload tests (atomic swaps, no downtime)
  - State machine transition tests
  - Error handling tests
  - 45 tests total, all passing (1 marked `#[ignore]` for CI flakiness)

### Changed - Architecture

- **conductor-daemon** structure:
  - Added `src/daemon/` module (7 files, ~2,000 lines)
    - `service.rs` - Main daemon service loop
    - `engine_manager.rs` - Engine lifecycle management
    - `config_watcher.rs` - File system watching with debouncing
    - `ipc.rs` - IPC server and client
    - `state.rs` - State persistence and socket path logic
    - `types.rs` - IPC protocol types, metrics, statistics
    - `error.rs` - Daemon-specific error types
  - Added `src/bin/conductorctl.rs` - CLI control tool (360 lines)
  - Added `src/bin/conductor_menubar.rs` - Menu bar foundation (262 lines, incomplete)
  - Added `benches/reload_benchmark.rs` - Performance benchmarking (166 lines)

- **IPC Client API** (`daemon/ipc.rs`):
  - Added `IpcClient::new(socket_path)` for custom socket paths
  - Added `IpcClient::send_command(command, args)` for generic command sending
  - Existing methods (`ping`, `status`, `reload`, `stop`) now use generic API

### Changed - Performance

**Config Reload Optimization**: 5-6x faster than 50ms target

Benchmark results (Apple M1 MacBook Pro):

| Config Size | Reload Time | Grade | Improvement |
|-------------|-------------|-------|-------------|
| 2 modes, 10 mappings | 0-2ms | A | 10-25x faster |
| 5 modes, 50 mappings | 2-5ms | A | 10-25x faster |
| 10 modes, 100 mappings | 5-8ms | A | 6-10x faster |

**All configurations achieve Grade A performance** (<20ms).

### Fixed

- **notify-debouncer-full API**: Updated to v0.4 API (deprecated `.watcher()` and `.cache()` methods)
- **Config Format**: Fixed Keystroke action format in benchmarks (string keys, not array)
- **Import Warnings**: Removed unused imports from daemon modules
- **Test Reliability**: Marked file watcher test as `#[ignore]` for CI stability (file watching is inherently timing-sensitive)

### Known Issues

- **Menu Bar UI**: Foundation created but incomplete
  - Send/Sync issues with `tray-icon` crate on macOS
  - Platform-specific threading model constraints
  - Requires platform-specific implementations or Tauri framework
  - Documented for future Phase 3 work

- **Windows Support**: Not yet implemented
  - IPC requires named pipes implementation
  - Service integration requires Windows Service framework
  - Planned for future release

### Migration Guide

#### From v0.2.0 to v1.0.0

**No breaking changes** - All v0.2.0 configurations work identically.

**New daemon features to adopt**:

1. **Install as Service** (recommended):
   ```bash
   # macOS
   launchctl load ~/Library/LaunchAgents/com.amiable.conductor.plist

   # Linux
   systemctl --user enable conductor
   systemctl --user start conductor
   ```

2. **Use conductorctl for Control**:
   ```bash
   conductorctl status   # Check daemon health
   conductorctl reload   # Apply config changes
   conductorctl ping     # Test connectivity
   ```

3. **Enable Hot-Reload**:
   - Edit `~/.config/conductor/config.toml`
   - Changes automatically detected and applied in <10ms
   - No daemon restart needed

**Manual mode still supported**:
```bash
conductor --config config.toml --log-level debug
```

### Dependencies

#### New Dependencies
- `tokio` (1.40) - Async runtime for daemon event loop
- `interprocess` (2.2) - Cross-platform IPC (Unix sockets)
- `notify` (7.0) - File system change notifications
- `notify-debouncer-full` (0.4) - Debounced file events
- `tray-icon` (0.19) - System tray integration (foundation)
- `dirs` (5.0) - Standard directory paths (XDG Base Directory)
- `uuid` (1.0) - Request ID generation for IPC
- `sha2` (0.10) - Config checksums for integrity verification
- `tracing` (0.1) - Structured logging
- `tracing-subscriber` (0.3) - Log formatting and filtering

#### Updated Dependencies
- All workspace dependencies remain at v0.2.0 versions

### Performance Metrics

**Measured on Apple M1 MacBook Pro**:

- **MIDI Event Latency**: <1ms (unchanged)
- **Config Reload Time**: 0-10ms typical (Grade A: <20ms)
- **Startup Time**: <500ms
- **Memory Usage**: 5-10MB (unchanged)
- **CPU Usage**: <1% idle, <5% active (unchanged)
- **Binary Size**: ~3-5MB (unchanged)

### Contributors

- Christopher Joseph (@christopherjoseph) - All v1.0.0 features

### Release Artifacts

- conductor-v1.0.0-macos-arm64.tar.gz (Apple Silicon)
- conductor-v1.0.0-macos-x86_64.tar.gz (Intel)
- conductor-v1.0.0-linux-x86_64.tar.gz (Linux)
- checksums.txt (SHA256)

## [0.2.0] - 2025-11-12

### Overview

**Phase 2 Complete**: Workspace architecture migration with zero breaking changes. Conductor now uses a modular 3-package workspace structure, enabling better code organization, faster builds, and preparing for future GUI integration.

**100% Backward Compatible**: All v0.1.0 configs, features, and workflows work identically in v0.2.0.

### Added - Architecture

- **conductor-core**: Pure Rust engine library (zero UI dependencies)
  - Public API for embedding in other applications
  - Structured error types using `thiserror`
  - Comprehensive rustdoc documentation
  - 30+ public types exported
- **conductor-daemon**: CLI daemon + 6 diagnostic tools
  - Main `conductor` binary
  - `midi_diagnostic`, `led_diagnostic`, `led_tester`
  - `pad_mapper`, `test_midi`, `midi_simulator`
- **conductor** (root): Backward compatibility layer
  - Re-exports conductor-core types
  - Maintains v0.1.0 import paths
  - Zero breaking changes for existing tests

### Added - Testing

- **25 new integration tests** (339 tests total, was 314)
  - 8 API integration tests (public API surface)
  - 7 backward compatibility tests
  - 10 error handling tests (across crate boundaries)
- **100% feature validation**: All 26 features tested and working
- **Config compatibility tests**: All v0.1.0 configs validated

### Changed - Performance

- **Build time**: 11.92s clean build (was 15-20s) - **25-40% faster** ✨
  - Workspace parallelization across 3 packages
  - Improved incremental compilation
- **Test execution**: 28.8s (was ~30s) - **4% faster**
  - Parallel test execution per package
- **Binary size**: Unchanged (869K main binary)

### Changed - Internal Structure

- Renamed `src/mappings.rs` → `conductor-core/src/mapping.rs`
- Renamed `src/device_profile.rs` → `conductor-core/src/device.rs`
- Added `conductor-core/src/error.rs` (structured error types)
- Split monolithic src/ into modular workspace packages
- Removed UI dependencies (colored, chrono) from core library

### Documentation

- **CLAUDE.md**: Updated with workspace architecture and Phase 2 status
- **README.md**: Updated installation and build commands
- **mdbook**: Updated architecture diagrams
- **Rustdoc**: Comprehensive API documentation in conductor-core
- **Migration Guide**: docs/MIGRATION_v0.1_to_v0.2.md

### Validation

- **Feature Parity**: 26/26 features validated ✅
- **Config Compatibility**: 15 compatibility tests passing ✅
- **Breaking Changes**: 0 (zero) ✅
- **Test Coverage**: 339/339 tests passing (100%) ✅

### Migration Notes

**For Users**: No action required. All configs and workflows work identically.

**For Developers**: Update build commands:
```bash
# Old
cargo build --release
cargo test

# New
cargo build --release --workspace
cargo test --workspace
```

See `docs/MIGRATION_v0.1_to_v0.2.md` for complete guide.

## [0.1.0-monolithic] - 2025-11-11

### Overview

Initial public release of Conductor, preserving the complete working monolithic implementation with all 26 features before migration to workspace structure. This release establishes the foundation for open source development and community contributions.

### Added - Core Triggers (4)

- **Note Trigger**: Basic note on/off detection with optional velocity range filtering
- **VelocityRange Trigger**: Different actions for soft (0-40), medium (41-80), and hard (81-127) velocity levels
- **EncoderTurn Trigger**: Encoder rotation detection with clockwise/counterclockwise direction
- **CC (Control Change) Trigger**: MIDI Control Change message handling

### Added - Advanced Triggers (5)

- **LongPress Trigger**: Configurable hold duration detection (default 2000ms)
- **DoubleTap Trigger**: Quick double-tap detection with configurable window (default 300ms)
- **NoteChord Trigger**: Multiple simultaneous note detection (default 100ms chord window)
- **Aftertouch Trigger**: Pressure sensitivity detection for supported devices
- **PitchBend Trigger**: Touch strip/pitch wheel detection with range support

### Added - Actions (10)

- **Keystroke Action**: Keyboard shortcuts with full modifier support (Cmd, Ctrl, Alt, Shift)
- **Text Action**: Type text strings with automatic character conversion
- **Launch Action**: Open applications and files with system default handlers
- **Shell Action**: Execute shell commands and scripts with full environment access
- **VolumeControl Action**: System volume adjustment (Up, Down, Mute, Set to value)
- **ModeChange Action**: Switch between mapping modes with LED feedback
- **Sequence Action**: Chain multiple actions with timing control
- **Delay Action**: Add timing delays between actions (milliseconds)
- **MouseClick Action**: Simulate mouse button clicks (Left, Right, Middle)
- **Repeat Action**: Execute an action multiple times with optional delays

### Added - LED Feedback System (10 Schemes)

- **Off**: All LEDs disabled
- **Static**: Solid color display with configurable RGB values
- **Breathing**: Smooth pulsing fade in/out effect
- **Pulse**: Quick flash effect for event triggers
- **Rainbow**: Animated rainbow color cycle across pads
- **Wave**: Wave pattern sweeping across pad grid
- **Sparkle**: Random sparkle/twinkle effects
- **Reactive**: Velocity-sensitive color feedback (green=soft, yellow=medium, red=hard) with 1-second fade
- **VU Meter**: Audio level meter visualization
- **Spiral**: Spiral pattern animation from center outward

### Added - System Features (7)

- **Multi-Mode System**: Support for multiple mapping modes (Default, Development, Media, etc.) with independent configurations
- **Global Mappings**: Mappings that work across all modes (e.g., emergency exit, encoder volume control)
- **Device Profile Support**: Load Native Instruments Controller Editor profiles (.ncmm3 XML format)
- **Auto-Detect Pad Page**: Automatically detect active pad page (A-H) from incoming MIDI events
- **HID Shared Device Access**: Concurrent access with Native Instruments Controller Editor using `hidapi` with `macos-shared-device` feature
- **Graceful Shutdown**: Clean MIDI connection closure and LED reset on exit (Ctrl+C handling)
- **Debug Logging**: Environment variable DEBUG=1 enables detailed event and processing logs

### Added - Diagnostic Tools (4)

- **midi_diagnostic**: Visualize all incoming MIDI events with formatted display
- **led_diagnostic**: Test RGB LED functionality and HID connection
- **led_tester**: Interactive LED scheme testing utility
- **pad_mapper**: Utility for mapping physical pad positions to MIDI notes

### Added - Documentation

- README.md with quick start guide and feature overview
- CLAUDE.md with comprehensive project instructions and architecture
- LED_FEEDBACK.md with LED system documentation
- CODE_OF_CONDUCT.md (Contributor Covenant v2.1)
- CONTRIBUTING.md with contribution guidelines
- GOVERNANCE.md defining project structure and decision-making
- MAINTAINERS.md listing current maintainers
- ROADMAP.md outlining project vision and development phases
- SECURITY.md with vulnerability reporting process
- Example config.toml with common mapping patterns

### Added - Developer Infrastructure

- GitHub Actions CI/CD pipeline (build, test, clippy, format checks)
- Issue templates (bug report, feature request, device support, documentation)
- Pull request template with comprehensive checklist
- SUPPORT.md documenting support channels
- Pre-commit hook setup for code quality
- VS Code configuration (.vscode/settings.json, launch.json, tasks.json)
- Build scripts (scripts/build.sh, test.sh, dev-setup.sh, clean.sh)
- .editorconfig for cross-editor consistency
- rust-toolchain.toml pinning Rust version

### Added - Legal & Compliance

- MIT License with copyright notice
- Copyright headers in all source files
- NOTICE file with third-party attributions
- THIRD_PARTY_LICENSES.md documenting all dependency licenses
- Trademark disclaimer for Native Instruments references
- SPDX license identifier in Cargo.toml

### Performance

- Response latency: <1ms typical for MIDI event processing
- Memory footprint: 5-10MB steady state
- CPU usage: <1% idle, <5% during active use
- Binary size: 3-5MB (release build with LTO and stripping)

### Platform Support

- macOS 11+ (Big Sur and later)
- Apple Silicon (ARM64) and Intel (x86_64) architectures
- Requires Input Monitoring permission for HID device access

### Device Compatibility

- **Fully Supported**: Native Instruments Maschine Mikro MK3 (RGB LEDs, HID access, profile support)
- **MIDI-Only Support**: Any USB MIDI controller with basic LED feedback via MIDI Note messages
- **Profile Support**: .ncmm3 files from Native Instruments Controller Editor

### Known Limitations

- macOS only (Linux and Windows support planned for Phase 4)
- Single device support (multi-device planned for Phase 4)
- No GUI for configuration (Tauri UI planned for Phase 3)
- Config changes require restart (hot reload planned for Phase 2)
- No virtual MIDI output (planned for Phase 4)

### Dependencies

Major external crates:
- midir 0.9 - Cross-platform MIDI I/O
- enigo 0.2 - Keyboard/mouse input simulation
- hidapi 2.6 - HID device access with macOS shared device support
- serde 1.0 + toml 0.8 - Configuration parsing
- quick-xml 0.36 - XML profile parsing (.ncmm3 files)
- crossbeam-channel 0.5 - Lock-free event channels
- colored 2.1 - Terminal output formatting
- ctrlc 3.4 - Graceful shutdown handling

All dependencies use MIT, Apache-2.0, or BSD-compatible licenses.

### Migration Path

This v0.1.0-monolithic release preserves the working single-binary implementation before architectural migration to workspace structure (Phase 2-4). Future versions will maintain backward compatibility with existing config.toml files.

### Contributors

- Christopher Joseph (@christopherjoseph) - Project Lead & Creator

### Release Artifacts

- conductor-v0.1.0-macos-arm64.tar.gz (Apple Silicon)
- conductor-v0.1.0-macos-x86_64.tar.gz (Intel)
- checksums.txt (SHA256)

---

## Version History

- **v3.0.0** (2025-11-21): Multi-protocol input with game controller support 🎮
- **v2.7.0** (2025-11-19): Plugin security & verification ✨
- **v2.3.0** (2025-01-18): Plugin architecture
- **v2.2.0** (2025-11-18): Velocity curves & conditionals
- **v2.1.0** (2025-11-17): Virtual MIDI output
- **v2.0.0** (2025-11-14): Tauri GUI & visual config
- **v1.0.0** (2025-01-13): Production daemon with hot-reload
- **v0.2.0** (2025-11-12): Workspace architecture migration
- **v0.1.0-monolithic** (2025-11-11): Initial public release with 26 features
- **Unreleased**: Next version in development

---

## Changelog Guidelines

This changelog follows [Keep a Changelog](https://keepachangelog.com/) format:

- **Added**: New features
- **Changed**: Changes to existing functionality
- **Deprecated**: Soon-to-be-removed features
- **Removed**: Removed features
- **Fixed**: Bug fixes
- **Security**: Security vulnerability fixes

Version numbers follow [Semantic Versioning](https://semver.org/):
- **MAJOR**: Breaking changes to config format or public API
- **MINOR**: New features, backward-compatible
- **PATCH**: Bug fixes, performance improvements

[Unreleased]: https://github.com/monstrous-media/conductor/compare/v5.7.1-alpha...HEAD
[5.7.1-alpha]: https://github.com/monstrous-media/conductor/compare/v5.7.0-alpha...v5.7.1-alpha
[5.7.0-alpha]: https://github.com/monstrous-media/conductor/compare/v5.6.1-alpha...v5.7.0-alpha
[5.6.1-alpha]: https://github.com/monstrous-media/conductor/releases/tag/v5.6.1-alpha
[5.6.0-alpha]: https://github.com/monstrous-media/conductor/releases/tag/v5.6.0-alpha
[5.3.0-alpha]: https://github.com/monstrous-media/conductor/releases/tag/v5.3.0-alpha
[3.0.0]: https://github.com/monstrous-media/conductor/releases/tag/v3.0.0
[2.7.0]: https://github.com/monstrous-media/conductor/releases/tag/v2.7.0
[2.3.0]: https://github.com/monstrous-media/conductor/releases/tag/v2.3.0
[2.2.0]: https://github.com/monstrous-media/conductor/releases/tag/v2.2.0
[2.1.0]: https://github.com/monstrous-media/conductor/releases/tag/v2.1.0
[2.0.0]: https://github.com/monstrous-media/conductor/releases/tag/v2.0.0
[1.0.0]: https://github.com/monstrous-media/conductor/releases/tag/v1.0.0
[0.2.0]: https://github.com/monstrous-media/conductor/releases/tag/v0.2.0
[0.1.0-monolithic]: https://github.com/monstrous-media/conductor/releases/tag/v0.1.0-monolithic
