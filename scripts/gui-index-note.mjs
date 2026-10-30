// SPDX-License-Identifier: Apache-2.0
//
// Runtime check for #index-note (M6 X4): when the library index save is
// left pending because another program holds the index's writer lock, the
// page shows one quiet line "Updating the library index…" in headless
// Chrome or Edge, and it goes away once the lock is free and the
// background retry saves. No npm packages; synthetic files only.
//
//   node scripts/gui-index-note.mjs target/debug/albumen
//
// Env: CHROME (default google-chrome; Edge works: msedge.exe). The lock is
// held with flock(1) on Unix and an exclusive open in PowerShell on Windows.
import {spawn} from 'node:child_process';
import {mkdtempSync, mkdirSync, writeFileSync, readdirSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createInterface} from 'node:readline';

const bin = process.argv[2] || 'target/debug/albumen';
const work = mkdtempSync(join(tmpdir(), 'albumen-index-note-'));
const lib = join(work, 'library');
mkdirSync(lib);
const N = 40;
for (let i = 0; i < N; i++) writeFileSync(join(lib, `IMG_${String(i).padStart(4, '0')}.jpg`), 'synthetic');
const sleep = ms => new Promise(r => setTimeout(r, ms));
const kids = [];
const done = code => { try { release(); } catch {} for (const k of kids) k.kill(); process.exit(code); };
const index = join(work, 'index');
const env = {...process.env, ALBUMEN_INDEX_DIR: index, ALBUMEN_HOME: join(work, 'home')};

async function startGui() {
  const gui = spawn(bin, ['gui', '--no-open', '--port', '0', '--library', lib, '--work', join(work, 'state')], {stdio: ['ignore', 'pipe', 'inherit'], env});
  kids.push(gui);
  const url = await new Promise((res, rej) => {
    const rl = createInterface({input: gui.stdout});
    rl.on('line', l => { const m = l.match(/^albumen gui: (http:\/\/\S+?)\/?$/); if (m) res(m[1]); });
    gui.on('exit', c => rej(new Error('gui exited ' + c)));
    setTimeout(() => rej(new Error('gui printed no URL')), 120000);
  });
  return {gui, url};
}

// First run saves the index and creates its lock file.
{
  const {gui, url} = await startGui();
  for (let i = 0; i < 300; i++) { const l = await (await fetch(url + '/api/library')).json(); if (l.status == 'ready') break; await sleep(100); }
  for (let i = 0; i < 100; i++) { const x = await (await fetch(url + '/api/index')).json(); if (!x.pending) break; await sleep(100); }
  gui.kill(); await new Promise(r => gui.once('exit', r));
}
const lockName = readdirSync(index).find(n => n.endsWith('.idx.lock'));
if (!lockName) { console.error('no .idx.lock after the first save'); done(2); }
const lockPath = join(index, lockName);
const holder = process.platform == 'win32'
  ? spawn('powershell', ['-NoProfile', '-Command', `$f=[IO.File]::Open('${lockPath}','Open','ReadWrite','None'); 'held'; Start-Sleep 120`], {stdio: ['ignore', 'pipe', 'inherit']})
  : spawn('flock', ['-x', lockPath, '-c', 'echo held; exec sleep 120'], {stdio: ['ignore', 'pipe', 'inherit'], detached: true});
// flock forks the holder; free the lock by killing its whole process group.
const release = () => { try { process.platform == 'win32' ? holder.kill() : process.kill(-holder.pid, 'SIGKILL'); } catch {} };
kids.push(holder);
await new Promise((res, rej) => { holder.stdout.on('data', d => { if (String(d).includes('held')) res(); }); setTimeout(() => rej(new Error('lock holder did not start')), 20000); });
// A new folder so the next open has to save.
mkdirSync(join(lib, 'new'));
writeFileSync(join(lib, 'new', 'IMG_9000.jpg'), 'synthetic');
const {url} = await startGui();

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
const note = `(()=>{const e=document.getElementById('index-note');return e&&{hidden:e.hidden,shown:!e.hidden&&e.offsetParent!==null,text:e.textContent};})()`;

await cmd('Runtime.enable');
await cmd('Page.navigate', {url: url + '/'});
try {
  await until(`typeof lib!=='undefined'&&lib.status=='ready'&&lib.images.length==${N + 1}`, 'library of ' + (N + 1) + ' loaded');
  // The line lives in the Info panel's Library section; open it with a real click.
  const b = await ev(`(()=>{const r=document.getElementById('b-info').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};})()`);
  await cmd('Input.dispatchMouseEvent', {type: 'mousePressed', x: b.x, y: b.y, button: 'left', clickCount: 1});
  await cmd('Input.dispatchMouseEvent', {type: 'mouseReleased', x: b.x, y: b.y, button: 'left', clickCount: 1});
  await until(`!document.getElementById('info').hidden`, 'Info panel opened', 50);
  await until(`!document.getElementById('index-note').hidden`, '#index-note shown while the save is pending', 100);
  const shown = await ev(note);
  check('#index-note is shown while the index save is pending', !!shown && shown.shown, shown);
  check('#index-note says exactly "Updating the library index…"', !!shown && shown.text == 'Updating the library index…', shown);
  const grid = await ev(`document.querySelectorAll('#grid figure[data-p]').length`);
  check('the grid is whole while the save is pending', grid == N + 1, grid);
  const t0 = Date.now();
  release();
  await until(`document.getElementById('index-note').hidden`, '#index-note hidden after the lock is freed', 400);
  const gone = await ev(note);
  check('#index-note goes away once the save lands', !!gone && gone.hidden, gone);
  console.log(`M6 index-note shown=${shown && shown.shown} cleared_after_ms=${Date.now() - t0} files=${N + 1}`);
} catch (e) { console.error(e); failed++; }

check('no script errors', errors.length == 0, errors);
console.log(failed ? `${failed} check(s) failed` : 'all index-note checks passed');
done(failed ? 1 : 0);
