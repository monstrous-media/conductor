// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Persistent MIDI watcher thread for macOS CoreMIDI hot-plug detection (v4.26.1)
//!
//! On macOS, CoreMIDI caches the list of MIDI sources. Newly connected devices
//! (or apps that create virtual MIDI ports after the daemon starts) are only
//! discovered when an active `MIDIClient` receives a "device added" notification.
//! midir creates CoreMIDI clients without notification callbacks, but simply
//! keeping a long-lived `MidiInput` alive and running `CFRunLoopRun()` on the
//! same thread is sufficient — CoreMIDI delivers notifications to any active
//! client's run loop, which updates the global source/destination cache.
//!
//! Device **removal** works via IOKit (push-based) and does not require this
//! watcher. Device **addition** is pull-based and requires an active client.
//!
//! The daemon's 5-second `HotPlugCheck` rescan creates transient `MidiInput`
//! instances that are too short-lived to receive notifications. This watcher
//! solves the problem by maintaining a persistent client for the daemon's
//! entire lifetime.
//!
//! On non-macOS platforms this is a no-op.
//!
//! See: GitHub issue #116, v4.18.3 (GUI watcher), ADR-009 Phase 4a (hot-plug)

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::debug;
#[cfg(target_os = "macos")]
use tracing::{error, warn};

/// Handle to a running MIDI watcher thread.
///
/// The watcher thread runs until this handle is dropped or [`MidiWatcherHandle::stop`]
/// is called. On non-macOS platforms, no thread is spawned.
///
/// On macOS, dropping the handle calls `CFRunLoopStop` on the watcher thread's
/// run loop and joins the thread for clean shutdown.
pub struct MidiWatcherHandle {
    running: Arc<AtomicBool>,
    /// CFRunLoopRef for the watcher thread, used to call CFRunLoopStop from any thread.
    #[cfg(target_os = "macos")]
    run_loop_ref: Arc<std::sync::atomic::AtomicPtr<std::ffi::c_void>>,
    /// Join handle for the watcher thread.
    #[cfg(target_os = "macos")]
    thread: Option<std::thread::JoinHandle<()>>,
}

// Explicitly link CoreFoundation for CFRunLoop APIs.
// midir links it transitively via CoreMIDI, but explicit is more robust.
#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRunLoopRun();
    fn CFRunLoopGetCurrent() -> *mut std::ffi::c_void;
    fn CFRunLoopStop(rl: *mut std::ffi::c_void);
}

impl MidiWatcherHandle {
    /// Whether the watcher is currently active.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Signal the watcher to stop and join the thread.
    ///
    /// On macOS, this calls `CFRunLoopStop` to unblock the run loop, then
    /// joins the thread. On other platforms, this is a no-op.
    #[allow(clippy::needless_return)]
    pub fn stop(&mut self) {
        if !self.running.swap(false, Ordering::SeqCst) {
            return; // Already stopped
        }

        #[cfg(target_os = "macos")]
        {
            // Wait briefly for the thread to store its CFRunLoopRef.
            // The thread stores it before entering CFRunLoopRun(), but there's
            // a small window between spawn and store. 100ms is generous.
            for _ in 0..100 {
                if !self.run_loop_ref.load(Ordering::Acquire).is_null() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }

            // Stop the CFRunLoop so the thread can exit
            let rl = self.run_loop_ref.load(Ordering::Acquire);
            if !rl.is_null() {
                // SAFETY: CFRunLoopStop is safe to call from any thread.
                // The CFRunLoopRef remains valid for the lifetime of the
                // watcher thread (which we're about to join).
                unsafe {
                    CFRunLoopStop(rl);
                }
            } else {
                warn!("MIDI watcher run loop ref not set; thread may not stop cleanly");
            }

            // Join the thread for clean resource cleanup
            if let Some(thread) = self.thread.take() {
                if let Err(e) = thread.join() {
                    error!("Failed to join MIDI watcher thread: {:?}", e);
                } else {
                    debug!("MIDI watcher thread stopped gracefully");
                }
            }
        }
    }
}

impl Drop for MidiWatcherHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawn a persistent MIDI watcher thread that keeps CoreMIDI's device cache
/// up to date for the lifetime of the returned handle.
///
/// On macOS: creates a long-lived `MidiInput` and runs `CFRunLoopRun()` so
/// that CoreMIDI delivers device-change notifications to the process.
///
/// On other platforms: returns `None` (no watcher needed).
///
/// Returns `None` if the watcher thread fails to spawn. The daemon continues
/// to function without it — the 5-second `HotPlugCheck` rescan still works,
/// but newly connected devices may not be detected until a daemon restart.
#[must_use = "dropping the handle immediately stops the watcher; \
              bind with `let _watcher = ...` to keep alive"]
pub fn start_midi_watcher() -> Option<MidiWatcherHandle> {
    #[cfg(target_os = "macos")]
    {
        use std::sync::atomic::AtomicPtr;

        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);
        let run_loop_ref = Arc::new(AtomicPtr::new(std::ptr::null_mut()));
        let run_loop_clone = Arc::clone(&run_loop_ref);

        match std::thread::Builder::new()
            .name("conductor-midi-watcher".to_string())
            .spawn(move || {
                // Capture this thread's CFRunLoopRef before entering the run loop,
                // so the handle can call CFRunLoopStop from another thread.
                // SAFETY: CFRunLoopGetCurrent returns the current thread's run loop,
                // which is auto-created and valid for the thread's lifetime.
                let current_rl = unsafe { CFRunLoopGetCurrent() };
                run_loop_clone.store(current_rl, Ordering::Release);

                midi_watcher_thread(running_clone);
            }) {
            Ok(thread) => Some(MidiWatcherHandle {
                running,
                run_loop_ref,
                thread: Some(thread),
            }),
            Err(e) => {
                warn!(
                    "Failed to spawn MIDI watcher thread: {}. \
                     Hot-plug detection may be delayed.",
                    e
                );
                None
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        debug!("MIDI watcher not needed on this platform");
        None
    }
}

/// The actual watcher thread function (macOS only).
#[cfg(target_os = "macos")]
fn midi_watcher_thread(running: Arc<AtomicBool>) {
    use midir::MidiInput;

    match MidiInput::new("Conductor Persistent Watcher") {
        Ok(persistent) => {
            let port_count = persistent.ports().len();
            debug!(
                ports = port_count,
                "MIDI watcher thread started — persistent CoreMIDI client active"
            );

            // Keep the MidiInput alive for the lifetime of this thread.
            // CoreMIDI delivers device-change notifications to active clients.
            let _persistent = persistent;

            debug!("MIDI watcher: starting CFRunLoop for CoreMIDI notifications");
            // SAFETY: CFRunLoopRun is a well-defined CoreFoundation API that blocks
            // the current thread, processing run loop sources (including CoreMIDI
            // device notifications). Returns when CFRunLoopStop is called from
            // MidiWatcherHandle::stop/drop, or when the process exits.
            unsafe {
                CFRunLoopRun();
            }

            running.store(false, Ordering::Release);
            debug!("MIDI watcher thread exiting");
        }
        Err(e) => {
            warn!("Failed to create persistent MIDI watcher client: {}", e);
            running.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Integration test: spawns a real MIDI watcher thread with CoreMIDI.
    /// Ignored by default because it interacts with the OS MIDI subsystem
    /// and CFRunLoopStop may not return immediately if CoreMIDI has active
    /// notification sources. Run manually with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn test_start_midi_watcher_returns_handle() {
        let handle = start_midi_watcher();
        #[cfg(target_os = "macos")]
        {
            let mut handle = handle.expect("should spawn on macOS");
            assert!(handle.is_running());
            handle.stop();
            assert!(!handle.is_running());
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert!(handle.is_none());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_handle_stop_with_run_loop() {
        use std::sync::atomic::AtomicPtr;

        let running = Arc::new(AtomicBool::new(true));
        let run_loop_ref = Arc::new(AtomicPtr::new(std::ptr::null_mut()));
        let mut handle = MidiWatcherHandle {
            running: Arc::clone(&running),
            run_loop_ref,
            thread: None,
        };
        assert!(handle.is_running());
        handle.stop();
        assert!(!handle.is_running());
        assert!(!running.load(Ordering::Acquire));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_handle_drop_stops() {
        use std::sync::atomic::AtomicPtr;

        let running = Arc::new(AtomicBool::new(true));
        {
            let _handle = MidiWatcherHandle {
                running: Arc::clone(&running),
                run_loop_ref: Arc::new(AtomicPtr::new(std::ptr::null_mut())),
                thread: None,
            };
            assert!(running.load(Ordering::Acquire));
        }
        // After drop, flag should be false
        assert!(!running.load(Ordering::Acquire));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_double_stop_is_safe() {
        use std::sync::atomic::AtomicPtr;

        let running = Arc::new(AtomicBool::new(true));
        let mut handle = MidiWatcherHandle {
            running: Arc::clone(&running),
            run_loop_ref: Arc::new(AtomicPtr::new(std::ptr::null_mut())),
            thread: None,
        };
        handle.stop();
        handle.stop(); // Should not panic
        assert!(!handle.is_running());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_non_macos_returns_none() {
        assert!(start_midi_watcher().is_none());
    }
}
