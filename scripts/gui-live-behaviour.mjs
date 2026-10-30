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
// G2: a lone HEVC movie (same stub, no still): quiet tile, no player.
writeFileSync(join(lib, 'roll', 'IMG_0030.MOV'), hevc);
// G3: a view of 20 lone HEIC stills. G5: 2,000 items, 200 of them movies.
const lib20 = join(work, 'lib20'); mkdirSync(join(lib20, 'heic'), {recursive: true});
for (let i = 0; i < 20; i++) copyFileSync('fixtures/live-photo/IMG_0005.heic', join(lib20, 'heic', `IMG_${1000 + i}.HEIC`));
const lib2k = join(work, 'lib2k'); mkdirSync(join(lib2k, 'mix'), {recursive: true});
for (let i = 0; i < 2000; i++) { const n = String(i).padStart(4, '0');
  if (i % 10 == 5) copyFileSync('fixtures/live-photo/IMG_0005.mov', join(lib2k, 'mix', `CLIP_${n}.MOV`));
  else copyFileSync('fixtures/raw-jpeg/IMG_0003.jpg', join(lib2k, 'mix', `PIC_${n}.jpg`)); }
const sleep = ms => new Promise(r => setTimeout(r, ms));
const kids = [];
let closeBrowser = async () => {};
const done = async code => { try { await closeBrowser(); } catch {} await sleep(500); for (const k of kids) k.kill(); process.exit(code); };

const env = {...process.env, ALBUMEN_HOME: join(work, 'home')};
const startGui = (l, st) => { const gui = spawn(bin, ['gui', '--library', l, '--no-open', '--port', '0', '--work', join(work, st)], {stdio: ['ignore', 'pipe', 'inherit'], env});
kids.push(gui);
return new Promise((res, rej) => {
  const rl = createInterface({input: gui.stdout});
  rl.on('line', l => { const m = l.match(/^albumen gui: (http:\/\/\S+?)\/?$/); if (m) res(m[1]); });
  gui.on('exit', c => rej(new Error('gui exited ' + c)));
  setTimeout(() => rej(new Error('gui printed no URL')), 120000);
}); };
const url = await startGui(lib, 'state');

const port = 9300 + Math.floor(Math.random() * 500);
// HEADED=1: a normal visible window (used for the Edge run); the temp
// --user-data-dir keeps the user's own profile and settings untouched.
const headed = process.env.HEADED == '1';
const chrome = spawn(process.env.CHROME || 'google-chrome', [...(headed ? [] : ['--headless=new', '--hide-scrollbars']), '--no-sandbox',
  '--no-first-run', '--no-default-browser-check',
  '--remote-debugging-port=' + port, '--window-size=1280,800', '--user-data-dir=' + join(work, 'chrome'), 'about:blank'],
  {stdio: 'ignore'});
kids.push(chrome);
let tabs;
for (let i = 0; i < 100 && !tabs; i++) { try { tabs = await (await fetch(`http://127.0.0.1:${port}/json`)).json(); } catch { await sleep(200); } }
if (!tabs) { console.error('browser did not start'); await done(2); }
const version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
console.log('browser: ' + version.Browser);
const ws = new WebSocket(tabs.find(t => t.type == 'page').webSocketDebuggerUrl);
await new Promise(r => ws.onopen = r);
console.log('mode: ' + (headed ? 'headed (visible window)' : 'headless'));
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

const hover = async sel => {
  const b = await ev(`(()=>{const r=document.querySelector(${JSON.stringify(sel)}).getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height-12};})()`);
  await cmd('Input.dispatchMouseEvent', {type: 'mouseMoved', x: b.x, y: b.y}); await sleep(600);
};
let failed = 0;
const CANT = "Can't preview this file here. Your original is kept unchanged.";
const check = (name, ok, got) => { console.log((ok ? 'ok   ' : 'FAIL ') + name + '  ' + JSON.stringify(got)); if (!ok) failed++; };

closeBrowser = () => Promise.race([cmd('Browser.close'), sleep(2000)]);
await cmd('Runtime.enable');
await cmd('Page.enable');
await cmd('Network.enable');
const videoResponses = [];
const onMsg = ws.onmessage;
// G5: /video requests in flight, from the browser's own network log.
const inflight = new Set(); let maxInflight = 0, videoReqs = 0;
ws.onmessage = m => { const d = JSON.parse(m.data);
  if (d.method == 'Network.requestWillBeSent' && d.params.request.url.includes('/video?')) { inflight.add(d.params.requestId); videoReqs++; maxInflight = Math.max(maxInflight, inflight.size); }
  if (d.method == 'Network.loadingFinished' || d.method == 'Network.loadingFailed') inflight.delete(d.params.requestId);
  if (d.method == 'Network.responseReceived' && d.params.response.url.includes('/video?'))
    videoResponses.push({status: d.params.response.status, range: d.params.response.headers['Content-Range'] || d.params.response.headers['content-range'] || null});
  onMsg(m); };
const snap = async name => { if (!shots) return;
  const png = await cmd('Page.captureScreenshot', {format: 'png'});
  writeFileSync(join(shots, name + '.png'), Buffer.from(png.result.data, 'base64')); console.log('screenshot ' + name + '.png'); };
const fig = name => ({js: `[...document.querySelectorAll('#grid figure')].find(f=>f.innerText.includes(${JSON.stringify(name)}))`});
const badge = name => ({js: `${fig(name).js}?.querySelector('.live')`});
if (!headed) await cmd('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
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
  const tiles = await ev(`[...document.querySelectorAll('#grid figure')].map(f=>f.innerText.replace(/\\s+/g,' ').trim())`);
  check('LIVE badges on the two ID-matched pairs only', await ev(`document.querySelectorAll('#grid figure .live').length`) == 2 &&
    (await ev(`${badge('IMG_0005').js}!=null&&${badge('IMG_0010').js}!=null&&${badge('IMG_0008').js}==null`)), tiles);
  const heads = await ev(`({photos:document.getElementById('photos-count').textContent,lib:document.getElementById('lib-count').textContent})`);
  check('header counts: Live pair is one photo, lone movie is a video', heads.photos == '3 photos, 2 videos' && heads.lib == '3 photos, 2 videos', heads);
  const forms = await ev(`[countText(['roll/IMG_0005.HEIC']),countText(['roll/IMG_0008.MOV']),countText(['roll/IMG_0005.HEIC','roll/IMG_0008.MOV']),countText([]),countText(['roll/IMG_0005.HEIC','roll/IMG_0010.HEIC'])]`);
  check('count wording singular/plural', JSON.stringify(forms) == JSON.stringify(['1 photo', '1 video', '1 photo, 1 video', '0 photos', '2 photos']), forms);
  check('tile copy has no stray leading dash', !tiles.some(t => t.includes('\u2014')), tiles);
  const heicTile = await ev(`(()=>{const f=${fig('IMG_0005.HEIC').js};return f&&{ph:f.querySelector('.ph').innerText.replace(/\\s+/g,' ').trim(),title:f.querySelector('.ph').title,live:!!f.querySelector('.live'),cap:f.querySelector('figcaption').textContent,all:f.innerText};})()`);
  check('G3 HEIC tile: quiet HEIC glyph, LIVE badge, name once as the caption, copy only on hover', heicTile && heicTile.cap == 'IMG_0005.HEIC' &&
    heicTile.ph == 'HEIC' && heicTile.title == CANT && heicTile.live && heicTile.all.split('IMG_0005.HEIC').length == 2, heicTile);
  const np = await ev(`({shown:!document.getElementById('np').hidden,text:document.getElementById('np-t').textContent,n:document.querySelectorAll('.np').length})`);
  check('G3 one banner for the view', np.shown && np.n == 1 && np.text == "3 HEIC photos and 1 other file can't be previewed here yet. Your originals are kept unchanged.", np);
  check('G3 no tile carries the can\'t-preview text', await ev(`[...document.querySelectorAll('#grid figure')].filter(f=>f.innerText.includes("Can't preview")).length`) == 0, null);
  // G1: the lone H.264 movie tile gets a browser-drawn poster, length and play glyph.
  await until(`(()=>{const v=${fig('IMG_0008.MOV').js}?.querySelector('video');return v&&v.readyState>=2&&!v.seeking;})()`, 'G1 poster frame');
  const g1 = await ev(`(()=>{const f=${fig('IMG_0008.MOV').js};const v=f.querySelector('video');const c=document.createElement('canvas');c.width=v.videoWidth;c.height=v.videoHeight;
    const x=c.getContext('2d');x.drawImage(v,0,0);const d=x.getImageData(0,0,c.width,c.height).data;let s=0,s2=0,n=0;for(let i=0;i<d.length;i+=4){const y=(d[i]+d[i+1]+d[i+2])/3;s+=y;s2+=y*y;n++;}
    const m=s/n;return {preload:v.preload,muted:v.muted,controls:v.controls,src:v.getAttribute('src'),w:v.videoWidth,h:v.videoHeight,variance:s2/n-m*m,dur:f.querySelector('.dur').textContent,play:!!f.querySelector('.play'),text:f.innerText.replace(/\\s+/g,' ').trim()};})()`);
  check('G1 movie tile: poster pixels vary, duration, play glyph, no can\'t-preview', g1.preload == 'metadata' && g1.muted && !g1.controls && g1.src.startsWith('/video?p=') && g1.w > 0 && g1.variance > 0 &&
    /^\d+:\d\d$/.test(g1.dur) && g1.play && !g1.text.includes("Can't preview"), g1);
  await snap('g1-poster');
  // G2: the lone HEVC movie keeps the quiet tile and gets no <video>.
  const g2 = await ev(`(()=>{const f=${fig('IMG_0030.MOV').js};return f&&{videos:f.querySelectorAll('video').length,ph:f.querySelector('.ph')?.innerText.trim(),title:f.querySelector('.ph')?.title};})()`);
  check('G2 HEVC movie tile: no <video>, quiet placeholder, copy on hover', g2 && g2.videos == 0 && g2.ph == 'MOV' && g2.title == CANT, g2);
  await click(fig('IMG_0030.MOV'));
  await sleep(200);
  const g2i = await ev(`document.getElementById('i-size').textContent`);
  check('G2 HEVC movie Info keeps the full can\'t-preview copy', g2i.endsWith('MOV. ' + CANT), g2i);
  await click(fig('IMG_0005.HEIC'));
  await sleep(200);
  const g3i = await ev(`({name:document.getElementById('i-name').textContent,size:document.getElementById('i-size').textContent,taken:document.getElementById('i-taken').textContent})`);
  check('G3 HEIC Info: file name, type, date and the full copy', g3i.name == 'IMG_0005.HEIC' && g3i.size.endsWith('HEIC. ' + CANT) && g3i.taken.length > 0, g3i);
  await click(fig('IMG_0008.MOV'));
  await sleep(200);
  const g1i = await ev(`document.getElementById('i-size').textContent`);
  check('G1 H.264 movie Info has no can\'t-preview copy', !g1i.includes("Can't preview") && g1i.includes('H.264 video'), g1i);
  await snap('badge');

  // The real click on the LIVE badge.
  await click(badge('IMG_0005'));
  await until(`!!document.querySelector('#i-live video')`, 'video element');
  await snap('clicked');
  await until(`(()=>{const v=document.querySelector('#i-live video');return v.currentTime>0.15&&!v.paused;})()`, 'currentTime advances');
  await hover('#i-live video');
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
  await ev(`document.querySelector('#i-live video').pause()`);
  await hover('#i-live video');
  const ro = await ev(`(()=>{const v=document.querySelector('#i-live video');return {t:v.currentTime,paused:v.paused,controls:v.controls};})()`);
  check('after scrub: paused mid-clip with controls shown', ro.t > 0.5 && ro.paused && ro.controls, ro);
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

  // G3: a view of 20 HEIC photos says it once, in one dismissible banner.
  const url20 = await startGui(lib20, 'state20');
  await cmd('Page.navigate', {url: url20 + '/'});
  await until(`typeof lib!=='undefined'&&lib.images.length==20&&document.querySelectorAll('#grid figure').length==20`, 'G3 library');
  await sleep(100);
  const b20 = await ev(`({tiles:[...document.querySelectorAll('#grid figure')].filter(f=>f.innerText.includes("Can't preview")).length,
    banners:[...document.querySelectorAll('.np')].filter(b=>!b.hidden).length,text:document.getElementById('np-t').textContent,ph:[...new Set([...document.querySelectorAll('#grid .ph')].map(p=>p.innerText.trim()))]})`);
  check('G3 20 HEIC: 0 tile copies, 1 banner', b20.tiles == 0 && b20.banners == 1 && b20.text == "20 HEIC photos can't be previewed here yet. Your originals are kept unchanged." && b20.ph.join() == 'HEIC', b20);
  await snap('g3-banner');
  await click('#np-x');
  await ev(`(()=>{const s=document.getElementById('search');s.dispatchEvent(new Event('input'));})()`);
  await sleep(200);
  check('G3 banner stays dismissed for the view', await ev(`document.getElementById('np').hidden`) === true, null);

  // G5: 2,000 items with 200 movies: posters lazy, at most 6 /video at once.
  const url2k = await startGui(lib2k, 'state2k');
  inflight.clear(); maxInflight = 0; videoReqs = 0;
  const t0 = Date.now();
  await cmd('Page.navigate', {url: url2k + '/'});
  await until(`typeof lib!=='undefined'&&lib.images.length==2000&&[...document.querySelectorAll('#grid img')].some(i=>i.complete&&i.naturalWidth>0)`, 'G5 first tile');
  const firstTile = Date.now() - t0;
  check('G5 first screen of tiles within 1.0 s (first-thumbnail budget)', firstTile <= 1000, firstTile);
  await sleep(1500);
  const early = await ev(`({posters:window.__poster.started,movies:lib.images.filter(i=>i.video).length})`);
  check('G5 posters are lazy: far-off movies not loaded yet', early.movies == 200 && early.posters > 0 && early.posters < 200, early);
  for (let i = 0; i < 80; i++) { await ev(`(()=>{const p=document.getElementById('photos');p.scrollTop+=p.clientHeight;})()`); await sleep(120); }
  await sleep(2000);
  const late = await ev(`({posters:window.__poster.started,max:window.__poster.max,tiles:document.querySelectorAll('#grid figure').length,done:gs&&gs.done})`);
  console.log('G5 /video requests ' + videoReqs + ', max in flight ' + maxInflight + ', page ' + JSON.stringify(late));
  check('G5 at most 6 /video requests at once', maxInflight > 0 && maxInflight <= 6 && late.max <= 6, {maxInflight, page: late.max});
  await snap('g5-grid');
} catch (e) { console.error(e); failed++; }

check('no script errors', errors.length == 0, errors);
console.log(failed ? `${failed} check(s) failed` : 'all live behaviour checks passed');
await done(failed ? 1 : 0);
