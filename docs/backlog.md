# Backlog

## Ideas

- [ ] **Instant volume via OBS WebSocket** — the only way to get truly instant
      volume changes: the VS Code extension (or the backend) talks to OBS over
      its WebSocket API and adjusts the Media Source gain client-side, instead
      of waiting for the client's audio buffer to play out. Server-side gain
      stays as fallback.
- [ ] **Settings page: migrate to SvelteKit (static adapter) if it grows
      complex** — the settings SPA is currently a single vanilla HTML file
      embedded via `include_str!`. If it gains non-trivial state or UI, migrate
      to SvelteKit with the static adapter and embed the built assets the same
      way.
- [ ] **Two playlist collections: "for stream" vs "for me"** — split playlists
      into two collections and switch between them with one click: (1)
      stream-safe music (DMCA-free, routed to OBS), (2) personal music (any
      radio/local, routed to a different output). Requires deciding how the
      "other output" works for personal listening (second HTTP stream, local
      audio device, or just swapping what OBS hears).
- [ ] **Two streams** — one play music for live twitch (track 1), one for vod
      (track 2).
- [ ] **Settings for local files** — shuffle, song title
