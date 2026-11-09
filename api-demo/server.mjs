// Tiny static server for the Voice Button API demo page.
//
// It serves this folder only: the page talks to the Rust app's API on
// 127.0.0.1:8931 by itself. No dependencies, no build step.
//
//   node api-demo/server.mjs            # http://127.0.0.1:8080
//   PORT=9000 node api-demo/server.mjs  # another port

import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('.', import.meta.url));
const port = Number(process.env.PORT || 8080);
const api = process.env.VOICE_BUTTON_API || 'http://127.0.0.1:8931';

const types = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
};

createServer(async (request, response) => {
  const url = new URL(request.url, `http://${request.headers.host || 'localhost'}`);
  const relative = normalize(url.pathname === '/' ? '/index.html' : url.pathname)
    .replace(/^[/\\]+/, '');
  const path = join(root, relative);
  // `normalize` collapses `..`, and the prefix check keeps the server inside
  // this folder.
  if (!path.startsWith(root)) {
    response.writeHead(403, { 'Content-Type': 'text/plain' });
    response.end('forbidden');
    return;
  }
  try {
    const body = await readFile(path);
    response.writeHead(200, {
      'Content-Type': types[extname(path)] || 'application/octet-stream',
      'Cache-Control': 'no-store',
    });
    response.end(body);
  } catch {
    response.writeHead(404, { 'Content-Type': 'text/plain; charset=utf-8' });
    response.end('not found');
  }
}).listen(port, '127.0.0.1', () => {
  console.log(`Voice Button API demo: http://127.0.0.1:${port}/`);
  console.log(`API expected at: ${api} (enable it in the app's API tab)`);
});
