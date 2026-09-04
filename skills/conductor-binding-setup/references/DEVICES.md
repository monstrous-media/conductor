# Supported Devices Reference

Comprehensive list of supported input devices and their configuration notes.

## MIDI Controllers

### Native Instruments

#### Maschine Mikro MK3

**Connection:**
- USB only (no standalone MIDI)
- Port name: "Maschine Mikro MK3 MIDI"
- May require Controller Editor for LED control

**Layout:**
- 16 velocity-sensitive pads (notes 36-51)
- 1 encoder (CC 16)
- 8 touch strips (CC 1-8)
- Transport buttons (notes 52-59)

**Notes:**
- Controller Editor may capture events - close it or use shared mode
- LEDs controlled via SysEx (not currently supported)
- Supports aftertouch

**Template:** `mikro-mk3-basic.toml`

---

#### Maschine+

**Connection:**
- USB or standalone WiFi
- Port name: "Maschine+ MIDI"

**Layout:**
- 16 velocity-sensitive pads
- 8 knobs (CC 16-23)
- Screen buttons (notes 60-75)

**Template:** `maschine-plus.toml`

---

### Novation

#### Launchpad Mini MK3

**Connection:**
- USB only
- Port name: "Launchpad Mini MK3 LPMiniMK3 MIDI"

**Layout:**
- 64 RGB pads (8x8 grid, notes 0-63)
- 8 top buttons (CC 91-98)
- 8 side buttons (notes 64-71)

**Notes:**
- Programmer mode provides direct note access
- LED colors via note velocity (0=off, 1-127=colors)

**Template:** `launchpad-mini.toml`

---

#### Launchpad X

**Connection:**
- USB
- Port name: "Launchpad X LPX MIDI"

**Layout:**
- 64 velocity-sensitive RGB pads
- 8 top buttons (CC 91-98)
- 8 side buttons (notes 89-96)

**Notes:**
- More velocity sensitivity than Mini
- Pressure-sensitive (polyphonic aftertouch)

**Template:** `launchpad-x.toml`

---

#### Launch Control XL

**Connection:**
- USB
- Port name: "Launch Control XL"

**Layout:**
- 24 knobs (CC 13-20, 29-36, 49-56)
- 8 faders (CC 77-84)
- 16 buttons (notes 41-56, 73-88)

**Notes:**
- Great for mixing/fader control
- All controls send CC (no notes from knobs)

**Template:** `launch-control-xl.toml`

---

### Akai

#### APC Mini MK2

**Connection:**
- USB
- Port name: "APC mini mk2"

**Layout:**
- 64 RGB pads (8x8 grid)
- 8 faders
- Track/scene buttons

**Notes:**
- Clip launch grid layout
- LED colors via note velocity

**Template:** `apc-mini-mk2.toml`

---

#### MPK Mini MK3

**Connection:**
- USB
- Port name: "MPK mini 3"

**Layout:**
- 25 mini keys (notes 48-72)
- 8 pads (notes 36-43)
- 8 knobs (CC 70-77)

**Notes:**
- Keys and pads are velocity-sensitive
- Multiple banks available

**Template:** `mpk-mini-mk3.toml`

---

### KORG

#### nanoKONTROL2

**Connection:**
- USB
- Port name: "nanoKONTROL2"

**Layout:**
- 8 faders (CC 0-7)
- 8 knobs (CC 16-23)
- 24 buttons (notes and CCs vary by scene)
- Transport section

**Notes:**
- Multiple scene presets
- Highly configurable via KORG software

**Template:** `nanokontrol2.toml`

---

#### nanoPAD2

**Connection:**
- USB
- Port name: "nanoPAD2"

**Layout:**
- 16 pads (notes 36-51)
- XY pad (CC 1, CC 2)

**Notes:**
- XY pad sends two CC values
- Scene button changes pad mappings

**Template:** `nanopad2.toml`

---

## Game Controllers (HID)

### Xbox Controllers

#### Xbox Wireless Controller (Series X|S)

**Connection:**
- USB or Bluetooth
- Detected as: "Xbox Wireless Controller"

**Button Mapping:**
| Button | ID | Notes |
|--------|----|----|
| A | 128 | South face button |
| B | 129 | East face button |
| X | 130 | West face button |
| Y | 131 | North face button |
| LB | 136 | Left bumper |
| RB | 137 | Right bumper |
| LT | 143 | Left trigger (digital) |
| RT | 144 | Right trigger (digital) |
| D-Pad | 132-135 | Up/Down/Left/Right |
| LS | 138 | Left stick click |
| RS | 139 | Right stick click |
| Menu | 140 | Start equivalent |
| View | 141 | Select equivalent |
| Xbox | 142 | Guide button |

**Analog:**
| Control | ID | Range |
|---------|----|----|
| Left Stick X | 128 | -1.0 to 1.0 |
| Left Stick Y | 129 | -1.0 to 1.0 |
| Right Stick X | 130 | -1.0 to 1.0 |
| Right Stick Y | 131 | -1.0 to 1.0 |
| Left Trigger | 132 | 0.0 to 1.0 |
| Right Trigger | 133 | 0.0 to 1.0 |

**Template:** `xbox-controller.toml`

---

#### Xbox Elite Series 2

Same as standard Xbox controller plus:
- 4 back paddles (may require driver configuration)
- Hair triggers with adjustable travel
- Multiple profiles

**Notes:**
- Paddles may not be detected by default SDL2
- Use Xbox Accessories app for configuration

---

### PlayStation Controllers

#### DualSense (PS5)

**Connection:**
- USB or Bluetooth
- Detected as: "PS5 Controller" or "DualSense"

**Button Mapping:**
| Button | ID | Notes |
|--------|----|----|
| Cross | 128 | South (A equivalent) |
| Circle | 129 | East (B equivalent) |
| Square | 130 | West (X equivalent) |
| Triangle | 131 | North (Y equivalent) |
| L1 | 136 | Left bumper |
| R1 | 137 | Right bumper |
| L2 | 143 | Left trigger (digital) |
| R2 | 144 | Right trigger (digital) |
| D-Pad | 132-135 | Up/Down/Left/Right |
| L3 | 138 | Left stick click |
| R3 | 139 | Right stick click |
| Options | 140 | Start equivalent |
| Share | 141 | Select equivalent |
| PS | 142 | Guide button |

**Additional Features (not currently mapped):**
- Touchpad: Not supported via HID
- Adaptive triggers: Not exposed via SDL2
- Haptics: Not exposed via SDL2

**Template:** `ps5-dualsense.toml`

---

#### DualShock 4 (PS4)

Similar to DualSense, slight differences in detection name.

**Connection:**
- USB or Bluetooth
- Detected as: "PS4 Controller" or "DualShock 4"

**Template:** `ps4-dualshock.toml`

---

### Nintendo Controllers

#### Switch Pro Controller

**Connection:**
- USB or Bluetooth
- Detected as: "Nintendo Switch Pro Controller"

**Button Mapping:**
| Button | ID | Notes |
|--------|----|----|
| B | 128 | South (A on Xbox) |
| A | 129 | East (B on Xbox) |
| Y | 130 | West (X on Xbox) |
| X | 131 | North (Y on Xbox) |
| L | 136 | Left bumper |
| R | 137 | Right bumper |
| ZL | 143 | Left trigger |
| ZR | 144 | Right trigger |
| D-Pad | 132-135 | Up/Down/Left/Right |
| LS | 138 | Left stick click |
| RS | 139 | Right stick click |
| + | 140 | Start equivalent |
| - | 141 | Select equivalent |
| Home | 142 | Guide button |

**Notes:**
- Button labels are swapped compared to Xbox (A/B, X/Y)
- No analog triggers (ZL/ZR are digital)

**Template:** `switch-pro.toml`

---

### Specialty Controllers

#### Racing Wheels

Most racing wheels are detected as standard gamepads with:
- Steering axis (Left Stick X)
- Throttle (Right Trigger)
- Brake (Left Trigger)
- Paddle shifters (various buttons)

**Notes:**
- Force feedback not supported
- Wheel rotation range may need OS configuration

---

#### Flight Sticks / HOTAS

Typically detected with:
- Multiple axes (stick X/Y, throttle, twist)
- Many buttons (often more than standard gamepad)

**Notes:**
- May require SDL2 controller mapping database entry
- Some advanced features (MFDs, LEDs) not supported

---

## Configuring Unknown Devices

If your device isn't listed:

1. **Connect and run MIDI Learn** (for MIDI devices)
   - Discover note numbers, CC values
   - Build mappings from captured events

2. **Use `conductor_list_discovered_ports`** (for HID devices)
   - Check if gamepad is detected in the ports list
   - Note the device name for config

3. **Check SDL2 controller database**
   - https://github.com/gabomdq/SDL_GameControllerDB
   - May need to add mapping for exotic controllers

4. **Submit device info** to Conductor project
   - Help us add templates for more devices!
