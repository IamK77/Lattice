import { test } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import fs from 'node:fs';

import { Connection } from '../src/connection.js';
import { decode, encode, renderLine, replyChunk } from '../src/protocol.js';
import { startMockDaemon } from '../scripts/mock-daemon.js';

test('renderLine mirrors the Rust rule', () => {
  assert.deepEqual(renderLine({ type: 'core.input.user_message', payload: { text: 'hi' } }), {
    who: 'you',
    text: 'hi',
  });
  assert.deepEqual(renderLine({ type: 'core.output.reply', payload: { text: 'yo' } }), {
    who: 'agent',
    text: 'yo',
  });
  assert.deepEqual(renderLine({ type: 'core.output.reply', payload: { cancelled: true } }), {
    who: 'agent',
    text: '[interrupted]',
  });
  assert.deepEqual(
    renderLine({ type: 'core.output.reply', payload: { error: { message: 'nope' } } }),
    { who: 'error', text: 'nope' },
  );
  const tool = renderLine({
    type: 'core.tool.exec_started',
    payload: { tool: 'calc', arguments: { n: 1 } },
  });
  assert.equal(tool.who, 'tool');
  assert.match(tool.text, /calc/);
  // Non-visible events render nothing
  assert.equal(renderLine({ type: 'core.control.turn_completed', payload: {} }), null);
});

test('replyChunk keeps the reply and drops thinking and background streams', () => {
  assert.equal(replyChunk({ chunk: 'hello' }), 'hello');
  // Thinking is marked with its phase: it is not the answer and must never be
  // concatenated into one
  assert.equal(replyChunk({ chunk: 'let me see', phase: 'reasoning' }), null);
  // A background call (the condenser) is not the conversation
  assert.equal(replyChunk({ chunk: 'summarizing', purpose: 'context.condense' }), null);
  assert.equal(replyChunk({}), null);
  assert.equal(replyChunk(undefined), null);
});

test('encode produces the exact wire shape', () => {
  assert.deepEqual(encode.attach('main'), {
    attach: { stream: 'main', template: null, derive_from: null },
  });
  assert.deepEqual(encode.attach('btw', { deriveFrom: 'main' }), {
    attach: { stream: 'btw', template: null, derive_from: 'main' },
  });
  assert.deepEqual(encode.sendText('main', 'hi'), { send_text: { stream: 'main', text: 'hi' } });
  assert.deepEqual(encode.detach('main'), { detach: { stream: 'main' } });
  assert.deepEqual(encode.interrupt('main'), { interrupt: { stream: 'main' } });
  assert.deepEqual(decode({ appended: { x: 1 } }), { tag: 'appended', body: { x: 1 } });
});

test('a full turn round-trips against the mock daemon', async () => {
  const socketPath = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'lattice-')), 's');
  const server = await startMockDaemon(socketPath);

  const conn = new Connection(socketPath);
  const messages = [];
  await new Promise((resolve, reject) => {
    conn.on('error', reject);
    conn.on('connect', () => conn.attach('main'));
    conn.on('message', (raw) => {
      const { tag } = decode(raw);
      messages.push(tag);
      if (tag === 'attached') conn.sendText('main', 'hello');
      if (tag === 'quiescent') resolve();
    });
  });

  // The client saw: the backlog on attach, then the turn broadcast to quiescence
  assert.deepEqual(messages, ['attached', 'appended', 'appended', 'quiescent']);
  conn.end();
  server.close();
});

test('a late attach carries the backlog', async () => {
  const socketPath = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'lattice-')), 's');
  const server = await startMockDaemon(socketPath);

  const conn = new Connection(socketPath);
  const replay = await new Promise((resolve, reject) => {
    conn.on('error', reject);
    conn.on('connect', () => conn.attach('main'));
    conn.on('message', (raw) => {
      const { tag, body } = decode(raw);
      if (tag === 'attached') resolve(body.replay);
    });
  });

  const lines = replay.map(renderLine).filter(Boolean);
  assert.deepEqual(lines, [{ who: 'you', text: 'earlier' }]);
  conn.end();
  server.close();
});
