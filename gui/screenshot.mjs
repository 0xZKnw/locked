import { chromium } from 'playwright';

const OUT = 'C:/Users/crist/AppData/Local/Temp/claude/C--Users-crist-Desktop-stageHumnTech/b7794e73-f2ca-49d9-8a41-ba46d9af7486/scratchpad';

const receipts = [
  { seq:0, prev:'sha256:genesis', ts:'2026-07-25T09:00:00Z', event:'run_started', integrity:{level:'degraded',reason:'x'}, tools:['tap_discover','tap_call','tap_check','tap_await'], sandbox_image:null, attestation:'harness_attested', digest:'sha256:'+'a'.repeat(64) },
  { seq:1, prev:'sha256:'+'a'.repeat(64), ts:'2026-07-25T09:00:03Z', event:'inference', model:'kimi-for-coding', prompt_digest:'sha256:x', response_digest:'sha256:y', input_tokens:1420, output_tokens:210, attestation:'harness_attested', digest:'sha256:'+'b'.repeat(64) },
  { seq:2, prev:'sha256:'+'b'.repeat(64), ts:'2026-07-25T09:00:09Z', event:'tap_call', credential:'cerebras', target_host:'api.cerebras.ai', method:'GET', upstream_status:200, attestation:'source_attested', scheme:'request_id', id:'req_88ab', digest:'sha256:'+'c'.repeat(64) },
  { seq:3, prev:'sha256:'+'c'.repeat(64), ts:'2026-07-25T09:00:14Z', event:'tap_call', credential:'discord', target_host:'discord.com', method:'POST', upstream_status:200, attestation:'tap_attested', txn_id:'txn_4f21', digest:'sha256:'+'d'.repeat(64) },
];
const caps = [
  { name:'cerebras', target_shape:'full_url', writes_auto_approve:true, description:'cerebras api key' },
  { name:'dune-anal-2', target_shape:'full_url', writes_auto_approve:true, description:'Dune Analytics' },
  { name:'discord', target_shape:'full_url', writes_auto_approve:false, description:'discord pour mon bot' },
];

const browser = await chromium.launch({ channel: 'chrome', args: ['--force-color-profile=srgb'] });
const page = await browser.newPage({ viewport: { width: 1400, height: 900 }, deviceScaleFactor: 2 });

await page.addInitScript(([r, c]) => {
  window.__TAURI_INTERNALS__ = {
    transformCallback: (cb) => { const id = Date.now() + Math.random(); window[`_cb_${id}`] = cb; return id; },
    invoke: (cmd) => {
      if (cmd === 'load_journal') return Promise.resolve(r);
      if (cmd === 'list_capabilities') return Promise.resolve(c);
      return Promise.resolve(null);
    },
  };
}, [receipts, caps]);

await page.goto('http://localhost:5173', { waitUntil: 'networkidle' });
await page.waitForTimeout(1200);

// Wake the ambient light and settle it near the composer.
await page.mouse.move(700, 420, { steps: 12 });
await page.waitForTimeout(500);
await page.mouse.move(1150, 800, { steps: 20 });
await page.waitForTimeout(900);

// Judge the active state: with an empty field the button is disabled, which is
// deliberately the quiet variant and tells you nothing about the real design.
await page.fill('textarea', 'Check what the analytics credentials can reach');
await page.waitForTimeout(350);
await page.mouse.move(1300, 1218, { steps: 10 });
await page.waitForTimeout(450);
await page.screenshot({ path: `${OUT}/ui-full.png` });
await page.locator('.dock').screenshot({ path: `${OUT}/ui-composer.png` });

console.log('ok');
await browser.close();
