// The Lattice daemon wire protocol, on the client side.
//
// Frames are NDJSON — one JSON object per line (the same framing as the Rust
// cross-process bridge). Messages are externally tagged: a server message is
// an object with exactly one key, e.g. { "appended": { ... } }. This file is
// the client's entire knowledge of the core; it never touches Rust.

import { validateAuthorization } from './authorization.js';

/// Decode an externally-tagged message into { tag, body }.
export function decode(message) {
  const tag = Object.keys(message)[0];
  if (tag === 'attached_v2') validateAuthorization(message[tag]?.authorization);
  return { tag, body: message[tag] };
}

/// Encode helpers — the things a frontend ever says. `attach` opens/attaches
/// a stream; passing `deriveFrom` opens it as a /btw sidechannel observing
/// that parent. `capabilities` is the handshake's self-declaration of what
/// this client can do beyond display+text (e.g. 'authorize'); omitted when
/// empty, matching the Rust side's skip-when-empty serialization.
export const encode = {
  history: (stream, cursor) => ({ history: { stream, cursor } }),
  attach: (stream, { template, deriveFrom, capabilities } = {}) => ({
    attach: {
      stream,
      template: template ?? null,
      derive_from: deriveFrom ?? null,
      ...(capabilities?.length ? { capabilities } : {}),
    },
  }),
  sendText: (stream, text) => ({ send_text: { stream, text } }),
  authorize: (stream, request, approve) => ({ authorize: { stream, request, approve } }),
  setPermission: (stream, attachment, enabled) => ({ set_permission: { stream, attachment, enabled } }),
  authorizeOperation: (stream, attachment, request, approve, scope) => {
    if (!['once', 'flow'].includes(scope)) throw new Error('approval scope must be once or flow');
    return { authorize_operation: { stream, attachment, request, approve, scope } };
  },
  revokeGrant: (stream, attachment, grant) => ({ revoke_grant: { stream, attachment, grant } }),
  detach: (stream) => ({ detach: { stream } }),
  interrupt: (stream) => ({ interrupt: { stream } }),
};

/// The live reply text carried by a notice, or null when the notice is not
/// reply text. Two things disqualify it: a `purpose` marks a background call
/// (the condense model), and a `phase` marks thinking. Both would otherwise be
/// pasted straight into the agent's answer.
export function replyChunk(payload) {
  const p = payload ?? {};
  if (typeof p.chunk !== 'string') return null;
  if (p.purpose != null || p.phase != null) return null;
  return p.chunk;
}

/// Turn one ledger event into a screen line, or null if it shows nothing.
/// Same event → line mapping as the Rust `render_line`; the marker glyph is
/// added by the UI's gutter, so tool text here carries no leading ⚙.
export function renderLine(event) {
  const p = event.payload ?? {};
  switch (event.type) {
    case 'core.input.user_message':
      // A CAUSED user message is the expansion station's re-emission of the
      // typed original — rendering both would double every line (and print
      // expanded skill bodies)
      if (event.causes?.length) return null;
      return typeof p.text === 'string' ? { who: 'you', text: p.text } : null;
    case 'core.output.reply':
      if (typeof p.text === 'string') return { who: 'agent', text: p.text };
      if (p.cancelled === true) return { who: 'agent', text: '[interrupted]' };
      if (p.error && typeof p.error.message === 'string')
        return { who: 'error', text: p.error.message };
      return null;
    case 'core.tool.exec_started':
      // `call` rides along so a gated assembly's two records of one request
      // (the loop's emission and the gate's forward) can be recognised as
      // the same call rather than compared as text — two identical calls in
      // a row are legitimate and must both show.
      return {
        who: 'tool',
        call: p.call ?? null,
        text: `${p.tool ?? '?'} ${JSON.stringify(p.arguments ?? {})}`,
      };
    case 'operation.authorization_requested':
    case 'experts.authorization_requested':
    case 'browser.authorization_requested':
    case 'trust.authorization_requested':
      return {
        who: 'notice',
        text: `authorization required: ${p.summary ?? 'an operation'}`,
      };
    case 'operation.authorization_decided':
    case 'experts.authorization_decided':
    case 'browser.authorization_decided':
      return { who: 'notice', text: `authorization: ${event.reason ?? p.error ?? ''}` };
    case 'operation.authorization.state':
      return event.causes?.length ? { who: 'notice', text: `${event.reason ?? 'Flow grants changed'} — /grants to inspect` } : null;
    case 'interface.permission.state':
      return p.accepted === false ? { who: 'error', text: p.error ?? 'Permission change refused' } : null;
    case 'trust.gate.decision':
      return {
        who: 'notice',
        text: `${p.verdict === 'granted' ? '✓' : '✗'} trust: ${event.reason ?? ''}`,
      };
    default:
      return null;
  }
}
