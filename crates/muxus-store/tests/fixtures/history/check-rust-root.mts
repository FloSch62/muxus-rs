// Reads a root written by the Rust store (the ignored
// `export_a_root_for_the_typescript_build` test) with the TypeScript
// SessionHistoryStore and compares its answers with what Rust recorded.
// Run from the TypeScript checkout's server/ directory:
//   HOME=$(mktemp -d) ./node_modules/.bin/tsx <this file> <MUXUS_HISTORY_EXPORT dir>
import { readFileSync } from 'node:fs';
import { isDeepStrictEqual } from 'node:util';
import path from 'node:path';
const muxus = process.env.MUXUS_TS ?? path.resolve(import.meta.dirname, '../../../../../../muxus');
const { SessionHistoryStore } = await import(path.join(muxus, 'server/src/session-logging/history-store.ts'));

const dir = process.argv[2]!;
const expected = JSON.parse(readFileSync(path.join(dir, 'rust-expected.json'), 'utf8'));
const store = await SessionHistoryStore.open({ root: path.join(dir, 'rust-root'), settings: { maxTotalBytes: 5 * 1024 ** 3, minFreeBytes: 0, minFreePercent: 0 } });
const raw = async (id: string) => (await store.rawSessionLogEvents(id))!.map((e) => ({ ...e, raw: e.raw.toString('base64') }));
const json = (v: unknown) => JSON.parse(JSON.stringify(v));
const actual = {
  ids: expected.ids,
  all: json(await store.sessionHistory({ limit: 10 })),
  search: json(await store.sessionHistory({ query: 'line protocol', limit: 10 })),
  edgeDetail: json(await store.sessionLog(expected.ids.edge)),
  edgeRaw: json(await raw(expected.ids.edge)),
};
const live = json(await store.sessionLog(expected.ids.live));
const liveRaw = await raw(expected.ids.live);
await store.close();
for (const key of Object.keys(actual) as (keyof typeof actual)[]) {
  console.log(key, isDeepStrictEqual(actual[key], expected[key]) ? 'equal' : `DIFFERENT\n${JSON.stringify(actual[key])}\n${JSON.stringify(expected[key])}`);
}
console.log('live session after close:', live.status, live.partCount, Buffer.from(liveRaw[0].raw, 'base64').toString());
