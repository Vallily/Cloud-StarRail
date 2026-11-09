import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, openSync, closeSync, readdirSync, unlinkSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const release = join(root, 'src-tauri', 'target', 'release');
const bundles = join(release, 'bundle', 'nsis');
const output = join(root, 'dist');
const appName = '云·星穹铁道';
const appFile = join(output, `${appName}.exe`);
const installerFile = join(output, `${appName}-安装.exe`);

function ensureWritable(file) {
  if (!existsSync(file)) return;
  try { closeSync(openSync(file, 'r+')); }
  catch { throw new Error(`文件正在使用或不可写，请关闭程序后再构建：${file}`); }
}

function cleanup(directory, pattern) {
  if (!existsSync(directory)) return;
  for (const name of readdirSync(directory)) {
    if (!pattern.test(name)) continue;
    try { unlinkSync(join(directory, name)); }
    catch { console.warn(`暂时无法清理（可能仍在运行），下次构建会重试：${name}`); }
  }
}

try {
  ensureWritable(appFile);
  ensureWritable(installerFile);
  if (!process.argv.includes('--publish-only')) {
    ensureWritable(join(release, 'cloud-starrail.exe'));
    const cli = join(root, 'node_modules', '@tauri-apps', 'cli', 'tauri.js');
    const build = spawnSync(process.execPath, [cli, 'build'], { cwd: root, stdio: 'inherit' });
    if (build.error) throw build.error;
    if (build.status !== 0) process.exit(build.status ?? 1);
  }
  const { default: config } = await import('../src-tauri/tauri.conf.json', { with: { type: 'json' } });
  const installer = join(bundles, `${appName}_${config.version}_x64-setup.exe`);
  if (!existsSync(installer) || !existsSync(join(release, 'cloud-starrail.exe'))) {
    throw new Error('找不到当前版本的构建产物，请运行 npm run build。');
  }
  mkdirSync(output, { recursive: true });
  copyFileSync(join(release, 'cloud-starrail.exe'), appFile);
  copyFileSync(installer, installerFile);
  // Only remove known generated artifacts, after both stable deliverables exist.
  cleanup(release, /^(starrail-cloud-desktop|cloud-starrail-previous-\d+)\.exe$/);
  cleanup(bundles, /^(云·星穹铁道|云星铁桌面端)_\d+\.\d+\.\d+_x64-setup\.exe$/);
  console.log(`\n程序：${appFile}\n安装包：${installerFile}`);
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}