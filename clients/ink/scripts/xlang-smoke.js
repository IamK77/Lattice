// Cross-language smoke: this Node client connects to a REAL Rust daemon over
// the Unix socket and drives one full turn. Not part of `npm test` (it needs a
// running daemon); invoked by the repo's integration check with a socket path.
//
//   node test/xlang-smoke.js /path/to/daemon.sock

import { Connection } from '../src/connection.js';
import { decode, renderLine } from '../src/protocol.js';

const socketPath = process.argv[2];
if (!socketPath) {
  console.error('usage: node xlang-smoke.js <socket>');
  process.exit(2);
}

const conn = new Connection(socketPath);
const timeout = setTimeout(() => {
  console.error('SMOKE: timed out waiting for the daemon');
  process.exit(1);
}, 8000);

let replied = null;

conn.on('error', (err) => {
  console.error(`SMOKE: connection error ${err.code || err.message}`);
  process.exit(1);
});
conn.on('connect', () => conn.attach('main'));
conn.on('message', (raw) => {
  const { tag, body } = decode(raw);
  if (tag === 'attached') conn.sendText('main', 'hi from node');
  if (tag === 'appended') {
    const line = renderLine(body.event);
    if (line && line.who === 'agent') replied = line.text;
  }
  if (tag === 'quiescent') {
    clearTimeout(timeout);
    conn.end();
    if (replied) {
      console.log(`SMOKE OK: the Rust daemon replied "${replied}"`);
      process.exit(0);
    }
    console.error('SMOKE: turn finished with no agent reply');
    process.exit(1);
  }
});
