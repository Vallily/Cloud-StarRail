import { spawnSync } from 'node:child_process';
import { constants, copyFileSync, existsSync, mkdirSync, openSync, closeSync, readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { buildPlan, assertNewVersion } from './build-plan.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const release = join(root, 'src-tauri', 'target', 'release');
const bundles = join(release, 'bundle', 'nsis');
const config = JSON.parse(readFileSync(join(root, 'src-tauri', 'tauri.conf.json'), 'utf8'));
const plan = buildPlan(root, config.version);

try {
  // The old dist root and all previous version directories are left untouched.
  assertNewVersion(plan);
  if (!process.argv.includes('--publish-only')) {
    const binary = join(release, 'cloud-starrail.exe');
    if (existsSync(binary)) {
      try { closeSync(openSync(binary, 'r+')); }
      catch { throw new Error('内部构建文件正在使用，请关闭该客户端后重新构建。'); }
    }
    const cli = join(root, 'node_modules', '@tauri-apps', 'cli', 'tauri.js');
    const build = spawnSync(process.execPath, [cli, 'build'], { cwd: root, stdio: 'inherit' });
    if (build.error) throw build.error;
    if (build.status !== 0) process.exit(build.status ?? 1);
  }
  const installer = join(bundles, `${config.productName}_${config.version}_x64-setup.exe`);
  const executable = join(release, 'cloud-starrail.exe');
  if (!existsSync(installer) || !existsSync(executable)) throw new Error('找不到当前版本的构建产物。');
  mkdirSync(join(root, 'dist'), { recursive: true });
  // Exclusive directory/file creation also prevents concurrent builds overwriting a release.
  mkdirSync(plan.output);
  copyFileSync(executable, plan.appFile, constants.COPYFILE_EXCL);
  copyFileSync(installer, plan.installerFile, constants.COPYFILE_EXCL);
  const sums = [plan.appFile, plan.installerFile].map(file =>
    `${createHash('sha256').update(readFileSync(file)).digest('hex')}  ${file.slice(plan.output.length + 1)}`);
  writeFileSync(join(plan.output, 'SHA256SUMS.txt'), `${sums.join('\n')}\n`, { flag: 'wx' });
  console.log(`\n版本目录：${plan.output}\n程序：${plan.appFile}\n安装包：${plan.installerFile}`);
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}