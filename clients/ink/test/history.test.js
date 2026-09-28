import { test } from 'node:test';
import assert from 'node:assert/strict';
import { reduce, initialState, active } from '../src/state.js';

const cursor = { stream: 'main', generation: 1, through: 1000, before: 873 };
const current = { busy: true, waiting: false, pending_auth: [{ request: 'auth', held: 'call' }], skills: [{ name: 'old-skill' }] };
function attached() {
  return reduce(initialState('main'), { type: 'ATTACHED', id: 'main', lines: [],
    history: { through: 1000, older: cursor, state: current } });
}
test('old pages do not roll back controls, and stale replies do not advance cursors', () => {
  let s = attached();
  s = reduce(s, { type: 'HISTORY_REQUEST', id: 'main' });
  const page = { type: 'HISTORY_PAGE', id: 'main', cursor, older: null, events: [
    { seq: 1, type: 'core.control.turn_completed', payload: {}, causes: [] },
    { seq: 2, type: 'skill.listing', payload: { skills: [] }, causes: [] },
    { seq: 3, type: 'core.input.user_message', payload: { text: 'old question' }, causes: [] },
  ] };
  const stale = reduce(s, { ...page, cursor: { ...cursor, generation: 0 } });
  assert.equal(active(stale).historyLoading, true);
  s = reduce(s, page);
  assert.equal(active(s).historyLines[0].text, 'old question');
  assert.equal(active(s).busy, true);
  assert.deepEqual(active(s).pendingAuth, current.pending_auth);
  assert.deepEqual(active(s).skills, current.skills);
  assert.equal(active(s).historyCursor, null);
  assert.equal(active(s).lastSeq, 1000);
});
test('live delivery is deduplicated and retained display stays bounded while browsing', () => {
  let s = attached();
  for (let seq = 1000; seq <= 1700; seq++) {
    const event = { seq, type: 'core.input.user_message', causes: [], payload: { text: String(seq) } };
    s = reduce(s, { type: 'APPENDED', id: 'main', event, key: String(seq), line: { who: 'you', text: String(seq) } });
  }
  assert.equal(active(s).transcript.length, 500);
  assert.equal(active(s).transcript[0].text, '1201');
  assert.equal(active(s).lastSeq, 1700);
  s = reduce(s, { type: 'APPENDED', id: 'main', event: { seq: 1000, type: 'skill.listing', payload: { skills: [] } } });
  assert.deepEqual(active(s).skills, current.skills);
});
test('only the matching page error clears the outstanding request', () => {
  let s = reduce(attached(), { type: 'HISTORY_REQUEST', id: 'main' });
  s = reduce(s, { type: 'ERROR', id: 'main', message: 'unrelated' });
  assert.equal(active(s).historyLoading, true);
  s = reduce(s, { type: 'HISTORY_ERROR', id: 'main', cursor, message: 'read failed' });
  assert.equal(active(s).historyLoading, false);
  assert.deepEqual(active(s).historyCursor, cursor);
});
