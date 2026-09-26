//! What a takeover leaves on the machine when its rollback records are gone.
//!
//! The snapshot and the desktop session are how a takeover is normally found and
//! undone. Lose them (a crash, a cleaner, a reinstall of this client) and the machine
//! looks clean while Kiro is still patched, its settings still send the customer's own
//! traffic to the gateway, and the token Kiro holds is the gateway's. This finds those
//! from the files themselves, and undoes what can be undone without the records.

use crate::detect::{get_candidate_install_paths, inspect_installation_dir};
use crate::patch::{ExtensionPatcher, PatchOwnership};
use crate::settings::{Profile, SettingsManager};
use crate::token_storage::TokenStorage;
use std::path::{Path, PathBuf};

/// The account in the profile ARNs the gateway issues. A token carrying it is ours.
const GATEWAY_ACCOUNT: &str = ":123456789012:";

/// What was found. Another user's patch on a shared install is not among it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Leftovers {
    /// Patched extension bundles of this user that can be rolled back.
    pub patches: Vec<PathBuf>,
    /// Patched bundles whose rollback material is gone or no longer matches. Only
    /// replacing the file, by reinstalling or updating Kiro, undoes them.
    pub unrecoverable: Vec<PathBuf>,
    /// settings.json still points Kiro at a gateway this client knows.
    pub settings: bool,
    /// The settings.json of other Kiro profiles that still point Kiro at one.
    pub profile_settings: Vec<PathBuf>,
    /// The token Kiro holds was issued by the gateway.
    pub token: bool,
}

impl Leftovers {
    /// Look for leftovers in the extension bundles at `extensions`, the settings and
    /// the token. `gateway_hosts` are the gateways this client could have used.
    pub fn scan(
        extensions: &[PathBuf],
        settings: &SettingsManager,
        token: &TokenStorage,
        gateway_hosts: &[String],
    ) -> Self {
        let mut found = Self::default();
        for path in extensions {
            let patcher = ExtensionPatcher::new(path);
            if patcher.ownership() != PatchOwnership::Ours {
                continue;
            }
            // A backup that is gone or no longer matches is as lost as no material at
            // all: counted as restorable, it made restore fail on every attempt with
            // no word of the reinstall that alone can help.
            if patcher.restore_material_is_lost() {
                found.unrecoverable.push(path.clone());
            } else {
                found.patches.push(path.clone());
            }
        }
        found.settings = settings.names_gateway(gateway_hosts);
        found.profile_settings = settings
            .profile_files()
            .into_iter()
            .filter(|profile| profile.settings.names_gateway(gateway_hosts))
            .map(|profile| profile.settings.path().to_path_buf())
            .collect();
        found.token = token
            .load()
            .is_ok_and(|token| token.profile_arn.contains(GATEWAY_ACCOUNT));
        found
    }

    pub fn found(&self) -> bool {
        !self.patches.is_empty()
            || !self.unrecoverable.is_empty()
            || self.settings
            || !self.profile_settings.is_empty()
            || self.token
    }

    /// Undo what was found. Kiro must not be running.
    ///
    /// Bundles are rolled back from their own material first, then the settings and
    /// the token are cleared. While a bundle that cannot be rolled back remains, the
    /// settings and the gateway's token stay, as a restore with records keeps them: that
    /// bundle still sends Kiro's runtime calls to the gateway, so a fresh official sign-in
    /// would carry the customer's own token there. Reinstalling Kiro replaces it, and the
    /// same cleanup then completes.
    pub fn remove(
        &self,
        settings: &SettingsManager,
        token: &TokenStorage,
        gateway_hosts: &[String],
    ) -> Result<(), String> {
        // Settings no edit can safely change (a syntax error) fail the cleanup before any
        // file changes, rather than leaving Kiro's official bundle with the gateway's
        // settings and token.
        if self.settings {
            settings
                .plan_orphan_removal(gateway_hosts)
                .map_err(|error| error.to_string())?;
        }
        let profiles: Vec<Profile> = settings
            .profile_files()
            .into_iter()
            .filter(|profile| {
                self.profile_settings
                    .contains(&profile.settings.path().to_path_buf())
            })
            .collect();
        for profile in &profiles {
            profile
                .settings
                .plan_orphan_removal(gateway_hosts)
                .map_err(|error| error.in_profile(&profile.name).to_string())?;
        }
        for path in &self.patches {
            ExtensionPatcher::new(path)
                .restore()
                .map_err(|error| error.to_string())?;
        }
        if !self.unrecoverable.is_empty() {
            return Err(
                "Kiro's extension is still modified and its backup is gone; reinstall Kiro to replace it"
                    .into(),
            );
        }
        if self.settings {
            settings
                .remove_orphaned_takeover(gateway_hosts)
                .map_err(|error| error.to_string())?;
        }
        for profile in &profiles {
            profile
                .settings
                .remove_orphaned_takeover(gateway_hosts)
                .map_err(|error| error.in_profile(&profile.name).to_string())?;
        }
        if self.token {
            token.clear().map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

/// extension.js of every Kiro this client may have patched: the chosen install, the
/// one named by SUPERKIRO_INSTALL_DIR, and each standard location. A takeover may
/// have patched another install than the one found now.
pub fn candidate_extensions(custom: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = custom.map(Path::to_path_buf).into_iter().collect();
    dirs.extend(std::env::var_os("SUPERKIRO_INSTALL_DIR").map(PathBuf::from));
    dirs.extend(get_candidate_install_paths());
    let mut extensions: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        let Some(agent) = inspect_installation_dir(&dir)
            .ok()
            .and_then(|install| install.agent_extension_dir)
        else {
            continue;
        };
        let extension = agent.join("dist").join("extension.js");
        let identity = std::fs::canonicalize(&extension).unwrap_or_else(|_| extension.clone());
        if !extensions
            .iter()
            .any(|known| std::fs::canonicalize(known).unwrap_or_else(|_| known.clone()) == identity)
        {
            extensions.push(extension);
        }
    }
    extensions
}
