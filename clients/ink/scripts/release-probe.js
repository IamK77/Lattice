// A strict, small-history scripted-turn probe, not a general frontend.
// Keep the evidence reducer independent of sockets so false-success cases
// remain regression tests. This does not verify real provider behavior.
import { decode } from '../src/protocol.js';

export class ReleaseProbe {
  constructor(stream, text, previousInput = null) {
    this.stream = stream;
    this.text = text;
    this.previousInput = previousInput;
    this.boundary = null;
    this.last = null;
    this.events = new Map();
  }

  descends(event, ancestor) {
    const pending = [...event.causes];
    const visited = new Set();
    while (pending.length) {
      const id = pending.pop();
      if (id === ancestor) return true;
      if (visited.has(id)) continue;
      visited.add(id);
      pending.push(...(this.events.get(id)?.causes ?? []));
    }
    return false;
  }

  accept(message) {
    const { tag, body } = decode(message);
    if (tag === 'error' && (body.stream == null || body.stream === this.stream)) {
      throw new Error(`daemon error: ${body.message}`);
    }
    if (body?.stream !== this.stream) return null;
    if (tag === 'attached') {
      if (this.boundary !== null) throw new Error('duplicate attachment');
      const history = body.history;
      if (!history || !Number.isSafeInteger(history.through) || history.through < 0 ||
          history.older !== null || history.state?.busy !== false ||
          history.state?.waiting !== false || !Array.isArray(history.state?.pending_auth) ||
          history.state.pending_auth.length) {
        throw new Error('probe requires complete small history and an idle attachment');
      }
      if (!Array.isArray(body.replay) || (this.previousInput && !body.replay.some(
        e => e.id === this.previousInput && e.stream === this.stream &&
          e.seq <= history.through && e.type === 'core.input.user_message',
      ))) throw new Error('previous input is absent from restored history');
      this.boundary = history.through;
      this.last = this.boundary;
      return { send: this.text };
    }
    if (tag === 'appended') {
      const e = body.event;
      if (this.boundary === null || !e || e.stream !== this.stream ||
          !Number.isSafeInteger(e.seq) || e.seq <= this.last ||
          typeof e.id !== 'string' || !e.id || this.events.has(e.id) ||
          !Array.isArray(e.causes) || !e.causes.every(id => typeof id === 'string') ||
          this.events.size >= 1000) throw new Error('invalid or unbounded live event sequence');
      this.last = e.seq;
      this.events.set(e.id, e);
      return null;
    }
    if (tag !== 'quiescent') return null;
    const events = [...this.events.values()];
    const inputs = events.filter(e => e.type === 'core.input.user_message' &&
      e.causes.length === 0 && e.payload?.text === this.text);
    if (inputs.length !== 1) throw new Error('no unique new input before quiescence');
    const input = inputs[0];
    const requests = events.filter(e => e.type === 'core.model.call_started' &&
      this.descends(e, input.id) && (!this.previousInput ||
        e.payload?.input?.parts?.some(p => p.event === this.previousInput)));
    for (const completed of events.filter(e => e.type === 'core.model.call_completed' &&
      e.payload?.status === 'ok')) {
      const request = requests.find(e => this.descends(completed, e.id));
      if (!request) continue;
      // Standard assembly sends the expanded input, not its raw parent, as
      // model material. Carry that exact pointer into the next restart check.
      const material = events.find(e => e.type === 'core.input.user_message' &&
        (e.id === input.id || this.descends(e, input.id)) &&
        request.payload?.input?.parts?.some(p => p.event === e.id));
      if (!material) continue;
      const reply = events.find(e => e.type === 'core.output.reply' &&
        e.causes.includes(completed.id) && e.payload?.text === 'scripted reply 1' &&
        e.payload?.error == null && e.payload?.cancelled !== true);
      const turn = events.find(e => e.type === 'core.control.turn_completed' &&
        e.causes.includes(completed.id));
      if (!reply || !turn) continue;
      return { result: {
        stream: this.stream,
        boundary: this.boundary,
        previousInput: this.previousInput,
        input: input.id,
        materialInput: material.id,
        evidence: events.map(e => ({
          id: e.id, seq: e.seq, type: e.type, causes: e.causes,
          status: e.payload?.status,
          material: e.payload?.input?.parts?.map(p => p.event).filter(Boolean),
        })),
      } };
    }
    const diagnostic = events.map(e => ({ id: e.id, type: e.type, causes: e.causes,
      status: e.payload?.status, text: e.type === 'core.output.reply' ? e.payload?.text : undefined,
      material: e.payload?.input?.parts?.map(p => p.event).filter(Boolean),
    }));
    throw new Error(`quiescence did not prove a successful new turn with restored material: ${JSON.stringify(diagnostic)}`);
  }
}
