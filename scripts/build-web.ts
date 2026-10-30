import { createHash } from 'node:crypto';
import type { BinaryLike } from 'node:crypto';
import { access, copyFile, readFile, readdir, unlink, writeFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = new URL('../', import.meta.url);
const assets = new URL('web/assets/', root);
const native = new URL('native/', assets);
const pages = ['index.html', 'creators.html', 'deorphanize.html', 'connections.html'];
const digest = (bytes: BinaryLike): string => createHash('sha256').update(bytes).digest('hex').slice(0, 12);

function run(command: string, args: string[]): void {
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
run(generator, ['--target', 'web', '--no-typescript', '--out-dir', 'web/assets/native', '--out-name', 'dashboard', 'target/wasm32-unknown-unknown/release/orphanwiki.wasm']);

const wasm = await readFile(new URL('dashboard_bg.wasm', native));
const wasmName = `dashboard_${digest(wasm)}.wasm`;
const bindings = (await readFile(new URL('dashboard.js', native), 'utf8')).replaceAll('dashboard_bg.wasm', wasmName);
await writeFile(new URL('dashboard.js', native), bindings);
await writeFile(new URL(wasmName, native), wasm);
for (const name of await readdir(native)) {
  if ((name.endsWith('.wasm') && name !== wasmName) || name.endsWith('.d.ts')) await unlink(new URL(name, native));
}

run(process.execPath, ['node_modules/typescript/bin/tsc']);
// Version the complete module graph so a cached entry point never imports an
// older validator/controller after a new build.
const modules = new Map<string, string>();
async function prepareModule(name: string, ancestors: Set<string> = new Set()): Promise<string> {
  const cached = modules.get(name);
  if (cached !== undefined) return cached;
  if (ancestors.has(name)) throw new Error(`Circular browser module dependency: ${name}`);
  const source = await readFile(new URL(name, assets), 'utf8');
  const dependencies = [...source.matchAll(/from '(\.\/([a-z-]+\.js))';/g)];
  let compiled = source;
  for (const dependency of dependencies) {
    const child = await prepareModule(dependency[2], new Set([...ancestors, name]));
    compiled = compiled.replace(dependency[0], `from '${dependency[1]}?v=${digest(child)}';`);
  }
  modules.set(name, compiled);
  await writeFile(new URL(name, assets), compiled);
  return compiled;
}
const appPath = new URL('app.js', assets);
const compiled = (await prepareModule('app.js')).replace(/^['"]use strict['"];\s*/, '');
await writeFile(appPath, `'use strict';\nconst nativeModuleUrl='./native/dashboard.js?v=${digest(bindings)}';\n${compiled}`);
await copyFile(new URL('web/src/style.css', root), new URL('style.css', assets));

for (const asset of ['app.js', 'style.css']) {
  const version = digest(await readFile(new URL(asset, assets)));
  const reference = new RegExp(`assets/${asset.replace('.', '\\.')}\\?v=[a-f0-9]+`, 'g');
  for (const page of asset === 'style.css' ? [...pages, 'about.html'] : pages) {
    const path = new URL(`web/${page}`, root);
    const html = await readFile(path, 'utf8');
    if (!reference.test(html)) throw new Error(`${page}: missing ${asset} reference`);
    reference.lastIndex = 0;
    await writeFile(path, html.replace(reference, `assets/${asset}?v=${version}`));
  }
}
