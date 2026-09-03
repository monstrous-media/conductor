# Architecture Overview

## Current Architecture (v4.24.0)

Conductor uses a **Cargo workspace architecture** with four packages and a lock-free event processing pipeline.

### Workspace Packages

1. **conductor-core** (~5,200 lines): Pure Rust engine library (zero UI dependencies)
2. **conductor-daemon** (~5,200 lines): Background service, CLI daemon + 10 diagnostic tools
3. **conductor-gui** (Tauri v2): Visual configuration interface
4. **conductor** (root): Backward compatibility layer for existing tests

### System Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  conductor-gui (Tauri v2)                                   │
│  - Visual config editor, MIDI Learn, Chat UI                │
│  - Real-time daemon sync via IPC                            │
└──────────────────────┬──────────────────────────────────────┘
                       │ IPC (JSON over Unix socket)
┌──────────────────────▼──────────────────────────────────────┐
│  conductor-daemon (Background Service)                      │
├─────────────────────────────────────────────────────────────┤
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │ Engine       │  │ Input        │  │ Action           │  │
│  │ Manager      │  │ Manager      │  │ Executor         │  │
│  │ (ArcSwap)   │  │ (Multi-Dev)  │  │ (enigo)          │  │
│  └──────────────┘  └──────────────┘  └──────────────────┘  │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │ IPC Server   │  │ Config       │  │ MCP Server       │  │
│  │              │  │ Watcher      │  │ (JSON-RPC 2.0)   │  │
│  └──────────────┘  └──────────────┘  └──────────────────┘  │
└──────────────────────┬──────────────────────────────────────┘
                       │ imports
┌──────────────────────▼──────────────────────────────────────┐
│  conductor-core (Pure Rust Engine Library)                   │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │ Config      │  │ Compiled     │  │ Rule             │  │
│  │ Loader      │──▶│ RuleSet     │◀─│ Compiler         │  │
│  └─────────────┘  └──────────────┘  └──────────────────┘  │
│  ┌─────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │ Event       │  │ Mapping      │  │ Device           │  │
│  │ Processor   │  │ Engine       │  │ Identity         │  │
│  └─────────────┘  └──────────────┘  └──────────────────┘  │
│  ┌─────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │ Actions     │  │ Feedback     │  │ Plugin           │  │
│  │ Types       │  │ System       │  │ System           │  │
│  └─────────────┘  └──────────────┘  └──────────────────┘  │
│                                                             │
│  Zero UI deps • Lock-free rule matching • Public API       │
└─────────────────────────────────────────────────────────────┘
```

### Event Processing Pipeline

The system uses a four-stage event processing architecture with lock-free rule matching (v4.21.0):

```
┌──────────────────┐     ┌──────────────────┐
│   MIDI Device    │     │  Game Controller │
└────────┬─────────┘     └────────┬─────────┘
         │ Raw bytes              │ gilrs events
         ▼                        ▼
┌──────────────────────────────────────────┐
│  InputManager (Unified Input Layer)      │
│  - Multi-device support (v4.20.0)        │
│  - Hot-plug detection (v4.22.0, 5s)      │
│  - DeviceEvent<InputEvent> tagging       │
│  - Per-device EventProcessor (DashMap)   │
└────────────────┬─────────────────────────┘
                 │ InputEvent
                 ▼
┌──────────────────────────────────────────┐
│  EventProcessor (State Machine)          │
│  - Velocity levels (Soft/Med/Hard)       │
│  - Long press hold detection (2000ms)    │
│  - Double-tap (300ms window)             │
│  - Chord buffering (50ms window)         │
│  - Encoder direction detection           │
└────────────────┬─────────────────────────┘
                 │ ProcessedEvent
                 ▼
┌──────────────────────────────────────────┐
│  CompiledRuleSet (Lock-Free, v4.21.0)   │
│  - ArcSwap: wait-free reads (~1ns)      │
│  - Per-device HashMap indexing (O(1))    │
│  - Priority: device > any-device > global│
│  - Atomic config reload (never blocks)   │
└────────────────┬─────────────────────────┘
                 │ Action
                 ▼
┌──────────────────────────────────────────┐
│  ActionExecutor (enigo)                  │
│  - Keystroke, Shell, Launch, Volume      │
│  - MIDI output, Plugin, Conditional      │
└──────────────────────────────────────────┘
```

### Core Components

#### main.rs
- **Entry Point**: CLI argument parsing, device enumeration
- **MIDI Connection**: Sets up `midir` input callbacks
- **Event Loop**: Coordinates event processing thread
- **Mode Management**: Tracks current mode via `AtomicU8`
- **Device Profiles**: Loads `.ncmm3` profiles for pad mapping

#### config.rs
- **Configuration Types**: `Config`, `Mode`, `Mapping`, `Trigger`, `ActionConfig`
- **TOML Parsing**: Loads and validates `config.toml`
- **Serialization**: Supports saving default configs

#### event_processor.rs
- **State Machine**: Transforms raw MIDI → ProcessedEvent
- **Timing Detection**: Long press, double-tap, hold threshold
- **Velocity Ranges**: Soft (0-40), Medium (41-80), Hard (81-127)
- **Chord Detection**: Buffers notes within 50ms window
- **Encoder Direction**: Detects clockwise/counterclockwise rotation

#### mappings.rs + rule_set.rs + rule_compiler.rs
- **CompiledRuleSet** (v4.21.0): Immutable compiled rules with per-device HashMap indexing
- **RuleCompiler**: Transforms `Config` → `CompiledRuleSet` off the hot path
- **MappingEngine**: Backward-compatible matching (used by MCP tools)
- **Lock-Free Hot Path**: `ArcSwap<CompiledRuleSet>` for wait-free reads (~1ns)
- **Mode System**: Global mappings + per-mode mappings
- **Device-Aware Matching**: Device-specific rules checked before any-device rules
- **Trigger Matching**: Note, CC, Chord, Velocity, LongPress, DoubleTap, Encoder, Gamepad

#### actions.rs + dispatch.rs
- **Action Types**: Keystroke, Text, Launch, Shell, Sequence, VolumeControl, ModeChange, SendMidi, MidiForward, Conditional, Plugin
- **DispatchResult** (v4.25.0): Structured return type from action execution — `DispatchOutcome::Completed`, `DispatchOutcome::ModeChangeRequested`, or `DispatchError`
- **ModeChange Fix** (v4.25.0): ModeChange action now atomically updates `ArcSwap<ModeState>` instead of printing to stderr

#### transform.rs
- **MidiTransform** (v4.25.0): Real-time MIDI message transformation pipeline
- **Transform Pipeline**: curve → scale → offset → invert → clamp(0-127)
- **Value Curves**: Linear, Logarithmic, Exponential
- **Safety**: Rejects SysEx/system messages, handles NaN/Inf, preserves NoteOn vel=0 semantic

#### feedback.rs
- **Trait Abstraction**: `PadFeedback` trait for LED control
- **Device Factory**: Creates HID (Mikro MK3) or MIDI feedback
- **Lighting Schemes**: Off, Static, Breathing, Pulse, Rainbow, Wave, Sparkle, Reactive, VuMeter, Spiral

#### mikro_leds.rs
- **HID Control**: Direct RGB LED control via `hidapi`
- **Maschine Mikro MK3**: Full RGB control, 16 pads
- **Shared Device Mode**: macOS shared access (runs alongside NI Controller Editor)
- **Effects**: Velocity feedback, mode colors, reactive lighting

#### midi_feedback.rs
- **Standard MIDI**: Fallback LED control via MIDI Note messages
- **Generic Devices**: Works with any MIDI device with LED support
- **Limited Features**: On/off only, no color control

#### device_profile.rs
- **NI Profile Parser**: Parses `.ncmm3` XML from Controller Editor
- **Pad Mapping**: Maps physical pad positions → MIDI notes
- **Page Support**: Handles pad pages (A-H) for multi-page controllers
- **Auto-Detection**: Detects active pad page from incoming MIDI

### Key Design Patterns

#### Mode System
Multiple modes (Default, Development, Media, etc.) allow different mapping sets. Each mode has:
- Distinct name and color theme
- Mode-specific mappings
- LED color scheme

Mode changes triggered by:
- Encoder rotation (CC messages)
- Specific pad combinations
- Programmatic mode switching

#### Global vs Mode Mappings
- **Global Mappings**: Active in all modes (e.g., emergency exit, encoder volume)
- **Mode Mappings**: Scoped to specific modes (e.g., dev tools in Development mode)

#### Profile-Based Note Mapping
Supports loading NI Controller Editor profiles (`.ncmm3`) to:
- Map physical pad positions to MIDI notes
- Handle different pad pages (A-H)
- Auto-detect active pad page from events
- Support custom controller configurations

#### LED Feedback System
Trait-based abstraction (`PadFeedback`) supports:
- **HID Devices**: Full RGB control (Maschine Mikro MK3)
- **MIDI Devices**: Basic on/off via MIDI Note messages

**Reactive Mode**: LEDs respond to velocity:
- Soft (green), Medium (yellow), Hard (red)
- Fade-out 1 second after release

**Mode Colors**: Distinct color themes per mode
- Mode 0: Blue
- Mode 1: Green
- Mode 2: Purple

### Threading and Concurrency

```
┌──────────────────────┐
│   Tokio Runtime      │
│   - Engine Manager   │
│   - IPC Server       │
│   - Config Watcher   │
└──────────┬───────────┘
           │
           ├──────────────────────┬───────────────────┐
           │                      │                   │
           ▼                      ▼                   ▼
┌──────────────────┐   ┌──────────────────┐   ┌──────────────┐
│ MIDI Callbacks   │   │ Gamepad Polling  │   │ LED Effect   │
│ - Raw bytes      │   │ - 120Hz gilrs    │   │ - Background │
│ - mpsc send      │   │ - Blocking thr.  │   │ - Lighting   │
└──────────┬───────┘   └──────────┬───────┘   └──────────────┘
           │                      │
           └──────────┬───────────┘
                      ▼
           ┌──────────────────────┐
           │ Event Processing     │
           │ - ArcSwap::load()   │  (wait-free, ~1ns)
           │ - Rule matching      │
           │ - Action execution   │
           └──────────────────────┘
```

**Threading Model (v4.21.0)**:
- **Tokio Runtime**: Async tasks for engine manager, IPC, config watching
- **MIDI Callbacks**: Lock-free (via `mpsc` channel)
- **Gamepad Polling**: Dedicated blocking thread (gilrs is sync)
- **LED Effects**: Optional background thread
- **MIDI Watcher**: Persistent thread with CoreMIDI CFRunLoop (macOS)

**Lock-Free State (v4.21.0)**:
- `Arc<ArcSwap<CompiledRuleSet>>`: Wait-free rule reads on hot path
- `Arc<ArcSwap<ModeState>>`: Atomic mode switching
- `DashMap<DeviceId, EventProcessor>`: Sharded per-device state (v4.20.0)
- `AtomicU64` for rule set version tracking
- Config reload: `spawn_blocking` for CPU-intensive compilation, then `ArcSwap::store()`

### Performance Characteristics

- **Event Latency**: <1ms typical
- **Rule Matching**: ~1ns (wait-free ArcSwap read, v4.21.0)
- **Config Reload**: <10ms (atomic swap, never blocks hot path)
- **Gamepad Polling**: 120Hz (8.3ms interval)
- **Memory Usage**: 10-15MB resident
- **CPU Usage**: <2% idle, <6% active
- **Binary Size**: ~3-5MB (release with LTO)

### Dependencies

**Core Dependencies**:
- `midir`: Cross-platform MIDI I/O
- `gilrs`: Cross-platform gamepad/HID input (v3.0+)
- `enigo`: Keyboard/mouse simulation
- `hidapi`: HID device access (with `macos-shared-device`)
- `arc-swap`: Lock-free atomic pointer swap (v4.21.0)
- `dashmap`: Concurrent per-device HashMap (v4.20.0)
- `serde`/`toml`: Config parsing
- `quick-xml`: XML profile parsing
- `tokio`: Async runtime for daemon

**Platform-Specific**:
- macOS: AppleScript for volume control
- Linux: `xdotool` (optional, for input simulation)
- Windows: Native Windows APIs

## Future Phases (Phase 4+: GUI)

The roadmap includes migrating to a **workspace structure** with separate crates. See [Workspace Structure Design](../../docs/workspace-structure.md) for complete details (AMI-124).

### Target Structure

```
conductor/
├── Cargo.toml                      # Workspace root manifest
├── conductor-core/                   # Pure Rust engine (UI-free)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs                  # Public API
│       ├── engine.rs               # ConductorEngine
│       ├── config.rs               # Config types
│       ├── events.rs               # Event types
│       ├── mappings.rs             # Mapping engine
│       ├── actions.rs              # Action execution
│       ├── feedback.rs             # LED feedback
│       ├── device_profile.rs       # NI profile parser
│       └── error.rs                # Error types
├── conductor-daemon/                 # CLI binary (current main.rs)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs                 # CLI entry point
│       ├── cli.rs                  # Argument parsing
│       ├── debug.rs                # Debug output
│       └── bin/                    # Diagnostic tools
├── conductor-gui/                    # Tauri UI (Phase 4)
│   ├── Cargo.toml
│   ├── src-tauri/
│   └── ui/
└── config/                         # Config templates
    ├── default.toml
    ├── examples/
    └── device_templates/
```

### Workspace Dependency Graph

```mermaid
graph TD
    A[conductor-gui<br/>Tauri UI] -->|depends on| C[conductor-core<br/>Engine Library]
    B[conductor-daemon<br/>CLI Binary] -->|depends on| C
    C -->|no dependencies on| A
    C -->|no dependencies on| B

    style C fill:#4a90e2,stroke:#2e5f8a,stroke-width:3px,color:#fff
    style A fill:#50c878,stroke:#2d7a4d,stroke-width:2px,color:#fff
    style B fill:#50c878,stroke:#2d7a4d,stroke-width:2px,color:#fff
```

### Phase 2: Core Library Extraction ✅ COMPLETE (v0.2.0)

**Goal**: Extract engine logic into reusable `conductor-core` crate (AMI-123, AMI-124).

**Public API** (from [API Design](../../docs/api-design.md)):
```rust
pub struct ConductorEngine;
impl ConductorEngine {
    pub fn new(config: Config) -> Result<Self, EngineError>;
    pub fn start(&mut self) -> Result<(), EngineError>;
    pub fn stop(&mut self) -> Result<(), EngineError>;
    pub fn reload_config(&mut self, config: Config) -> Result<(), EngineError>;
    pub fn current_mode(&self) -> u8;
    pub fn set_mode(&mut self, mode: u8) -> Result<(), EngineError>;
    pub fn config(&self) -> Config;
    pub fn stats(&self) -> EngineStats;

    // Callbacks for integration
    pub fn on_mode_change<F>(&mut self, callback: F)
    where F: Fn(u8) + Send + 'static;

    pub fn on_action<F>(&mut self, callback: F)
    where F: Fn(&ProcessedEvent, &Action) + Send + 'static;
}
```

**Module Separation**:
- **Public Modules**: `config`, `engine`, `events`, `actions`, `feedback`, `device`, `error`
- **Private Modules**: `event_processor`, `timing`, `chord`, `velocity`

**Benefits**:
- Reusable engine for CLI, daemon, GUI
- Zero UI dependencies in core (no `colored`, `chrono` for display)
- Clean API boundaries with trait-based abstractions
- Easier testing and integration
- Thread-safe with Arc/RwLock for shared state

**Build Commands**:
```bash
# Build core library only
cargo build -p conductor-core

# Build CLI daemon
cargo build -p conductor-daemon --release

# Run with new structure
cargo run -p conductor-daemon --release -- 2 --led reactive

# Test workspace
cargo test --workspace
```

### Phase 3: Daemon and Menu Bar

**Goal**: Add `conductor-daemon` with macOS menu bar integration.

**Features**:
- System tray icon with status
- Quick actions (Pause, Reload, Open Config)
- Config hot-reloading via `notify` crate
- Auto-start via LaunchAgent or Tauri autostart plugin
- Frontmost app detection for per-app profiles

**Integration Pattern**:
```rust
use conductor_core::{Config, ConductorEngine};

fn main() -> Result<()> {
    let config = Config::load("config.toml")?;
    let mut engine = ConductorEngine::new(config)?;

    // Register callbacks for UI updates
    engine.on_mode_change(|mode| {
        update_menu_bar_icon(mode);
    });

    engine.start()?;
    Ok(())
}
```

### Phase 4: GUI Configuration

**Goal**: Add `conductor-gui` with Tauri-based visual config editor.

**Features**:
- Visual device mapping with SVG pad layouts
- MIDI Learn mode (click → press → bind)
- Profile management (import/export/share)
- Live event console with filtering
- Velocity curve editor
- Test bindings without saving

**UI Components**:
- Device visualizer (16-pad grid for Maschine Mikro MK3)
- Mapping editor (trigger → action configuration)
- Profile switcher (per-app profiles)
- Event log (real-time MIDI/HID events)
- Settings panel (advanced timing, thresholds)

## Configuration Backward Compatibility

**Core Commitment**: Configuration files created for v0.1.0 will work in all v1.x.x versions without modification.

### Stability Guarantees

| Section | v0.1.0 | v0.2.0 | v1.0.0 | Stability |
|---------|--------|--------|--------|-----------|
| `[device]` | ✅ | ✅ | ✅ | **Stable** |
| `[[modes]]` | ✅ | ✅ | ✅ | **Stable** |
| `[[global_mappings]]` | ✅ | ✅ | ✅ | **Stable** (optional, defaults to empty) |
| `[advanced_settings]` | ⚠️  | ✅ | ✅ | **Optional** (defaults provided) |

**Legend**:
- ✅ **Fully Supported**: Section works identically across versions
- ⚠️  **Optional**: Section is optional with sensible defaults

### Version Compatibility

```
v0.1.0 config → v0.2.0 engine ✅ (100% compatible)
v0.1.0 config → v1.0.0 engine ✅ (100% compatible)

v0.2.0 config → v0.1.0 engine ⚠️  (new features ignored with warnings)
v1.0.0 config → v0.1.0 engine ⚠️  (new features ignored with warnings)
```

### Deprecation Policy

Conductor follows **semantic versioning** (SemVer) for configuration:

- **Major version (x.0.0)**: May introduce breaking changes with migration tools
- **Minor version (0.x.0)**: Adds features in backward-compatible manner
- **Patch version (0.0.x)**: Bug fixes only, no config changes

**Breaking changes** require:
1. Deprecation notice (version N)
2. Deprecation period with warnings (version N+1)
3. Removal with migration tool (version N+2, major bump)

### Test Coverage

All v0.1.0 configuration examples are validated in CI/CD via `tests/config_compatibility_test.rs`:

- **CFG-001**: Basic config.toml loads without errors
- **CFG-003**: Minimal device-only configs use defaults
- **CFG-004**: All trigger types parse correctly
- **CFG-005**: All action types parse correctly
- **CFG-006**: Complex sequences work
- **CFG-007**: Multiple modes supported
- **CFG-009**: Legacy v0.1.0 syntax always works
- **CFG-010**: Invalid syntax produces clear error messages

**Coverage Requirements**:
- 100% coverage for config parsing (`src/config.rs`)
- All documentation examples must parse successfully
- Regression tests for every released version

For complete details, see: **[Configuration Backward Compatibility Strategy](../../docs/config-compatibility.md)**

---

## API Design

For detailed public API design (Phase 2+), see:
- **[API Design Document](../../docs/api-design.md)** - Complete API specification (AMI-123)
- **[Configuration Compatibility](../../docs/config-compatibility.md)** - Backward compatibility strategy (AMI-125)
- **[Implementation Viewpoint 1](../../.research/implementation-viewpoint-1.md)** - Monorepo with Tauri
- **[Implementation Viewpoint 2](../../.research/implementation-viewpoint-2.md)** - Alternative approach

## Related Documentation

- [Configuration Overview](https://getconductor.dev/configuration/overview.html)
- [Device Profiles](https://getconductor.dev/configuration/device-profiles.html)
- [LED Feedback System](https://getconductor.dev/configuration/led-feedback.html)
- [Contributing Guide](../../CONTRIBUTING.md)
- [Testing Strategy](testing.md)
