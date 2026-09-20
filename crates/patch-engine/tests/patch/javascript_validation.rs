// Included as patch unit tests to exercise the private syntax gate without patching files.
use super::*;

#[test]
#[ignore = "requires KIRO_TEST_EXTENSION pointing at an installed Kiro extension; read-only"]
fn installed_electron_checks_javascript_without_path() {
    let extension =
        PathBuf::from(std::env::var_os("KIRO_TEST_EXTENSION").expect("KIRO_TEST_EXTENSION"));
    let before = fs::read(&extension).unwrap();
    for (source, valid) in [
        (
            "throw new Error('syntax check must not execute code');",
            true,
        ),
        ("const broken = ;", false),
    ] {
        let mut command = javascript_command(&extension).unwrap();
        assert!(Path::new(command.get_program()).is_absolute());
        command
            .env("PATH", "")
            .env("NODE_OPTIONS", "--require=must-not-load");
        assert_eq!(check_javascript(command, source).is_ok(), valid);
    }
    assert_eq!(fs::read(&extension).unwrap(), before);
}

#[test]
fn real_layout_rejects_missing_or_unsafe_runtime_without_node_fallback() {
    let root = std::env::temp_dir().join(format!("kiro-parser-test-{}", std::process::id()));
    let resources = if cfg!(target_os = "macos") {
        root.join("Contents/Resources")
    } else {
        root.join("resources")
    };
    let app = resources.join("app");
    let extension = app.join("extensions/kiro.kiro-agent/dist/extension.js");
    fs::create_dir_all(extension.parent().unwrap()).unwrap();
    fs::write(&extension, "const ok = true;").unwrap();
    fs::write(app.join("product.json"), r#"{"applicationName":"kiro"}"#).unwrap();
    assert!(matches!(
        javascript_command(&extension),
        Err(PatchError::InvalidJavaScript(_))
    ));
    let executable = if cfg!(windows) {
        root.join("Kiro.exe")
    } else if cfg!(target_os = "macos") {
        root.join("Contents/MacOS/Electron")
    } else {
        root.join("kiro")
    };
    fs::create_dir_all(executable.parent().unwrap()).unwrap();
    let fuse_binary = if cfg!(target_os = "macos") {
        fs::write(&executable, b"launcher stub without fuses").unwrap();
        root.join("Contents/Frameworks/Electron Framework.framework/Versions/A/Electron Framework")
    } else {
        executable.clone()
    };
    fs::create_dir_all(fuse_binary.parent().unwrap()).unwrap();
    let sentinel = b"dL7pKGdnNz796PbbjQWNKmHXBZaB9tsX";
    for trailer in [vec![], vec![1, 1, b'0'], vec![2, 1, b'1'], vec![1, 0, b'1']] {
        let mut bytes = sentinel.to_vec();
        bytes.extend(trailer);
        fs::write(&fuse_binary, bytes).unwrap();
        assert!(javascript_command(&extension).is_err());
    }
    // Enabled fuse crossing a read boundary: inspect only, never run this fixture.
    let mut bytes = vec![0; 65536 - 10];
    bytes.extend(sentinel);
    bytes.extend([1, 1, b'1']);
    fs::write(&fuse_binary, bytes).unwrap();
    assert!(javascript_command(&extension).is_ok());
    fs::write(app.join("product.json"), r#"{"applicationName":"other"}"#).unwrap();
    assert!(javascript_command(&extension).is_err());
    if cfg!(target_os = "macos") {
        fs::remove_file(&fuse_binary).unwrap();
        let contents = root.join("Contents");
        for dir in fuse_binary
            .parent()
            .unwrap()
            .ancestors()
            .take_while(|p| *p != contents)
        {
            fs::remove_dir(dir).unwrap();
        }
    }
    fs::remove_file(&executable).unwrap();
    if cfg!(target_os = "macos") {
        fs::remove_dir(executable.parent().unwrap()).unwrap();
    }
    // Explicitly remove only the files/directories created by this test.
    fs::remove_file(&extension).unwrap();
    fs::remove_file(app.join("product.json")).unwrap();
    for dir in [
        extension.parent().unwrap().to_path_buf(),
        app.join("extensions/kiro.kiro-agent"),
        app.join("extensions"),
        app,
        resources,
    ] {
        fs::remove_dir(dir).unwrap();
    }
    if cfg!(target_os = "macos") {
        fs::remove_dir(root.join("Contents")).unwrap();
    }
    fs::remove_dir(root).unwrap();
}

#[test]
fn standalone_fixture_keeps_node_fallback() {
    let path = std::env::temp_dir().join(format!("kiro-js-fixture-{}.js", std::process::id()));
    fs::write(&path, "const fixture = true;").unwrap();
    assert_eq!(javascript_command(&path).unwrap().get_program(), "node");
    fs::remove_file(path).unwrap();
}

#[test]
fn macos_framework_fuse_is_separate_from_stub_and_confined_to_installation() {
    let root = std::env::temp_dir().join(format!("kiro-macos-fuse-{}", std::process::id()));
    let contents = root.join("Kiro.app/Contents");
    let stub = contents.join("MacOS/Kiro");
    let framework =
        contents.join("Frameworks/Electron Framework.framework/Versions/A/Electron Framework");
    fs::create_dir_all(stub.parent().unwrap()).unwrap();
    fs::create_dir_all(framework.parent().unwrap()).unwrap();
    fs::write(&stub, b"launcher stub without fuses").unwrap();
    assert!(macos_fuse_binary(&contents).is_err());
    let enabled = b"dL7pKGdnNz796PbbjQWNKmHXBZaB9tsX\x01\x011";
    fs::write(&framework, enabled).unwrap();
    let resolved = macos_fuse_binary(&contents).unwrap();
    assert_eq!(resolved, fs::canonicalize(&framework).unwrap());
    assert!(verify_run_as_node(&resolved).is_ok());
    assert!(verify_run_as_node(&stub).is_err());
    fs::write(&framework, b"dL7pKGdnNz796PbbjQWNKmHXBZaB9tsX\x01\x010").unwrap();
    assert!(verify_run_as_node(&resolved).is_err());
    fs::remove_file(&framework).unwrap();
    let outside = root.join("outside-framework");
    fs::write(&outside, enabled).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&outside, &framework).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &framework).unwrap();
    assert!(macos_fuse_binary(&contents).is_err());
    fs::remove_file(&framework).unwrap();
    fs::remove_file(outside).unwrap();
    fs::remove_file(&stub).unwrap();
    fs::remove_dir(stub.parent().unwrap()).unwrap();
    for dir in framework
        .parent()
        .unwrap()
        .ancestors()
        .take_while(|p| *p != root)
    {
        fs::remove_dir(dir).unwrap();
    }
    fs::remove_dir(root).unwrap();
}
