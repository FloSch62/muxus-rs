// Regenerates ts-root/ with the TypeScript SessionHistoryStore.
// Run from the TypeScript checkout's server/ directory (so its packages
// resolve), with a scratch HOME:
//   HOME=$(mktemp -d) ./node_modules/.bin/tsx <this file> <scratch dir>
// then copy <scratch dir>/fixture/ to ts-root/. MUXUS_TS points at the
// TypeScript checkout (default: a `muxus` directory beside `muxus-rs`).
import { mkdirSync, readFileSync, rmSync, writeFileSync, readdirSync, cpSync } from 'node:fs';
import path from 'node:path';
import { zstdCompressSync } from 'node:zlib';
import { DatabaseSync } from 'node:sqlite';
const muxus = process.env.MUXUS_TS ?? path.resolve(import.meta.dirname, '../../../../../../muxus');
const { SessionHistoryStore } = await import(path.join(muxus, 'server/src/session-logging/history-store.ts'));

const out = process.argv[2]!;
const root = path.join(out, 'work-root');
rmSync(root, { recursive: true, force: true });
const settings = { maxTotalBytes: 5 * 1024 ** 3, minFreeBytes: 0, minFreePercent: 0 };
const store = await SessionHistoryStore.open({ root, settings });
const t = (s: number) => `2026-03-01T09:00:0${s}.000Z`;
const small = { maxPartBytes: 40, maxParts: 10 };
const core = store.beginSession({ profileKey: 'ssh:core', title: 'Core router', kind: 'ssh', host: 'admin@core', startedAt: t(0), captureInput: true }, small);
store.append(core, [
  { sequence: 1, recordedAt: t(1), elapsedMs: 1000, direction: 'output', raw: Buffer.from('core# show bgp summary\r\n'), text: 'core# show bgp summary\n', lineTimestamps: [{ offset: 0, recordedAt: t(1) }] },
  { sequence: 2, recordedAt: t(2), elapsedMs: 2000, direction: 'input', raw: Buffer.from('conf t\r'), text: 'conf t\n' },
], small);
store.append(core, [
  { sequence: 3, recordedAt: t(3), elapsedMs: 3000, direction: 'output', raw: Buffer.from('\x1b[1mBGP neighbor 10.0.0.2 Established\x1b[0m\r\n'), text: 'BGP neighbor 10.0.0.2 Established\n' },
  { sequence: 4, recordedAt: t(4), elapsedMs: 4000, direction: 'system', raw: Buffer.from('reconnected'), text: '' },
], small);
store.finishSession(core, 'completed', t(5));
const shell = store.beginSession({ profileKey: 'local', title: 'Shell', kind: 'local', host: 'local', startedAt: t(6), captureInput: false }, { maxPartBytes: 1024 * 1024, maxParts: 2 });
store.append(shell, [
  { sequence: 1, recordedAt: t(7), elapsedMs: 10, direction: 'output', raw: Buffer.from('ünïcode ✓ prompt $ '), text: 'ünïcode ✓ prompt $ ' },
], { maxPartBytes: 1024 * 1024, maxParts: 2 });
store.finishSession(shell, 'disconnected', t(8));
await store.setPinned(core, true);
await store.setLabel(core, 'Pre-change');

const raw = async (id: string) => (await store.rawSessionLogEvents(id))!.map((e) => ({ ...e, raw: e.raw.toString('base64') }));
const expected = {
  ids: { core, shell },
  all: await store.sessionHistory({ limit: 10 }),
  neighbor: await store.sessionHistory({ query: 'neighbor 10.0', limit: 10 }),
  byName: await store.sessionHistory({ query: 'pre-change', limit: 10 }),
  page: await store.sessionHistory({ limit: 1 }),
  coreDetail: await store.sessionLog(core),
  coreLimited: await store.sessionLog(core, 1, 'show'),
  shellDetail: await store.sessionLog(shell),
  coreRaw: await raw(core),
  shellRaw: await raw(shell),
};
await store.close();
const db = new DatabaseSync(path.join(root, 'session-history.sqlite'), { readOnly: true });
const schema = db.prepare('SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name').all();
const migrations = db.prepare('SELECT version, name FROM history_schema_migrations ORDER BY version').all();
db.close();

const fixture = path.join(out, 'fixture');
rmSync(fixture, { recursive: true, force: true });
mkdirSync(fixture, { recursive: true });
writeFileSync(path.join(fixture, 'session-history.sqlite.zst'), zstdCompressSync(readFileSync(path.join(root, 'session-history.sqlite'))));
cpSync(path.join(root, 'sessions'), path.join(fixture, 'sessions'), { recursive: true });
writeFileSync(path.join(fixture, 'expected.json'), JSON.stringify(expected, null, 1) + '\n');
writeFileSync(path.join(fixture, 'schema.json'), JSON.stringify({ schema, migrations }, null, 1) + '\n');
console.log(readdirSync(root), readdirSync(path.join(root, 'sessions', core)));
