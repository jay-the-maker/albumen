// SPDX-License-Identifier: Apache-2.0
//
// Runtime check for #grid-more with more than one page of photos: a
// synthetic library of 1,500 files (more than GRID_PAGE = 600), served by
// `albumen gui --library`, opened in headless Chrome or Edge over the
// DevTools protocol. #grid-more must exist, sit after the tiles, and a real
// click on it must grow the grid. No npm packages; synthetic files only.
//
//   node scripts/gui-more-behaviour.mjs target/debug/albumen
//
// Env: CHROME (default google-chrome; Edge works: msedge.exe).
import {spawn} from 'node:child_process';
import {mkdtempSync, mkdirSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createInterface} from 'node:readline';

const bin = process.argv[2] || 'target/debug/albumen';
const work = mkdtempSync(join(tmpdir(), 'albumen-gui-more-'));
const lib = join(work, 'library');
mkdirSync(lib);
const N = 1500;
for (let i = 0; i < N; i++) writeFileSync(join(lib, `IMG_${String(i).padStart(5, '0')}.jpg`), 'synthetic');
const sleep = ms => new Promise(r => setTimeout(r, ms));
const kids = [];
const done = code => { for (const k of kids) k.kill(); process.exit(code); };

const env = {...process.env, ALBUMEN_INDEX_DIR: join(work, 'index'), ALBUMEN_HOME: join(work, 'home')};
const gui = spawn(bin, ['gui', '--no-open', '--port', '0', '--library', lib, '--work', join(work, 'state')], {stdio: ['ignore', 'pipe', 'inherit'], env});
kids.push(gui);
const url = await new Promise((res, rej) => {
  const rl = createInterface({input: gui.stdout});
  rl.on('line', l => { const m = l.match(/^albumen gui: (http:\/\/\S+?)\/?$/); if (m) res(m[1]); });
  gui.on('exit', c => rej(new Error('gui exited ' + c)));
  setTimeout(() => rej(new Error('gui printed no URL')), 120000);
});

const port = 9300 + Math.floor(Math.random() * 500);
const chrome = spawn(process.env.CHROME || 'google-chrome', ['--headless=new', '--no-sandbox', '--disable-gpu', '--hide-scrollbars',
  '--remote-debugging-port=' + port, '--window-size=1280,800', '--user-data-dir=' + join(work, 'chrome'), 'about:blank'], {stdio: 'ignore'});
kids.push(chrome);
let tabs;
for (let i = 0; i < 100 && !tabs; i++) { try { tabs = await (await fetch(`http://127.0.0.1:${port}/json`)).json(); } catch { await sleep(200); } }
if (!tabs) { console.error('browser did not start'); done(2); }
const ws = new WebSocket(tabs.find(t => t.type == 'page').webSocketDebuggerUrl);
await new Promise(r => ws.onopen = r);
let id = 0; const pend = {}; const errors = [];
ws.onmessage = m => { const d = JSON.parse(m.data);
  if (d.method == 'Runtime.exceptionThrown') errors.push(d.params.exceptionDetails.exception?.description || d.params.exceptionDetails.text);
  if (pend[d.id]) { pend[d.id](d); delete pend[d.id]; } };
const cmd = (method, params = {}) => new Promise(r => { const i = ++id; pend[i] = r; ws.send(JSON.stringify({id: i, method, params})); });
const ev = async js => { const r = await cmd('Runtime.evaluate', {expression: js, awaitPromise: true, returnByValue: true});
  if (r.result?.exceptionDetails) throw new Error(js + ': ' + r.result.exceptionDetails.exception?.description); return r.result?.result?.value; };
const until = async (js, what, tries = 300) => { for (let i = 0; i < tries; i++) { if (await ev(js)) return; await sleep(100); } throw new Error('timed out: ' + what); };

let failed = 0;
const check = (name, ok, got) => { console.log((ok ? 'ok   ' : 'FAIL ') + name + (ok ? '' : '  got: ' + JSON.stringify(got))); if (!ok) failed++; };
const tiles = `document.querySelectorAll('#grid figure[data-p]').length`;

await cmd('Runtime.enable');
await cmd('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
// No IntersectionObserver and no near-end timer firing: only the click may
// grow the grid, so the check proves the button itself works.
await cmd('Page.addScriptToEvaluateOnNewDocument', {source: 'delete window.IntersectionObserver;'});
await cmd('Page.navigate', {url: url + '/'});
try {
  await until(`typeof lib!=='undefined'&&lib.status=='ready'&&lib.images.length==${N}&&!!document.querySelector('#grid figure[data-p]')`, 'library of ' + N + ' loaded');
  await ev(`clearInterval(moreTimer),1`);
  const before = await ev(tiles);
  const more = await ev(`(()=>{const b=document.getElementById('grid-more');return b&&{connected:b.isConnected,last:b===b.parentElement.lastElementChild,text:b.textContent};})()`);
  check('#grid-more exists with more than one page of photos', !!more && more.connected, more);
  check('#grid-more sits after the tiles', !!more && more.last, more);
  check('first page shows fewer than all photos', before > 0 && before < N, before);
  const b = await ev(`(()=>{const e=document.getElementById('grid-more');e.scrollIntoView({block:'center'});const r=e.getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};})()`);
  await cmd('Input.dispatchMouseEvent', {type: 'mousePressed', x: b.x, y: b.y, button: 'left', clickCount: 1});
  await cmd('Input.dispatchMouseEvent', {type: 'mouseReleased', x: b.x, y: b.y, button: 'left', clickCount: 1});
  await until(`${tiles}>${before}&&!window.__gridBusy()`, 'grid grows after the click', 100);
  const after = await ev(tiles);
  check('clicking #grid-more grows the grid', after > before, [before, after]);
  console.log(`M6 grid-more tiles_before=${before} tiles_after=${after} total=${N}`);
} catch (e) { console.error(e); failed++; }

check('no script errors', errors.length == 0, errors);
console.log(failed ? `${failed} check(s) failed` : 'all grid-more checks passed');
done(failed ? 1 : 0);
