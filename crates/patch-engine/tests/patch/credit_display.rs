use super::*;

// Extracted read-only from Kiro 1.1.14. Keep the real formatter body and raw
// precision mapping so tests catch accidental rounding of the usage model.
const FIXTURE: &str = r#"
const endpoint=`https://runtime.${t}.kiro.dev`;
function mapUsage(t){return {currentUsage:t.currentUsageWithPrecision??t.currentUsage??0,usageLimit:t.usageLimitWithPrecision??t.usageLimit??0}}
function Hta(t){return t}
function _ef(t,e){let{currency:r,currentOverages:n,currentUsage:s,freeTrialUsage:a,overageCharges:l,usageLimit:u}=e,d=Hta(t);return a&&a.currentUsage<a.usageLimit?`${d} Bonus ${a.currentUsage} / ${a.usageLimit} (${a.daysRemaining} days left)`:n>0||l>0?`${d} Overage ${n} (${r.symbol}${l.toFixed(2)})`:`${d} ${s} / ${u}`}
"#;

#[test]
fn credit_display_is_targeted_idempotent_and_injected() {
    let rendered =
        render_patch(FIXTURE, "https://gateway.invalid", &PatchRecipe::default()).unwrap();
    assert!(rendered.contains("creditDisplay.format(s)"));
    assert_eq!(repair_credit_display(&rendered), rendered);
    let unrelated = "const n=1.15;const money=n.toFixed(2);const percent=Math.floor(n/10*100);";
    assert_eq!(repair_credit_display(unrelated), unrelated);
    assert!(rendered.contains("currentUsage:t.currentUsageWithPrecision??t.currentUsage??0"));
    check_javascript(Command::new("node"), &rendered).unwrap();
}

#[test]
fn credit_display_rounds_half_up_without_mutating_usage() {
    let rendered =
        render_patch(FIXTURE, "https://gateway.invalid", &PatchRecipe::default()).unwrap();
    let assertions = r#"
const assert=require('node:assert/strict');
for(const [value,expected] of [[0,'0.0'],[1,'1.0'],[0.049999,'0.0'],[0.05,'0.1'],[0.149999,'0.1'],[0.15,'0.2'],[1.15,'1.2'],[9.95,'10.0'],[0.493568,'0.5'],[123456.75,'123456.8']]){
    const payload=Object.freeze({currentUsageWithPrecision:value,currentUsage:999,usageLimitWithPrecision:1000000.05,usageLimit:999});
    const usage=Object.freeze({...mapUsage(payload),currency:{symbol:'$'},currentOverages:0,overageCharges:0});
    assert.equal(_ef('PRO',usage),`PRO ${expected} / 1000000.1`);
    assert.equal(usage.currentUsage,value);
    assert.equal(usage.usageLimit,1000000.05);
    assert.equal(payload.currentUsageWithPrecision,value);
}
const bonus=Object.freeze({currentUsage:1.15,usageLimit:9.95,daysRemaining:3});
assert.equal(_ef('PRO',{freeTrialUsage:bonus}),'PRO Bonus 1.2 / 10.0 (3 days left)');
assert.equal(bonus.currentUsage,1.15);
assert.equal(_ef('PRO',{currentOverages:1.15,overageCharges:2.34,currency:{symbol:'$'}}),'PRO Overage 1.2 ($2.34)');
assert.deepEqual(mapUsage({currentUsage:0.123456,usageLimit:9.876543}),{currentUsage:0.123456,usageLimit:9.876543});
"#;
    // Only run a standalone fixture in Node, never the installed extension or Kiro.
    let output = Command::new("node")
        .env_remove("NODE_OPTIONS")
        .args(["-e", &format!("{rendered}\n{assertions}")])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unknown_credit_formatter_does_not_block_endpoint_injection() {
    // A minifier change makes this formatter unsupported. Preserve it verbatim
    // rather than broadening the replacement into unrelated number formatting.
    let unknown = FIXTURE.replace("${d} ${s} / ${u}", "${d}: ${s} / ${u}");
    assert_eq!(repair_credit_display(&unknown), unknown);
    let rendered =
        render_patch(&unknown, "https://gateway.invalid", &PatchRecipe::default()).unwrap();
    // Injected: the endpoint literal is left only as what other users of the computer
    // keep, behind the check for the user who took over.
    assert!(rendered.contains(&format!(
        "?(process.env.KIRO_GATEWAY_URL||\"https://gateway.invalid\"):`{RUNTIME_ENDPOINT_NEEDLE}`)"
    )));
    assert_eq!(rendered.matches(RUNTIME_ENDPOINT_NEEDLE).count(), 1);
    assert!(rendered.contains("https://gateway.invalid"));
    assert!(rendered.contains("${d}: ${s} / ${u}"));
    assert!(!rendered.contains("creditDisplay"));
    check_javascript(Command::new("node"), &rendered).unwrap();
}

#[test]
fn repeated_known_credit_formatters_do_not_require_one_match() {
    let second = FIXTURE.split("function _ef").nth(1).unwrap();
    let bundle = format!("{FIXTURE}\nfunction anotherItemContent{second}");
    let rendered =
        render_patch(&bundle, "https://gateway.invalid", &PatchRecipe::default()).unwrap();
    assert_eq!(rendered.matches("creditDisplay.format(s)").count(), 2);
    assert_eq!(repair_credit_display(&rendered), rendered);
    check_javascript(Command::new("node"), &rendered).unwrap();
}

#[test]
fn credit_display_repair_preserves_authenticated_restore() {
    // Simulate reapplying an older patch using the same durable metadata as
    // apply_with_recipe, without process detection or any installed Kiro files.
    let root = std::env::temp_dir().join(format!("credit-display-restore-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let patcher = ExtensionPatcher::new(root.join("extension.js"));
    let previous = format!("{PATCH_MARKER_V1}\n{FIXTURE}");
    let repaired = repair_credit_display(&previous);
    assert_ne!(previous, repaired);
    fs::write(patcher.backup_path(), FIXTURE).unwrap();
    patcher
        .write_state(&PatchState {
            original_len: FIXTURE.len() as u64,
            original_hash: content_hash(FIXTURE.as_bytes()),
            patched_hash: content_hash(repaired.as_bytes()),
            previous_patched_hash: Some(content_hash(previous.as_bytes())),
            owner: None,
        })
        .unwrap();
    // Either side of an interrupted repair publication is recoverable.
    for content in [&previous, &repaired] {
        fs::write(patcher.path(), content).unwrap();
        patcher.verify_patched_content().unwrap();
        assert_eq!(
            patcher.restore_material().unwrap().unwrap(),
            FIXTURE.as_bytes()
        );
    }
    assert!(patcher.restore_validated().unwrap());
    assert_eq!(fs::read(patcher.path()).unwrap(), FIXTURE.as_bytes());
    assert!(!patcher.backup_path().exists());
    assert!(!patcher.state_path().exists());
    fs::remove_file(patcher.path()).unwrap();
    fs::remove_dir(root).unwrap();
}
