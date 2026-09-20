//! `patch-engine`: Kiro IDE installation detection, versioning, environment injection, and patching.

pub mod auth;
pub mod beacon;
pub mod desktop;
pub mod detect;
pub mod device;
pub mod doctor;
pub mod mem_guard;
pub mod patch;
pub mod preferences;
pub mod process;
pub mod runtime;
pub mod settings;
pub mod snapshot;
pub mod token_storage;
pub mod white_label;

pub use mem_guard::{
    CacheCleanResult, MemGuardConfig, MemoryGuard, MemorySnapshot, OrphanPurgeResult,
    ProcessCategory, ProcessMemoryInfo, TrimResult,
};

pub use auth::{AuthClient, AuthClientError, ClientLoginRequest, ClientRefreshRequest};
pub use beacon::{
    BeaconClient, BeaconError, ClientNegotiateRequest, ClientNegotiateResponse, HealthBeacon,
};
pub use detect::{
    detect_kiro, get_candidate_install_paths, inspect_installation_dir, DetectError,
    KiroInstallation,
};
pub use device::{generate_device_fingerprint, is_valid_device_fingerprint, DEVICE_ID_PREFIX};
pub use doctor::{CheckItem, CheckLevel, Doctor, DoctorError, DoctorReport, TakeoverStatus};
pub use patch::{
    get_launcher_env, ExtensionPatcher, PatchError, PatchRecipe, PatchStatus, BACKUP_SUFFIX,
    PATCH_MARKER_V1, RUNTIME_ENDPOINT_NEEDLE,
};
pub use preferences::{
    default_preferences_path, Announcement, ClientModelCatalogItem, ClientPreferences, Language,
    PreferencesError, ReasoningEffort, UiOverrides, DEFAULT_OFFLINE_GRACE_SECONDS,
};
pub use process::{launch_kiro, restart_kiro, stop_kiro, ProcessError};
pub use runtime::{
    detect_kiro_process_state, ensure_kiro_stopped, is_kiro_running, is_pid_alive,
    is_process_running_by_name, ProcessState, SingleInstanceError, SingleInstanceLock,
};
pub use settings::{
    default_settings_path, PriorSettingsState, SettingsError, SettingsManager, MANAGED_KEYS,
};
pub use snapshot::{
    RestoreSummary, SnapshotError, SnapshotManager, TakeoverSnapshot, SNAPSHOT_FILENAME,
};
pub use token_storage::{
    default_token_path, parse_iso8601_to_epoch, KiroAuthToken, TokenStorage, TokenStorageError,
    KIRO_TOKEN_FILENAME,
};
pub use white_label::{default_brand_config_path, ThemeMode, WhiteLabelConfig, WhiteLabelError};

#[allow(dead_code)] // ponytail: reserved for future Win32 named mutex single-instance
pub const WIN32_MUTEX_NAME: &str = "kiro";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_win32_mutex_name() {
        assert_eq!(WIN32_MUTEX_NAME, "kiro");
    }
}

mod http;

#[cfg(windows)]
mod windows_process;
