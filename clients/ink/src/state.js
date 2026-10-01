import { renderLine } from './protocol.js';

// The frontend's multi-stream state as a pure reducer, so the tab logic is
// testable without a terminal. Each stream (tab) has its own transcript,
// streaming fragment, busy flag and status; the UI renders the active one.

export function initialState(firstId) {
  return {
    streams: { [firstId]: newStream(firstId, 'main', null) },
    order: [firstId],
    activeId: firstId,
    keys: 0,
  };
}

function newStream(id, kind, parent) {
  return {
    id,
    kind,
    parent,
    transcript: [],
    historyLines: null,
    historyCursor: null,
    historyLoading: false,
    historyError: null,
    historyBoundary: null,
    lastSeq: 0,
    streaming: '',
    busy: false,
    waiting: false,
    status: 'connecting…',
    // An open authorization request (the trust gate's card): the event id
    // the y/n answer must name. Opened by the request event, closed by the
    // gate's decision — derived from the ledger, like busy.
    pendingAuth: [],
    connected: false,
    authorization: null,
    permission: 'unavailable',
    grants: {},
    // The invokable-skills menu, folded off skill.listing events:
    // [{ name, description }] — feeds the `/` palette
    skills: [],
  };
}

// Fold one event's effect on the open authorization cards.
//
// A QUEUE, not a slot. A single slot let a second request overwrite the
// first, and any decision clear the slot whether or not it was that request's
// — so with two questions open the first became unanswerable, and the call
// waiting on it waited forever. The Rust frontend was fixed for exactly this;
// this side kept the old shape, and a daemon shows the same cards to every
// client it has.
//
// A card holds the id of the call it is about (the request's cause), because
// a decision names the call it reviewed, not the question asked about it.
export function authPhase(prev, event, includeDetails = false) {
  const open = prev ?? [];
  if (['operation.authorization_requested', 'trust.authorization_requested', 'browser.authorization_requested', 'experts.authorization_requested'].includes(event.type)) {
    const held = event.payload?.held ?? event.causes?.[0] ?? null;
    if (open.some((card) => card.request === event.id)) return open;
    return [...open, { request: event.id, held, ...(includeDetails ? { question: event } : {}) }];
  }
  if (['operation.authorization_decided', 'trust.gate.decision', 'browser.authorization_decided', 'experts.authorization_decided', 'core.control.interrupted', 'core.tool.exec_completed'].includes(event.type)) {
    const settled = [...(event.causes ?? []), event.payload?.held].filter(Boolean);
    return open.filter((card) => !settled.includes(card.held));
  }
  return open;
}

// Whether this line merely repeats the previous one's tool CALL — the gate's
// forward arriving right after the loop's emission. By call id, not by text:
// asking for the same thing twice in a row is legitimate and both should
// show, and only the id can tell the two situations apart.
export const MAX_TRANSCRIPT = 500;

function sameCursor(left, right) {
  return left != null && right != null && left.stream === right.stream
    && left.generation === right.generation && left.through === right.through
    && left.before === right.before;
}

function repeatsLastCall(transcript, line) {
  const last = transcript[transcript.length - 1];
  return (
    line.who === 'tool' &&
    last?.who === 'tool' &&
    (line.call ? last.call === line.call : last.text === line.text)
  );
}

/// Fold a replay into the same shape the live path folds into, so a client
/// that reconnects sees what one that stayed sees. The replay used to map
/// events to lines with no dedup at all, so every gated tool call showed
/// twice to anyone who attached late.
export function foldReplay(events) {
  const lines = [];
  let busy = false;
  let waiting = false;
  let pendingAuth = [];
  for (const event of events) {
    busy = turnPhase(busy, event.type, event.causes, event.payload);
    waiting = waitingPhase(waiting, event);
    pendingAuth = authPhase(pendingAuth, event);
    const line = renderLine(event);
    if (line && !repeatsLastCall(lines, line)) lines.push(line);
  }
  return { lines, busy, waiting, pendingAuth };
}

function patch(state, id, changes) {
  const st = state.streams[id];
  if (!st) return state;
  const next = typeof changes === 'function' ? changes(st) : changes;
  return { ...state, streams: { ...state.streams, [id]: { ...st, ...next } } };
}

// Which event types open (true) or close (false) a turn. Deriving busy from
// the ledger — mirroring the bin's note_turn_boundary — is what lights the
// indicator for a turn a background wake started (a timer firing, a background
// run finishing), not only one the local user typed. Everything mid-turn holds
// the current phase.
function waitingPhase(previous, event) {
  if (event.type === 'loop.waiting') return true;
  // Reuse the same transition table. Undefined means no phase transition.
  return turnPhase(undefined, event.type, event.causes, event.payload) === undefined
    ? previous : false;
}

export function turnPhase(prevBusy, eventType, causes, payload) {
  switch (eventType) {
    case 'core.input.user_message':
      // A CAUSED user message is the expansion station's re-emission of a
      // turn already counted, not a new one
      if (causes?.length) return prevBusy;
      return true;
    case 'core.input.wake':
      return true;
    case 'core.model.call_started':
      return payload?.purpose === undefined ? true : prevBusy;
    case 'loop.waiting':
    case 'core.control.turn_completed':
      return false;
    default:
      return prevBusy;
  }
}

function authorizationState(st, event) {
  const binding = st.authorization;
  if (!binding || !st.connected) return {};
  if (event.type === 'interface.permission.state' && event.source === binding.interface_service) {
    const own = event.payload?.interfaces?.[binding.interface];
    return { permission: own?.open === true && own?.enabled === true ? 'on' : 'off' };
  }
  if (event.type === 'operation.authorization.state' && event.source === binding.operation_service) {
    return { grants: event.payload?.grants ?? {} };
  }
  if (event.source === 'core') {
    const retired = event.type === 'core.control.component_removed' ? event.payload?.instance
      : event.type === 'core.control.component_crashed' ? event.payload?.component
        : event.type === 'core.control.error' && event.payload?.code === 'core.component_failed'
          ? event.payload?.detail?.component : null;
    if (retired && retired === binding.interface_service) return {
      permission: 'unavailable', authorization: { ...binding, interface_service: null },
    };
    if (retired && retired === binding.operation_service) return {
      operationUnavailable: true, authorization: { ...binding, operation_service: null },
    };
  }
  return {};
}

export function reduce(state, action) {
  switch (action.type) {
    case 'OPEN': {
      if (state.streams[action.id]) return { ...state, activeId: action.id };
      return {
        ...state,
        streams: {
          ...state.streams,
          [action.id]: newStream(action.id, action.kind ?? 'main', action.parent ?? null),
        },
        order: [...state.order, action.id],
        activeId: action.id,
      };
    }
    case 'ACTIVE':
      return state.streams[action.id] ? { ...state, activeId: action.id } : state;
    case 'CYCLE': {
      const i = state.order.indexOf(state.activeId);
      const next = state.order[(i + action.by + state.order.length) % state.order.length];
      return { ...state, activeId: next };
    }
    case 'CLOSE': {
      if (state.order.length <= 1) return state; // always keep one tab
      const order = state.order.filter((x) => x !== action.id);
      const streams = { ...state.streams };
      delete streams[action.id];
      const activeId =
        state.activeId === action.id ? order[order.length - 1] : state.activeId;
      return { ...state, streams, order, activeId };
    }
    case 'STATUS':
      return patch(state, action.id, { status: action.status });
    case 'REBIND':
      return patch(state, action.id, { connected: false, authorization: null, permission: 'unavailable' });
    case 'DISCONNECTED':
      return { ...state, streams: Object.fromEntries(Object.entries(state.streams).map(([id, st]) => [id,
        { ...st, connected: false, authorization: null, permission: 'unavailable', status: action.status }])) };
    case 'ATTACHED':
      // Current controls come from the complete-prefix projection, never
      // from a tail page whose opening authorization may be out of view.
      return patch(state, action.id, (st) => ({
        busy: action.history?.state.busy ?? action.busy ?? false,
        waiting: action.history?.state.waiting ?? action.waiting ?? false,
        pendingAuth: action.authorization
          ? action.authorization.pending_authorizations.reduce((cards, event) => authPhase(cards, event, true), [])
          : action.history?.state.pending_auth ?? action.pendingAuth ?? [],
        connected: true,
        operationUnavailable: false,
        authorization: action.authorization ?? null,
        permission: action.authorization?.interface_service ? 'unknown' : 'unavailable',
        grants: action.authorization?.grants.grants ?? {},
        skills: action.history?.state.skills ?? st.skills,
        lastSeq: action.authorization?.through ?? action.history?.through ?? 0,
        historyCursor: action.history?.older ?? null,
        historyBoundary: action.lines[0] ?? null,
        historyLoading: false,
        historyError: null,
        historyLines: null,
        streaming: '',
        transcript: action.lines.map((l, i) => ({
          ...l,
          key: `r-${action.id}-${i}`,
          lead: l.who === 'you' && i !== 0,
        })).slice(-MAX_TRANSCRIPT),
      }));
    case 'HISTORY_REQUEST':
      return patch(state, action.id, { historyLoading: true, historyError: null });
    case 'HISTORY_PAGE':
      return patch(state, action.id, (st) => {
        if (!st.historyLoading || !sameCursor(st.historyCursor, action.cursor)) return {};
        const lines = foldReplay(action.events).lines;
        const boundary = lines[0] ?? st.historyBoundary;
        // The same forwarded call can straddle two pages. It was already
        // displayed at the newer boundary, so do not show it twice.
        if (st.historyBoundary && repeatsLastCall(lines, st.historyBoundary)) lines.pop();
        return {
          historyLines: lines.map((line, i) => ({ ...line, key: `h-${action.cursor.before}-${i}` })),
          historyBoundary: boundary,
          historyCursor: action.older ?? null,
          historyLoading: false,
        };
      });
    case 'HISTORY_ERROR':
      return patch(state, action.id, (st) => {
        if (!st.historyLoading || !sameCursor(st.historyCursor, action.cursor)) return {};
        return {
          historyLoading: false,
          historyError: action.message,
          transcript: [...st.transcript, { who: 'error', text: action.message,
            key: `history-error-${action.cursor.before}` }].slice(-MAX_TRANSCRIPT),
        };
      });
    case 'APPENDED':
      // Fold every ledger event: the raw event type drives the turn phase
      // (busy) and the authorization card, a rendered line (if any) joins the
      // transcript. Boundary events — a wake, a turn's end — carry no line.
      return patch(state, action.id, (st) => {
        // The three fields the folds need, however the action was built.
        const event = action.event ?? {
          type: action.eventType,
          id: action.key,
          causes: action.causes,
        };
        if (Number.isSafeInteger(event.seq) && event.seq <= st.lastSeq) return {};
        const lastSeq = Number.isSafeInteger(event.seq) ? event.seq : st.lastSeq;
        const skills = event.type === 'skill.listing' ? event.payload?.skills ?? [] : st.skills;
        const busy = turnPhase(st.busy, event.type, event.causes, event.payload);
        const waiting = waitingPhase(st.waiting, event);
        const pendingAuth = authPhase(st.pendingAuth, event, st.authorization !== null);
        const authorization = authorizationState(st, event);
        if (!action.line) return { ...authorization, busy, waiting, pendingAuth, lastSeq, skills, streaming: waiting ? '' : st.streaming };
        // A gated assembly records one tool request twice (the loop's
        // emission and the gate's forward) — one line is the truth
        if (repeatsLastCall(st.transcript, action.line)) {
          return { ...authorization, busy, waiting, pendingAuth, lastSeq, skills };
        }
        return {
          ...authorization,
          lastSeq,
          skills,
          streaming: '',
          busy,
          waiting,
          pendingAuth,
          transcript: [
            ...st.transcript,
            {
              ...action.line,
              key: action.key,
              lead: action.line.who === 'you' && st.transcript.length > 0,
            },
          ].slice(-MAX_TRANSCRIPT),
        };
      });
    case 'NOTICE':
      return patch(state, action.id, (st) => ({ streaming: st.streaming + action.chunk }));
    case 'QUIESCENT':
      return patch(state, action.id, { busy: false });
    case 'LOCAL_NOTICE':
    case 'ERROR': {
      const key = `x-${state.keys}`;
      return {
        ...patch(state, action.id, (st) => ({
          transcript: [...st.transcript, { who: action.type === 'ERROR' ? 'error' : 'notice', text: action.message, key }].slice(-MAX_TRANSCRIPT),
        })),
        keys: state.keys + 1,
      };
    }
    case 'LISTING':
      // The menu follows the ledger: each listing replaces it wholesale
      return patch(state, action.id, { skills: action.skills ?? [] });
    case 'AUTH_SENT':
      // Legacy clients advance locally. Negotiated controls can be rejected
      // (for example a stale token), so only the authority retires those cards.
      return patch(state, action.id, (st) => st.authorization ? {} : { pendingAuth: st.pendingAuth.slice(1) });
    case 'SENT':
      // The user's line is NOT echoed locally: the daemon broadcasts it back
      // the moment it is recorded, and the ledger is the single source of
      // truth (echoing both ways showed every message twice)
      return patch(state, action.id, { streaming: '', busy: true, waiting: false });
    case 'CLEAR':
      return patch(state, action.id, { transcript: [] });
    default:
      return state;
  }
}

export const active = (state) => state.streams[state.activeId];
