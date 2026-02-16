//! Windows Registry-based USB/Device sound suppression
//!
//! This module provides functionality to temporarily disable Windows USB and device
//! connection/disconnection sounds during application execution, automatically
//! restoring them when the application exits.

#[cfg(windows)]
use std::collections::HashMap;
#[cfg(windows)]
use tracing::{info, warn};
#[cfg(windows)]
use winreg::enums::*;
#[cfg(windows)]
use winreg::RegKey;

/// Registry path for Windows sound events
#[cfg(windows)]
const SOUND_EVENTS_PATH: &str = r"AppEvents\Schemes\Apps\.Default";

/// USB/Device sound events to disable
#[cfg(windows)]
const DEVICE_SOUND_EVENTS: &[&str] = &[
    "DeviceConnect",
    "DeviceDisconnect",
    "DeviceFail",
    "SystemNotification",
];

/// RAII guard that backs up and disables Windows device sounds,
/// automatically restoring them when dropped.
///
/// # Example
///
/// ```no_run
/// // Sounds are disabled when guard is created
/// let _guard = SoundSuppressor::new();
///
/// // ... application runs with silent USB events ...
///
/// // Sounds are automatically restored when guard is dropped
/// ```
#[cfg(windows)]
pub struct SoundSuppressor {
    backups: HashMap<String, Option<String>>,
}

#[cfg(windows)]
impl SoundSuppressor {
    /// Create a new sound suppressor and immediately disable device sounds.
    ///
    /// This function:
    /// 1. Opens HKEY_CURRENT_USER\AppEvents\Schemes\Apps\.Default
    /// 2. For each device sound event:
    ///    - Opens the event subkey (e.g., DeviceConnect\.Current)
    ///    - Backs up the current value (or None if key doesn't exist)
    ///    - Sets the value to empty string (disables sound)
    /// 3. Returns Some(Self) on success, None on failure
    ///
    /// # Returns
    ///
    /// - `Some(SoundSuppressor)` if at least one sound was successfully disabled
    /// - `None` if registry access fails (non-fatal, application continues)
    ///
    /// # Platform
    ///
    /// Windows only - returns None on other platforms
    pub fn new() -> Option<Self> {
        match Self::disable_sounds() {
            Ok(backups) => {
                if backups.is_empty() {
                    warn!("No device sounds were disabled (all operations failed)");
                    None
                } else {
                    info!("Device sounds disabled ({} events)", backups.len());
                    Some(Self { backups })
                }
            }
            Err(e) => {
                warn!("Failed to disable device sounds: {}", e);
                None
            }
        }
    }

    /// Internal function to disable sounds and return backups
    fn disable_sounds() -> Result<HashMap<String, Option<String>>, std::io::Error> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let sound_events = hkcu.open_subkey(SOUND_EVENTS_PATH)?;
        let mut backups = HashMap::new();

        for event_name in DEVICE_SOUND_EVENTS {
            let event_path = format!(r"{}\{}", event_name, ".Current");

            // Try to open the event key
            match sound_events.open_subkey_with_flags(&event_path, KEY_READ | KEY_WRITE) {
                Ok(event_key) => {
                    // Backup current value (may not exist)
                    let current_value: Option<String> = event_key.get_value("").ok();

                    // Set to empty string to disable sound
                    if let Err(e) = event_key.set_value("", &"") {
                        warn!("Failed to disable sound for {}: {}", event_name, e);
                        continue;
                    }

                    backups.insert(event_path, current_value);
                }
                Err(e) => {
                    warn!("Failed to open sound event {}: {}", event_name, e);
                    continue;
                }
            }
        }

        Ok(backups)
    }

    /// Restore original sound settings.
    ///
    /// This method is called automatically when the `SoundSuppressor` is dropped,
    /// but can also be called manually if needed.
    ///
    /// # Note
    ///
    /// It's safe to call this multiple times - subsequent calls will have no effect.
    pub fn restore(&mut self) {
        if self.backups.is_empty() {
            return; // Already restored
        }

        match Self::restore_sounds(&self.backups) {
            Ok(count) => {
                info!("Device sounds restored ({} events)", count);
            }
            Err(e) => {
                warn!("Failed to restore device sounds: {}", e);
            }
        }

        self.backups.clear();
    }

    /// Internal function to restore sounds from backups
    fn restore_sounds(
        backups: &HashMap<String, Option<String>>,
    ) -> Result<usize, std::io::Error> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let sound_events = hkcu.open_subkey(SOUND_EVENTS_PATH)?;
        let mut restored_count = 0;

        for (event_path, original_value) in backups {
            match sound_events.open_subkey_with_flags(event_path, KEY_WRITE) {
                Ok(event_key) => {
                    match original_value {
                        Some(value) => {
                            if let Err(e) = event_key.set_value("", value) {
                                warn!("Failed to restore sound for {}: {}", event_path, e);
                                continue;
                            }
                        }
                        None => {
                            // Key didn't have a value before, delete it
                            if let Err(e) = event_key.delete_value("") {
                                warn!("Failed to delete sound value for {}: {}", event_path, e);
                                continue;
                            }
                        }
                    }
                    restored_count += 1;
                }
                Err(e) => {
                    warn!("Failed to open sound event for restore {}: {}", event_path, e);
                    continue;
                }
            }
        }

        Ok(restored_count)
    }
}

#[cfg(windows)]
impl Drop for SoundSuppressor {
    fn drop(&mut self) {
        self.restore();
    }
}

// Non-Windows stub
#[cfg(not(windows))]
pub struct SoundSuppressor;

#[cfg(not(windows))]
impl SoundSuppressor {
    pub fn new() -> Option<Self> {
        None
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn test_sound_suppressor_creation() {
        // Should create successfully (or None if no permissions)
        // Just verify it doesn't panic
        let _guard = SoundSuppressor::new();
    }

    #[test]
    fn test_sound_suppressor_restore() {
        if let Some(mut guard) = SoundSuppressor::new() {
            guard.restore();
            // Should be safe to call restore multiple times
            guard.restore();
        }
    }

    #[test]
    fn test_sound_suppressor_drop() {
        // Create guard in inner scope
        {
            let _guard = SoundSuppressor::new();
        } // Drop called here
          // Should restore sounds automatically
    }

    #[test]
    fn test_sound_suppressor_is_send() {
        // Verify SoundSuppressor can be sent between threads
        fn assert_send<T: Send>() {}
        assert_send::<SoundSuppressor>();
    }
}
