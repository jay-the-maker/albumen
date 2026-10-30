// SPDX-License-Identifier: Apache-2.0
//
// M5 Live viewing behaviour test for `albumen gui --library`, in headless
// Chrome or Edge over the DevTools protocol with real mouse input. No npm
// packages. The library is a temp copy of the repo's synthetic CC0 Live
// Photo fixture (fixtures/live-photo/IMG_0005.heic + .mov, H.264).
//
//   node scripts/gui-live-behaviour.mjs target/debug/albumen [screenshot-dir]
//
// Env: CHROME (default google-chrome; Edge works too). Checks that clicking
// LIVE really plays the movie (currentTime advances, muted is true), that
// /video serves the original bytes with HTTP Range (206) so scrubbing works,
// that an HEVC Live movie (a synthetic stub: the fixture with its sample
// entry relabelled hvc1) gets the can't-preview copy and no player, that a
// mismatched pair shows its reason line, and which MIME label Chrome plays.
// Screenshots (badge, clicked, playing, scrubbed, mismatch, hevc) go to the
// optional directory.
import {spawn} from 'node:child_process';
import {mkdtempSync, mkdirSync, copyFileSync, readFileSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createInterface} from 'node:readline';

const bin = process.argv[2] || 'target/debug/albumen';
const shots = process.argv[3];
if (shots) mkdirSync(shots, {recursive: true});
const work = mkdtempSync(join(tmpdir(), 'albumen-gui-live-'));
const lib = join(work, 'library');
mkdirSync(join(lib, 'roll'), {recursive: true});
copyFileSync('fixtures/live-photo/IMG_0005.heic', join(lib, 'roll', 'IMG_0005.HEIC'));
copyFileSync('fixtures/live-photo/IMG_0005.mov', join(lib, 'roll', 'IMG_0005.MOV'));
const movie = readFileSync('fixtures/live-photo/IMG_0005.mov');
// Mismatched pair: same still, movie whose Live Photo ID differs by one digit.
copyFileSync('fixtures/live-photo/IMG_0005.heic', join(lib, 'roll', 'IMG_0008.HEIC'));
const other = Buffer.from(movie);
const idAt = other.toString('latin1').search(/[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-/);
if (idAt < 0) throw new Error('no Live Photo ID in the fixture movie');
other[idAt] = other[idAt] == 0x30 ? 0x31 : 0x30;
writeFileSync(join(lib, 'roll', 'IMG_0008.MOV'), other);
// HEVC stub: the fixture with its video sample entry code set to hvc1.
copyFileSync('fixtures/live-photo/IMG_0005.heic', join(lib, 'roll', 'IMG_0010.HEIC'));
const hevc = Buffer.from(movie);
hevc.write('hvc1', hevc.indexOf('avc1'), 'latin1');
writeFileSync(join(lib, 'roll', 'IMG_0010.MOV'), hevc);
const sleep = ms => new Promise(r => setTimeout(r, ms));
const kids = [];
const done = code => { for (const k of kids) k.kill(); process.exit(code); };

const env = {...process.env, ALBUMEN_HOME: join(work, 'home')};
const gui = spawn(bin, ['gui', '--library', lib, '--no-open', '--port', '0', '--work', join(work, 'state')], {stdio: ['ignore', 'pipe', 'inherit'], env});
kids.push(gui);
const url = await new Promise((res, rej) => {
  const rl = createInterface({input: gui.stdout});
  rl.on('line', l => { const m = l.match(/^albumen gui: (http:\/\/\S+?)\/?$/); if (m) res(m[1]); });
  gui.on('exit', c => rej(new Error('gui exited ' + c)));
  setTimeout(() => rej(new Error('gui printed no URL')), 120000);
});

const port = 9300 + Math.floor(Math.random() * 500);
const chrome = spawn(process.env.CHROME || 'google-chrome', ['--headless=new', '--no-sandbox', '--hide-scrollbars',
  '--remote-debugging-port=' + port, '--window-size=1280,800', '--user-data-dir=' + join(work, 'chrome'), 'about:blank'],
  {stdio: 'ignore'});
kids.push(chrome);
let tabs;
for (let i = 0; i < 100 && !tabs; i++) { try { tabs = await (await fetch(`http://127.0.0.1:${port}/json`)).json(); } catch { await sleep(200); } }
if (!tabs) { console.error('browser did not start'); done(2); }
const version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
console.log('browser: ' + version.Browser);
const ws = new WebSocket(tabs.find(t => t.type == 'page').webSocketDebuggerUrl);
await new Promise(r => ws.onopen = r);
let id = 0; const pend = {}; const errors = [];
ws.onmessage = m => { const d = JSON.parse(m.data);
  if (d.method == 'Runtime.exceptionThrown') errors.push(d.params.exceptionDetails.exception?.description || d.params.exceptionDetails.text);
  if (pend[d.id]) { pend[d.id](d); delete pend[d.id]; } };
const cmd = (method, params = {}) => new Promise(r => { const i = ++id; pend[i] = r; ws.send(JSON.stringify({id: i, method, params})); });
const ev = async js => { const r = await cmd('Runtime.evaluate', {expression: js, awaitPromise: true, returnByValue: true});
  if (r.result?.exceptionDetails) throw new Error(js + ': ' + r.result.exceptionDetails.exception?.description); return r.result?.result?.value; };
const until = async (js, what) => { for (let i = 0; i < 100; i++) { if (await ev(js)) return; await sleep(50); } throw new Error('timed out: ' + what); };
// sel: a CSS selector, or {js} returning the element.
const click = async sel => {
  const pick = typeof sel == 'string' ? `document.querySelector(${JSON.stringify(sel)})` : sel.js;
  const b = await ev(`(()=>{const e=${pick};if(!e)return null;e.scrollIntoView({block:'center'});const r=e.getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};})()`);
  if (!b) throw new Error('no element ' + JSON.stringify(sel));
  await cmd('Input.dispatchMouseEvent', {type: 'mousePressed', x: b.x, y: b.y, button: 'left', clickCount: 1});
  await cmd('Input.dispatchMouseEvent', {type: 'mouseReleased', x: b.x, y: b.y, button: 'left', clickCount: 1});
};

let failed = 0;
const check = (name, ok, got) => { console.log((ok ? 'ok   ' : 'FAIL ') + name + '  ' + JSON.stringify(got)); if (!ok) failed++; };

await cmd('Runtime.enable');
await cmd('Page.enable');
await cmd('Network.enable');
const videoResponses = [];
const onMsg = ws.onmessage;
ws.onmessage = m => { const d = JSON.parse(m.data);
  if (d.method == 'Network.responseReceived' && d.params.response.url.includes('/video?'))
    videoResponses.push({status: d.params.response.status, range: d.params.response.headers['Content-Range'] || d.params.response.headers['content-range'] || null});
  onMsg(m); };
const snap = async name => { if (!shots) return;
  const png = await cmd('Page.captureScreenshot', {format: 'png'});
  writeFileSync(join(shots, name + '.png'), Buffer.from(png.result.data, 'base64')); console.log('screenshot ' + name + '.png'); };
const fig = name => ({js: `[...document.querySelectorAll('#grid figure')].find(f=>f.innerText.includes(${JSON.stringify(name)}))`});
const badge = name => ({js: `${fig(name).js}?.querySelector('.live')`});
await cmd('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
await cmd('Page.navigate', {url: url + '/'});
await until(`typeof lib!=='undefined'&&lib.images.length>0`, 'library loaded');

try {
  const res = await fetch(url + '/video?p=' + encodeURIComponent('roll/IMG_0005.MOV'));
  const served = Buffer.from(await res.arrayBuffer());
  check('/video Content-Type', res.headers.get('content-type') == 'video/quicktime', res.headers.get('content-type'));
  check('/video bytes equal the original', served.equals(movie), served.length);
  check('/video says Accept-Ranges: bytes', res.headers.get('accept-ranges') == 'bytes', res.headers.get('accept-ranges'));
  const part = await fetch(url + '/video?p=' + encodeURIComponent('roll/IMG_0005.MOV'), {headers: {Range: 'bytes=100-199'}});
  const pb = Buffer.from(await part.arrayBuffer());
  check('Range bytes=100-199 gives 206 and the slice', part.status == 206 && part.headers.get('content-range') == `bytes 100-199/${movie.length}` && pb.equals(movie.subarray(100, 200)),
    [part.status, part.headers.get('content-range'), pb.length]);
  const bad = await fetch(url + '/video?p=' + encodeURIComponent('roll/IMG_0005.MOV'), {headers: {Range: `bytes=${movie.length}-`}});
  check('Range past the end gives 416', bad.status == 416 && bad.headers.get('content-range') == `bytes */${movie.length}`, [bad.status, bad.headers.get('content-range')]);
  const tiles = await ev(`[...document.querySelectorAll('#grid figure')].map(f=>f.innerText.replace(/\s+/g,' ').trim())`);
  check('LIVE badges on the two ID-matched pairs only', await ev(`document.querySelectorAll('#grid figure .live').length`) == 2 &&
    (await ev(`${badge('IMG_0005').js}!=null&&${badge('IMG_0010').js}!=null&&${badge('IMG_0008').js}==null`)), tiles);
  await snap('badge');

  // The real click on the LIVE badge.
  await click(badge('IMG_0005'));
  await until(`!!document.querySelector('#i-live video')`, 'video element');
  await snap('clicked');
  await until(`(()=>{const v=document.querySelector('#i-live video');return v.currentTime>0.15&&!v.paused;})()`, 'currentTime advances');
  await snap('playing');
  const s1 = await ev(`(()=>{const v=document.querySelector('#i-live video');return {t:v.currentTime,muted:v.muted,paused:v.paused,controls:v.controls,w:v.videoWidth,h:v.videoHeight,src:v.currentSrc,err:v.error&&v.error.code};})()`);
  await sleep(250);
  const s2 = await ev(`(()=>{const v=document.querySelector('#i-live video');return {t:v.currentTime,ended:v.ended};})()`);
  check('LIVE click plays: currentTime advances', s1.t > 0 && (s2.t > s1.t || s2.ended), [s1.t, s2.t, s2.ended]);
  check('LIVE plays muted', s1.muted === true, s1.muted);
  check('LIVE plays with controls, decoded frames', s1.controls && s1.w > 0 && s1.h > 0 && !s1.err, s1);

  // Scrub: jump mid-clip and play on from there.
  const sc = await ev(`(async()=>{const v=document.querySelector('#i-live video');v.currentTime=0.5;
    await new Promise(r=>{if(!v.seeking)return r();v.addEventListener('seeked',r,{once:true});});
    const at=v.currentTime;await v.play();
    for(let i=0;i<40&&v.currentTime<at+0.15&&!v.ended;i++)await new Promise(r=>setTimeout(r,50));
    return {at,now:v.currentTime,ended:v.ended,paused:v.paused,seekable:v.seekable.length?[v.seekable.start(0),v.seekable.end(0)]:null};})()`);
  check('scrub to 0.5 s then plays on', sc.at >= 0.45 && sc.at <= 0.55 && (sc.now > sc.at + 0.1 || sc.ended), sc);
  await snap('scrubbed');
  console.log('browser /video responses: ' + JSON.stringify(videoResponses));
  check('browser fetched the movie with 206 ranges', videoResponses.some(r => r.status == 206 && r.range), videoResponses.length);

  // Mismatched pair: two tiles, the Info reason line, no LIVE.
  await click(fig('IMG_0008.HEIC'));
  await until(`!document.getElementById('i-note').hidden`, 'mismatch reason');
  const note = await ev(`document.getElementById('i-note').textContent`);
  check('mismatched pair shows its reason line', note == 'Not joined as a Live Photo: IMG_0008.HEIC and IMG_0008.MOV have different Live Photo IDs.', note);
  await snap('mismatch');

  // HEVC: LIVE badge click shows the can't-preview copy, no player.
  await click(badge('IMG_0010'));
  await sleep(300);
  const hv = await ev(`({videos:document.querySelectorAll('#i-live video').length,note:document.getElementById('i-live-note').hidden?null:document.getElementById('i-live-note').textContent,play:!document.getElementById('i-play').hidden})`);
  check('HEVC Live movie: can\'t-preview copy, no player', hv.videos == 0 && !hv.play && hv.note == "IMG_0010.MOV (HEVC video). Can't preview this file here. Your original is kept unchanged.", hv);
  await snap('hevc');

  // MIME evidence: the same bytes under each label, played from a Blob.
  for (const type of ['video/quicktime', 'video/mp4']) {
    const r = await ev(`(async()=>{const b=await (await fetch('/video?p='+encodeURIComponent('roll/IMG_0005.MOV'))).blob();
      const v=document.createElement('video');v.muted=true;v.src=URL.createObjectURL(new Blob([b],{type:${JSON.stringify(type)}}));
      document.body.append(v);let err=null;try{await v.play();}catch(e){err=String(e);}
      for(let i=0;i<40&&v.currentTime<0.15;i++)await new Promise(r=>setTimeout(r,50));
      const out={canPlayType:v.canPlayType(${JSON.stringify(type + '; codecs="avc1.42E01E"')})||v.canPlayType(${JSON.stringify(type)}),t:v.currentTime,err:err||(v.error&&v.error.code)};v.remove();return out;})()`);
    console.log(`mime ${type}: ${JSON.stringify(r)}`);
  }
} catch (e) { console.error(e); failed++; }

check('no script errors', errors.length == 0, errors);
console.log(failed ? `${failed} check(s) failed` : 'all live behaviour checks passed');
done(failed ? 1 : 0);
