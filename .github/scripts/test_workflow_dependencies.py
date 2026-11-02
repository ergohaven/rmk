"""Ubuntu runners must provide the tools selected by each CI owner."""
import pathlib
import unittest

import yaml

ROOT = pathlib.Path(__file__).resolve().parents[2]


class RunnerDependencyTests(unittest.TestCase):
    def jobs(self):
        return yaml.safe_load((ROOT / '.github/workflows/ci.yml').read_text())['jobs']

    def test_qemu_uses_the_ubuntu_riscv_provider(self):
        self.assertEqual(self.jobs()['qemu-rynk']['runs-on'], 'ubuntu-24.04')
        steps = self.jobs()['qemu-rynk']['steps']
        install = next(step['run'] for step in steps if step.get('name') == 'Install QEMU')
        self.assertIn('qemu-system-misc', install)
        self.assertNotIn('install -y qemu-system-riscv ', install)
        self.assertIn('qemu-system-riscv32 --version', install)

    def test_espup_uses_only_builtin_token_at_install_step(self):
        workflow = yaml.safe_load((ROOT / '.github/workflows/ci.yml').read_text())
        job = workflow['jobs']['build-esp']
        install = next(step for step in job['steps']
                       if step.get('name') == 'Install espup and esp toolchain')
        self.assertEqual(install.get('env'), {'GITHUB_TOKEN': '${{ secrets.GITHUB_TOKEN }}'})
        self.assertEqual(install['run'], 'cargo binstall --force espup -y\nespup install\n')
        # Do not expose the token to example builds or broaden permissions.
        self.assertNotIn('GITHUB_TOKEN', workflow.get('env', {}))
        self.assertNotIn('GITHUB_TOKEN', job.get('env', {}))
        for step in job['steps']:
            if step is not install:
                self.assertNotIn('GITHUB_TOKEN', step.get('env', {}))
        self.assertNotIn('permissions', job)

    def test_nordic_bindgen_uses_installed_distro_llvm(self):
        job = self.jobs()['build']
        self.assertNotIn('LIBCLANG_PATH', job.get('env', {}))
        self.assertNotIn('CLANG_PATH', job.get('env', {}))
        install = next(step for step in job['steps'] if step.get('name') == 'Install ARM build dependencies')
        self.assertEqual(install['if'], "contains(matrix.dir, 'nrf')")
        for package in ('gcc-arm-none-eabi', 'clang', 'libclang-dev', 'llvm-dev'):
            self.assertIn(package, install['run'])
        self.assertIn('/usr/bin/llvm-config --libdir', install['run'])
        self.assertIn('CLANG_PATH=/usr/bin/clang', install['run'])
        self.assertIn('LIBCLANG_PATH=%s', install['run'])
        self.assertIn('GITHUB_ENV', install['run'])
        self.assertIn('test -e "$libclang_dir/libclang.so"', install['run'])


if __name__ == '__main__':
    unittest.main()
