# Conductor developer documentation

Reference and development docs for the Conductor engine. These live in the
code repository so they version with the code they describe.

| | |
|---|---|
| [`reference/`](reference/) | Config file schema, trigger and action types, CLI commands, MCP tools, gamepad API, LED system |
| [`development/`](development/) | Engine architecture, testing, plugin and WASM plugin development, LLM/MCP integration, agent skills, consumer contract |
| [`architecture/`](architecture/) | Architecture overview and ADR index |
| [`llm-reference.md`](llm-reference.md) | The canonical L1 reference given to LLM agents driving Conductor over MCP |
| [`cross-protocol-parity/`](cross-protocol-parity/) | Generated protocol lifecycle coverage matrix (regenerate via `LIFECYCLE_REGEN=1 cargo test -p conductor-daemon --test protocol_lifecycle_test`) |

Development setup and contribution workflow are covered in the root
[CONTRIBUTING.md](../CONTRIBUTING.md).

User-facing documentation — installation, guides, tutorials, troubleshooting
and configuration walkthroughs — lives at **<https://getconductor.dev>**
(source: [monstrous-media/conductor-docs](https://github.com/monstrous-media/conductor-docs)).
