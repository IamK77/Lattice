// Authorization UI describes server proposals; it never invents or persists rules.
export function validateAuthorization(value) {
  const object = (x) => x !== null && typeof x === 'object' && !Array.isArray(x);
  const optionalString = (x) => x === null || typeof x === 'string';
  if (!object(value) || typeof value.attachment !== 'string' || !value.attachment
      || !optionalString(value.interface) || !optionalString(value.interface_service)
      || !optionalString(value.operation_service) || !Number.isSafeInteger(value.through)
      || value.through < 0 || !object(value.grants) || !object(value.grants.grants)
      || !Array.isArray(value.pending_authorizations)
      || value.pending_authorizations.some((e) => !object(e) || typeof e.id !== 'string'
        || typeof e.type !== 'string' || !object(e.payload))) {
    throw new Error('invalid operation-permissions attachment');
  }
  return value;
}

export function authorizationCommand(command, arg, tab, connection) {
  if (!['permission', 'grants', 'revoke'].includes(command)) return null;
  const binding = tab.authorization;
  if (!tab.connected || !binding) return 'Authorization controls unavailable: attach to a supporting daemon first.';
  if (command === 'permission') {
    if (!binding.interface || !binding.interface_service) return 'This assembly has no interface permission service.';
    if (!arg) return `This interface: ${tab.permission}. Temporary permission ends on off, detach or restart; flow grants and permanent trust are separate.`;
    if (!['on', 'off'].includes(arg)) return 'Usage: /permission [on|off]';
    connection.setPermission(tab.id, binding.attachment, arg === 'on');
    return `Permission ${arg} requested; waiting for the authority. Existing grants and permanent trust are unchanged.`;
  }
  if (!binding.operation_service) return 'This assembly has no operation authorization service.';
  if (command === 'grants') {
    if (arg) return 'Usage: /grants';
    const entries = Object.entries(tab.grants);
    return entries.length ? `Persistent grants for this flow:\n${entries.map(([id, grant]) => `${id}: ${JSON.stringify(grant)}`).join('\n')}\n/revoke <id> removes a flow grant, not permanent trust.`
      : 'No flow grants. Permanent trust is separate.';
  }
  if (!arg || /\s/.test(arg)) return 'Usage: /revoke <grant-id>';
  if (!Object.hasOwn(tab.grants, arg)) return 'No such flow grant in the current authority snapshot.';
  connection.revokeGrant(tab.id, binding.attachment, arg);
  return `Revocation requested for ${arg}; waiting for the authority. Already executed work is not rolled back.`;
}

export function canGrantFlow(card) {
  return card?.question && (card.question.type !== 'operation.authorization_requested'
    || (Array.isArray(card.question.payload.grants) && card.question.payload.grants.length > 0));
}

export function answerAuthorization(tab, card, key, connection) {
  if (!tab.connected) return 'Disconnected: authorization was not sent.';
  if (tab.operationUnavailable) return 'Operation authority unavailable: authorization was not sent.';
  const binding = tab.authorization;
  if (key === 'p') {
    if (card.question?.type !== 'trust.authorization_requested') return 'Permanent trust is available only for an admission question.';
    connection.authorize(tab.id, card.request, true);
  } else if (binding?.operation_service) {
    if (key === 'f' && !canGrantFlow(card)) return 'This question does not offer a persistent flow grant; choose once or refuse.';
    connection.authorizeOperation(tab.id, binding.attachment, card.request, key !== 'n', key === 'f' ? 'flow' : 'once');
  } else {
    if (key === 'f') return 'Flow grants are unavailable on this attachment.';
    connection.authorize(tab.id, card.request, key === 'y');
  }
  return null;
}

export function authorizationChoices(tab) {
  if (!tab.connected) return 'Disconnected — approvals unavailable';
  if (tab.operationUnavailable) return 'Operation authority unavailable';
  const card = tab.pendingAuth?.[0];
  const scoped = tab.connected && tab.authorization?.operation_service;
  return `${scoped ? 'y once' : 'y allow (legacy service semantics)'} · n refuse`
    + (scoped && canGrantFlow(card) ? ' · f grant this flow' : '')
    + (card?.question?.type === 'trust.authorization_requested' ? ' · p permanent trust' : '');
}

export function authorizationDetails(card) {
  const question = card?.question;
  if (!question) return '';
  const proposals = question.payload.grants;
  return `${question.payload.summary ?? 'Authorization required'}`
    + (Array.isArray(proposals)
      ? proposals.length
        ? `\nProposed flow scope: ${JSON.stringify(proposals)}\nAn argv prefix permits trailing arguments; it does not bind directory, remote mapping or executable contents.`
        : '\nThis rule requires approval each time; no persistent grant is offered.'
      : '\nA flow grant covers only this exact request and its declared effects where applicable.');
}
