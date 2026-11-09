import 'dotenv/config';
import { ChildProcessWithoutNullStreams, spawn } from 'child_process';
import express from 'express';
import fs from 'fs';
import path from 'path';
import readline from 'readline';
import { fileURLToPath } from 'url';
import { createServer as createViteServer } from 'vite';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const OPRA_CURVES_DIR = path.resolve(__dirname, 'opra', 'curves');
const FLUIDEQ_SHELL_BIN = path.resolve(__dirname, 'target', 'debug', 'fluideq-shell');

interface NativeIpcResponse {
  kind: string;
  data: Record<string, unknown>;
}

class NativeEngineBridge {
  private child: ChildProcessWithoutNullStreams | null = null;
  private pendingQueue: Array<(res: NativeIpcResponse) => void> = [];

  private ensureProcess(): ChildProcessWithoutNullStreams | null {
    if (this.child && !this.child.killed && this.child.exitCode === null) {
      return this.child;
    }
    if (!fs.existsSync(FLUIDEQ_SHELL_BIN)) {
      return null;
    }
    const child = spawn(FLUIDEQ_SHELL_BIN, ['--ipc-stdio'], {
      cwd: __dirname,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    const rl = readline.createInterface({ input: child.stdout });
    rl.on('line', (line: string) => {
      const resolver = this.pendingQueue.shift();
      if (!resolver) return;
      try {
        const parsed = JSON.parse(line) as NativeIpcResponse;
        resolver(parsed);
      } catch (err) {
        resolver({
          kind: 'Error',
          data: { message: `Invalid JSON from native engine: ${String(err)}` },
        });
      }
    });
    child.on('exit', () => {
      this.child = null;
      while (this.pendingQueue.length > 0) {
        const resolver = this.pendingQueue.shift();
        if (resolver) {
          resolver({
            kind: 'Error',
            data: { message: 'Native engine process exited unexpectedly' },
          });
        }
      }
    });
    child.on('error', (err) => {
      this.child = null;
      while (this.pendingQueue.length > 0) {
        const resolver = this.pendingQueue.shift();
        if (resolver) {
          resolver({
            kind: 'Error',
            data: { message: `Native engine process error: ${err.message}` },
          });
        }
      }
    });
    this.child = child;
    return child;
  }

  public sendCommand(command: Record<string, unknown>): Promise<NativeIpcResponse> {
    const proc = this.ensureProcess();
    if (!proc) {
      return Promise.resolve({
        kind: 'Error',
        data: { message: 'Native fluideq-shell binary not found' },
      });
    }
    return new Promise<NativeIpcResponse>((resolve) => {
      this.pendingQueue.push(resolve);
      proc.stdin.write(`${JSON.stringify(command)}\n`, (err) => {
        if (err) {
          const idx = this.pendingQueue.indexOf(resolve);
          if (idx !== -1) {
            this.pendingQueue.splice(idx, 1);
          }
          resolve({
            kind: 'Error',
            data: { message: `Failed to write to native engine stdin: ${err.message}` },
          });
        }
      });
    });
  }
}

const nativeBridge = new NativeEngineBridge();

function formatBrandLabel(slug: string): string {
  const special: Record<string, string> = {
    '1custom': '1Custom',
    '1more': '1MORE',
    '634ears': '634ears',
    '64_audio': '64 Audio',
    '7hz': '7Hz',
    '7th_acoustics': '7th Acoustics',
    aaw: 'AAW',
    aful: 'AFUL',
    aiaiai: 'AIAIAI',
    akg: 'AKG',
    anker: 'Anker Soundcore',
    apple: 'Apple',
    astell_kern: 'Astell&Kern',
    asus_rog: 'ASUS ROG',
    audeze: 'Audeze',
    audio_technica: 'Audio-Technica',
    bang_olufsen: 'Bang & Olufsen',
    beyerdynamic: 'beyerdynamic',
    blon: 'BLON',
    bose: 'Bose',
    bowers_wilkins: 'Bowers & Wilkins',
    campfire_audio: 'Campfire Audio',
    cca: 'CCA',
    cmf: 'CMF by Nothing',
    corsair: 'Corsair',
  };
  if (special[slug]) {
    return special[slug];
  }
  return slug
    .split('_')
    .map((word) => (word.length > 0 ? word[0].toUpperCase() + word.slice(1) : word))
    .join(' ');
}

async function startServer() {
  const app = express();
  const PORT = Number(process.env.PORT) || 3000;

  app.use(express.json({ limit: '5mb' }));

  // Serve bundled repository assets and documentation previews
  app.use('/assets', express.static(path.resolve(__dirname, 'assets')));
  app.use('/docs', express.static(path.resolve(__dirname, 'docs')));

  app.get('/api/health', async (_req, res) => {
    const snap = await nativeBridge.sendCommand({ cmd: 'GetSnapshot' });
    const isConnected = snap.kind === 'Snapshot';
    res.json({
      status: 'ok',
      engine: 'FluidEQ Engine 1.25',
      version: '2.1.0',
      nativeBridgeActive: isConnected,
      nativeAbiVersion: isConnected ? snap.data.native_abi_version : null,
      nativeActiveStages: isConnected ? snap.data.native_active_stages : null,
      nativeLatencyFrames: isConnected ? snap.data.native_latency_frames : null,
      nativeWorkingSetBytes: isConnected ? snap.data.native_working_set_bytes : null,
      revision: isConnected ? snap.data.revision : null,
    });
  });

  app.get('/api/engine/snapshot', async (_req, res) => {
    const response = await nativeBridge.sendCommand({ cmd: 'GetSnapshot' });
    if (response.kind === 'Error') {
      res.status(503).json(response);
      return;
    }
    res.json(response);
  });

  app.post('/api/engine/ipc', async (req, res) => {
    const body = (req.body ?? {}) as Record<string, unknown>;
    const cmd = typeof body.cmd === 'string' ? body.cmd : '';
    if (!cmd) {
      res.status(400).json({ kind: 'Error', data: { message: 'Missing IPC cmd' } });
      return;
    }
    let payload = body.payload;
    if (
      cmd === 'ApplyOpraHeadphoneCurve' &&
      payload &&
      typeof payload === 'object' &&
      !Array.isArray(payload)
    ) {
      const payloadObj = payload as Record<string, unknown>;
      if (!payloadObj.opra_dir) {
        payload = { ...payloadObj, opra_dir: OPRA_CURVES_DIR };
      }
    }
    const ipcMessage: Record<string, unknown> =
      payload !== undefined ? { cmd, payload } : { cmd };
    const response = await nativeBridge.sendCommand(ipcMessage);
    if (response.kind === 'Error') {
      res.status(400).json(response);
      return;
    }
    res.json(response);
  });

  app.get('/api/opra/brands', async (_req, res) => {
    try {
      const files = await fs.promises.readdir(OPRA_CURVES_DIR);
      const brands = files
        .filter((file) => file.endsWith('.json'))
        .sort()
        .map((file) => {
          const slug = file.replace(/\.json$/, '');
          return {
            slug,
            name: formatBrandLabel(slug),
          };
        });
      res.json({ brands, totalBrands: brands.length });
    } catch (error) {
      console.error('Failed to read OPRA brands directory:', error);
      res.status(500).json({ error: 'Unable to read OPRA headphone directory' });
    }
  });

  app.get('/api/opra/brand/:slug', async (req, res) => {
    const rawSlug = req.params.slug ?? '';
    const safeSlug = rawSlug.replace(/[^a-z0-9_]/gi, '');
    if (!safeSlug) {
      res.status(400).json({ error: 'Invalid brand identifier' });
      return;
    }
    const targetPath = path.join(OPRA_CURVES_DIR, `${safeSlug}.json`);
    try {
      const content = await fs.promises.readFile(targetPath, 'utf-8');
      const parsed: unknown = JSON.parse(content);
      res.json({
        slug: safeSlug,
        name: formatBrandLabel(safeSlug),
        models: parsed,
      });
    } catch (error) {
      console.error(`Failed to read OPRA curve file for ${safeSlug}:`, error);
      res.status(404).json({ error: 'Brand curves not found' });
    }
  });

  if (process.env.NODE_ENV !== 'production') {
    const vite = await createViteServer({
      server: { middlewareMode: true },
      appType: 'spa',
    });
    app.use(vite.middlewares);
  } else {
    const distPath = path.resolve(__dirname, 'dist');
    app.use(express.static(distPath));
    app.get('*', (_req, res) => {
      res.sendFile(path.join(distPath, 'index.html'));
    });
  }

  app.listen(PORT, '0.0.0.0', () => {
    // Server bound on 0.0.0.0:3000
  });
}

startServer();
