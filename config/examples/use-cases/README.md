# Use-case templates

Ready-to-use Conductor profiles behind the demos on <https://getconductor.app/use-cases>.

| File | Controller | What it does |
|---|---|---|
| `producer.maschine-mikro.toml` | Maschine Mikro MK3 | DAW transport, record-arm on a hard hit, output volume on the encoder, Edit/Mixdown modes |
| `streamer.apc-mini.toml` | Akai APC mini mk2 | OBS scene hotkeys, `afplay` soundboard, desktop volume on fader 9, long-press "stop streaming" |
| `developer.launchpad-mini.toml` | Launchpad Mini [MK3] | git/test commands typed into the terminal, command palette, sandboxed deploy gated to Mon–Thu behind a 3 s hold |
| `gamer.gamepad.toml` | Any SDL gamepad | Launch apps, app switcher chord, keyboard-free focus navigation, mute toggle |
| `claude-code.launchpad-mini.toml` | Launchpad Mini [MK3] | Claude Code: approve/deny (terminal-focus guarded), permission-mode cycling, slash commands, `/clear` behind a long-press |
| `codex.lpd8.toml` | Akai LPD8 mk2 | Codex CLI: approve/approve-for-session/decline, reasoning effort on a knob, voice toggle, `/new` behind a long-press |

Install one:

```bash
cp codex.lpd8.toml ~/.config/conductor/profiles/codex.toml
conductorctl profile validate ~/.config/conductor/profiles/codex.toml
conductorctl profile switch codex
```

Each file's header lists the controller mode and note/CC numbers it expects. If yours differ,
run `conductorctl events` and press the pads to read them.

`conductor-core/tests/example_configs_test.rs` loads, validates and round-trips every file
here, so a schema change that breaks a template fails CI.
