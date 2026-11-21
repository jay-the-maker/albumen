#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// Drive a running `albumen gui` in headless Chrome over the DevTools
// protocol and save screenshots. No npm packages: Node 22+ (global
// WebSocket and fetch) and a Chrome or Chromium binary.
//
//   node drive.mjs --url http://127.0.0.1:PORT/ --out DIR --steps steps.json
//   node drive.mjs --url ... --out DIR --step '{"goto":"#events"}' --step '{"shot":"events"}'
//
// Steps (run in order; any failure exits 1 and saves DIR/failed-<n>.png):
//   {"goto": "#events"}            load URL+hash, wait for <html data-ready="1">
//   {"click": "css"} {"dblclick": "css"} {"rightclick": "css"}
//                                  real mouse events at the element's centre
//   {"key": "ArrowRight", "mods": ["shift"|"ctrl"|"meta"|"alt"]}
//   {"type": "text", "into": "css"} focus, select all, type
//   {"waitFor": "css"} | {"waitFn": "js expression"}   up to "timeout" ms (default 10000)
//   {"eval": "js expression"}      result is logged
//   {"assert": "js expression", "msg": "..."}  must be truthy
//   {"shot": "name"}               waits for visible images (bounded), saves DIR/name.png
// Every step and result goes to DIR/log.jsonl. Console errors and page
// exceptions are logged and fail the run at the end.
// CHROME=/path overrides the browser. --size 1440x900 sets the window.
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, appendFileSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const args = process.argv.slice(2);
const opt = (k, d) => { const i = args.indexOf(k); return i >= 0 ? args[i + 1] : d; };
const url = opt('--url'); const out = opt('--out');
if (!url || !out) { console.error('usage: drive.mjs --url URL --out DIR (--steps FILE | --step JSON ...)'); process.exit(2); }
let steps = [];
if (opt('--steps')) steps = JSON.parse(readFileSync(opt('--steps'), 'utf8'));
args.forEach((a, i) => { if (a === '--step') steps.push(JSON.parse(args[i + 1])); });
const [W, H] = (opt('--size', '1440x900')).split('x').map(Number);
mkdirSync(out, { recursive: true });
const logf = join(out, 'log.jsonl');
const log = o => { appendFileSync(logf, JSON.stringify({ t: new Date().toISOString(), ...o }) + '\n'); console.log(JSON.stringify(o)); };

function chromePath() {
  if (process.env.CHROME) return process.env.CHROME;
  for (const c of ['/usr/bin/google-chrome', '/usr/bin/chromium', '/usr/bin/chromium-browser',
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', '/Applications/Chromium.app/Contents/MacOS/Chromium'])
    if (existsSync(c)) return c;
  throw new Error('no Chrome found; set CHROME=/path/to/chrome');
}
const sleep = ms => new Promise(r => setTimeout(r, ms));

const profile = mkdtempSync(join(tmpdir(), 'albumen-drive-'));
const chrome = spawn(chromePath(), ['--headless=new', '--remote-debugging-port=0', `--user-data-dir=${profile}`,
  `--window-size=${W},${H}`, '--hide-scrollbars', '--no-first-run', '--no-default-browser-check', '--disable-gpu',
  '--disable-extensions', '--disable-background-networking', '--force-device-scale-factor=1', 'about:blank'],
  { stdio: ['ignore', 'ignore', 'pipe'] });
let chromeErr = ''; chrome.stderr.on('data', d => { chromeErr += d; });
const cleanup = () => { try { chrome.kill('SIGKILL'); } catch {} try { rmSync(profile, { recursive: true, force: true }); } catch {} };
process.on('exit', cleanup);

async function devtools() {
  const f = join(profile, 'DevToolsActivePort');
  for (let i = 0; i < 200; i++) { if (existsSync(f)) { const [port] = readFileSync(f, 'utf8').split('\n'); if (port) return Number(port); } await sleep(50); }
  throw new Error('Chrome did not start: ' + chromeErr.slice(-500));
}
const port = await devtools();
const pages = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
const ws = new WebSocket(pages.find(p => p.type === 'page').webSocketDebuggerUrl);
await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
let seq = 0; const waiting = new Map(); const problems = [];
ws.onmessage = e => {
  const m = JSON.parse(e.data);
  if (m.id && waiting.has(m.id)) { const { r, j } = waiting.get(m.id); waiting.delete(m.id); m.error ? j(new Error(m.error.message)) : r(m.result); return; }
  if (m.method === 'Runtime.exceptionThrown') problems.push('exception: ' + (m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text));
  if (m.method === 'Runtime.consoleAPICalled' && m.params.type === 'error') problems.push('console.error: ' + m.params.args.map(a => a.value ?? a.description).join(' '));
  if (m.method === 'Log.entryAdded' && m.params.entry.level === 'error') problems.push('log: ' + m.params.entry.text + ' ' + (m.params.entry.url || ''));
};
const send = (method, params = {}) => new Promise((r, j) => { const id = ++seq; waiting.set(id, { r, j }); ws.send(JSON.stringify({ id, method, params })); });
await send('Page.enable'); await send('Runtime.enable'); await send('Log.enable');
await send('Emulation.setDeviceMetricsOverride', { width: W, height: H, deviceScaleFactor: 1, mobile: false });

async function evaluate(expr) {
  const r = await send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true });
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
  return r.result.value;
}
async function waitFn(expr, timeout = 10000) {
  const end = Date.now() + timeout;
  for (;;) { try { if (await evaluate(expr)) return; } catch {} if (Date.now() > end) throw new Error(`timed out after ${timeout} ms waiting for ${expr}`); await sleep(50); }
}
const sel = s => JSON.stringify(s);
async function centre(css, timeout) {
  await waitFn(`(()=>{const e=document.querySelector(${sel(css)});if(!e)return false;const r=e.getBoundingClientRect();return r.width>0&&r.height>0})()`, timeout);
  return evaluate(`(()=>{const e=document.querySelector(${sel(css)});e.scrollIntoView({block:'nearest'});const r=e.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2}})()`);
}
const MODS = { alt: 1, ctrl: 2, meta: 4, shift: 8 };
const modBits = m => (m || []).reduce((s, k) => s | MODS[k], 0);
async function mouse(css, clicks, button = 'left', mods) {
  const { x, y } = await centre(css);
  const modifiers = modBits(mods);
  await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y, modifiers });
  for (let c = 1; c <= clicks; c++) {
    await send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button, clickCount: c, modifiers });
    await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button, clickCount: c, modifiers });
  }
}
const KEYS = { ArrowLeft: 37, ArrowUp: 38, ArrowRight: 39, ArrowDown: 40, Enter: 13, Escape: 27, ' ': 32, Delete: 46, Backspace: 8, Home: 36, End: 35, PageDown: 34, PageUp: 33, Tab: 9 };
async function key(k, mods) {
  const modifiers = modBits(mods);
  const code = KEYS[k] ?? k.toUpperCase().charCodeAt(0);
  const text = k.length === 1 && !(modifiers & 6) ? k : undefined;
  await send('Input.dispatchKeyEvent', { type: text ? 'keyDown' : 'rawKeyDown', key: k, windowsVirtualKeyCode: code, modifiers, text });
  await send('Input.dispatchKeyEvent', { type: 'keyUp', key: k, windowsVirtualKeyCode: code, modifiers });
}
// Screenshot readiness: the page says it rendered, then images in the
// viewport finish (or 8 s pass). Never waits on virtual time or on network
// idle, which is what made the old --screenshot of Events time out.
async function settle() {
  await waitFn(`document.documentElement.dataset.ready==='1'`, 15000);
  const end = Date.now() + 8000;
  let pending = 0;
  while (Date.now() < end) {
    pending = await evaluate(`[...document.images].filter(i=>{const r=i.getBoundingClientRect();return r.bottom>0&&r.top<innerHeight&&r.width>0&&i.getAttribute('src')&&!i.complete}).length`);
    if (!pending) break;
    await sleep(100);
  }
  await evaluate('new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))');
  return pending;
}
async function shot(name) {
  const pending = await settle();
  const { data } = await send('Page.captureScreenshot', { format: 'png' });
  const file = join(out, name + '.png');
  writeFileSync(file, Buffer.from(data, 'base64'));
  return { file, images_still_loading: pending };
}

let failed = false;
for (let n = 0; n < steps.length; n++) {
  const s = steps[n]; const t0 = Date.now();
  try {
    let result;
    if (s.goto !== undefined) {
      const target = s.goto.startsWith('http') ? s.goto : url.replace(/#.*$/, '') + s.goto;
      await send('Page.navigate', { url: 'about:blank' });
      await send('Page.navigate', { url: target });
      await waitFn(`document.documentElement.dataset.ready==='1'`, s.timeout || 15000);
    } else if (s.click) await mouse(s.click, 1, 'left', s.mods);
    else if (s.dblclick) await mouse(s.dblclick, 2, 'left', s.mods);
    else if (s.rightclick) await mouse(s.rightclick, 1, 'right', s.mods);
    else if (s.key) await key(s.key, s.mods);
    else if (s.type !== undefined) {
      await mouse(s.into, 1);
      await evaluate(`(()=>{const e=document.querySelector(${sel(s.into)});e.focus();e.select&&e.select();})()`);
      await send('Input.insertText', { text: s.type });
    } else if (s.waitFor) await centre(s.waitFor, s.timeout);
    else if (s.waitFn) await waitFn(s.waitFn, s.timeout);
    else if (s.eval) result = await evaluate(s.eval);
    else if (s.assert) { result = await evaluate(s.assert); if (!result) throw new Error('assert failed: ' + (s.msg || s.assert)); }
    else if (s.shot) result = await shot(s.shot);
    else throw new Error('unknown step ' + JSON.stringify(s));
    if (s.pause) await sleep(s.pause);
    log({ step: n, ...s, ok: true, ms: Date.now() - t0, result });
  } catch (e) {
    failed = true;
    log({ step: n, ...s, ok: false, ms: Date.now() - t0, error: String(e.message || e) });
    try { const { data } = await send('Page.captureScreenshot', { format: 'png' }); writeFileSync(join(out, `failed-${n}.png`), Buffer.from(data, 'base64')); } catch {}
    break;
  }
}
if (problems.length) { log({ page_problems: problems }); failed = true; }
log({ done: true, ok: !failed });
ws.close(); cleanup();
process.exit(failed ? 1 : 0);
