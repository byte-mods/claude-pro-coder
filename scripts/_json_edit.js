#!/usr/bin/env node
// Dependency-free JSON editor for the install pipeline.
//
// Behavioural twin of scripts/_json_edit.py — _lib.sh picks whichever runtime
// the machine has (python3, python, then node), so these two MUST stay
// identical. Change one, change the other, and cover it in round_trip.sh.
//
// Usage / exit codes: see the docstring at the top of _json_edit.py.

'use strict';

const fs = require('fs');
const path = require('path');

function die(msg) {
  process.stderr.write('_json_edit: ' + msg + '\n');
  process.exit(1);
}

// Deep structural equality — used for the idempotency checks (NOCHANGE) and for
// comparing hook arrays. Key order is deliberately ignored; JSON objects are
// unordered maps and a reordered-but-equal config must not count as a change.
function eq(a, b) {
  if (a === b) return true;
  if (a === null || b === null) return a === b;
  if (typeof a !== typeof b) return false;
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
    return a.every((v, i) => eq(v, b[i]));
  }
  if (typeof a !== 'object') return false;
  const ka = Object.keys(a), kb = Object.keys(b);
  if (ka.length !== kb.length) return false;
  return ka.every(k => Object.prototype.hasOwnProperty.call(b, k) && eq(a[k], b[k]));
}

function isObj(v) {
  return v !== null && typeof v === 'object' && !Array.isArray(v);
}

function load(file) {
  // Missing file -> {}. Malformed file -> abort; never overwrite a config we
  // could not parse, because that would silently destroy user state.
  if (!fs.existsSync(file)) return {};
  let text;
  try {
    text = fs.readFileSync(file, 'utf8');
  } catch (e) {
    die('cannot read ' + file + ': ' + e.message);
  }
  if (text.trim() === '') return {};
  let data;
  try {
    data = JSON.parse(text);
  } catch (e) {
    die(file + ' is not valid JSON: ' + e.message + '. Refusing to overwrite.');
  }
  if (!isObj(data)) die(file + ' is not a JSON object; refusing to merge.');
  return data;
}

function save(file, data, dryRun) {
  if (dryRun) return;
  const outDir = path.dirname(path.resolve(file)) || '.';
  fs.mkdirSync(outDir, { recursive: true });
  // Atomic: stage a sibling tmp file on the same filesystem, then rename over.
  const tmp = path.join(outDir, '.json-edit.staging.' + process.pid + '.' + Date.now());
  try {
    fs.writeFileSync(tmp, JSON.stringify(data, null, 2) + '\n', 'utf8');
    fs.renameSync(tmp, file);
  } catch (e) {
    try { fs.unlinkSync(tmp); } catch (_) { /* best effort */ }
    throw e;
  }
}

function getPath(data, parts) {
  let cur = data;
  for (const p of parts) {
    if (!isObj(cur) || !Object.prototype.hasOwnProperty.call(cur, p)) {
      return { found: false, value: undefined };
    }
    cur = cur[p];
  }
  return { found: true, value: cur };
}

function setPath(data, parts, value) {
  let cur = data;
  for (const p of parts.slice(0, -1)) {
    if (cur[p] === undefined || cur[p] === null) cur[p] = {};
    else if (!isObj(cur[p])) die("cannot descend into '" + p + "' — it is not an object.");
    cur = cur[p];
  }
  cur[parts[parts.length - 1]] = value;
}

// Remove the leaf, then walk back up pruning containers we emptied.
function unsetPath(data, parts) {
  const chain = [data];
  let cur = data;
  for (const p of parts.slice(0, -1)) {
    if (!isObj(cur[p])) return false;
    chain.push(cur[p]);
    cur = cur[p];
  }
  const leaf = parts[parts.length - 1];
  if (!Object.prototype.hasOwnProperty.call(cur, leaf)) return false;
  delete cur[leaf];
  for (let i = chain.length - 1; i > 0; i--) {
    if (Object.keys(chain[i]).length === 0) delete chain[i - 1][parts[i - 1]];
    else break;
  }
  return true;
}

function hookEntries(data, event) {
  const hooks = data.hooks;
  if (hooks !== undefined && !isObj(hooks)) die("existing 'hooks' is not an object; refusing to merge.");
  const lst = hooks ? hooks[event] : undefined;
  if (lst !== undefined && !Array.isArray(lst)) die('existing hooks.' + event + ' is not an array; refusing to merge.');
  return lst ? lst.slice() : [];
}

function main() {
  const raw = process.argv.slice(2);
  const dryRun = raw.includes('--dry-run');
  const argv = raw.filter(a => a !== '--dry-run');
  if (argv.length < 2) die('usage: _json_edit.js <file> <op> [args...]');
  const file = argv[0], op = argv[1], rest = argv.slice(2);
  const data = load(file);

  if (op === 'get') {
    if (rest.length !== 1) die('get takes <dotpath>');
    const r = getPath(data, rest[0].split('.'));
    if (!r.found) process.exit(3);
    process.stdout.write(JSON.stringify(r.value) + '\n');
    return;
  }

  if (op === 'set') {
    if (rest.length !== 2) die('set takes <dotpath> <json-value>');
    let value;
    try { value = JSON.parse(rest[1]); } catch (e) { die('value is not valid JSON: ' + e.message); }
    const parts = rest[0].split('.');
    const before = getPath(data, parts);
    if (before.found && eq(before.value, value)) { console.log('NOCHANGE'); return; }
    setPath(data, parts, value);
    save(file, data, dryRun);
    console.log('CHANGED');
    return;
  }

  if (op === 'unset') {
    if (rest.length !== 1) die('unset takes <dotpath>');
    if (!unsetPath(data, rest[0].split('.'))) { console.log('NOCHANGE'); return; }
    save(file, data, dryRun);
    console.log('CHANGED');
    return;
  }

  if (op === 'hook-upsert') {
    if (rest.length !== 3) die('hook-upsert takes <event> <matcher> <command>');
    const [event, matcher, command] = rest;
    const entries = hookEntries(data, event);
    const target = { matcher: matcher, hooks: [{ type: 'command', command: command }] };
    let placed = false;
    for (let i = 0; i < entries.length; i++) {
      const e = entries[i];
      if (isObj(e) && e.matcher === matcher) {
        if (eq(e, target)) { console.log('NOCHANGE'); return; }
        entries[i] = target;
        placed = true;
        break;
      }
    }
    if (!placed) entries.push(target);
    setPath(data, ['hooks', event], entries);
    save(file, data, dryRun);
    console.log('CHANGED');
    return;
  }

  if (op === 'hook-remove') {
    if (rest.length !== 2) die('hook-remove takes <event> <marker>');
    const [event, marker] = rest;
    const entries = hookEntries(data, event);
    const kept = [];
    for (const e of entries) {
      if (!isObj(e) || !Array.isArray(e.hooks)) { kept.push(e); continue; }
      const survivors = e.hooks.filter(h => !(isObj(h) && String(h.command || '').indexOf(marker) !== -1));
      if (survivors.length === 0) continue; // drop the matcher block; it held only our hook
      if (survivors.length !== e.hooks.length) kept.push(Object.assign({}, e, { hooks: survivors }));
      else kept.push(e);
    }
    if (eq(kept, entries)) { console.log('NOCHANGE'); return; }
    if (kept.length) setPath(data, ['hooks', event], kept);
    else unsetPath(data, ['hooks', event]);
    save(file, data, dryRun);
    console.log('CHANGED');
    return;
  }

  die('unknown op: ' + op);
}

main();
