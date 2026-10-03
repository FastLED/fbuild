"""Cache-save classification must preserve failures and reject incomplete builds."""

import unittest

from ci.test_cache_status import TestEvidence


class TestCacheStatusTests(unittest.TestCase):
    def evidence(self, *lines: str) -> TestEvidence:
        result = TestEvidence()
        for line in lines:
            result = result.observe(line)
        return result

    def test_completed_assertion_failure(self):
        result = self.evidence('{"reason":"build-finished","success":true}',
                               'test result: FAILED. 0 passed; 1 failed;',
                               '    0.59 error: test failed, to rerun pass `--lib`',
                               '    0.60 soldr[cache] probe [MISS]')
        self.assertTrue(result.assertion_failure_only(101))
        self.assertFalse(result.assertion_failure_only(-15))
        self.assertFalse(result.assertion_failure_only(0))

    def test_compiler_failure_and_incomplete_output_reject(self):
        for build in ('{"reason":"build-finished","success":false}', '',
                      '{"reason":"compiler-message","message":{"level":"error"}}'):
            result = self.evidence(build, 'test result: FAILED.', 'error: test failed')
            self.assertFalse(result.assertion_failure_only(101))

    def test_doctest_compilation_failure_rejects(self):
        result = self.evidence('{"reason":"build-finished","success":true}',
                               "Couldn't compile the test.", 'test result: FAILED.',
                               'error: doctest failed, to rerun pass `--doc`')
        self.assertFalse(result.assertion_failure_only(101))

    def test_unrelated_terminal_failure_rejects(self):
        result = self.evidence('{"reason":"build-finished","success":true}',
                               'test result: FAILED.', 'error: could not compile `x`')
        self.assertFalse(result.assertion_failure_only(101))
