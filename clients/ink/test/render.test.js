// Headless render smoke test: mount the real App (htm + Ink + React) against a
// fake connection and assert the rendered frame. Verifies the risky
// integration I cannot watch interactively.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { render } from 'ink-testing-library';
import { html } from 'htm/react';
import { App } from '../src/app.js';

function fakeConnection() {
  const conn = new EventEmitter();
  conn.state = 'connecting';
  conn.lastError = null;
  conn.attached = [];
  conn.detached = [];
  conn.interrupts = [];
  conn.attach = (stream) => conn.attached.push(stream);
  conn.sendText = () => {};
  conn.detach = (stream) => conn.detached.push(stream);
  conn.interrupt = (stream) => conn.interrupts.push(stream);
  conn.end = () => {};
  return conn;
}

const flush = () => new Promise((r) => setTimeout(r, 20));

test('history commands load one page without hiding current authorization or live delivery', async () => {
  const conn = fakeConnection();
  const requests = [];
  conn.history = (stream, cursor) => requests.push({ stream, cursor });
  const { lastFrame, stdin, unmount } = render(html`<${App} connection=${conn} stream="main" title="test" />`);
  const until = async (condition) => {
    const deadline = Date.now() + 3000;
    while (!condition()) {
      assert.ok(Date.now() < deadline, 'render did not reach the expected state');
      await new Promise((resolve) => setImmediate(resolve));
    }
  };
  try {
    await until(() => conn.listenerCount('message') > 0);
    const cursor = { stream: 'main', generation: 1, through: 500, before: 373 };
    conn.emit('message', { attached: { stream: 'main', replay: [], history: {
      through: 500, older: cursor,
      state: { busy: false, waiting: false, pending_auth: [{ request: 'auth', held: 'call' }], skills: [] },
    } } });
    await until(() => lastFrame().includes('/older loads'));
    stdin.write('/older');
    await until(() => lastFrame().includes('❯ /older'));
    stdin.write('\r');
    await until(() => requests.length === 1);
    assert.deepEqual(requests[0], { stream: 'main', cursor });
    conn.emit('message', { history_page: { stream: 'main', cursor, older: null,
      replay: [{ seq: 1, type: 'core.input.user_message', payload: { text: 'old page question' }, causes: [] }],
    } });
    await until(() => lastFrame().includes('old page question'));
    assert.match(lastFrame(), /authorization waiting/);
    conn.emit('message', { appended: { stream: 'main', event: {
      seq: 501, id: 'new', type: 'core.output.reply', payload: { text: 'live answer' }, causes: [],
    } } });
    stdin.write('/latest');
    await until(() => lastFrame().includes('❯ /latest'));
    stdin.write('\r');
    await until(() => conn.attached.includes('main'));
    conn.emit('message', { attached: { stream: 'main', replay: [
      { seq: 501, type: 'core.output.reply', payload: { text: 'live answer' } },
    ], history: { through: 501, older: cursor, state: { busy: false, waiting: false, pending_auth: [], skills: [] } } } });
    await until(() => lastFrame().includes('live answer'));
    assert.doesNotMatch(lastFrame(), /old page question/);
  } finally { unmount(); }
});

test('the app mounts and renders a conversation', async () => {
  const conn = fakeConnection();
  const { lastFrame, unmount } = render(
    html`<${App} connection=${conn} stream="main" title="test" />`,
  );
  await flush(); // let useEffect attach the connection listeners

  conn.emit('connect');
  conn.emit('message', {
    attached: {
      stream: 'main',
      replay: [{ type: 'core.input.user_message', payload: { text: 'hello there' } }],
    },
  });
  conn.emit('message', {
    appended: {
      stream: 'main',
      event: { id: 'e1', type: 'core.output.reply', payload: { text: 'hi back' } },
    },
  });
  // A real turn ends with a turn-completed boundary; without it the frontend
  // rightly reads the stream as still busy (see the mid-turn attach test)
  conn.emit('message', {
    appended: {
      stream: 'main',
      event: { id: 'e2', type: 'core.control.turn_completed', payload: {} },
    },
  });
  await flush();

  const frame = lastFrame();
  assert.match(frame, /hello there/); // the replayed user line
  assert.match(frame, /hi back/); // the agent reply
  assert.match(frame, /Enter sends/); // the status bar, back to idle
  unmount();
});

test('a socket that connected before mount still gets attached', async () => {
  // The race that shipped broken: the socket connects while React is still
  // mounting, the 'connect' event fires with no listener, and the app sat
  // on "connecting…" forever — every typed message then failed
  const conn = fakeConnection();
  conn.state = 'connected'; // the event is already gone; only state remains
  const { lastFrame, unmount } = render(
    html`<${App} connection=${conn} stream="main" title="test" />`,
  );
  await flush();

  assert.deepEqual(conn.attached, ['main'], 'must attach from state, not the missed event');
  // A late 'connect' event must not attach a second time
  conn.emit('connect');
  await flush();
  assert.deepEqual(conn.attached, ['main']);
  assert.match(lastFrame(), /connected/);
  unmount();
});

test('a streaming notice shows before the completed reply lands', async () => {
  const conn = fakeConnection();
  const { lastFrame, unmount } = render(
    html`<${App} connection=${conn} stream="main" title="test" />`,
  );
  await flush(); // let useEffect attach the connection listeners
  conn.emit('connect');
  conn.emit('message', { notice: { stream: 'main', source: 'model', payload: { chunk: 'strea' } } });
  conn.emit('message', { notice: { stream: 'main', source: 'model', payload: { chunk: 'ming…' } } });
  await flush();

  assert.match(lastFrame(), /streaming…/);
  unmount();
});

test('handshake warnings from Attached land in the transcript', async () => {
  const conn = fakeConnection();
  const { lastFrame, unmount } = render(
    html`<${App} connection=${conn} stream="main" title="test" />`,
  );
  await flush();
  conn.emit('connect');
  conn.emit('message', {
    attached: {
      stream: 'main',
      replay: [],
      warnings: ['this client declared no "authorize" capability'],
    },
  });
  await flush();

  assert.match(lastFrame(), /declared no "authorize" capability/);
  unmount();
});

test('a typed message shows exactly once after the daemon echoes it back', async () => {
  const conn = fakeConnection();
  const { lastFrame, stdin, unmount } = render(
    html`<${App} connection=${conn} stream="main" title="test" />`,
  );
  await flush();
  conn.emit('connect');
  conn.emit('message', { attached: { stream: 'main', replay: [] } });

  stdin.write('count me once');
  await flush();
  stdin.write('\r');
  await flush();
  // The daemon records the message and broadcasts it back — the only echo
  conn.emit('message', {
    appended: {
      stream: 'main',
      event: { id: 'u1', type: 'core.input.user_message', payload: { text: 'count me once' } },
    },
  });
  await flush();

  const seen = (lastFrame().match(/count me once/g) ?? []).length;
  assert.equal(seen, 1, 'no local echo on top of the ledger echo');
  unmount();
});

test('tabs open via /new and a /btw sidechannel', async () => {
  const conn = fakeConnection();
  const { lastFrame, stdin, unmount } = render(
    html`<${App} connection=${conn} stream="main" title="test" />`,
  );
  await flush();
  conn.emit('connect');

  const type = async (line) => {
    stdin.write(line);
    await flush();
    stdin.write('\r'); // Enter
    await flush();
  };
  await type('/new work');
  await type('/btw');

  const frame = lastFrame();
  assert.match(frame, /1:main/);
  assert.match(frame, /2:work/);
  assert.match(frame, /sidechannel of work/); // the /btw derived from the active tab
  unmount();
});

test('Esc interrupts only a busy stream, with the right stream id', async () => {
  const conn = fakeConnection();
  const { stdin, unmount } = render(html`<${App} connection=${conn} stream="main" title="t" />`);
  await flush();
  conn.emit('connect');
  conn.emit('message', { attached: { stream: 'main', replay: [] } });
  await flush();

  // Idle: Esc must NOT send an interrupt (it would pollute the ledger)
  stdin.write('\u001B');
  await flush();
  assert.deepEqual(conn.interrupts, []);

  // A recorded user line marks the turn running; now Esc reaches the wire
  conn.emit('message', {
    appended: {
      stream: 'main',
      event: { id: 'u1', type: 'core.input.user_message', payload: { text: 'go' } },
    },
  });
  await flush();
  stdin.write('\u001B');
  await flush();
  assert.deepEqual(conn.interrupts, ['main']);
  unmount();
});

test('/close detaches the daemon subscription, but never the last tab', async () => {
  const conn = fakeConnection();
  const { stdin, lastFrame, unmount } = render(
    html`<${App} connection=${conn} stream="main" title="t" />`,
  );
  await flush();
  conn.emit('connect');

  const type = async (line) => {
    stdin.write(line);
    await flush();
    stdin.write('\r');
    await flush();
  };

  // The last remaining tab refuses to close — twice, for good measure
  await type('/close');
  await type('/close');
  assert.deepEqual(conn.detached, []);
  assert.match(lastFrame(), /1:main/);

  // A second tab closes for real: the daemon is told, the tab disappears
  await type('/new work');
  await type('/close');
  assert.deepEqual(conn.detached, ['work']);
  assert.doesNotMatch(lastFrame(), /work/);
  unmount();
});

test('typing a slash offers the installed skills from the ledger listing', async () => {
  const { paletteMatches } = await import('../src/app.js');
  const skills = [
    { name: 'greeting', description: 'greet someone' },
    { name: 'research-notes', description: 'organize notes' },
  ];
  assert.deepEqual(paletteMatches('hello', skills), [], 'plain text: no palette');
  assert.equal(paletteMatches('/', skills).length, 2, "'/' offers every skill");
  const gr = paletteMatches('/gr', skills);
  assert.equal(gr.length, 1);
  assert.equal(gr[0].name, 'greeting');
  assert.deepEqual(paletteMatches('/greeting', skills), [], 'fully typed: palette closes');

  // And the listing reaches the palette through the wire: an attached replay
  // carrying a skill.listing event populates the menu the UI reads
  const conn = fakeConnection();
  const { lastFrame, stdin, unmount } = render(
    html`<${App} connection=${conn} stream="main" title="test" />`,
  );
  await flush();
  conn.emit('connect');
  conn.emit('message', {
    attached: {
      stream: 'main',
      replay: [
        {
          id: 'l1',
          type: 'skill.listing',
          causes: [],
          payload: { skills: [{ name: 'greeting', description: 'greet someone' }] },
        },
      ],
    },
  });
  await flush();
  stdin.write('/gr');
  await flush();
  assert.match(lastFrame(), /greet someone/, 'the matching skill row shows');
  unmount();
});
