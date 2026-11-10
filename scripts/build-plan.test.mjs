import test from 'node:test';
import assert from 'node:assert/strict';
import { buildPlan, assertNewVersion } from './build-plan.mjs';

test('versions have separate output directories and stable filenames', () => {
  const old = buildPlan('/project', '0.3.0');
  const next = buildPlan('/project', '0.4.0-alpha.1');
  assert.notEqual(next.output, old.output);
  assert.ok(next.appFile.endsWith('云·星穹铁道.exe'));
  assert.ok(next.installerFile.endsWith('云·星穹铁道-安装.exe'));
});
test('an existing release is rejected before any build or copy', () => {
  assert.throws(() => assertNewVersion(buildPlan('/project', '0.4.0-alpha.1'), () => true), /不会覆盖/);
  assert.doesNotThrow(() => assertNewVersion(buildPlan('/project', '0.4.0-alpha.1'), () => false));
});
test('version input cannot escape the release directory', () => {
  for (const version of ['../old', '1.0.0/../../old', '', 'latest']) {
    assert.throws(() => buildPlan('/project', version), /无效/);
  }
});