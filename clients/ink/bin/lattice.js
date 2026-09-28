#!/usr/bin/env node
// Entry point: connect to the daemon socket and render the Ink app.
//
//   lattice                       # connect to ~/.lattice/daemon.sock, stream "main"
//   LATTICE_SOCKET=... lattice    # a different socket
//   LATTICE_STREAM=work lattice   # a different stream (conversation)

import os from 'node:os';
import path from 'node:path';
import { render } from 'ink';
import { html } from 'htm/react';
import { Connection } from '../src/connection.js';
import { App } from '../src/app.js';

const socketPath =
  process.env.LATTICE_SOCKET || path.join(os.homedir(), '.lattice', 'daemon.sock');
const stream = process.env.LATTICE_STREAM || 'main';

const connection = new Connection(socketPath);
render(
  html`<${App} connection=${connection} stream=${stream} title=${`lattice · ${socketPath}`} />`,
);
