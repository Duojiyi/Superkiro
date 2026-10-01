// Read-only excerpts from Kiro 1.2.4 / kiro-agent 1.1.237. Minified names are
// fixtures, not patch needles: the production patch must remain name-independent.
use super::*;
use crate::settings::SettingsManager;

const ACTIVITY: &str = r#"s=n!==void 0&&c7i.has(n)?this.routing.endpointOverride||`https://runtime.${n}.kiro.dev`:void 0;"#;
const ROUTING: &str = r#"
function r3o(t,e){let r=VDl(e)??"us-east-1";return{region:r,endpoint:e.endpointOverride||`https://${t}.${r}.kiro.dev`}}
function Xee(t){return r3o("management",t)}function Zee(t){return r3o("runtime",t)}
function J7d(t,e,r){return r?e??[]:t?.globalValue??t?.defaultValue??[]}
"#;
const HTTPS_TUNNEL: &str = r#"g3o.connect({...y3o(A3o(r),"host","path","port"),socket:s})"#;
const SOCKS_TUNNEL: &str = r#"KLl.connect({...ZLl(JLl(r),"host","path","port"),socket:h})"#;

#[test]
fn kiro_124_routes_using_the_same_global_settings_as_older_versions() {
    let root = std::env::temp_dir().join(format!("kiro-124-routing-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("settings.json");
    let original = "{\n  // Keep the user's preferences.\n  \"editor.fontSize\": 15\n}\n";
    fs::write(&path, original).unwrap();
    let manager = SettingsManager::at(&path);
    let prior = manager.merge_byok("https://gateway.invalid").unwrap();
    let settings = manager.read_settings().unwrap();
    let source = format!(
        r#"
const assert=require('node:assert/strict');
const config={};
function VDl(e){{return e.regionOverride}}
{}
for(const trusted of [true,false]){{
  for(const key of ['endpoints','krsEndpoints','cpsEndpoints']){{
    const selected=J7d({{globalValue:config[key]}},config[key],trusted);
    assert.equal(selected[0].region,'us-east-1');
    assert.equal(selected[0].endpoint,'https://gateway.invalid');
  }}
  // In an untrusted workspace, Kiro ignores a workspace endpoint override.
  const workspace=[{{region:'us-east-1',endpoint:'https://workspace.invalid'}}];
  const chosen=J7d({{globalValue:config.krsEndpoints}},workspace,false)[0];
  const options={{regionOverride:chosen.region,endpointOverride:chosen.endpoint}};
  assert.equal(Zee(options).endpoint,'https://gateway.invalid');
  assert.equal(Xee(options).endpoint,'https://gateway.invalid');
}}
// Other users with no takeover retain Kiro's own default endpoints.
assert.equal(Zee({{}}).endpoint,'https://runtime.us-east-1.kiro.dev');
assert.equal(Xee({{}}).endpoint,'https://management.us-east-1.kiro.dev');
"#,
        settings["codewhisperer.config"], ROUTING
    );
    let output = Command::new("node").arg("-e").arg(source).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    manager.revert(&prior, "https://gateway.invalid").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    fs::remove_file(path).unwrap();
    fs::remove_dir(root).unwrap();
}

#[test]
fn kiro_124_activity_and_proxy_shapes_keep_the_existing_safe_patch() {
    let source = format!("function activity(n,c7i){{let {ACTIVITY}return s;}}\n{ROUTING}\nfunction https(r,s){{return {HTTPS_TUNNEL};}}\nfunction socks(r,h){{return {SOCKS_TUNNEL};}}");
    let rendered = render_patch_for(
        &source,
        "https://gateway.invalid",
        &PatchRecipe::default(),
        Some("owner"),
    )
    .unwrap();
    assert!(
        rendered.contains(ROUTING),
        "ordinary SDK routing must not be rewritten"
    );
    assert!(rendered.contains(&HTTPS_TUNNEL.replace("\"host\",", "")));
    assert!(rendered.contains(&SOCKS_TUNNEL.replace("\"host\",", "")));
    assert_eq!(repair_proxy_tls(&rendered), rendered);
    let home = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let source = format!(
        r#"
const assert=require('node:assert/strict');
{}
const regions=new Set(['us-east-1']);
process.env.{}='owner';
delete process.env.KIRO_GATEWAY_URL;
assert.equal(activity.call({{routing:{{}}}},'us-east-1',regions),'https://gateway.invalid');
assert.equal(activity.call({{routing:{{endpointOverride:'https://explicit.invalid'}}}},'us-east-1',regions),'https://explicit.invalid');
assert.equal(activity.call({{routing:{{}}}},'unsupported',regions),undefined);
process.env.{}='another-user';
assert.equal(activity.call({{routing:{{}}}},'us-east-1',regions),'https://runtime.us-east-1.kiro.dev');
"#,
        rendered, home, home
    );
    let output = Command::new("node").arg("-e").arg(source).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "requires KIRO_TEST_EXTENSION; checks actual installed bundle read-only"]
fn installed_takeover_preflight_does_not_change_the_bundle() {
    let path = PathBuf::from(std::env::var_os("KIRO_TEST_EXTENSION").unwrap());
    let before = fs::read(&path).unwrap();
    let source = std::str::from_utf8(&before).unwrap();
    for excerpt in [ACTIVITY, HTTPS_TUNNEL, SOCKS_TUNNEL] {
        assert!(
            source.contains(excerpt),
            "installed Kiro differs from the 1.2.4 fixture"
        );
    }
    for excerpt in ROUTING.lines().filter(|line| !line.is_empty()) {
        assert!(
            source.contains(excerpt),
            "installed routing differs from the 1.2.4 fixture"
        );
    }
    let patcher = ExtensionPatcher::new(&path);
    let prepared = patcher.prepare("https://gateway.invalid").unwrap();
    assert!(patcher.is_as_prepared(&prepared).unwrap());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn older_kiro_versions_remain_supported_alongside_124() {
    for version in ["1.1.14", "1.1.70", "1.2.0", "1.2.4"] {
        assert!(crate::detect::kiro_version_is_supported(version));
    }
    for version in ["1.1.13", "1.2", "unknown"] {
        assert!(!crate::detect::kiro_version_is_supported(version));
    }
}
