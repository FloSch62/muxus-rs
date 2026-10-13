// The TypeScript checkout; defaults to ../muxus beside this repository.
const root = process.env.MUXUS_TS_ROOT ?? new URL('../../../../../../../muxus', import.meta.url).pathname;
import { cases } from './cases.mjs';
import { execFileSync } from 'node:child_process';
const out = [];
for (const c of cases) {
  let stored = c.raw !== undefined ? c.raw : JSON.stringify(c.stored);
  if (stored && stored.includes('"BUILTIN_UNNAMED"')) {
    const { BUILTIN_HIGHLIGHT_PROFILES } = await import(`${root}/client/src/builtin-highlight-profiles.ts`);
    const unnamed = BUILTIN_HIGHLIGHT_PROFILES.map((p) => ({ ...p, rules: p.rules.map(({ name: _n, ...r }) => r) }));
    stored = stored.replace('"BUILTIN_UNNAMED"', JSON.stringify(unnamed));
  }
  const result = JSON.parse(execFileSync(`${root}/server/node_modules/.bin/tsx`, [`${import.meta.dirname}/gen.mts`, stored ?? '', c.touch ? 'touch' : ''], { cwd: `${root}/client`, encoding: 'utf8' }));
  out.push({ name: c.name, stored, written: result.writes.map((w) => w[1]) });
}
console.log(JSON.stringify(out, null, 1));
