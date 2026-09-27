// Toasts go after three seconds (errors after six): a test that waits for one to be visible misses
// it when the runner is slow enough that it has come and gone. Each toast is recorded as it
// appears, in this process, across reloads, and a test looks for it in that record. Test-only.
const assert = require('node:assert/strict');

/** Starts recording `page`'s toasts (call once per page, before or after it loads). */
module.exports = async function recordToasts(page) {
  const records = [];
  // The first record not yet looked at: what a check found, and what came before it, is passed.
  let next = 0;
  await page.exposeFunction('__toastShown', text => {records.push(String(text));});
  const watch = () => {
    if (window.__toastsWatched) return;
    window.__toastsWatched = true;
    const note = node => {for (const toast of node.matches('.toast') ? [node] : node.querySelectorAll('.toast')) void window.__toastShown(toast.textContent ?? '');};
    new MutationObserver(changes => {for (const change of changes) for (const node of change.addedNodes) if (node.nodeType === 1) note(node);})
      .observe(document, {childList: true, subtree: true});
  };
  await page.addInitScript(watch);
  await page.evaluate(watch);
  return {
    /** Waits for a toast containing `text`, shown after the last one found (or the mark). */
    async shown(text, timeout = 10000) {
      const deadline = Date.now() + timeout;
      for (;;) {
        const index = records.findIndex((record, i) => i >= next && record.includes(text));
        if (index >= 0) {next = index + 1; return records[index];}
        if (Date.now() > deadline) assert.fail(`no toast with "${text}"; shown since the last one found: ${JSON.stringify(records.slice(next))}`);
        await new Promise(resolve => setTimeout(resolve, 20));
      }
    },
    /** Before an action: only toasts shown from now on are looked at. */
    mark() {next = records.length;},
    /** The toasts shown after the last one found (or the mark), once what the page has shown so far is in. */
    async since() {await page.evaluate(() => 0); return records.slice(next);},
  };
};
