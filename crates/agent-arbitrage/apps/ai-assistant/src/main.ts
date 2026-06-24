import { createApp } from './server.js';
import { loadConfig } from './config.js';

const cfg = loadConfig();
const app = createApp(cfg);

app.listen(cfg.port, () => {
  // eslint-disable-next-line no-console
  console.log(`[ai-assistant] listening on :${cfg.port} (provider=${cfg.provider}, enabled=${cfg.enabled}) — advisory only`);
});
