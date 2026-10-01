import test from 'node:test';
import assert from 'node:assert/strict';
import { encode, decode } from '../src/protocol.js';
import { reduce, initialState, active } from '../src/state.js';
import { authorizationCommand, answerAuthorization, authorizationChoices, authorizationDetails } from '../src/authorization.js';

const question = (id = 'question', grants = [{ kind: 'command_prefix', tool: 'Run', prefix: ['git', 'push', 'origin'] }]) => ({
  id, seq: 2, type: 'operation.authorization_requested', source: 'operations', causes: ['held'],
  payload: { held: 'held', summary: 'push', grants },
});
const attachment = (id = 'a', questions = [question()]) => ({
  attachment: `token-${id}`, interface: id, interface_service: 'permissions', operation_service: 'operations',
  through: 10, grants: { grants: {} }, pending_authorizations: questions,
});
function attach(state, id = 's', auth = attachment()) {
  return reduce(state, { type: 'ATTACHED', id, lines: [], authorization: auth });
}
function append(state, type, payload, source = 'permissions', seq = 11) {
  return reduce(state, { type: 'APPENDED', id: state.activeId, event: { id: `e${seq}`, seq, type, payload, source, causes: [] } });
}
function recorder() {
  const calls = [];
  const connection = Object.fromEntries(['setPermission', 'authorizeOperation', 'authorize', 'revokeGrant'].map((name) => [name, (...args) => calls.push([name, ...args])]));
  return { calls, connection };
}

test('controls have explicit scoped wire shapes and reject an absent scope', () => {
  assert.deepEqual(encode.setPermission('s', 't', true), { set_permission: { stream: 's', attachment: 't', enabled: true } });
  assert.deepEqual(encode.authorizeOperation('s', 't', 'q', true, 'flow'), { authorize_operation: { stream: 's', attachment: 't', request: 'q', approve: true, scope: 'flow' } });
  assert.deepEqual(encode.revokeGrant('s', 't', 'g'), { revoke_grant: { stream: 's', attachment: 't', grant: 'g' } });
  assert.throws(() => encode.authorizeOperation('s', 't', 'q', true), /scope/);
  assert.throws(() => encode.authorizeOperation('s', 't', 'q', true, 'forever'), /scope/);
  assert.equal(decode({ attached_v2: { authorization: attachment() } }).tag, 'attached_v2');
  for (const broken of [null, { ...attachment(), attachment: '' }, { ...attachment(), grants: [] }, { ...attachment(), pending_authorizations: [{}] }]) {
    assert.throws(() => decode({ attached_v2: { authorization: broken } }), /invalid/);
  }
});

test('the attachment owns current cards outside the replay page, not historical permission', () => {
  const state = attach(initialState('s'));
  assert.equal(active(state).pendingAuth[0].question.type, 'operation.authorization_requested');
  assert.equal(active(state).lastSeq, 10);
  assert.equal(active(state).permission, 'unknown');
  const historical = append(state, 'interface.permission.state', { interfaces: { a: { open: true, enabled: true } } }, 'permissions', 9);
  assert.equal(active(historical).permission, 'unknown');
  const sent = reduce(state, { type: 'AUTH_SENT', id: 's' });
  assert.equal(active(sent).pendingAuth.length, 1, 'a transport send is not an authority decision');
  const refusedAnswer = append(sent, 'operation.authorization_decided', { verdict: 'denied', answer: 'bad-answer' }, 'operations');
  assert.equal(active(refusedAnswer).pendingAuth.length, 1);
  const decided = append(refusedAnswer, 'operation.authorization_decided', { held: 'held' }, 'operations', 12);
  assert.equal(active(decided).pendingAuth.length, 0);
});

test('only the attached authority and own interface affect permission; grants use snapshots', () => {
  let state = attach(initialState('s'));
  state = append(state, 'interface.permission.state', { interfaces: { a: { open: true, enabled: true } } }, 'forged');
  assert.equal(active(state).permission, 'unknown');
  state = append(state, 'interface.permission.state', { interfaces: { observer: { open: true, enabled: true } } }, 'permissions', 12);
  assert.equal(active(state).permission, 'off');
  state = append(state, 'interface.permission.state', { accepted: false, interfaces: { a: { open: true, enabled: true } } }, 'permissions', 13);
  assert.equal(active(state).permission, 'on', 'even a refused change carries the authority snapshot');
  state = append(state, 'operation.authorization.state', { grants: { g: { matchers: ['test'] } } }, 'operations', 14);
  assert.ok(active(state).grants.g);
  state = append(state, 'operation.authorization.state', { grants: {} }, 'operations', 15);
  assert.deepEqual(active(state).grants, {});
  state = append(state, 'core.control.component_removed', { instance: 'permissions', component: 'interface-permissions' }, 'core', 16);
  assert.equal(active(state).permission, 'unavailable');
});

test('tokens and capabilities are per tab and expire immediately on rebind, downgrade or disconnect', () => {
  let state = attach(initialState('s'));
  state = reduce(state, { type: 'OPEN', id: 'other' });
  state = attach(state, 'other', attachment('b'));
  assert.equal(state.streams.s.authorization.attachment, 'token-a');
  assert.equal(state.streams.other.authorization.attachment, 'token-b');
  state = reduce(state, { type: 'REBIND', id: 's' });
  assert.equal(state.streams.s.authorization, null);
  assert.equal(state.streams.other.authorization.attachment, 'token-b');
  state = attach(state, 's', attachment('new'));
  state = attach(state, 's', undefined);
  // Explicit legacy attach (the helper's default is deliberately not used).
  state = reduce(state, { type: 'ATTACHED', id: 's', lines: [] });
  assert.equal(state.streams.s.authorization, null);
  assert.equal(state.streams.s.permission, 'unavailable');
  state = reduce(state, { type: 'DISCONNECTED', status: 'closed' });
  assert.ok(Object.values(state.streams).every((s) => !s.connected && s.authorization === null));
});

test('once, flow and permanent trust never silently downgrade to each other', () => {
  const { calls, connection } = recorder();
  const tab = active(attach(initialState('s')));
  const card = tab.pendingAuth[0];
  assert.equal(answerAuthorization(tab, card, 'y', connection), null);
  assert.equal(answerAuthorization(tab, card, 'f', connection), null);
  assert.equal(answerAuthorization(tab, card, 'n', connection), null);
  assert.match(answerAuthorization(tab, card, 'p', connection), /only/);
  assert.deepEqual(calls.map((call) => call.slice(-2)), [[true, 'once'], [true, 'flow'], [false, 'once']]);
  const forced = active(attach(initialState('s'), 's', attachment('a', [question('forced', [])])));
  assert.match(answerAuthorization(forced, forced.pendingAuth[0], 'f', connection), /does not offer/);
  assert.doesNotMatch(authorizationChoices(forced), /f grant/);
  const trust = { ...card, question: { ...card.question, type: 'trust.authorization_requested' } };
  answerAuthorization(tab, trust, 'p', connection);
  assert.deepEqual(calls.at(-1), ['authorize', 's', 'question', true]);
  const legacy = { ...tab, authorization: null };
  assert.match(answerAuthorization(legacy, card, 'f', connection), /unavailable/);
  assert.match(authorizationChoices(legacy), /legacy service semantics/);
  assert.match(authorizationDetails(card), /git.*push.*origin/);
  assert.match(authorizationDetails(card), /trailing arguments/);
});

test('management commands are local, wait for snapshots and leave unrelated scopes alone', () => {
  const { calls, connection } = recorder();
  const tab = active(attach(initialState('s')));
  assert.match(authorizationCommand('permission', 'on', tab, connection), /waiting for the authority/);
  assert.equal(tab.permission, 'unknown');
  assert.deepEqual(calls[0], ['setPermission', 's', 'token-a', true]);
  assert.match(authorizationCommand('permission', '', tab, connection), /this interface/i);
  assert.match(authorizationCommand('permission', 'maybe', tab, connection), /Usage/);
  assert.match(authorizationCommand('grants', '', tab, connection), /No flow grants/);
  tab.grants = { g: { matchers: [{ prefix: ['git', 'push', 'origin'] }], question: 'q', interface: 'a' } };
  assert.match(authorizationCommand('grants', '', tab, connection), /git.*push.*origin/);
  assert.match(authorizationCommand('revoke', 'g', tab, connection), /not rolled back/);
  assert.ok(tab.grants.g);
  assert.deepEqual(calls.at(-1), ['revokeGrant', 's', 'token-a', 'g']);
  assert.match(authorizationCommand('revoke', 'missing', tab, connection), /No such/);
  assert.equal(calls.length, 2);
  assert.match(authorizationCommand('permission', 'on', { ...tab, connected: false }, connection), /unavailable/);
  assert.equal(authorizationCommand('unknown', '', tab, connection), null);
});
