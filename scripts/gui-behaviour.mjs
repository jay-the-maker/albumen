// SPDX-License-Identifier: Apache-2.0
//
// Behaviour tests for `albumen gui --demo`, driven in headless Chrome over
// the DevTools protocol with real mouse and keyboard input. No npm packages.
//
//   node scripts/gui-behaviour.mjs target/debug/albumen
//
// Env: CHROME (default google-chrome). The GUI server and Chrome run with
// TZ=America/Los_Angeles so the local-time checks are meaningful (Windows
// ignores TZ and uses the PC's zone). Exits non-zero on any failure.
import {spawn} from 'node:child_process';
import {mkdtempSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createInterface} from 'node:readline';

const bin = process.argv[2] || 'target/debug/albumen';
const work = mkdtempSync(join(tmpdir(), 'albumen-gui-behaviour-'));
const sleep = ms => new Promise(r => setTimeout(r, ms));
const kids = [];
const done = code => { for (const k of kids) k.kill(); process.exit(code); };

const gui = spawn(bin, ['gui', '--demo', '--no-open', '--port', '0', '--work', work], {stdio: ['ignore', 'pipe', 'inherit'], env: {...process.env, TZ: 'America/Los_Angeles'}});
kids.push(gui);
const url = await new Promise((res, rej) => {
  const rl = createInterface({input: gui.stdout});
  rl.on('line', l => { const m = l.match(/^albumen gui: (http:\/\/\S+?)\/?$/); if (m) res(m[1]); });
  gui.on('exit', c => rej(new Error('gui exited ' + c)));
  setTimeout(() => rej(new Error('gui printed no URL')), 120000);
});

const port = 9300 + Math.floor(Math.random() * 500);
const chrome = spawn(process.env.CHROME || 'google-chrome', ['--headless=new', '--no-sandbox', '--disable-gpu', '--hide-scrollbars',
  '--remote-debugging-port=' + port, '--window-size=1280,800', '--user-data-dir=' + join(work, 'chrome'), 'about:blank'],
  {stdio: 'ignore', env: {...process.env, TZ: 'America/Los_Angeles'}});
kids.push(chrome);
let tabs;
for (let i = 0; i < 100 && !tabs; i++) { try { tabs = await (await fetch(`http://127.0.0.1:${port}/json`)).json(); } catch { await sleep(200); } }
if (!tabs) { console.error('chrome did not start'); done(2); }
const ws = new WebSocket(tabs.find(t => t.type == 'page').webSocketDebuggerUrl);
await new Promise(r => ws.onopen = r);
let id = 0; const pend = {}; const errors = [];
ws.onmessage = m => { const d = JSON.parse(m.data);
  if (d.method == 'Runtime.exceptionThrown') errors.push(d.params.exceptionDetails.exception?.description || d.params.exceptionDetails.text);
  if (pend[d.id]) { pend[d.id](d); delete pend[d.id]; } };
const cmd = (method, params = {}) => new Promise(r => { const i = ++id; pend[i] = r; ws.send(JSON.stringify({id: i, method, params})); });
const ev = async js => { const r = await cmd('Runtime.evaluate', {expression: js, awaitPromise: true, returnByValue: true});
  if (r.result?.exceptionDetails) throw new Error(js + ': ' + r.result.exceptionDetails.exception?.description); return r.result?.result?.value; };
const until = async (js, what) => { for (let i = 0; i < 100; i++) { if (await ev(js)) return; await sleep(100); } throw new Error('timed out: ' + what); };
// Real pointer input at the element's centre.
const click = async (sel, count = 1) => {
  const b = await ev(`(()=>{const e=document.querySelector(${JSON.stringify(sel)});if(!e)return null;e.scrollIntoView({block:'center'});const r=e.getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};})()`);
  if (!b) throw new Error('no element ' + sel);
  for (let n = 1; n <= count; n++) {
    await cmd('Input.dispatchMouseEvent', {type: 'mousePressed', x: b.x, y: b.y, button: 'left', clickCount: n});
    await cmd('Input.dispatchMouseEvent', {type: 'mouseReleased', x: b.x, y: b.y, button: 'left', clickCount: n});
  }
  await sleep(150);
};
const sideLit = () => ev(`[...document.querySelectorAll('aside button.on')].map(b=>b.querySelector('span').textContent)`);
const visibleView = () => ev(`[...document.querySelectorAll('.main section')].filter(s=>!s.hidden).map(s=>s.id)`);

let failed = 0;
const check = (name, ok, got) => { console.log((ok ? 'ok   ' : 'FAIL ') + name + (ok ? '' : '  got: ' + JSON.stringify(got))); if (!ok) failed++; };
const eq = (a, b) => JSON.stringify(a) == JSON.stringify(b);

await cmd('Runtime.enable');
await cmd('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
await cmd('Page.navigate', {url: url + '/'});
await until(`typeof lib!=='undefined'&&lib.events.length>0`, 'library loaded');

try {
  // Ask 1: double-click into an Event; the sidebar lights Events.
  await click('aside button[data-s="events"]');
  check('ask 1: Events view open', eq(await visibleView(), ['events']), await visibleView());
  const tile = await ev(`[...document.querySelectorAll('#event-grid figure')].findIndex(f=>f.querySelector('figcaption').firstChild.textContent=='Oct 3, 2026')`);
  check('R5: Oct 3 tile exists', tile >= 0, tile);
  const tileCount = await ev(`document.querySelectorAll('#event-grid figure')[${tile}].querySelector('small').textContent`);
  await click(`#event-grid figure:nth-child(${tile + 1})`, 2);
  check('ask 1: inside the event the sidebar lights Events only', eq(await sideLit(), ['Events']), await sideLit());
  check('ask 1: photos view shows the event', await ev(`$('photos-title').textContent`) == 'Oct 3, 2026', await ev(`$('photos-title').textContent`));
  const inside = await ev(`$('photos-count').textContent`);
  check('R5: tile count and inside count agree (5 photos)', inside == tileCount && inside == '5 photos', [tileCount, inside]);
  await click('#to-events');
  check('ask 1: back to Events keeps Events lit', eq(await sideLit(), ['Events']), await sideLit());
  await click('aside button[data-s="photos"]');
  check('ask 1: Photos lights Photos', eq(await sideLit(), ['Photos']), await sideLit());

  // FIX-4: clicking the disabled Places/Faces rows changes nothing.
  for (const n of ['Places', 'Faces']) {
    const sel = `aside button.soon[title^="${n}"]`;
    check(`FIX-4: ${n} row is disabled`, await ev(`document.querySelector(${JSON.stringify(sel)}).disabled`), false);
    await click(sel);
    check(`FIX-4: clicking ${n} opens nothing`, eq(await visibleView(), ['photos']) && eq(await sideLit(), ['Photos']), [await visibleView(), await sideLit()]);
  }

  // Ask 6: at the smallest zoom captions hide and placeholders show only the format.
  await ev(`(()=>{const z=$('zoom');z.value=z.min;z.dispatchEvent(new Event('input'));})()`);
  await sleep(150);
  const caps = await ev(`[...document.querySelectorAll('#grid figcaption')].map(c=>getComputedStyle(c).display)`);
  check('ask 6: every caption hidden at min zoom', caps.length > 0 && caps.every(d => d == 'none'), caps);
  const ph = await ev(`[...document.querySelectorAll('#grid .ph')].map(p=>p.innerText.trim())`);
  check('ask 6: placeholders say only the format', ph.length == 4 && ph.every(t => /^(DNG|HEIC|CR3|NEF)$/.test(t)), ph);
  await ev(`(()=>{const z=$('zoom');z.value=200;z.dispatchEvent(new Event('input'));})()`);
  await sleep(150);
  const caps2 = await ev(`[...document.querySelectorAll('#grid figcaption')].map(c=>getComputedStyle(c).display)`);
  check('ask 6: captions return at normal zoom', caps2.every(d => d != 'none'), caps2);

  // Ask 7b: typing the RAW name finds the RAW + JPEG pair.
  await click('#search');
  await cmd('Input.insertText', {text: 'IMG_0003.dng'});
  await sleep(200);
  const hits = await ev(`[...document.querySelectorAll('#grid figure')].map(f=>f.querySelector('figcaption').textContent)`);
  check('ask 7b: search "IMG_0003.dng" finds the pair, once', eq(hits, ['IMG_0003']), hits);
  check('ask 7b: count says 1 photo', await ev(`$('photos-count').textContent`) == '1 photo', await ev(`$('photos-count').textContent`));

  // R1: Info labels a camera time honestly.
  await click('#grid figure');
  if (await ev(`$('info').hidden`)) await click('#b-info');
  const taken = await ev(`$('i-taken').textContent`);
  const infoBg = await ev(`getComputedStyle($('info')).backgroundColor`);
  check('Info pane is dark like the grid', infoBg != 'rgb(255, 255, 255)', infoBg);
  check('R1: camera time says it has no zone', taken == 'Taken 2015-09-03 18:45 (camera clock, no time zone)', taken);
  await ev(`(()=>{const s=$('search');s.value='';s.dispatchEvent(new Event('input'));})()`);

  // R2: Backup health is dark and shows local time with a numeric offset.
  await click('aside button[data-s="health"]');
  await until(`document.querySelectorAll('#cards .card').length==4`, 'health cards');
  check('R2: health uses the dark chrome', await ev(`$('main').classList.contains('dark')`), false);
  const bg = await ev(`getComputedStyle(document.querySelector('#cards .card')).backgroundColor`);
  check('R2: cards are dark, not white', bg != 'rgb(255, 255, 255)', bg);
  const shown = await ev(`[...document.querySelectorAll('#cards .card .txt')].map(t=>t.innerText).join('\\n')+'\\n'+$('health-summary').innerText`);
  check('R2: no zone abbreviation on the health screen', !/\b[A-Z]{2,4}T\b/.test(shown), shown);
  check('R2: stale time shown with a numeric offset', /\d{4}-\d\d-\d\d \d\d:\d\d UTC-[78]\b/.test(shown), shown);
  // Parity: the page's local() and the Rust formatter (clock::stamp_at, via
  // the demo-only /api/demo/clock) give the same text at the same explicit
  // offset, including part-hour zones, plain UTC and US DST edges.
  const T = 1793320920; // 2026-10-30T00:42:00Z
  const cases = [
    [T, -420, '2026-10-29 17:42 UTC-7'], [T, 330, '2026-10-30 06:12 UTC+5:30'], [T, 0, '2026-10-30 00:42 UTC'],
    [T, -480], [T, 345], [T, -570], [T, 840], [T, -720], [-1, 0], [951782400, 0],
    // US spring forward 2026-03-08 10:00Z and fall back 2026-11-01 09:00Z, either side.
    [1772963999, -480], [1772964000, -420], [1793523599, -420], [1793523600, -480],
    // India crosses midnight at 18:30Z.
    [1793298599, 330], [1793298600, 330],
  ];
  for (const [unix, off, want] of cases) {
    const rust = (await (await fetch(`${url}/api/demo/clock?unix=${unix}&offset=${off}`)).json()).stamp;
    const js = await ev(`local(new Date(${unix}*1000),${off})`);
    check(`parity ${unix}@${off}: ${js}`, rust == js && (!want || js == want), {rust, js, want});
  }
  // R13 / S1: backups show their folder name, never a temp path or an invented drive name.
  const names = await ev(`[...document.querySelectorAll('#side-dests span, #cards .card h3')].map(e=>e.textContent)`);
  check('R13: sidebar and cards show folder names only', names.length == 8 && names.every(n => !/[\\/:]/.test(n) && !/temp|tmp/i.test(n)), names);
  check('S1: the four demo backups show their folder names', eq([...new Set(names)].sort(), ['damaged-backup', 'demo-backup', 'new-backup', 'stale-backup']), names);
  check('S1: no invented drive names', names.every(n => !/drive/i.test(n)), names);
  // S2: the summary counts backups, and disabled toolbar buttons look disabled.
  check("S2: summary says '4 backups'", /^4 backups: /.test(await ev(`$('health-summary').innerText`)), await ev(`$('health-summary').innerText`));
  const dim = await ev(`['b-info','b-show'].map(id=>[$(id).disabled,getComputedStyle($(id)).opacity])`);
  check('S2: disabled Info and Slideshow are dimmed', dim.every(([d, o]) => d && Number(o) < 0.6), dim);
  // R14: Info is closed on Backup health and Proof, and cannot be opened there.
  check('R14: Info closed on Backup health', await ev(`$('info').hidden`), false);
  check('R14: Info button disabled on Backup health', await ev(`$('b-info').disabled`), false);
  await click('aside button[data-s="proof"]');
  check('R14: Info closed on Proof', await ev(`$('info').hidden`), false);
  check('R14: Info button disabled on Proof', await ev(`$('b-info').disabled`), false);
  await click('aside button[data-s="health"]');
} catch (e) { console.error(e); failed++; }

check('no script errors', errors.length == 0, errors);
console.log(failed ? `${failed} check(s) failed` : 'all behaviour checks passed');
done(failed ? 1 : 0);
