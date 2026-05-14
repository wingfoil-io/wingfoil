// Connection status pill bound to a WingfoilClient.
//
// The host element receives one of three CSS classes — `live`,
// `connecting`, `idle` — alongside its base `pill` class, and its text
// content is set to the current state.

export class StatusPill {
  constructor({ host, client }) {
    if (!host) throw new Error('StatusPill: host element required');
    this.host = host;
    this._set('disconnected', 'idle');
    if (client) {
      this._unbind = client.onConnection((s) => {
        if (s.kind === 'open') this._set('live', 'live');
        else if (s.kind === 'connecting') this._set('connecting', 'connecting');
        else this._set('disconnected', 'idle');
      });
    }
  }

  _set(label, cls) {
    this.host.textContent = label;
    this.host.className = 'pill ' + cls;
  }

  destroy() {
    this._unbind?.();
  }
}
