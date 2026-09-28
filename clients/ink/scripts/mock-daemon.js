// A mock daemon: a Unix-socket server speaking the exact wire format the Rust
// daemon uses (verified byte-for-byte against `serde_json` output). It lets us
// test the client's protocol layer with no Rust, no model key — the Node-side
// equivalent of the Rust `tests/daemon.rs`.

import net from 'node:net';

function envelope(type, payload) {
  return {
    v: 1,
    id: `ev_${Math.random().toString(36).slice(2)}`,
    seq: 1,
    stream: 'main',
    time: new Date().toISOString(),
    type,
    source: 'mock',
    causes: [],
    payload,
  };
}

export function startMockDaemon(socketPath) {
  const server = net.createServer((socket) => {
    socket.setEncoding('utf8');
    let buffer = '';
    socket.on('data', (chunk) => {
      buffer += chunk;
      let i;
      while ((i = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, i).trim();
        buffer = buffer.slice(i + 1);
        if (!line) continue;
        const msg = JSON.parse(line);
        const tag = Object.keys(msg)[0];
        const send = (m) => socket.write(JSON.stringify(m) + '\n');
        if (tag === 'attach') {
          send({
            attached: {
              stream: 'main',
              replay: [envelope('core.input.user_message', { text: 'earlier' })],
            },
          });
        } else if (tag === 'send_text') {
          const text = msg.send_text.text;
          send({ appended: { stream: 'main', event: envelope('core.input.user_message', { text }) } });
          send({ appended: { stream: 'main', event: envelope('core.output.reply', { text: `echo: ${text}` }) } });
          send({ quiescent: { stream: 'main' } });
        } else if (tag === 'interrupt') {
          send({ appended: { stream: 'main', event: envelope('core.output.reply', { cancelled: true }) } });
          send({ quiescent: { stream: 'main' } });
        }
      }
    });
  });
  return new Promise((resolve) => server.listen(socketPath, () => resolve(server)));
}
