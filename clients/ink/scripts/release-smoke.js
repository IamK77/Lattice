// Run only against a private scripted daemon after its startup handshake.
// Usage: node release-smoke.js SOCKET STREAM UNIQUE_TEXT [PREVIOUS_INPUT_ID]
import { Connection } from '../src/connection.js';
import { ReleaseProbe } from './release-probe.js';

const args = process.argv.slice(2);
if (args.length < 3 || args.length > 4 || args.some(arg => !arg)) {
  console.error('usage: node release-smoke.js SOCKET STREAM UNIQUE_TEXT [PREVIOUS_INPUT_ID]');
  process.exit(2);
}
const [socket, stream, text, previous] = args;
const probe = new ReleaseProbe(stream, text, previous);
const conn = new Connection(socket);
let settled = false;
const timer = setTimeout(() => fail('scripted turn exceeded its deadline'), 30000);
function fail(message) {
  if (settled) return;
  settled = true;
  clearTimeout(timer);
  console.error(`RELEASE SMOKE FAILED: ${message}`);
  process.exitCode = 1;
  conn.socket.destroy();
}
conn.on('error', error => fail(error.message));
conn.on('close', () => {
  if (!settled) fail('connection closed before turn evidence was complete');
});
conn.on('connect', () => conn.attach(stream, { capabilities: ['history-pages'] }));
conn.on('message', message => {
  if (settled) return;
  try {
    const action = probe.accept(message);
    if (action?.send) conn.sendText(stream, action.send);
    if (action?.result) {
      settled = true;
      clearTimeout(timer);
      console.log(JSON.stringify(action.result));
      // Evidence is complete; do not wait for the daemon's write-half to close.
      conn.socket.destroy();
    }
  } catch (error) {
    fail(error.message);
  }
});
