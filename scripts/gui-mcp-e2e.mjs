// SPDX-License-Identifier: Apache-2.0
//
// Real-binary E2E for the shared index (M6): the GUI (`albumen gui`) and the
// search tool (`albumen-mcp`) run against one synthetic library and one
// index directory. Each sees the other's changes, they keep one index (one
// key), the GUI keeps answering whole listings while the search tool saves,
// and no original is changed. No npm packages; synthetic files only.
//
//   node scripts/gui-mcp-e2e.mjs target/debug/albumen target/debug/albumen-mcp
import {spawn} from 'node:child_process';
import {createHash} from 'node:crypto';
import {mkdtempSync, mkdirSync, writeFileSync, readdirSync, readFileSync, statSync, unlinkSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, relative} from 'node:path';
import {createInterface} from 'node:readline';

const bin = process.argv[2] || 'target/debug/albumen';
const mcp = process.argv[3] || 'target/debug/albumen-mcp';
const work = mkdtempSync(join(tmpdir(), 'albumen-gui-mcp-'));
const lib = join(work, 'library');
const index = join(work, 'index');
const sleep = ms => new Promise(r => setTimeout(r, ms));
const kids = [];
let failed = 0;
const check = (ok, what) => { console.log((ok ? 'ok   ' : 'FAIL ') + what); if (!ok) failed++; };
const done = code => { for (const k of kids) { try { k.kill(); } catch {} } process.exit(code); };

const put = (rel, body) => { mkdirSync(join(lib, rel, '..'), {recursive: true}); writeFileSync(join(lib, rel), body); };
for (let i = 0; i < 50; i++) put(`20${10 + i % 5}/IMG_${String(i).padStart(4, '0')}.jpg`, `synthetic ${i}`);
const walk = dir => readdirSync(dir, {withFileTypes: true}).flatMap(e => e.isDirectory() ? walk(join(dir, e.name)) : [join(dir, e.name)]);
const snapshot = () => Object.fromEntries(walk(lib).map(f => [relative(lib, f).replaceAll('\\', '/'),
  createHash('sha256').update(readFileSync(f)).digest('hex') + ' ' + statSync(f).mtimeMs]));

const env = {...process.env, ALBUMEN_INDEX_DIR: index, ALBUMEN_HOME: join(work, 'home')};
const gui = spawn(bin, ['gui', '--no-open', '--port', '0', '--library', lib, '--work', join(work, 'state')], {stdio: ['ignore', 'pipe', 'inherit'], env});
kids.push(gui);
const url = await new Promise((res, rej) => {
  createInterface({input: gui.stdout}).on('line', l => { const m = l.match(/^albumen gui: (http:\/\/\S+?)\/?$/); if (m) res(m[1]); });
  gui.on('exit', c => rej(new Error('gui exited ' + c)));
  setTimeout(() => rej(new Error('gui printed no URL')), 120000);
}).catch(e => { console.error(e.message); done(2); });

// One listing as the page gets it. With `refresh` the GUI re-reads the
// shared index in the background (as after a scan); poll until that listing
// is published (`want`) or 10 s pass, and return what it shows last.
async function guiPaths(refresh = false, want = null) {
  const get = async q => { const l = await (await fetch(url + '/api/library?offset=0&limit=5000' + q)).json();
    return l.status == 'ready' ? (l.images || []).map(x => x.path).sort() : null; };
  let p = await get(refresh ? '&refresh=1' : '');
  for (let i = 0; i < 600 && (!p || (want && !same(p, want))); i++) { await sleep(refresh ? 20 : 100); p = await get(''); }
  if (!p) throw new Error('gui never ready');
  return p;
}
async function guiIdle() {
  for (let i = 0; i < 200; i++) { const x = await (await fetch(url + '/api/index')).json(); if (!x.pending) return true; await sleep(100); }
  return false;
}
function mcpSearch(query) {
  return new Promise((res, rej) => {
    const c = spawn(mcp, ['--library', lib, '--index-dir', index, '--thumbnail-dir', join(work, 'thumbs')], {stdio: ['pipe', 'pipe', 'inherit'], env});
    let out = '';
    c.stdout.on('data', d => out += d);
    c.on('exit', () => {
      const line = out.split('\n').find(l => l.includes('"id":2'));
      if (!line) return rej(new Error('albumen-mcp gave no answer: ' + out.slice(0, 300)));
      const r = JSON.parse(line).result;
      if (r.isError) return rej(new Error('albumen-mcp error: ' + r.content[0].text));
      res(JSON.parse(r.content[0].text).files.map(f => f.path).sort());
    });
    c.stdin.end([
      '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}',
      JSON.stringify({jsonrpc: '2.0', id: 2, method: 'tools/call', params: {name: 'search', arguments: {query}}}), ''].join('\n'));
  });
}
const same = (a, b) => JSON.stringify(a) == JSON.stringify(b);
const libPaths = () => walk(lib).map(f => relative(lib, f).replaceAll('\\', '/')).sort();
const keys = () => [...new Set(readdirSync(index).filter(n => /\.idx\.(a|b|lock)$/.test(n)).map(n => n.split('.')[0]))];

try {
  let want = libPaths();
  check(same(await guiPaths(false, want), want), `gui lists the library (${want.length} files)`);
  check(await guiIdle(), 'gui index save not pending');
  check(same(await mcpSearch('img_'), want), 'albumen-mcp search finds the same files from the shared index');
  check(keys().length == 1, `one index key in the index dir (${keys().join(',')})`);

  // GUI-side change seen by the search tool.
  for (let i = 0; i < 5; i++) put(`new-roll/IMG_${9000 + i}.jpg`, `synthetic new ${i}`);
  want = libPaths();
  check(same(await guiPaths(true, want), want), `gui picks up 5 new files (${want.length})`);
  await guiIdle();
  check(same(await mcpSearch('img_'), want), 'albumen-mcp sees the files the gui indexed');

  // Search-side change seen by the GUI.
  for (const rel of want.slice(0, 3)) unlinkSync(join(lib, rel));
  put('later/IMG_9100.jpg', 'synthetic later');
  want = libPaths();
  check(same(await mcpSearch('img_'), want), `albumen-mcp drops 3 removed files and finds 1 added (${want.length})`);
  check(same(await guiPaths(true, want), want), 'gui listing matches after the search tool saved');

  // Both at once: the GUI keeps answering whole listings while searches save.
  const before = snapshot();
  let listings = 0, partial = 0, searches = 0, wrong = 0;
  let stop = false;
  const poller = (async () => { while (!stop) { const p = await guiPaths(true); listings++; if (!same(p, want)) partial++; } })();
  for (let i = 0; i < 10; i++) { const p = await mcpSearch('img_'); searches++; if (!same(p, want)) wrong++; }
  stop = true; await poller;
  check(partial == 0 && listings > 0, `gui listings during searches: ${listings}, partial ${partial}`);
  check(wrong == 0, `searches during gui listings: ${searches}, wrong ${wrong}`);
  check(keys().length == 1, 'still one index key');
  check(same(snapshot(), before), `originals unchanged (sha256 + mtime of ${Object.keys(before).length} files)`);
  check(readdirSync(lib).every(n => !/idx|albumen-index/.test(n)), 'nothing written inside the library');
  console.log(`M6 E2E RESULT gui_listings=${listings} partial=${partial} searches=${searches} wrong=${wrong} keys=${keys().length} failed=${failed}`);
} catch (e) { console.error('FAIL ' + e.message); failed++; }
console.log(failed ? `${failed} gui+mcp checks failed` : 'all gui+mcp checks passed');
done(failed ? 1 : 0);
