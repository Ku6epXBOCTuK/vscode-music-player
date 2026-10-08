# Backlog

## Ideas

- [ ] **Instant volume via OBS WebSocket** — the only way to get truly instant
      volume changes: the VS Code extension (or the backend) talks to OBS over
      its WebSocket API and adjusts the Media Source gain client-side, instead
      of waiting for the client's audio buffer to play out. Server-side gain
      stays as fallback.
