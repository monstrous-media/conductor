# Pattern Detection Reference

Advanced algorithms and heuristics for detecting trigger patterns from captured MIDI events.

## Event Structure

Captured events contain (DaemonMidiLearnEvent shape):
```json
{
  "timestamp": 1234567890,
  "event_type": "note_on",
  "channel": 0,
  "note": 60,
  "velocity": 100
}
```

## Pattern Detection Algorithms

### LongPress Detection

**Algorithm:**
```
FOR each NoteOn event:
    Find matching NoteOff event (same note, same channel)
    duration = NoteOff.timestamp - NoteOn.timestamp

    IF duration > MIN_LONG_PRESS_MS (500ms):
        Mark as LongPress candidate
        suggested_threshold = duration * 0.8  // 80% of actual hold time
```

**Thresholds:**
- Minimum for LongPress: 500ms (below this is likely just a slow release)
- Default suggestion: 80% of detected hold duration
- Cap suggestion at 3000ms (anything longer is probably intentional)

**Edge Cases:**
- Multiple NoteOn without NoteOff: Controller may have note stuck, warn user
- NoteOff before NoteOn: Event ordering issue, ignore this pair

---

### DoubleTap Detection

**Algorithm:**
```
FOR each note number:
    Group NoteOn events by note
    Sort by timestamp

    FOR consecutive pairs (event1, event2):
        interval = event2.timestamp - event1.timestamp

        IF interval < MAX_DOUBLE_TAP_MS (400ms):
            IF event1 has matching NoteOff before event2:
                Mark as DoubleTap candidate
                suggested_timeout = interval * 1.5  // 150% of actual interval
```

**Thresholds:**
- Maximum interval for DoubleTap: 400ms
- Default suggestion: 150% of detected interval (gives margin for variation)
- Minimum suggestion: 200ms (faster is hard to perform consistently)

**Important:**
- Must have NoteOff between taps (tap-release-tap-release)
- Three quick taps should NOT trigger three DoubleTaps

---

### Chord Detection

**Algorithm:**
```
events_sorted = sort(all_events, by=timestamp)

FOR i, event in events_sorted:
    IF event.type != "NoteOn":
        continue

    chord_candidates = [event]

    FOR j in (i+1 to len(events_sorted)):
        next_event = events_sorted[j]

        IF next_event.timestamp - event.timestamp > MAX_CHORD_WINDOW_MS (50ms):
            break

        IF next_event.type == "NoteOn":
            chord_candidates.append(next_event)

    IF len(chord_candidates) >= 2:
        Mark as Chord candidate with notes = [e.note for e in chord_candidates]
```

**Thresholds:**
- Maximum chord window: 50ms (physically hard to press within tighter window)
- Minimum chord size: 2 notes (single note is just Note trigger)
- Maximum chord size: No limit, but warn if >4 notes (hard to reproduce)

**Notes:**
- Order within chord doesn't matter
- Same note pressed twice is NOT a chord (filter duplicates)

---

### VelocityRange Detection

**Algorithm:**
```
FOR each note number with multiple events:
    velocities = [e.velocity for e in events if e.note == note]

    IF max(velocities) - min(velocities) > VELOCITY_SPREAD_THRESHOLD (40):
        Mark as velocity-sensitive

        IF any(v < 40 for v in velocities) AND any(v > 100 for v in velocities):
            Suggest VelocityRange trigger
```

**Thresholds:**
- Velocity spread threshold: 40 (if user plays with >40 velocity difference, they're using velocity)
- Soft threshold: 40
- Medium threshold: 80
- Hard: 81-127

**Notes:**
- Some controllers always send velocity 127 (not velocity-sensitive)
- Some send fixed velocity 64 (also not velocity-sensitive)
- Check if spread exists before suggesting VelocityRange

---

### Encoder vs Fader Detection

**Algorithm:**
```
FOR each CC number with multiple events:
    values = [e.value for e in events if e.cc == cc]
    timestamps = [e.timestamp for e in events]

    // Check for relative encoder pattern
    IF all(v == 1 or v == 127 or v == 65 or v == 63 for v in values):
        // Common relative encoder values
        Mark as Encoder (relative)

    ELSE IF values form sequential increase/decrease:
        // Absolute fader being moved
        Mark as Fader (absolute)

    ELSE IF values jump around non-sequentially:
        // Could be encoder with different encoding
        Ask user for clarification
```

**Encoder Encoding Schemes:**
| Encoding | Clockwise | Counter-Clockwise |
|----------|-----------|-------------------|
| 2's complement | 1-63 | 65-127 |
| Binary offset | 1-64 | 65-127 |
| Sign-magnitude | 1-63 | 65-127 |
| Relative 3 | 1-15 | 65-79 |

**Notes:**
- Many encoders use value 1 for CW and 127 for CCW
- Some use value 65 for CW and 63 for CCW
- Faders send absolute values (0-127) in sequence

---

## Confidence Scoring

For ambiguous patterns, compute confidence score:

```
LongPress confidence:
    IF duration > 2000ms: high
    IF duration 1000-2000ms: medium
    IF duration 500-1000ms: low

DoubleTap confidence:
    IF interval < 200ms: high
    IF interval 200-300ms: medium
    IF interval 300-400ms: low

Chord confidence:
    IF all notes within 20ms: high
    IF all notes within 35ms: medium
    IF all notes within 50ms: low

VelocityRange confidence:
    IF spread > 80: high
    IF spread 60-80: medium
    IF spread 40-60: low
```

## Conflict Resolution

When patterns overlap:

1. **LongPress vs Note**: LongPress takes precedence if duration > threshold
2. **DoubleTap vs Note**: Note fires on first tap, DoubleTap on second
3. **Chord vs Note**: Chord takes precedence if all notes pressed within window
4. **VelocityRange vs Note**: User choice (ask if they want velocity sensitivity)

## Pattern Validation

Before suggesting a pattern, validate:

1. **Reproducibility**: Can the user realistically perform this gesture?
   - LongPress > 5000ms: Warn "That's a very long hold"
   - DoubleTap < 150ms: Warn "That's very fast"
   - Chord > 4 notes: Warn "Complex chord, consider simplifying"

2. **Distinctiveness**: Will this conflict with other mappings?
   - Check existing config for overlaps
   - Warn about potential conflicts

3. **Hardware capability**: Does the controller support this?
   - Non-velocity-sensitive pads can't do VelocityRange
   - Buttons (not pads) may not support aftertouch
