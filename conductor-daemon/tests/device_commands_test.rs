// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Test device management IPC commands

use conductor_daemon::{IpcCommand, IpcRequest, MidiDeviceInfo};
use serde_json::json;

#[test]
fn test_list_devices_command_serialization() {
    let request = IpcRequest {
        id: "test-1".to_string(),
        command: IpcCommand::ListDevices,
        args: json!({}),
    };

    // Pin the EXACT external JSON shape IPC clients consume — field names
    // (`id`/`command`/`args`) and the command tag. A substring check would
    // survive a field rename, a mis-keyed command, or an added wrapper as long
    // as the text appeared somewhere; exact Value equality will not.
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(
        value,
        json!({"id": "test-1", "command": "LIST_DEVICES", "args": {}})
    );

    // And it round-trips back to the same typed request.
    let parsed: IpcRequest = serde_json::from_value(value).unwrap();
    assert_eq!(parsed.id, "test-1");
    assert!(matches!(parsed.command, IpcCommand::ListDevices));
}

#[test]
fn test_set_device_command_serialization() {
    let request = IpcRequest {
        id: "test-2".to_string(),
        command: IpcCommand::SetDevice,
        args: json!({"port": 2}),
    };

    // Exact wire shape, including the nested `args.port` an external client
    // must send to select a device.
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(
        value,
        json!({"id": "test-2", "command": "SET_DEVICE", "args": {"port": 2}})
    );

    let parsed: IpcRequest = serde_json::from_value(value).unwrap();
    assert_eq!(parsed.id, "test-2");
    assert!(matches!(parsed.command, IpcCommand::SetDevice));
    assert_eq!(parsed.args.get("port").and_then(|v| v.as_u64()), Some(2));
}

#[test]
fn test_get_device_command_serialization() {
    let request = IpcRequest {
        id: "test-3".to_string(),
        command: IpcCommand::GetDevice,
        args: json!({}),
    };

    // Same exact-shape guarantee as the other two command tests.
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(
        value,
        json!({"id": "test-3", "command": "GET_DEVICE", "args": {}})
    );

    let parsed: IpcRequest = serde_json::from_value(value).unwrap();
    assert_eq!(parsed.id, "test-3");
    assert!(matches!(parsed.command, IpcCommand::GetDevice));
}

#[test]
fn test_midi_device_info_serialization() {
    let device = MidiDeviceInfo {
        port_index: 0,
        port_name: "Maschine Mikro MK3".to_string(),
        manufacturer: Some("Native Instruments".to_string()),
        connected: true,
    };

    // Exact wire shape: every externally-visible field and its key, not just
    // that the two name strings appear somewhere in the blob.
    let value = serde_json::to_value(&device).unwrap();
    assert_eq!(
        value,
        json!({
            "port_index": 0,
            "port_name": "Maschine Mikro MK3",
            "manufacturer": "Native Instruments",
            "connected": true
        })
    );

    // And it round-trips back to the same typed value.
    let parsed: MidiDeviceInfo = serde_json::from_value(value).unwrap();
    assert_eq!(parsed.port_index, 0);
    assert_eq!(parsed.port_name, "Maschine Mikro MK3");
    assert_eq!(parsed.manufacturer, Some("Native Instruments".to_string()));
    assert!(parsed.connected);
}

#[test]
fn test_midi_device_info_none_manufacturer_serializes_as_null() {
    // `manufacturer` has no `skip_serializing_if`, so a device with no
    // manufacturer must serialise the key explicitly as JSON `null`. Pin that:
    // an accidental skip would change the wire shape for clients that read
    // `manufacturer`, and the Some(..) tests alone wouldn't catch it.
    let device = MidiDeviceInfo {
        port_index: 3,
        port_name: "Generic MIDI".to_string(),
        manufacturer: None,
        connected: false,
    };
    let value = serde_json::to_value(&device).unwrap();
    assert_eq!(
        value,
        json!({
            "port_index": 3,
            "port_name": "Generic MIDI",
            "manufacturer": null,
            "connected": false
        })
    );

    let parsed: MidiDeviceInfo = serde_json::from_value(value).unwrap();
    assert_eq!(parsed.manufacturer, None);
}

#[test]
fn test_device_list_response_format() {
    let devices = vec![
        MidiDeviceInfo {
            port_index: 0,
            port_name: "Maschine Mikro MK3".to_string(),
            manufacturer: Some("Native Instruments".to_string()),
            connected: true,
        },
        MidiDeviceInfo {
            port_index: 1,
            port_name: "IAC Driver Bus 1".to_string(),
            manufacturer: Some("IAC".to_string()),
            connected: false,
        },
    ];

    // `devices` is serialized via MidiDeviceInfo's own Serialize impl, so this
    // asserts the actual application-type wire format for the whole list —
    // both elements, every field — rather than spot-checking one device's
    // fields (which would pass even if the second device or the connected/
    // manufacturer keys regressed).
    let response_data = json!({
        "devices": devices
    });

    assert_eq!(
        response_data,
        json!({
            "devices": [
                {
                    "port_index": 0,
                    "port_name": "Maschine Mikro MK3",
                    "manufacturer": "Native Instruments",
                    "connected": true
                },
                {
                    "port_index": 1,
                    "port_name": "IAC Driver Bus 1",
                    "manufacturer": "IAC",
                    "connected": false
                }
            ]
        })
    );
}
