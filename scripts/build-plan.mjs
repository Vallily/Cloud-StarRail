import { existsSync } from 'node:fs';
import { join } from 'node:path';

export function buildPlan(root, version, revision) {
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) {
    throw new Error('无效的版本号，无法创建版本目录。');
  }
  if (revision !== undefined && (!Number.isSafeInteger(revision) || revision < 1)) {
    throw new Error('无效的构建序号。');
  }
  const output = join(root, 'dist', `v${version}${revision === undefined ? '' : `-build.${revision}`}`);
  return {
    output,
    appFile: join(output, '云·星穹铁道.exe'),
    installerFile: join(output, '云·星穹铁道-安装.exe'),
  };
}

export function assertNewVersion(plan, exists = existsSync) {
  if (exists(plan.output)) {
    throw new Error(`该版本目录已存在，为保留原版不会覆盖：${plan.output}。请指定新的构建序号或版本号。`);
  }
}