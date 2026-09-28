// The Lattice frontend, in React/Ink. Multiple streams live as tabs (one per
// conversation); `/btw` opens a sidechannel derived from the current one. The
// tab state is a pure reducer (src/state.js); this file is its view + the
// wiring from connection events and keys into dispatch.

import React, { useEffect, useReducer, useRef } from 'react';
import { Box, Text, useApp, useInput } from 'ink';
import { html } from 'htm/react';
import { decode, renderLine, replyChunk } from './protocol.js';
import { reduce, initialState, active, foldReplay } from './state.js';
import { theme, marker } from './theme.js';

const MAX_LINES = 500;
const SPINNER = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

// What this client declares at the handshake beyond display+text: it renders
// authorization cards and answers them with y/n, and offers a `/` palette
// folded from the ledger's skill listings
const CLIENT_CAPABILITIES = ['authorize', 'palette', 'history-pages'];

// The `/` palette: installed skills (folded off skill.listing events) whose
// name the typed input is a prefix of. Empty unless a slash is being typed;
// empty again once the full name is typed (Enter then sends it as text and
// the expansion station does the rest). Capped so it never eats the screen.
export function paletteMatches(inputText, skills) {
  if (!inputText.startsWith('/')) return [];
  const token = inputText.split(/\s/, 1)[0];
  return (skills ?? [])
    .filter((sk) => `/${sk.name}`.startsWith(token) && `/${sk.name}` !== token)
    .slice(0, 6);
}

// A stream id that will not collide with a closed tab here or an older
// stream still alive in the daemon (an unnamed /new or /btw wants a fresh
// dossier; only a user-NAMED stream should ever re-attach to an old one)
const freshId = (prefix) => `${prefix}-${Math.random().toString(36).slice(2, 6)}`;

function useSpinner(activeFlag) {
  const [i, setI] = React.useState(0);
  useEffect(() => {
    if (!activeFlag) return undefined;
    const t = setInterval(() => setI((n) => (n + 1) % SPINNER.length), 80);
    return () => clearInterval(t);
  }, [activeFlag]);
  return activeFlag ? SPINNER[i] : ' ';
}

function Line({ who, text, lead }) {
  const color = theme[who] ?? theme.agent;
  return html`
    <${Box} marginTop=${lead ? 1 : 0}>
      <${Box} width=${2}><${Text} color=${color}>${marker[who] ?? ' '}<//><//>
      <${Text} color=${color} wrap="wrap">${text}<//>
    <//>
  `;
}

function Tabs({ order, streams, activeId }) {
  return html`
    <${Box} marginBottom=${1}>
      ${order.map((id, i) => {
        const st = streams[id];
        const on = id === activeId;
        const label = st.kind === 'side' ? `${id} ⌥` : id;
        return html`
          <${Box} key=${id} marginRight=${1}>
            <${Text}
              color=${on ? theme.accent : theme.dim}
              bold=${on}
              underline=${on}
            >${`${i + 1}:${label}`}<//>
          <//>
        `;
      })}
    <//>
  `;
}

export function App({ connection, stream, title }) {
  const { exit } = useApp();
  const [state, dispatch] = useReducer(reduce, stream, initialState);
  const input = useRef('');
  const [, force] = React.useReducer((n) => n + 1, 0);
  const setInput = (v) => {
    input.current = v;
    force();
  };
  const cur = active(state);
  const spin = useSpinner(cur.busy && !cur.streaming);
  // The connection listeners are registered once and would otherwise close
  // over the first render's state; the ref always points at the current one
  const stateRef = useRef(state);
  stateRef.current = state;

  useEffect(() => {
    let attached = false;
    const onConnect = () => {
      if (attached) return;
      attached = true;
      dispatch({ type: 'STATUS', id: stream, status: 'connected' });
      connection.attach(stream, { capabilities: CLIENT_CAPABILITIES });
    };
    const onError = (err) =>
      dispatch({
        type: 'STATUS',
        id: stream,
        status: `cannot reach the daemon (${err?.code || err?.message || 'error'}) — is it running?`,
      });
    const onClose = () => dispatch({ type: 'STATUS', id: stream, status: 'daemon disconnected' });
    connection.on('connect', onConnect);
    connection.on('error', onError);
    connection.on('close', onClose);
    connection.on('message', (raw) => {
      const { tag, body } = decode(raw);
      const id = body?.stream;
      if (tag === 'attached') {
        const events = body.replay ?? [];
        // One fold for the replay and the live stream both — see
        // `foldReplay`. Two hand-written versions had already drifted apart.
        const folded = foldReplay(events);
        const lines = folded.lines.concat(
          (body.warnings ?? []).map((text) => ({ who: 'notice', text })),
        );
        const { busy, waiting, pendingAuth } = folded;
        const listing = events.filter((e) => e.type === 'skill.listing').pop();
        dispatch({ type: 'ATTACHED', id, lines, busy, waiting, pendingAuth, history: body.history });
        if (!body.history && listing) dispatch({ type: 'LISTING', id, skills: listing.payload?.skills ?? [] });
      } else if (tag === 'history_page') {
        dispatch({ type: 'HISTORY_PAGE', id, cursor: body.cursor, events: body.replay, older: body.older });
      } else if (tag === 'history_error') {
        dispatch({ type: 'HISTORY_ERROR', id, cursor: body.cursor, message: body.message });
      } else if (tag === 'appended') {
        // Dispatch every event, line or not: the raw type drives the turn
        // phase (wake and turn-end carry no line but move busy), while the
        // line, if any, joins the transcript
        const line = renderLine(body.event);
        dispatch({
          type: 'APPENDED',
          id,
          eventType: body.event.type,
          causes: body.event.causes,
          event: body.event,
          line,
          key: body.event.id,
        });
      } else if (tag === 'notice') {
        // Only the FOREGROUND REPLY belongs in the transcript — not a
        // background call's stream, not the model thinking out loud. Both are
        // marked in the payload, so neither needs an instance name to spot.
        const chunk = replyChunk(body.payload);
        if (chunk !== null) {
          dispatch({ type: 'NOTICE', id, chunk });
        }
      } else if (tag === 'quiescent') {
        dispatch({ type: 'QUIESCENT', id });
      } else if (tag === 'error') {
        dispatch({ type: 'ERROR', id: id ?? stateRef.current.activeId, message: body.message });
      }
    });
    // The socket may have won the race before this effect ran (connecting
    // starts at construction, listeners land after mount): act on the state
    // it already reached, not only on future events
    if (connection.state === 'connected') onConnect();
    else if (connection.state === 'error') onError(connection.lastError);
    else if (connection.state === 'closed') onClose();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const runSlash = (line, activeId) => {
    const [cmd, ...rest] = line.slice(1).split(' ');
    const arg = rest.join(' ').trim();
    switch (cmd) {
      case 'exit':
        connection.end();
        exit();
        return true;
      case 'older': {
        const tab = stateRef.current.streams[activeId];
        if (tab.historyCursor && !tab.historyLoading) {
          dispatch({ type: 'HISTORY_REQUEST', id: activeId });
          connection.history(activeId, tab.historyCursor);
        }
        return true;
      }
      case 'latest':
        connection.attach(activeId, { capabilities: CLIENT_CAPABILITIES });
        return true;
      case 'clear':
        dispatch({ type: 'CLEAR', id: activeId });
        return true;
      case 'new': {
        const id = arg || freshId('s');
        if (state.streams[id]) {
          dispatch({ type: 'ACTIVE', id }); // the tab exists — just go there
          return true;
        }
        dispatch({ type: 'OPEN', id, kind: 'main' });
        connection.attach(id, { capabilities: CLIENT_CAPABILITIES });
        return true;
      }
      case 'btw': {
        // A sidechannel is always a fresh dossier: the id must not collide
        // with a closed tab or an older stream still alive in the daemon
        const id = freshId('btw');
        dispatch({ type: 'OPEN', id, kind: 'side', parent: activeId });
        connection.attach(id, { deriveFrom: activeId, capabilities: CLIENT_CAPABILITIES });
        if (arg) {
          dispatch({ type: 'SENT', id });
          connection.sendText(id, arg);
        }
        return true;
      }
      case 'tab': {
        const n = parseInt(arg, 10);
        if (n >= 1 && n <= state.order.length) dispatch({ type: 'ACTIVE', id: state.order[n - 1] });
        return true;
      }
      case 'close':
        // The stream stays open in the daemon; only our subscription ends
        if (state.order.length > 1) {
          connection.detach(activeId);
          dispatch({ type: 'CLOSE', id: activeId });
        }
        return true;
      default:
        return false; // unknown slash falls through to being sent as text
    }
  };

  useInput((ch, key) => {
    if (key.ctrl && ch === 'c') {
      connection.end();
      exit();
      return;
    }
    if (key.tab) {
      dispatch({ type: 'CYCLE', by: key.shift ? -1 : 1 });
      return;
    }
    if (key.escape) {
      if (cur.busy) connection.interrupt(cur.id);
      return;
    }
    // y/n answers an open authorization card — only on an empty input line,
    // so typing a message containing y/n is never hijacked
    const openCard = cur.pendingAuth?.[0];
    if (openCard && !input.current.length && (ch === 'y' || ch === 'n')) {
      connection.authorize(cur.id, openCard.request, ch === 'y');
      dispatch({ type: 'AUTH_SENT', id: cur.id });
      return;
    }
    if (key.return) {
      const line = input.current.trim();
      setInput('');
      if (!line) return;
      if (line.startsWith('/') && runSlash(line, cur.id)) return;
      dispatch({ type: 'SENT', id: cur.id });
      connection.sendText(cur.id, line);
      return;
    }
    if (key.backspace || key.delete) {
      setInput(input.current.slice(0, -1));
      return;
    }
    if (ch && !key.ctrl && !key.meta) setInput(input.current + ch);
  });

  const shown = cur.historyLines ?? cur.transcript.slice(-MAX_LINES);

  return html`
    <${Box} flexDirection="column" paddingX=${1} paddingTop=${1}>
      <${Box}>
        <${Text} color=${theme.accent} bold>${'✦ lattice'}<//>
        <${Text} color=${theme.dim}>${`  ${title}`}<//>
      <//>
      <${Tabs} order=${state.order} streams=${state.streams} activeId=${state.activeId} />

      ${cur.kind === 'side'
        ? html`<${Box} marginBottom=${1}><${Text} color=${theme.dim} italic>${`⌥ sidechannel of ${cur.parent} — observing, private`}<//><//>`
        : null}

      ${cur.historyError ? html`<${Text} color=${theme.error}>${cur.historyError}<//>` : null}
      <${Text} color=${theme.dim}>${cur.historyLoading ? 'Loading history…' : `${cur.historyLines !== null ? 'Older history · /latest returns to live · ' : ''}${cur.historyCursor ? '/older loads the previous page' : 'Beginning of retained history'}`}<//>
      <${Box} flexDirection="column">
        ${shown.map(
          (item) => html`<${Line} key=${item.key} who=${item.who} text=${item.text} lead=${item.lead} />`,
        )}
      <//>

      ${cur.streaming
        ? html`<${Line} who="agent" text=${cur.streaming} />`
        : cur.busy
          ? html`
              <${Box}>
                <${Box} width=${2}><${Text} color=${theme.accent}>${spin}<//><//>
                <${Text} color=${theme.dim}>thinking…<//>
              <//>
            `
          : cur.waiting
            ? html`<${Text} color=${theme.dim}>Waiting for results — you can still type<//>`
            : null}

      <${Box}
        marginTop=${1}
        borderStyle="round"
        borderColor=${cur.busy ? theme.dim : theme.accent}
        paddingX=${1}
      >
        <${Text} color=${theme.accent} bold>${'❯ '}<//>
        <${Text}>${input.current}<//>
        <${Text} color=${theme.dim}>${input.current.length ? '' : 'Type a message…  ·  /new /btw /tab /exit'}<//>
        <${Text} color=${theme.accent}>▏<//>
      <//>

      ${paletteMatches(input.current, cur.skills).map(
        (sk) => html`
          <${Box} key=${sk.name} paddingX=${1}>
            <${Text} color=${theme.dim}>${`  /${sk.name}  `}<//>
            <${Text} color=${theme.dim} italic>${sk.description}<//>
          <//>
        `,
      )}

      <${Box} paddingX=${1}>
        <${Text} color=${theme.dim}>
          ${cur.pendingAuth?.length
            ? `${cur.pendingAuth.length > 1 ? `${cur.pendingAuth.length} authorizations waiting` : 'authorization waiting'} · y allow · n refuse`
            : cur.busy
              ? 'Esc interrupts · Ctrl-C quits'
              : `${cur.status} · Enter sends · Tab switches · /btw sidechannel · /new tab`}
        <//>
      <//>
    <//>
  `;
}
