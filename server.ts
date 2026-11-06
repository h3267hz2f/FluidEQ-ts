import 'dotenv/config';
import express from 'express';
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import { createServer as createViteServer } from 'vite';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const OPRA_CURVES_DIR = path.resolve(__dirname, 'opra', 'curves');

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

  app.get('/api/health', (_req, res) => {
    res.json({ status: 'ok', engine: 'FluidEQ Engine 1.25', version: '2.1.0' });
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
