# Architecture Decision Records

Conductor's design is anchored by numbered ADRs written during its original
(pre-open-source) development. Code comments and test names reference them
(`ADR-031 § 5.4`, `ADR-045 D1`, …) as stable decision anchors. The curated
public corpus is still being prepared for publication; until it lands here,
this index maps the most-referenced numbers to their subject so the anchors
in the code are readable:

| ADR | Subject |
|---|---|
| ADR-009 | OSC protocol support (`osc` cargo feature) |
| ADR-025 | Program-change context switching (`PcContextSwitch`) and branch ordering |
| ADR-027 | Daemon security posture: IPC trust, peer classification, minimal surface |
| ADR-031 | Signal routing graph: `[[endpoints]]` / `[[routes]]`, routing MCP tools |
| ADR-035 | Endpoint-based config model (removed the legacy `[device]` / `[[bindings]]` / `[[connectors]]` forms) |
| ADR-036 | Route engine: dispatch, evaluation priority (`mappings > routes`), tracing |
| ADR-039 | Cross-protocol lifecycle parity (see [`../cross-protocol-parity/lifecycle-coverage.md`](../cross-protocol-parity/lifecycle-coverage.md)) |
| ADR-045 | Open-core tier boundary: the `mcp` / `llm-executor` / `mcp-write` / `audit-db` feature matrix |
| ADR-046 | Public repository decomposition (this repo's existence) |

When the corpus is published, each row becomes a link to the full record.
