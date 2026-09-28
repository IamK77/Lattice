// A thin Unix-socket connection to the Lattice daemon.
//
// Emits 'connect', 'message' (a decoded server message object), 'error',
// 'close'. Node's StringDecoder (via setEncoding('utf8')) reassembles
// multi-byte characters split across TCP chunks, so line-splitting is safe.

import net from 'node:net';
import { EventEmitter } from 'node:events';
import { encode } from './protocol.js';

export class Connection extends EventEmitter {
  constructor(socketPath) {
    super();
    this.buffer = '';
    // Connecting starts NOW, before any listener exists — so the current
    // state is also kept as data. A late subscriber (React registers its
    // listeners in an effect, after mount) must read `state` instead of
    // hoping it did not miss the event.
    this.state = 'connecting';
    this.lastError = null;
    this.socket = net.createConnection(socketPath);
    this.socket.setEncoding('utf8');
    this.socket.on('connect', () => {
      this.state = 'connected';
      this.emit('connect');
    });
    this.socket.on('error', (err) => {
      this.state = 'error';
      this.lastError = err;
      this.emit('error', err);
    });
    this.socket.on('close', () => {
      if (this.state !== 'error') this.state = 'closed';
      this.emit('close');
    });
    this.socket.on('data', (chunk) => this._onData(chunk));
  }

  _onData(chunk) {
    this.buffer += chunk;
    let newline;
    while ((newline = this.buffer.indexOf('\n')) >= 0) {
      const line = this.buffer.slice(0, newline).trim();
      this.buffer = this.buffer.slice(newline + 1);
      if (!line) continue;
      try {
        this.emit('message', JSON.parse(line));
      } catch {
        // tolerate a malformed line rather than crashing the frontend
      }
    }
  }

  _send(message) {
    this.socket.write(JSON.stringify(message) + '\n');
  }

  attach(stream, opts) {
    this._send(encode.attach(stream, opts));
  }
  history(stream, cursor) {
    this._send(encode.history(stream, cursor));
  }
  sendText(stream, text) {
    this._send(encode.sendText(stream, text));
  }
  authorize(stream, request, approve) {
    this._send(encode.authorize(stream, request, approve));
  }
  detach(stream) {
    this._send(encode.detach(stream));
  }
  interrupt(stream) {
    this._send(encode.interrupt(stream));
  }
  end() {
    this.socket.end();
  }
}
