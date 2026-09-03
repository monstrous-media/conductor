# Plugin Development

Conductor's plugin architecture allows third-party developers to create custom actions through dynamically loaded shared libraries.

## Overview

Plugins extend Conductor's functionality by implementing the `ActionPlugin` trait. They can:

- Execute custom logic when MIDI events occur
- Access event metadata (velocity, mode, timestamp)
- Request specific capabilities (network, filesystem, etc.)
- Be loaded/unloaded dynamically without restart
- Be managed via `conductorctl plugin` and the `[plugins.<name>]` config table

## Quick Start

### 1. Create a New Plugin Project

```bash
cargo new --lib my_plugin
cd my_plugin
```

### 2. Configure Cargo.toml

```toml
[package]
name = "conductor-my-plugin"
version = "1.0.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]  # Required for dynamic loading

[dependencies]
conductor-core = { version = "0.1", features = ["plugin"] }
serde_json = "1.0"
```

Building inside this workspace (e.g. under `examples/`), `conductor-core` is
typically a `path` dependency instead. Either way, `features = ["plugin"]`
is what pulls in the `ActionPlugin` trait and related types.

### 3. Implement the ActionPlugin Trait

```rust
use conductor_core::plugin::{ActionPlugin, Capability, TriggerContext};
use serde_json::Value;
use std::error::Error;

pub struct MyPlugin;

impl ActionPlugin for MyPlugin {
    fn name(&self) -> &str {
        "my_plugin"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn description(&self) -> &str {
        "My custom Conductor plugin"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Network]  // Request network access
    }

    fn execute(&mut self, params: Value, context: TriggerContext) -> Result<(), Box<dyn Error>> {
        // Your plugin logic here
        let velocity = context.velocity.unwrap_or(0);
        eprintln!("Plugin executed with velocity: {}", velocity);
        Ok(())
    }
}

#[no_mangle]
pub extern "C" fn _create_plugin() -> *mut dyn ActionPlugin {
    Box::into_raw(Box::new(MyPlugin))
}
```

### 4. Create Plugin Manifest

Create `plugin.toml` in your plugin directory:

```toml
[plugin]
name = "my_plugin"
version = "1.0.0"
description = "My custom Conductor plugin"
author = "Your Name"
homepage = "https://github.com/yourname/conductor-my-plugin"
license = "MIT"
type = "action"
binary = "libconductor_my_plugin.dylib"  # .so on Linux, .dll on Windows
checksum = ""  # Optional SHA256 checksum

[plugin.capabilities]
network = true
```

### 5. Build and Install

```bash
# Build the plugin
cargo build --release

# Install to Conductor plugins directory
mkdir -p ~/.conductor/plugins/my_plugin
cp target/release/libconductor_my_plugin.dylib ~/.conductor/plugins/my_plugin/
cp plugin.toml ~/.conductor/plugins/my_plugin/
```

### 6. Use in Configuration

```toml
[[modes.mappings]]
trigger = { Note = { note = 60 } }
action = { Plugin = {
    plugin = "my_plugin",
    params = {
        "custom_param": "value"
    }
}}
```

## Capability System

Plugins request capabilities to access system resources. Conductor uses a risk-level based security model:

### Capability Types

| Capability | Risk Level | Description |
|-----------|-----------|-------------|
| `Audio` | Low | Audio device access |
| `Midi` | Low | MIDI device access |
| `Network` | Medium | HTTP requests, websockets |
| `SystemControl` | Medium | System-level control |
| `Filesystem` | High | File read/write |
| `Subprocess` | High | Execute shell commands |

### Risk Levels

- **Low** (🟢): Auto-granted by default, considered safe
- **Medium** (🟡): Requires explicit grant via `granted_capabilities`
- **High** (🔴): Requires explicit grant via `granted_capabilities`

### Requesting Capabilities

```rust
fn capabilities(&self) -> Vec<Capability> {
    vec![
        Capability::Midi,         // Low risk: auto-granted
        Capability::Network,      // Medium risk: requires explicit grant
    ]
}
```

## Plugin Lifecycle

1. **Discovery**: Conductor scans `~/.conductor/plugins/` for `plugin.toml` files
2. **Load**: Binary is loaded via `libloading`, plugin instance created
3. **Initialize**: `initialize()` method called (if implemented)
4. **Execute**: `execute()` called for each MIDI event
5. **Shutdown**: `shutdown()` method called before unload (if implemented)
6. **Unload**: Plugin removed from memory

## Advanced Features

### Initialization and Shutdown

```rust
impl ActionPlugin for MyPlugin {
    fn initialize(&mut self) -> Result<(), Box<dyn Error>> {
        eprintln!("Plugin initializing...");
        // Setup code here
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), Box<dyn Error>> {
        eprintln!("Plugin shutting down...");
        // Cleanup code here
        Ok(())
    }
}
```

### Accessing Context

```rust
fn execute(&mut self, params: Value, context: TriggerContext) -> Result<(), Box<dyn Error>> {
    let velocity = context.velocity.unwrap_or(0);
    let mode = context.current_mode.unwrap_or(0);
    let timestamp = context.timestamp;

    eprintln!("Velocity: {}, Mode: {}", velocity, mode);
    Ok(())
}
```

### Parameter Parsing

```rust
fn execute(&mut self, params: Value, _context: TriggerContext) -> Result<(), Box<dyn Error>> {
    let url = params["url"]
        .as_str()
        .ok_or("Missing 'url' parameter")?;

    let method = params["method"]
        .as_str()
        .unwrap_or("GET");

    // Use parameters...
    Ok(())
}
```

## Testing Plugins

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_metadata() {
        let plugin = MyPlugin;
        assert_eq!(plugin.name(), "my_plugin");
        assert_eq!(plugin.version(), "1.0.0");
    }

    #[test]
    fn test_plugin_execute() {
        let mut plugin = MyPlugin;
        let params = serde_json::json!({
            "param1": "value1"
        });
        let context = TriggerContext {
            velocity: Some(127),
            current_mode: Some(0),
            timestamp: 0, // milliseconds since Unix epoch
        };

        assert!(plugin.execute(params, context).is_ok());
    }
}
```

## Granting Capabilities

Capabilities above Low risk are not auto-granted. Grant them explicitly per
plugin in `config.toml`:

```toml
[plugins.my_plugin]
granted_capabilities = ["Network", "Filesystem"]
```

Plugin lifecycle (list, inspect, enable, disable) is managed with
`conductorctl plugin`:

```bash
conductorctl plugin list
conductorctl plugin info my_plugin
conductorctl plugin enable my_plugin
conductorctl plugin disable my_plugin
```

Note that `conductorctl plugin` does not itself grant or revoke capabilities
— that's config-only, via `granted_capabilities` above. A downstream GUI
built on top of Conductor may expose capability granting as a richer
approval flow, but nothing like that ships in this repository.

## Example Plugins

### HTTP Request Plugin

See `examples/http-plugin/` for a complete example that demonstrates:

- Making HTTP requests (GET, POST, PUT, DELETE)
- Custom headers
- JSON body
- Velocity substitution
- Error handling

### Creating a Simple Logger Plugin

```rust
use std::fs::OpenOptions;
use std::io::Write;

pub struct LoggerPlugin {
    log_file: String,
}

impl LoggerPlugin {
    pub fn new() -> Self {
        Self {
            log_file: "/tmp/conductor.log".to_string(),
        }
    }
}

impl ActionPlugin for LoggerPlugin {
    // ... metadata methods ...

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Filesystem]
    }

    fn execute(&mut self, params: Value, context: TriggerContext) -> Result<(), Box<dyn Error>> {
        let message = params["message"].as_str().unwrap_or("Event triggered");
        let velocity = context.velocity.unwrap_or(0);

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)?;

        writeln!(file, "[v={}] {}", velocity, message)?;
        Ok(())
    }
}
```

## Best Practices

1. **Error Handling**: Always return proper errors, never panic
2. **Performance**: Keep `execute()` fast (<10ms ideal)
3. **Resource Cleanup**: Implement `shutdown()` for cleanup
4. **Documentation**: Document all parameters in README
5. **Testing**: Write tests for all functionality
6. **Security**: Request minimum necessary capabilities
7. **Logging**: Use `eprintln!()` for debug output

## Distribution

### Binary Naming

- macOS: `libmyplugin.dylib`
- Linux: `libmyplugin.so`
- Windows: `myplugin.dll`

### Directory Structure

```
~/.conductor/plugins/
  └── my_plugin/
      ├── plugin.toml
      └── libconductor_my_plugin.dylib
```

### Checksum Verification

Generate SHA256 for security:

```bash
shasum -a 256 target/release/libconductor_my_plugin.dylib
```

Add to `plugin.toml`:

```toml
[plugin]
checksum = "abc123..."
```

## Troubleshooting

### Plugin Not Discovered

- Check `plugin.toml` is valid TOML
- Verify `~/.conductor/plugins/` directory exists
- Ensure binary name matches in manifest

### Plugin Fails to Load

- Check binary is compiled for correct platform
- Verify `crate-type = ["cdylib"]` in Cargo.toml
- Ensure `_create_plugin` symbol is exported

### Capability Denied

- Check the capability's risk level (see the table above)
- Add it to `granted_capabilities` in `config.toml` if it isn't Low risk
- Consider using lower-risk alternatives

## Further Reading

- [HTTP Plugin Example](../../examples/http-plugin/) - Reference native plugin implementation
- [Plugin Examples](plugin-examples.md) - Overview of every shipped example
- [Plugin Security](plugin-security.md) - Signing and capability model

## Community Plugins

A public plugin registry for sharing community plugins is planned but does
not exist yet in this repository.
