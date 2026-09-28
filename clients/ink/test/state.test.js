import { test } from 'node:test';
import assert from 'node:assert/strict';
import { reduce, initialState, active, turnPhase, foldReplay, authPhase } from '../src/state.js';

test('waiting is distinct from busy and is restored identically on attach', () => {
  const events = [
    { type: 'core.input.user_message', payload: { text: 'run tests' }, causes: [] },
    { type: 'loop.waiting', payload: {}, causes: ['receipt'] },
  ];
  let state = initialState('main');
  state = reduce(state, { type: 'NOTICE', id: 'main', chunk: 'previous reply' });
  for (const event of events) state = reduce(state, { type: 'APPENDED', id: 'main', event, eventType: event.type });
  assert.equal(active(state).busy, false);
  assert.equal(active(state).waiting, true);
  assert.equal(active(state).streaming, '');
  const replay = foldReplay(events);
  assert.equal(replay.busy, false);
  assert.equal(replay.waiting, true);
  let attached = reduce(initialState('main'), { type: 'ATTACHED', id: 'main', ...replay });
  assert.equal(active(attached).waiting, true);
  attached = reduce(attached, { type: 'QUIESCENT', id: 'main' });
  assert.equal(active(attached).waiting, true);
  for (const type of ['core.input.user_message', 'core.input.wake', 'core.model.call_started', 'core.control.turn_completed']) {
    const next = reduce(state, { type: 'APPENDED', id: 'main', event: { type, causes: [], payload: {} } });
    assert.equal(active(next).waiting, false, type);
  }
  const sideCall = { type: 'core.model.call_started', payload: { purpose: 'context.compact' } };
  const next = reduce(state, { type: 'APPENDED', id: 'main', event: sideCall });
  assert.equal(active(next).waiting, true);
  assert.equal(active(next).busy, false);
});

test('starts with one main stream, active', () => {
  const s = initialState('main');
  assert.deepEqual(s.order, ['main']);
  assert.equal(s.activeId, 'main');
  assert.equal(active(s).kind, 'main');
});

test('opening a new stream adds a tab and switches to it', () => {
  let s = initialState('main');
  s = reduce(s, { type: 'OPEN', id: 'work', kind: 'main' });
  assert.deepEqual(s.order, ['main', 'work']);
  assert.equal(s.activeId, 'work');
  // opening an existing id just activates it
  s = reduce(s, { type: 'OPEN', id: 'main' });
  assert.equal(s.activeId, 'main');
  assert.deepEqual(s.order, ['main', 'work']);
});

test('a sidechannel records its kind and parent', () => {
  let s = initialState('main');
  s = reduce(s, { type: 'OPEN', id: 'btw-1', kind: 'side', parent: 'main' });
  assert.equal(active(s).kind, 'side');
  assert.equal(active(s).parent, 'main');
});

test('per-stream updates never leak across tabs', () => {
  let s = initialState('main');
  s = reduce(s, { type: 'OPEN', id: 'work', kind: 'main' });
  // append to work only
  s = reduce(s, { type: 'APPENDED', id: 'work', line: { who: 'agent', text: 'in work' }, key: 'k1' });
  assert.equal(s.streams.work.transcript.length, 1);
  assert.equal(s.streams.main.transcript.length, 0);
  // a notice to main only
  s = reduce(s, { type: 'NOTICE', id: 'main', chunk: 'partial' });
  assert.equal(s.streams.main.streaming, 'partial');
  assert.equal(s.streams.work.streaming, '');
});

test('cycle and close keep the tab set sane', () => {
  let s = initialState('main');
  s = reduce(s, { type: 'OPEN', id: 'a', kind: 'main' });
  s = reduce(s, { type: 'OPEN', id: 'b', kind: 'main' }); // active b
  s = reduce(s, { type: 'CYCLE', by: 1 }); // wraps to main
  assert.equal(s.activeId, 'main');
  s = reduce(s, { type: 'CLOSE', id: 'a' });
  assert.deepEqual(s.order, ['main', 'b']);
  // cannot close the last tab
  s = reduce(initialState('only'), { type: 'CLOSE', id: 'only' });
  assert.deepEqual(s.order, ['only']);
});

test('sending marks the stream busy; the echoed line lands exactly once', () => {
  let s = initialState('main');
  // The user types: no local echo, only busy — the ledger is the sole source
  s = reduce(s, { type: 'SENT', id: 'main' });
  assert.equal(active(s).busy, true);
  assert.equal(active(s).transcript.length, 0);
  // The daemon broadcasts the recorded user message back: one line, no dupe
  s = reduce(s, {
    type: 'APPENDED',
    id: 'main',
    eventType: 'core.input.user_message',
    line: { who: 'you', text: 'hi' },
    key: 'e1',
  });
  assert.deepEqual(
    active(s).transcript.map((l) => l.text),
    ['hi'],
  );
  s = reduce(s, { type: 'QUIESCENT', id: 'main' });
  assert.equal(active(s).busy, false);
});

test('a user line seen from another terminal also marks busy', () => {
  let s = initialState('main');
  s = reduce(s, {
    type: 'APPENDED',
    id: 'main',
    eventType: 'core.input.user_message',
    line: { who: 'you', text: 'from phone' },
    key: 'e2',
  });
  assert.equal(active(s).busy, true); // busy derives from the ledger, not local typing
  s = reduce(s, {
    type: 'APPENDED',
    id: 'main',
    eventType: 'core.output.reply',
    line: { who: 'agent', text: 'reply' },
    key: 'e3',
  });
  assert.equal(active(s).transcript.at(-1).lead, false);
  s = reduce(s, { type: 'QUIESCENT', id: 'main' });
  assert.equal(active(s).busy, false);
});

test('turnPhase opens on user/wake, closes on turn-end, holds otherwise', () => {
  assert.equal(turnPhase(false, 'core.input.user_message'), true);
  assert.equal(turnPhase(false, 'core.input.wake'), true);
  assert.equal(turnPhase(true, 'core.control.turn_completed'), false);
  assert.equal(turnPhase(true, 'core.output.reply'), true); // mid-turn holds
  assert.equal(turnPhase(false, 'core.tool.exec_started'), false);
});

test('a background wake lights busy though it renders no line', () => {
  let s = initialState('main');
  // A timer fires or a background run finishes: the daemon broadcasts a wake
  // event that renders no transcript line, yet the turn is real and busy
  s = reduce(s, { type: 'APPENDED', id: 'main', eventType: 'core.input.wake', line: null, key: 'w1' });
  assert.equal(active(s).busy, true);
  assert.equal(active(s).transcript.length, 0);
  // its reply lands, then the turn-end boundary — also line-less — closes it
  s = reduce(s, {
    type: 'APPENDED',
    id: 'main',
    eventType: 'core.output.reply',
    line: { who: 'agent', text: 'done' },
    key: 'w2',
  });
  assert.equal(active(s).busy, true);
  s = reduce(s, { type: 'APPENDED', id: 'main', eventType: 'core.control.turn_completed', line: null, key: 'w3' });
  assert.equal(active(s).busy, false);
  assert.deepEqual(
    active(s).transcript.map((l) => l.text),
    ['done'],
  );
});

test('attaching mid-turn shows busy from the replayed ledger', () => {
  let s = initialState('main');
  // Replay ends on a user message with no turn-end yet: still running
  const busy = ['core.input.user_message', 'core.output.reply'].reduce(
    (b, t) => turnPhase(b, t),
    false,
  );
  s = reduce(s, { type: 'ATTACHED', id: 'main', lines: [{ who: 'you', text: 'hi' }], busy });
  assert.equal(active(s).busy, true);
  // An idle replay (the turn already ended) attaches not busy
  const idle = ['core.input.user_message', 'core.control.turn_completed'].reduce(
    (b, t) => turnPhase(b, t),
    false,
  );
  s = reduce(s, { type: 'ATTACHED', id: 'main', lines: [], busy: idle });
  assert.equal(active(s).busy, false);
});

test('trust cards queue, and a decision closes the one it settles', () => {
  const card = (id, held) => ({
    type: 'APPENDED',
    id: 'main',
    eventType: 'trust.authorization_requested',
    event: { type: 'trust.authorization_requested', id, causes: [held] },
    line: { who: 'notice', text: `authorization required: ${id} — allow? (y/n)` },
    key: id,
  });
  const decide = (id, held) => ({
    type: 'APPENDED',
    id: 'main',
    eventType: 'trust.gate.decision',
    event: { type: 'trust.gate.decision', id, causes: [held] },
    line: { who: 'notice', text: '✓ trust: fine' },
    key: id,
  });

  let s = initialState('main');
  s = reduce(s, card('ev_req_a', 'call_a'));
  assert.deepEqual(active(s).pendingAuth, [{ request: 'ev_req_a', held: 'call_a' }]);

  // A SECOND question while the first is open. A single slot lost the first
  // one here, and the call waiting on it could never be answered.
  s = reduce(s, card('ev_req_b', 'call_b'));
  assert.deepEqual(active(s).pendingAuth, [
    { request: 'ev_req_a', held: 'call_a' },
    { request: 'ev_req_b', held: 'call_b' },
  ]);

  // A decision closes only the card whose CALL it settled.
  s = reduce(s, decide('ev_dec_b', 'call_b'));
  assert.deepEqual(active(s).pendingAuth, [{ request: 'ev_req_a', held: 'call_a' }]);
  s = reduce(s, decide('ev_dec_a', 'call_a'));
  assert.deepEqual(active(s).pendingAuth, []);

  // AUTH_SENT clears the one just answered, optimistically.
  s = reduce(s, card('ev_req_c', 'call_c'));
  s = reduce(s, { type: 'AUTH_SENT', id: 'main' });
  assert.deepEqual(active(s).pendingAuth, []);
});

test('a gated double tool line collapses to one', () => {
  let s = initialState('main');
  const line = { who: 'tool', text: 'run {"command":"ls"}' };
  s = reduce(s, { type: 'APPENDED', id: 'main', eventType: 'core.tool.exec_started', line, key: 'a' });
  s = reduce(s, { type: 'APPENDED', id: 'main', eventType: 'core.tool.exec_started', line, key: 'b' });
  assert.equal(active(s).transcript.length, 1, 'the forward is the same call, one line');
});

test('skill listings replace the palette menu wholesale', () => {
  let s = initialState('main');
  s = reduce(s, {
    type: 'LISTING',
    id: 'main',
    skills: [{ name: 'greeting', description: 'greet someone' }],
  });
  assert.deepEqual(active(s).skills, [{ name: 'greeting', description: 'greet someone' }]);
  s = reduce(s, { type: 'LISTING', id: 'main', skills: [] });
  assert.deepEqual(active(s).skills, [], 'an emptied menu empties the palette');
});

test('a caused user message does not open a second turn', () => {
  let s = initialState('main');
  s = reduce(s, {
    type: 'APPENDED', id: 'main', eventType: 'core.input.user_message',
    causes: [], line: { who: 'you', text: '/greeting' }, key: 'u1',
  });
  assert.equal(active(s).busy, true, 'the typed original starts the turn');
  s = reduce(s, { type: 'QUIESCENT', id: 'main' });
  // The expansion station's forward (caused) must not re-open the turn
  s = reduce(s, {
    type: 'APPENDED', id: 'main', eventType: 'core.input.user_message',
    causes: ['u1'], line: null, key: 'u2',
  });
  assert.equal(active(s).busy, false, 'a forwarded message is not a new turn');
});

test('a reconnecting client sees the same lines as one that stayed', () => {
  // The replay used to be mapped straight to lines with no dedup, while the
  // live path deduped — so every gated tool call showed twice to anyone who
  // attached late, and only to them.
  const started = (id, call) => ({
    v: 1,
    id,
    seq: 1,
    stream: 'main',
    time: 't',
    type: 'core.tool.exec_started',
    source: 'loop',
    causes: [],
    payload: { call, tool: 'Read', arguments: { path: 'a.txt' } },
  });
  const { lines } = foldReplay([
    started('ev_1', 'c1'), // the loop's emission
    started('ev_2', 'c1'), // the gate's forward — the same call
  ]);
  assert.equal(lines.length, 1, 'one call, one line');

  // Two genuinely separate calls for the same thing both show. Deduping by
  // TEXT could not tell this apart from the case above.
  const twice = foldReplay([started('ev_1', 'c1'), started('ev_2', 'c2')]);
  assert.equal(twice.lines.length, 2, 'two calls, two lines');
});

test('browser authorization opens and settles without granting an admission', () => {
  const pending = authPhase([], {type: 'browser.authorization_requested', id: 'question', causes: ['batch']});
  assert.deepEqual(pending, [{request: 'question', held: 'batch'}]);
  assert.deepEqual(authPhase(pending, {type: 'browser.authorization_decided', causes: ['batch']}), []);
});

test('reopening cannot revive an interrupted forwarded browser batch', () => {
  const pending = authPhase([], {type: 'browser.authorization_requested', id: 'question', causes: ['forwarded'], payload: {held: 'original'}});
  assert.deepEqual(authPhase(pending, {type: 'core.control.interrupted', causes: ['original']}), []);
  assert.deepEqual(authPhase(pending, {type: 'browser.authorization_decided', causes: ['forwarded'], payload: {held: 'original'}}), []);
});

test('an unanswered card in a replay is answerable after reconnecting', () => {
  const event = (id, type, causes) => ({
    v: 1,
    id,
    seq: 1,
    stream: 'main',
    time: 't',
    type,
    source: 'trust',
    causes,
    payload: { summary: 'install something' },
  });
  const { pendingAuth } = foldReplay([
    event('ev_a', 'trust.authorization_requested', ['call_a']),
    event('ev_b', 'trust.authorization_requested', ['call_b']),
    event('ev_d', 'trust.gate.decision', ['call_a']),
  ]);
  assert.deepEqual(
    pendingAuth,
    [{ request: 'ev_b', held: 'call_b' }],
    'the one still open, and only it',
  );
});
