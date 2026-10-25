// Table-driven test runner with a registry of known engine gaps.
//
// Every case in a group runs through `run(case)`, which returns null (ok) or a failure string.
// Failures that match an entry in `bugs` are NOT silently dropped: they are re-asserted in a
// separate `{ todo }` test per bug id, so the gap stays visible in the test report and the todo
// starts failing loudly (a "todo passed" signal) once the engine is fixed.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';

export const BUGS = [];

export function bug(id, reason, match) {
  BUGS.push({ id, reason, match });
}

async function pool(items, size, fn) {
  const out = new Array(items.length);
  let next = 0;
  const workers = Array.from({ length: Math.min(size, items.length) }, async () => {
    while (next < items.length) {
      const i = next++;
      out[i] = await fn(items[i], i);
    }
  });
  await Promise.all(workers);
  return out;
}

export function group(title, cases, run, { concurrency = 6, scope = title } = {}) {
  const applicable = BUGS.filter((b) => b.scope === undefined || b.scope === scope);
  let results;
  const compute = async () => {
    results ??= await pool(cases, concurrency, async (c) => {
      let msg;
      try {
        msg = await run(c);
      } catch (error) {
        msg = `threw: ${String(error && error.message).slice(0, 100)}`;
      }
      return msg ? { name: c.name, msg } : null;
    });
    return results.filter(Boolean);
  };
  // A failure message may hold several " || "-separated findings. It is "known" only when every
  // finding matches a registered gap; each gap then reports the finding it explains.
  const classify = (failures) => {
    const unclassified = [];
    const byBug = new Map();
    for (const f of failures) {
      const parts = f.msg.split(' || ');
      const hits = parts.map((part) => applicable.find((x) => x.match(f.name, part, title)));
      if (hits.some((h) => !h)) {
        unclassified.push(f);
        continue;
      }
      hits.forEach((b, i) => {
        if (!byBug.has(b.id)) byBug.set(b.id, []);
        byBug.get(b.id).push({ name: f.name, msg: parts[i] });
      });
    }
    return { unclassified, byBug };
  };
  test(`${title} (${cases.length} cases)`, async (t) => {
    const failures = await compute();
    if (process.env.DIFF_REPORT) {
      fs.appendFileSync(process.env.DIFF_REPORT, failures.map((f) => `${title} | ${f.name} | ${f.msg}`).join('\n') + '\n');
    }
    const { unclassified, byBug } = classify(failures);
    // Known gaps are reported as todo subtests (visible, but they do not fail CI).
    for (const [id, hits] of byBug) {
      const b = applicable.find((x) => x.id === id);
      await t.test(`known gap ${id}: ${hits.length} case(s)`, { todo: b.reason }, () => {
        const err = new Error(`e.g.\n${hits.slice(0, 6).map((f) => `  ${f.name}: ${f.msg}`).join('\n')}`);
        err.stack = err.message; // keep the todo report readable
        throw err;
      });
    }
    const shown = unclassified.slice(0, 15).map((f) => `  ${f.name}: ${f.msg}`);
    assert.equal(
      unclassified.length,
      0,
      `${unclassified.length}/${cases.length} cases differ from sharp:\n${shown.join('\n')}`
    );
  });
}
