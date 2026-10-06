import { createHash } from 'node:crypto';
import { access, copyFile, readFile, readdir, unlink, writeFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = new URL('../', import.meta.url);
const assets = new URL('web/assets/', root);
const native = new URL('native/', assets);
const pages = ['index.html', 'creators.html', 'deorphanize.html', 'connections.html'];
const digest = bytes => createHash('sha256').update(bytes).digest('hex').slice(0, 12);

function run(command, args) {
  const result = spawnSync(command, args, { cwd: fileURLToPath(root), stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}

run('cargo', ['build', '--release', '--locked', '--no-default-features', '--features', 'dashboard', '--target', 'wasm32-unknown-unknown']);

let generator = process.env.WASM_BINDGEN;
if (!generator) {
  const cached = new URL('.cache/wasm-bindgen/wasm-bindgen', root);
  try {
    await access(cached);
    generator = fileURLToPath(cached);
  } catch {
    generator = 'wasm-bindgen';
  }
}
run(generator, ['--target', 'web', '--no-typescript', '--out-dir', 'web/assets/native', '--out-name', 'dashboard', 'target/wasm32-unknown-unknown/release/_native.wasm']);

const wasm = await readFile(new URL('dashboard_bg.wasm', native));
const wasmName = `dashboard_${digest(wasm)}.wasm`;
const bindings = (await readFile(new URL('dashboard.js', native), 'utf8')).replaceAll('dashboard_bg.wasm', wasmName);
await writeFile(new URL('dashboard.js', native), bindings);
await writeFile(new URL(wasmName, native), wasm);
for (const name of await readdir(native)) {
  if ((name.endsWith('.wasm') && name !== wasmName) || name.endsWith('.d.ts')) await unlink(new URL(name, native));
}

run(process.execPath, ['node_modules/typescript/bin/tsc']);
const appPath = new URL('app.js', assets);
const compiled = (await readFile(appPath, 'utf8')).replace(/^['"]use strict['"];\s*/, '');
await writeFile(appPath, `'use strict';\nconst nativeModuleUrl='./native/dashboard.js?v=${digest(bindings)}';\n${compiled}`);
await copyFile(new URL('web/src/style.css', root), new URL('style.css', assets));

for (const asset of ['app.js', 'style.css']) {
  const version = digest(await readFile(new URL(asset, assets)));
  const reference = new RegExp(`assets/${asset.replace('.', '\\.')}\\?v=[a-f0-9]+`, 'g');
  for (const page of pages) {
    const path = new URL(`web/${page}`, root);
    const html = await readFile(path, 'utf8');
    if (!reference.test(html)) throw new Error(`${page}: missing ${asset} reference`);
    reference.lastIndex = 0;
    await writeFile(path, html.replace(reference, `assets/${asset}?v=${version}`));
  }
}
