// Render the app with a sample conversation and print the frame, so the
// layout can be eyeballed without a live terminal.
//   node scripts/preview.js

import { EventEmitter } from 'node:events';
import { render } from 'ink-testing-library';
import { html } from 'htm/react';
import { App } from '../src/app.js';

const conn = new EventEmitter();
conn.sendText = conn.detach = conn.interrupt = conn.end = () => {};
conn.attach = (stream) => {
  conn.lastAttached = stream; // /btw picks a fresh random id — remember it
};

const { lastFrame, stdin } = render(
  html`<${App} connection=${conn} stream="main" title="~/.lattice/daemon.sock" />`,
);

const ev = (type, payload, id) => ({ id: id ?? Math.random().toString(36).slice(2), type, payload });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function main() {
  await sleep(30);
  conn.emit('connect');
  conn.emit('message', {
    attached: {
      stream: 'main',
      replay: [ev('core.input.user_message', { text: 'add 4 and 7, then a haiku' })],
    },
  });
  conn.emit('message', {
    appended: { stream: 'main', event: ev('core.tool.exec_started', { tool: 'calc', arguments: { numbers: [4, 7] } }, 't1') },
  });
  conn.emit('message', {
    appended: {
      stream: 'main',
      event: ev('core.output.reply', {
        text: '11.\n\nlattice hums softly —\nevents flow, each one recorded,\ncauses never lost',
      }, 'r1'),
    },
  });
  await sleep(40);
  console.log('\n─── one conversation ───\n' + lastFrame() + '\n');

  // Open a second tab and a sidechannel (type text, then Enter separately)
  const type = async (line) => {
    stdin.write(line);
    await sleep(15);
    stdin.write('\r');
    await sleep(40);
  };
  await type('/new work');
  await type('/btw what did we mean by causes?');
  // Play the daemon's part: the question comes back as a recorded event
  // (there is no local echo — the ledger is the single source of truth)
  const btw = conn.lastAttached;
  conn.emit('message', { attached: { stream: btw, replay: [] } });
  conn.emit('message', {
    appended: {
      stream: btw,
      event: ev('core.input.user_message', { text: 'what did we mean by causes?' }, 'q1'),
    },
  });
  await sleep(40);
  console.log('\n─── tabs + a /btw sidechannel ───\n' + lastFrame() + '\n');
  process.exit(0);
}
main();
