# Chat Troubleshooting

This guide covers common issues with Conductor's Chat feature and how to resolve them.

## Provider Issues

### "API key not configured"

**Cause**: No API key has been set for the selected provider.

**Solution**:
1. Open **Settings** > **Chat**
2. Select your provider (OpenAI or Anthropic)
3. Enter your API key
4. Click **Save**

See [LLM Providers](../guides/llm-providers.md) for detailed setup instructions.

### "Authentication failed"

**Cause**: Your API key is invalid, expired, or revoked.

**Solution**:
1. Log into your provider's dashboard (OpenAI or Anthropic)
2. Generate a new API key
3. Update the key in Conductor settings

### "Rate limit exceeded"

**Cause**: Too many requests in a short period.

**Solution**:
1. Wait 30-60 seconds before trying again
2. For heavy usage, consider upgrading your API tier
3. OpenAI and Anthropic have different rate limits per tier

### "Network error"

**Cause**: Cannot reach the provider's API.

**Solution**:
1. Check your internet connection
2. Verify no firewall is blocking:
   - `api.openai.com` (for OpenAI)
   - `api.anthropic.com` (for Anthropic)
3. Try again in a few moments
4. Check the provider's status page for outages

## Plan/Apply Issues

### Plan expired

**Cause**: Plans expire after 5 minutes for security.

**Solution**:
1. Make your change request again
2. Review and apply the plan within 5 minutes
3. The expiration countdown is shown in the modal

### "Config has changed" error

**Cause**: The configuration was modified while a plan was pending. This is a security feature (TOCTOU protection) to prevent conflicts.

**Solution**:
1. Make your change request again
2. The new plan will be based on the current configuration
3. Review and apply promptly

### Changes not appearing after apply

**Cause**: The config file may not have hot-reloaded.

**Solution**:
1. Check that the daemon is running: `conductorctl status`
2. Manually reload: `conductorctl reload`
3. Verify the config file was actually modified

## Chat Response Issues

### Assistant doesn't understand my request

**Solution**:
1. Be more specific:
   - Instead of: "map a pad"
   - Try: "Map note 36 to Command+C"

2. Use standard terminology:
   - "note" for pads/keys
   - "cc" for knobs/faders
   - "encoder" for rotary encoders

3. Specify the mode if relevant:
   - "In DJ mode, map note 40 to..."

### Wrong note/CC suggested

**Solution**:
1. Use MIDI Learn to capture the exact values:
   ```
   Start MIDI Learn
   ```
2. Press the pad/turn the knob you want to map
3. The assistant will use the captured values

### Response is slow

**Cause**: API response time varies based on provider load.

**Solution**:
1. GPT-4o-mini is faster than GPT-4o
2. Claude 3.5 Sonnet has consistent response times
3. Check provider status pages during peak times

## MIDI Learn Issues

### MIDI Learn not capturing events

**Cause**: The daemon may not be receiving MIDI input.

**Solution**:
1. Verify your MIDI device is connected: `conductorctl status`
2. Check that the correct port is selected
3. Try the Event Console to see if events are being received

### Captured wrong event

**Solution**:
1. Say "cancel midi learn"
2. Start again with "start midi learn"
3. Press/turn the intended control

## Device Detection Issues

### Chat reports "daemon disconnected"

**Cause**: Prior to v4.17.0, the MCP status response used `connected` to mean "device connected", not "daemon connected". When no MIDI device was active, chat clients interpreted `connected: false` as the daemon being offline.

**Solution**:
1. Update to Conductor v4.17.0 or later
2. Check `daemon_running` (always `true` when daemon responds) instead of `connected`
3. Use `device_connected` for explicit device connection status
4. Verify daemon status: `conductorctl status`

### Chat shows fewer MIDI devices than expected

**Cause**: Prior to v4.17.0, the daemon's MCP device enumeration did not use the warmup pattern required by macOS Core MIDI. This caused stale or incomplete device lists, especially after connecting/disconnecting devices.

**Solution**:
1. Update to Conductor v4.17.0 or later (uses shared `device_utils` with warmup pattern)
2. If still seeing stale devices, try:
   - Disconnect and reconnect your MIDI devices
   - Restart the daemon: `conductorctl stop && conductor`
   - Check Audio MIDI Setup: `open -a "Audio MIDI Setup"`

### GUI shows devices but chat doesn't

**Cause**: The GUI and daemon MCP tools used different device enumeration code paths. The GUI's enumeration included a macOS Core MIDI cache-busting warmup step, while the daemon's MCP tools did not.

**Solution**: Update to v4.17.0, which consolidates all enumeration into a shared `device_utils` module with the warmup pattern.

### Chat says "No device connected" but GUI shows connected

**Cause**: Prior to v4.17.1, the chat's tool executor (IPC path) had no reference to daemon state. The `conductor_get_status` tool always returned fallback data with `connected: false` regardless of the actual device connection.

**Solution**: Update to v4.17.1, which gives the `ToolExecutor` shared daemon state references for live status reporting.

### MIDI output port not found for apps started after daemon

**Cause**: Prior to v4.17.1, MIDI output port enumeration (`connect_by_name`) did not use the macOS Core MIDI warmup pattern. Apps started after the daemon (e.g., Absynth 5 Virtual Input) would not appear in the cached port list.

**Solution**:
1. Update to v4.17.1, which adds retry-with-warmup to `connect_by_name()`
2. If still not found, restart the target application
3. As a fallback, restart the daemon after starting the target app

### Re-adding a MIDI device not detected

**Cause**: macOS Core MIDI delivers "device added" notifications only to active `MIDIClient` instances. The daemon's 5-second hot-plug rescan creates transient Core MIDI clients that are too short-lived to receive device-added notifications.

**Solution**: Update to v4.26.1, which spawns a persistent MIDI watcher thread in the daemon at startup. This thread keeps one `MidiInput` alive with `CFRunLoopRun()` for the daemon's entire lifetime, ensuring Core MIDI always has an active client to receive device-change notifications. The watcher supports clean shutdown via `CFRunLoopStop()`. (Prior to v4.26.1, the watcher was in the GUI — it was moved to the daemon where hot-plug detection actually occurs.)

### "unexpected tool_use_id" API error in long conversations

**Cause**: Prior to v4.26.43, when a conversation exceeded ~20 messages with tool calls, the 20-message context window could start with a `tool_result` whose matching assistant message (containing the `tool_use`) had been sliced off. The LLM API rejects orphaned `tool_result` blocks. An off-by-one bug in the orphan detection code prevented filtering when the orphaned message was at the start of the window.

**Solution**:
1. Update to Conductor v4.26.43 or later
2. As a workaround in older versions, start a new conversation to reset the message window

### "unexpected tool_use_id" error after Plan/Apply

**Cause**: Prior to v4.26.44, the Plan/Apply workflow did not add a `tool_result` message for the tool call that produced the plan. When the agentic loop returned early with a `PlanCreated` result, the conversation history contained an orphaned `tool_use` block. The next user message would trigger an LLM API error.

**Solution**:
1. Update to Conductor v4.26.44 or later
2. As a workaround in older versions, clear the chat and start a new conversation

### Chat stuck after plan creation

**Cause**: Prior to v4.26.44, clearing the chat or starting a new conversation did not clear the pending plan state. If a plan was pending when the chat was cleared, the UI would remain in a stuck state with no way to dismiss the plan.

**Solution**:
1. Update to Conductor v4.26.44 or later — clearing messages or starting a new conversation now clears pending plans
2. A dedicated "Cancel Plan" button appears in the chat header when a plan is pending

### Plan shows "applied successfully" but changes not saved

**Cause**: Prior to v4.26.46, `applyPendingPlan()` did not check the daemon's `PlanApplyResult.success` field. When the daemon returned `{ success: false, error: "..." }` (e.g., expired plan or TOCTOU conflict), the frontend silently showed "applied successfully" and cleared the plan.

**Solution**: Update to v4.26.46, which checks `result.success` and shows error messages in the chat when apply fails. The plan remains visible for retry.

### Plan changes show blank descriptions or missing diff preview

**Cause**: Prior to v4.26.47, `ConfigPlan` did not serialize the `diff_preview` or computed change descriptions. The `preview_diff()` method existed but was never called before serialization, and `ConfigChange` variants like `DeleteMapping`, `CreateMode`, and `DeleteMode` have no `description` field in their serde output.

**Solution**: Update to v4.26.47, which pre-computes `diff_preview` and `change_descriptions` in `ConfigPlan::new()`. The inline plan review now shows human-readable descriptions for all change types and an expandable diff preview.

### LLM stops responding after plan apply

**Cause**: Prior to v4.26.52, the agentic loop exited via `return` when a PlanCreated result was received, and nothing resumed it after the user approved or rejected the plan. The LLM could not verify changes, make follow-up suggestions, or continue the conversation.

**Solution**: Update to v4.26.52, which adds `_resumeAfterPlanDecision()`. After plan apply or reject, the agentic loop is re-invoked so the LLM can see the outcome message and continue with follow-up actions.

### Plan applied but config unchanged after restart

**Cause**: Prior to v4.26.51, the daemon's `engine_manager` did not write the modified config to disk after a successful plan apply. The `ToolExecutor` updated its in-memory config, but the changes were never synced back to the engine manager, saved to `config.toml`, or recompiled into the rule set.

**Solution**: Update to v4.26.51, which adds `sync_config_after_apply()` to the engine manager. After a plan is applied, the modified config is retrieved from the ToolExecutor, saved to disk, the rule set is recompiled and atomically swapped, and the current mode is reconciled.

### Plan deletes wrong mappings when removing multiple items

**Cause**: Prior to v4.26.48, deleting multiple mappings in the same mode within a single plan could remove the wrong items. The indices referenced the original config, but earlier deletes shifted subsequent indices during sequential application.

**Solution**: Update to v4.26.48, which sorts delete operations by descending index before applying, ensuring all indices reference the original config state.

### Tool call messages show as generic assistant messages after reload

**Cause**: Prior to v4.26.50, `_messageTypeToDbRole()` mapped `TOOL_CALL`, `PLAN_PENDING`, and `SKILL` message types to `'assistant'` or `'system'` in the database. When loading conversations, `_dbRoleToMessageType()` had no way to distinguish these from regular assistant/system messages, so tool call messages lost their type information on round-trip.

**Solution**: Update to v4.26.50, which stores each message type as a distinguishable DB role string (`'tool_call'`, `'skill'`, `'plan_pending'`, `'error'`). Existing conversations with the old role strings will continue to load correctly — the old `'assistant'` and `'system'` mappings still work, but tool call messages saved before the fix will appear as assistant messages.

### Usage and costs always show zero

**Cause**: Prior to v4.26.53, the `llm_record_cost` Tauri command existed but was never called from the frontend. Token counts from LLM responses were included in message metadata but never persisted to the costs database table.

**Solution**: Update to v4.26.53, which adds `_recordCost()` to the chat store. After each LLM response (both intermediate tool_use and final), token usage is recorded via fire-and-forget invocation. The CostSummaryPanel will then show accurate costs by provider and model.

### MIDI Learn captures only 2 notes for chords

**Cause**: Prior to v4.26.56, `midi_learn.rs` immediately completed chord learning when `held_notes.len() >= 2`. There was no debounce window to wait for additional notes.

**Solution**: Update to v4.26.56, which adds a 100ms debounce timer to chord completion. Each new note within the window cancels and restarts the timer, allowing 3+ note chords to be captured. The same fix applies to GamepadButtonChord detection.

## Configuration Issues

### Invalid configuration after apply

**Cause**: Edge case in generated configuration.

**Solution**:
1. Check the config file for syntax errors: `conductorctl validate`
2. Review the last applied changes
3. Manually fix any issues in `config.toml`

### Duplicate mappings

**Cause**: Creating a mapping that already exists.

**Solution**:
1. Ask "what mappings exist for note 36?"
2. Update the existing mapping instead:
   ```
   Change note 36 to Command+V instead
   ```

## Exporting Chat for Debugging

### Copy Chat to clipboard (v4.26.58+)

Click the **Copy** button in the chat header to copy the entire conversation as structured markdown. The export includes:

- **Metadata header**: Session ID, provider, model, and export timestamp
- **User and assistant messages**: Full message content
- **Tool calls**: Tool name with JSON-formatted arguments
- **Tool results**: Tool name, success/error status, and result data
- **Plan proposals**: Description, list of changes, and diff preview
- **Skill and system messages**: Contextual information

Prior to v4.26.58, the copy only included basic user and assistant message text.

### Enable trace-level logging

For detailed debugging of LLM requests and tool execution:

```bash
RUST_LOG=conductor_gui::llm_commands=trace conductor-gui
```

This logs:
- Full LLM request payloads (message count, model, stream flag)
- MCP tool names and full argument JSON
- Tool execution results

## Getting Help

If you continue to experience issues:

1. Check the [FAQ](faq.md)
2. Copy the chat with the **Copy** button for sharing
3. Enable trace logging: `RUST_LOG=conductor_gui::llm_commands=trace`
4. Report issues on [GitHub](https://github.com/anthropics/conductor/issues)

When reporting issues, include:
- Provider being used (OpenAI/Anthropic)
- Error message (if any)
- What you were trying to do
- Conductor version (`conductor --version`)
- Chat export (from the Copy button)
