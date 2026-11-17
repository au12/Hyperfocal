// Precompress the build output so the server can send .br/.gz files directly
// (tower-http ServeDir picks them up) instead of compressing on every request.
import { readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { brotliCompressSync, gzipSync, constants } from 'node:zlib';

const root = new URL('../dist/', import.meta.url).pathname;
const exts = /\.(js|mjs|css|html|svg|json|webmanifest|txt|map)$/;
let n = 0;
let before = 0;
let after = 0;

function walk(dir) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    const st = statSync(p);
    if (st.isDirectory()) walk(p);
    else if (exts.test(name) && st.size > 512) {
      const buf = readFileSync(p);
      const br = brotliCompressSync(buf, {
        params: {
          [constants.BROTLI_PARAM_QUALITY]: 11,
          [constants.BROTLI_PARAM_SIZE_HINT]: buf.length,
        },
      });
      writeFileSync(p + '.br', br);
      writeFileSync(p + '.gz', gzipSync(buf, { level: 9 }));
      n++;
      before += buf.length;
      after += br.length;
    }
  }
}

walk(root);
console.log(`precompressed ${n} files: ${(before / 1024).toFixed(0)} KB -> ${(after / 1024).toFixed(0)} KB (brotli)`);
