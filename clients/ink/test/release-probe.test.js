import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ReleaseProbe } from '../scripts/release-probe.js';

const stream = 'rehearsal';
const text = 'unique installation input';
const old = { id: 'old', seq: 1, stream, type: 'core.input.user_message', causes: [], payload: { text: 'old input' } };
const attached = () => ({ attached: { stream, replay: [old], history: {
  through: 5, older: null, state: { busy: false, waiting: false, pending_auth: [] },
} } });
const event = (id, seq, type, causes, payload = {}) => ({ appended: {
  stream, event: { id, seq, stream, type: `core.${type}`, causes, payload },
} });
const flow = () => [
  event('input', 6, 'input.user_message', [], { text }),
  event('expanded', 7, 'input.user_message', ['input'], { text }),
  event('request', 8, 'model.call_started', ['expanded'], { input: { parts: [{ event: 'old' }, { event: 'expanded' }] } }),
  event('completed', 9, 'model.call_completed', ['request'], { status: 'ok' }),
  event('reply', 10, 'output.reply', ['completed'], { text: 'scripted reply 1', error: null, cancelled: false }),
  event('turn', 11, 'control.turn_completed', ['completed']),
];
const quiet = { quiescent: { stream } };
function probe() {
  const p = new ReleaseProbe(stream, text, 'old');
  assert.deepEqual(p.accept(attached()), { send: text });
  return p;
}
function complete(messages) {
  const p = probe();
  for (const message of messages) p.accept(message);
  return p.accept(quiet);
}

test('release probe proves new sibling outcomes and restored model material', () => {
  const { result } = complete(flow());
  assert.equal(result.input, 'input');
  assert.equal(result.previousInput, 'old');
  assert.equal(result.boundary, 5);
  assert.equal(result.materialInput, 'expanded');
  assert.deepEqual(result.evidence.map(e => e.id), ['input', 'expanded', 'request', 'completed', 'reply', 'turn']);
});

test('release probe refuses old replay and waits through idle attachment notifications', () => {
  const p = probe();
  assert.equal(p.accept(quiet), null);
  const permission = event('permission', 6, 'input.external', [], { channel: 'interface.permission', action: 'open' });
  p.accept(permission);
  assert.equal(p.accept(quiet), null);
  assert.equal(p.accept(quiet), null);
  const messages = flow();
  for (const message of messages) {
    message.appended.event.seq += 1;
    p.accept(message);
  }
  const { result } = p.accept(quiet);
  assert.equal(result.input, 'input');
  assert.equal(result.materialInput, 'expanded');
  assert.throws(() => probe().accept({ appended: { stream, event: old } }), /live event sequence/);
});

test('release probe rejects duplicate matching inputs and an unfinished started turn', () => {
  const p = probe();
  p.accept(flow()[0]);
  assert.throws(() => p.accept(quiet), /successful new turn/);
  p.accept(event('second-input', 7, 'input.user_message', [], { text }));
  assert.throws(() => p.accept(quiet), /unique new input/);
});

test('release probe requires prior input both on disk and in new model material', () => {
  const p = new ReleaseProbe(stream, text, 'missing');
  assert.throws(() => p.accept(attached()), /restored history/);
  const messages = flow();
  messages[2].appended.event.payload.input.parts = [{ event: 'input' }];
  assert.throws(() => complete(messages), /restored material/);
});

test('release probe rejects unrelated, cancelled, failed, or incomplete outcomes', () => {
  for (const change of [
    messages => { messages[2].appended.event.causes = ['old']; },
    messages => { messages[3].appended.event.payload.status = 'error'; },
    messages => { messages[4].appended.event.payload.cancelled = true; },
    messages => { messages[4].appended.event.payload.error = { message: 'failure' }; },
    messages => { messages[4].appended.event.causes = ['old']; },
    messages => { messages[5].appended.event.causes = ['reply']; },
    messages => { messages.pop(); },
  ]) {
    const messages = flow();
    change(messages);
    assert.throws(() => complete(messages), /successful new turn/);
  }
});

test('release probe refuses busy, partial, or duplicate attachments', () => {
  for (const change of [
    body => { body.history.state.busy = true; },
    body => { body.history.state.waiting = true; },
    body => { body.history.state.pending_auth = [{ request: 'auth' }]; },
    body => { body.history.older = { before: 1 }; },
  ]) {
    const message = attached();
    change(message.attached);
    assert.throws(() => new ReleaseProbe(stream, text).accept(message), /idle attachment/);
  }
  assert.throws(() => probe().accept(attached()), /duplicate attachment/);
});

test('release probe ignores other streams but rejects duplicate event identity', () => {
  const p = probe();
  assert.equal(p.accept({ quiescent: { stream: 'elsewhere' } }), null);
  const messages = flow();
  p.accept(messages[0]);
  const duplicate = structuredClone(messages[0]);
  duplicate.appended.event.seq += 1;
  assert.throws(() => p.accept(duplicate), /live event sequence/);
  assert.throws(() => p.accept({ error: { stream: null, message: 'broken' } }), /daemon error: broken/);
});
