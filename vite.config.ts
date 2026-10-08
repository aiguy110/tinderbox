import { cpSync, createReadStream, existsSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { join, normalize } from 'node:path';
import { defineConfig, type Plugin } from 'vite';

// Generated binary assets (sprite atlases) live outside the repo. See CLAUDE.md.
const dataDir = process.env.TINDERBOX_DATA ?? join(homedir(), '.local/share/tinderbox');
const atlasDir = join(dataDir, 'atlas');

/** Serve `/atlas/*` from the data dir in dev, and copy it into `dist/` on build. */
function externalAtlas(): Plugin {
  let outDir = 'dist';
  return {
    name: 'external-atlas',
    configResolved(c) {
      outDir = c.build.outDir;
    },
    configureServer(server) {
      server.middlewares.use('/atlas', (req, res, next) => {
        const file = normalize(join(atlasDir, decodeURIComponent((req.url ?? '/').split('?')[0])));
        if (!file.startsWith(atlasDir) || !existsSync(file) || !statSync(file).isFile()) return next();
        res.setHeader('Cache-Control', 'no-cache');
        createReadStream(file).pipe(res);
      });
    },
    closeBundle() {
      if (existsSync(atlasDir)) cpSync(atlasDir, join(outDir, 'atlas'), { recursive: true });
    },
  };
}

export default defineConfig({ plugins: [externalAtlas()] });
