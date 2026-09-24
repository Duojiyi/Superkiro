use super::*;

fn assert_private(path: &Path, directory: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            if directory { 0o700 } else { 0o600 }
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Inspect actual OS security descriptors, not icacls exit status alone.
        let output = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "$ErrorActionPreference='Stop'; $a=if ($env:AUDIT_DIRECTORY -eq 'true') {[System.IO.Directory]::GetAccessControl($env:AUDIT_PRIVATE_PATH)} else {[System.IO.File]::GetAccessControl($env:AUDIT_PRIVATE_PATH)}; $sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value; if (!$a.AreAccessRulesProtected) {throw 'inherited ACL'}; $r=@($a.GetAccessRules($true,$true,[System.Security.Principal.SecurityIdentifier])); if ($r.Count -ne 1 -or $r[0].IdentityReference.Value -ne $sid -or $r[0].IsInherited -or $r[0].AccessControlType -ne 'Allow' -or $r[0].FileSystemRights -ne 'FullControl') {throw 'unexpected ACL'}; if ($env:AUDIT_DIRECTORY -eq 'true' -and ($r[0].InheritanceFlags.ToString() -notmatch 'ContainerInherit')) {throw 'directory inheritance missing'}"])
            .env("AUDIT_PRIVATE_PATH", path).env("AUDIT_DIRECTORY", directory.to_string())
            .creation_flags(0x08000000).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn audit_snapshot_and_staging_permissions_and_replace_cleanup() {
    let root = std::env::temp_dir().join(format!("audit-privacy-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let staging = root.join("staging");
    fs::create_dir(&staging).unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Explicit grants must be removed too, not merely inherited ACEs.
        assert!(std::process::Command::new("icacls")
            .arg(&staging)
            .args(["/grant", "*S-1-1-0:(OI)(CI)(RX)"])
            .creation_flags(0x08000000)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap()
            .success());
    }
    restrict_private(&staging, true).unwrap();
    assert_private(&staging, true);
    let temp = staging.join("empty");
    fs::write(&temp, b"").unwrap();
    restrict_private(&temp, false).unwrap();
    assert_private(&temp, false);
    fs::remove_file(temp).unwrap();
    fs::remove_dir(staging).unwrap();

    let nested = root.join("private/nested");
    create_private_parents(&nested).unwrap();
    assert_private(nested.parent().unwrap(), true);
    assert_private(&nested, true);
    fs::remove_dir(&nested).unwrap();
    fs::remove_dir(nested.parent().unwrap()).unwrap();

    let path = root.join("snapshot.json");
    let manager = crate::SnapshotManager::at(&path);
    let snapshot = crate::TakeoverSnapshot {
        created_at: 1,
        gateway_url: "https://fixture.invalid".into(),
        settings_path: root.join("settings.json"),
        extension_path: None,
        settings_state: crate::PriorSettingsState {
            prior_raw: Some(b"{\"proxySecret\":\"fixture\"}".to_vec()),
            ..Default::default()
        },
    };
    // Replacement must also repair a pre-existing permissive destination.
    fs::write(&path, b"old").unwrap();
    for _ in 0..2 {
        manager.save(&snapshot).unwrap();
        assert_eq!(manager.load().unwrap(), snapshot);
        assert_private(&path, false);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    }
    let blocked = root.join("blocked");
    fs::create_dir(&blocked).unwrap();
    assert!(private_atomic_write(&blocked, b"fixture secret").is_err());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
    fs::remove_dir(blocked).unwrap();
    fs::remove_file(path).unwrap();
    fs::remove_dir(root).unwrap();
}

#[test]
fn audit_macos_launchers_share_resolution() {
    let root = std::env::temp_dir().join(format!("audit-mac-{}", std::process::id()));
    let macos = root.join("MacOS");
    fs::create_dir_all(&macos).unwrap();
    assert!(crate::detect::resolve_macos_executable(&root).is_err());
    let electron = macos.join("Electron");
    let kiro = macos.join("Kiro");
    fs::write(&electron, b"fixture; never execute").unwrap();
    assert_eq!(
        crate::detect::resolve_macos_executable(&root).unwrap(),
        electron
    );
    fs::write(&kiro, b"fixture; never execute").unwrap();
    assert_eq!(
        crate::detect::resolve_macos_executable(&root).unwrap(),
        kiro
    );
    fs::remove_file(kiro).unwrap();
    fs::remove_file(electron).unwrap();
    fs::remove_dir(macos).unwrap();
    fs::remove_dir(root).unwrap();
}

/// Private writes must not depend on a shell. They used to rewrite the DACL through
/// PowerShell twice per write, which Constrained Language Mode and AppLocker block —
/// and that made takeover impossible on hardened machines. The child re-runs the
/// private-write flow with PowerShell unreachable; it must still succeed and still
/// produce an owner-only DACL.
#[cfg(windows)]
#[test]
fn private_writes_do_not_need_powershell() {
    if std::env::var_os("SUPERKIRO_NO_SHELL_FIXTURE").is_some() {
        let root = std::env::temp_dir().join(format!("audit-noshell-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        let path = root.join("secret.json");
        private_atomic_write(&path, b"fixture secret").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"fixture secret");
        fs::remove_dir_all(&root).unwrap();
        return;
    }
    let empty = std::env::temp_dir().join(format!("audit-empty-path-{}", std::process::id()));
    fs::create_dir_all(&empty).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "token_storage::audit_privacy::private_writes_do_not_need_powershell",
            "--nocapture",
        ])
        .env("SUPERKIRO_NO_SHELL_FIXTURE", "1")
        // PowerShell lives outside System32 proper, so an empty PATH makes it
        // unreachable, as a blocking policy would.
        .env("PATH", &empty)
        .output()
        .unwrap();
    fs::remove_dir_all(&empty).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // The ACL itself is checked by the oracle above; this only proves no shell.
    let root = std::env::temp_dir().join(format!("audit-noshell-acl-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();
    let path = root.join("secret.json");
    private_atomic_write(&path, b"fixture").unwrap();
    assert_private(&path, false);
    fs::remove_dir_all(&root).unwrap();
}

/// A token put back on restore takes the permissions Kiro's own write gives it, inherited
/// from its directory, not the owner-only ACL used while the file held the gateway's token.
#[cfg(windows)]
#[test]
fn a_restored_file_inherits_its_directory_permissions_again() {
    use std::os::windows::process::CommandExt;
    let dir = std::env::temp_dir().join(format!("audit-inherit-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let token = dir.join("kiro-auth-token.json");
    private_atomic_write(&token, b"{\"gateway\":true}").unwrap();
    assert_private(&token, false);
    inherited_atomic_write(&token, b"{\"official\":true}").unwrap();
    assert_eq!(fs::read(&token).unwrap(), b"{\"official\":true}");
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command",
            "$ErrorActionPreference='Stop'; $a=[System.IO.File]::GetAccessControl($env:AUDIT_PATH); if ($a.AreAccessRulesProtected) {throw 'protected ACL'}; if (@($a.GetAccessRules($true,$true,[System.Security.Principal.SecurityIdentifier]) | Where-Object { -not $_.IsInherited }).Count -ne 0) {throw 'explicit ACE left'}"])
        .env("AUDIT_PATH", &token)
        .creation_flags(0x08000000)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::read_dir(&dir).unwrap().count() == 1,
        "no temporary file left behind"
    );
    let _ = fs::remove_dir_all(&dir);
}
